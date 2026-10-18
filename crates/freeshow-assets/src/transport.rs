use crate::{io, spec, Connection, Control, Error, Shared, Stats};
use bytes::Bytes;
use futures_core::Stream;
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full, StreamBody};
use hyper::{
    body::{Frame, Incoming},
    header,
    server::conn::http1,
    service::service_fn,
    Method, Request, Response, StatusCode,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::{atomic::Ordering, Arc},
    task::{Context, Poll},
    time::Instant,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

type Body = UnsyncBoxBody<Bytes, std::io::Error>;
type ReadFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, Error>> + Send>>;
struct Buffer {
    bytes: Vec<u8>,
    stats: Arc<Stats>,
}
impl AsRef<[u8]> for Buffer {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        self.stats
            .buffered
            .fetch_sub(self.bytes.len(), Ordering::AcqRel);
    }
}
struct FileStream {
    pool: io::Pool,
    handle: io::Handle,
    offset: u64,
    left: u64,
    chunk: usize,
    control: Control,
    pending: Option<ReadFuture>,
    stats: Arc<Stats>,
}
impl Stream for FileStream {
    type Item = Result<Frame<Bytes>, std::io::Error>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.left == 0 {
            return Poll::Ready(None);
        }
        if this.control.cancel.is_cancelled() || Instant::now() >= this.control.deadline {
            this.left = 0;
            return Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Asset stream cancelled or expired",
            ))));
        }
        if this.pending.is_none() {
            let pool = this.pool.clone();
            let handle = this.handle.clone();
            let offset = this.offset;
            let count = this.left.min(this.chunk as u64) as usize;
            let control = this.control.clone();
            this.pending = Some(Box::pin(async move {
                pool.read(handle, offset, count, control).await
            }));
        }
        let ready = match this.pending.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(ready) => ready,
        };
        this.pending.take();
        match ready {
            Ok(buffer) => {
                this.offset += buffer.len() as u64;
                this.left -= buffer.len() as u64;
                let size = this
                    .stats
                    .buffered
                    .fetch_add(buffer.len(), Ordering::AcqRel)
                    + buffer.len();
                this.stats.peak_buffered.fetch_max(size, Ordering::AcqRel);
                Poll::Ready(Some(Ok(Frame::data(Bytes::from_owner(Buffer {
                    bytes: buffer,
                    stats: this.stats.clone(),
                })))))
            }
            Err(error) => {
                this.left = 0;
                Poll::Ready(Some(Err(std::io::Error::other(error))))
            }
        }
    }
}
struct ConnectionGuard {
    shared: Arc<Shared>,
    id: u64,
    cancel: CancellationToken,
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Ok(mut registry) = self.shared.registry.lock() {
            registry.connections.remove(&self.id);
        }
    }
}

fn empty(status: u16) -> Response<Body> {
    let mut response = Response::new(
        Full::new(Bytes::new())
            .map_err(|never: Infallible| match never {})
            .boxed_unsync(),
    );
    *response.status_mut() = StatusCode::from_u16(status).unwrap();
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        header::HeaderValue::from_static("0"),
    );
    response
}
fn secure_headers(response: &mut Response<Body>, origin: Option<&str>) {
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        header::CONNECTION,
        header::HeaderValue::from_static("close"),
    );
    headers.insert(header::VARY, header::HeaderValue::from_static("Origin"));
    headers.insert(
        "x-content-type-options",
        header::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "referrer-policy",
        header::HeaderValue::from_static("no-referrer"),
    );
    if let Some(origin) = origin {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            header::HeaderValue::from_str(origin).unwrap(),
        );
        headers.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            header::HeaderValue::from_static(
                "Accept-Ranges, Content-Range, Content-Length, Content-Type",
            ),
        );
    }
}
fn secret_equal(given: &str, expected: &str) -> bool {
    if given.len() != expected.len() {
        return false;
    }
    given
        .bytes()
        .zip(expected.bytes())
        .fold(0, |difference, (a, b)| difference | (a ^ b))
        == 0
}
fn error_status(error: &Error) -> u16 {
    match error.code.as_str() {
        "assets_missing" | "assets_changed" | "assets_invalid" | "assets_closed" => 404,
        "assets_permission" => 403,
        "assets_busy" | "assets_shutdown" => 503,
        "assets_limit" => 413,
        "assets_deadline" => 504,
        _ => 500,
    }
}

async fn handle(
    shared: Arc<Shared>,
    connection: u64,
    cancel: CancellationToken,
    deadline: Instant,
    request: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    let result = async {
        if request.headers().get_all(header::HOST).iter().count() != 1
            || request
                .headers()
                .get(header::HOST)
                .and_then(|value| value.to_str().ok())
                != Some(&shared.authority)
            || request
                .uri()
                .authority()
                .is_some_and(|authority| authority.as_str() != shared.authority)
        {
            return empty(400);
        }
        if request.headers().get_all(header::ORIGIN).iter().count() > 1
            || (request.headers().contains_key(header::ORIGIN) && origin.is_none())
            || origin.is_some_and(|origin| {
                !shared
                    .config
                    .allowed_origins
                    .iter()
                    .any(|allowed| allowed == origin)
            })
        {
            return empty(403);
        }
        let Some((token, id)) = request
            .uri()
            .path()
            .strip_prefix('/')
            .and_then(|path| path.split_once('/'))
        else {
            return empty(404);
        };
        if !secret_equal(token, &shared.token)
            || id.len() != 32
            || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return empty(404);
        }
        let entry = {
            let mut registry = match shared.registry.lock() {
                Ok(registry) => registry,
                Err(_) => return empty(500),
            };
            let Some(entry) = registry.grants.get(id).cloned() else {
                return empty(404);
            };
            if let Some(active) = registry.connections.get_mut(&connection) {
                active.grant = Some(entry.public.id.clone());
            }
            if cancel.is_cancelled() {
                return empty(410);
            }
            entry
        };
        if request.method() == Method::OPTIONS {
            if origin.is_none() {
                return empty(403);
            }
            let method = request
                .headers()
                .get(header::ACCESS_CONTROL_REQUEST_METHOD)
                .and_then(|value| value.to_str().ok());
            if !matches!(method, Some("GET" | "HEAD")) {
                return empty(405);
            }
            if request
                .headers()
                .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
                .is_some_and(|value| {
                    value.to_str().map_or(true, |value| {
                        value
                            .split(',')
                            .any(|header| !header.trim().eq_ignore_ascii_case("range"))
                    })
                })
            {
                return empty(403);
            }
            let mut response = empty(204);
            response.headers_mut().insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                header::HeaderValue::from_static("GET, HEAD, OPTIONS"),
            );
            response.headers_mut().insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                header::HeaderValue::from_static("Range"),
            );
            if request
                .headers()
                .get("access-control-request-private-network")
                .is_some_and(|value| value == "true")
            {
                response.headers_mut().insert(
                    "access-control-allow-private-network",
                    header::HeaderValue::from_static("true"),
                );
            }
            return response;
        }
        if request.method() != Method::GET && request.method() != Method::HEAD {
            let mut response = empty(405);
            response.headers_mut().insert(
                header::ALLOW,
                header::HeaderValue::from_static("GET, HEAD, OPTIONS"),
            );
            return response;
        }
        let ranges = request
            .headers()
            .get_all(header::RANGE)
            .iter()
            .map(|value| value.to_str())
            .collect::<Result<Vec<_>, _>>();
        let ranges = match ranges {
            Ok(ranges) => ranges.join(", "),
            Err(_) => return empty(400),
        };
        let control = Control {
            deadline,
            cancel,
            epoch: None,
        };
        let opened = match shared
            .pool
            .open(entry.path, Some(entry.identity), control.clone())
            .await
        {
            Ok(opened) => opened,
            Err(error) => return empty(error_status(&error)),
        };
        let range = spec::parse_range(Some(&ranges), opened.length);
        if range.status_code == 416 {
            let mut response = empty(416);
            response.headers_mut().insert(
                header::ACCEPT_RANGES,
                header::HeaderValue::from_static("bytes"),
            );
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                header::HeaderValue::from_str(&format!("bytes */{}", opened.length)).unwrap(),
            );
            return response;
        }
        let count = if opened.length == 0 {
            0
        } else {
            range.end - range.start + 1
        };
        if count > shared.config.max_response_bytes {
            return empty(413);
        }
        let mut response = empty(range.status_code);
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_str(&entry.public.mime_type).unwrap(),
        );
        headers.insert(
            header::CONTENT_LENGTH,
            header::HeaderValue::from_str(&count.to_string()).unwrap(),
        );
        headers.insert(
            header::ACCEPT_RANGES,
            header::HeaderValue::from_static("bytes"),
        );
        headers.insert(
            header::CONTENT_DISPOSITION,
            header::HeaderValue::from_static("inline"),
        );
        if range.status_code == 206 {
            headers.insert(
                header::CONTENT_RANGE,
                header::HeaderValue::from_str(&format!(
                    "bytes {}-{}/{}",
                    range.start, range.end, opened.length
                ))
                .unwrap(),
            );
        }
        if request.method() == Method::GET && count > 0 {
            *response.body_mut() = BodyExt::boxed_unsync(StreamBody::new(FileStream {
                pool: shared.pool.clone(),
                handle: opened.handle,
                offset: range.start,
                left: count,
                chunk: shared.config.chunk_bytes,
                control,
                pending: None,
                stats: shared.stats.clone(),
            }));
        }
        response
    }
    .await;
    let mut result = result;
    // Rejected origins receive no CORS authorization, even on errors.
    let allowed_origin = origin.filter(|origin| {
        shared
            .config
            .allowed_origins
            .iter()
            .any(|allowed| allowed == origin)
    });
    secure_headers(&mut result, allowed_origin);
    Ok(result)
}

pub(crate) async fn serve(listener: TcpListener, shared: Arc<Shared>) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = shared.stop.cancelled() => break,
            _ = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let Ok((mut stream, peer)) = accepted else { break };
                while connections.try_join_next().is_some() {}
                if !peer.ip().is_loopback() || connections.len() >= shared.config.max_connections {
                    shared.stats.rejected_connections.fetch_add(1, Ordering::Relaxed);
                    // One bounded rejection runs on the listener, without spawning an
                    // extra task. Drain the ordinary request header before close so
                    // an unread request does not turn the 503 into a TCP reset.
                    let _ = tokio::time::timeout(std::time::Duration::from_millis(25), async {
                        let mut header = [0u8; 1024];
                        let _ = stream.read(&mut header).await;
                        stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\nCache-Control: no-store\r\n\r\n").await?;
                        stream.shutdown().await
                    }).await;
                    continue
                }
                if socket2::SockRef::from(&stream).set_send_buffer_size(shared.config.socket_send_bytes).is_err() { continue }
                let _ = stream.set_nodelay(true);
                let number = shared.next_connection.fetch_add(1, Ordering::Relaxed);
                let cancel = CancellationToken::new();
                if let Ok(mut registry) = shared.registry.lock() { registry.connections.insert(number, Connection { grant: None, cancel: cancel.clone() }); } else { break }
                let connection_shared = shared.clone();
                connections.spawn(async move {
                    let _guard = ConnectionGuard { shared: connection_shared.clone(), id: number, cancel: cancel.clone() };
                    let deadline = Instant::now() + connection_shared.config.request_timeout;
                    let service_shared = connection_shared.clone(); let service_cancel = cancel.clone();
                    let service = service_fn(move |request| handle(service_shared.clone(), number, service_cancel.clone(), deadline, request));
                    let mut builder = http1::Builder::new();
                    builder.keep_alive(false).writev(false).timer(TokioTimer::new()).header_read_timeout(connection_shared.config.header_timeout).max_buf_size(connection_shared.config.header_bytes).max_headers(64);
                    tokio::select! {
                        _ = cancel.cancelled() => {},
                        _ = connection_shared.stop.cancelled() => {},
                        _ = tokio::time::sleep_until(deadline.into()) => {},
                        _ = builder.serve_connection(TokioIo::new(stream), service) => {},
                    }
                });
            }
        }
    }
    drop(listener);
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    shared.closing.store(true, Ordering::Release);
    shared.pool.close();
    if let Ok(mut registry) = shared.registry.lock() {
        registry.epoch.cancel();
        registry.grants.clear();
        registry.bytes = 0;
    }
    shared.done.send_replace(true);
}
