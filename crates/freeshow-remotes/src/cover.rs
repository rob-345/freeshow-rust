//! Native fixed-source HTTP preparation on the existing file pool. Requests
//! never select a file. Entered source/finish futures are owned by that pool.
use crate::{state::Run, static_files::PoolSender, RemoteError, Result, ServerKind};
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use freeshow_cover_http::{
    self as http, ErrorRendering, Headers, Method, Plan, Route, SourceState,
};
use std::{
    collections::BTreeMap,
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit};
use tokio_stream::Stream;
use tokio_util::sync::CancellationToken;

pub type CoverFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
#[derive(Clone)]
pub struct CoverControl {
    pub cancel: CancellationToken,
    pub deadline: Instant,
}
/// Trusted native per-start authority, never deserialized. Capture the frozen
/// library/context here; no request path/query/header is passed to prepare.
/// Even an Err must settle actual accepted native preparation/cleanup first.
pub trait CoverSource: Send + Sync + 'static {
    fn error_rendering(&self) -> ErrorRendering {
        ErrorRendering::Deferred
    }
    fn prepare(&self, control: CoverControl) -> CoverFuture<'_, Box<dyn PreparedCover>>;
}
/// Native bounded descriptor/observation and notification owner. Implementations
/// retain every accepted effect through finish, including on cancel/Err. Neither
/// prepare nor finish is select-dropped by the HTTP worker. Drop is no drain proof.
pub trait PreparedCover: Send + 'static {
    fn state(&self) -> SourceState;
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<usize>;
    fn before_headers(&mut self) -> CoverFuture<'_, ()>;
    /// Cleanup unconditionally, including when CoverControl is cancelled. Return
    /// only after actual native resources/independent notifications have drained.
    fn finish(self: Box<Self>) -> CoverFuture<'static, ()>;
}

#[derive(Clone)]
pub(crate) struct ConnectionAbort(pub CancellationToken);
struct Lease {
    _permit: OwnedSemaphorePermit,
}
pub(crate) struct Work {
    source: Arc<dyn CoverSource>,
    control: CoverControl,
    abort: ConnectionAbort,
    method: Method,
    headers: Headers,
    reply: Option<oneshot::Sender<Response>>,
    run: Arc<Run>,
    kind: ServerKind,
    lease: Arc<Lease>,
}
struct RequestCancel {
    token: CancellationToken,
    armed: bool,
}
impl Drop for RequestCancel {
    fn drop(&mut self) {
        if self.armed {
            self.token.cancel();
        }
    }
}
struct CoverBody {
    receiver: mpsc::Receiver<std::result::Result<Vec<u8>, io::Error>>,
    disposed: Option<oneshot::Sender<()>>,
    cancel: CancellationToken,
    _lease: Arc<Lease>,
}
#[derive(Clone)]
struct MissingHead;
struct UnknownEmpty;
impl hyper::body::Body for UnknownEmpty {
    type Data = axum::body::Bytes;
    type Error = io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(None)
    }
}
pub(crate) fn transport_response(mut response: Response) -> Response {
    if response.extensions_mut().remove::<MissingHead>().is_some() {
        // Axum's nested HEAD routes replace the body and can infer CL:0. The
        // source's absent-file HEAD explicitly has no length. Repair only this
        // sealed planner case after Axum, including Hyper's size-hint inference.
        response.headers_mut().remove("content-length");
        *response.body_mut() = Body::new(UnknownEmpty);
    }
    response
}
impl Stream for CoverBody {
    type Item = std::result::Result<Vec<u8>, io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}
impl Drop for CoverBody {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(disposed) = self.disposed.take() {
            let _ = disposed.send(());
        }
    }
}

pub(crate) fn route(request: &Request) -> std::result::Result<Route, http::Error> {
    http::match_route(request.method().as_str(), &request.uri().to_string())
}
fn failure(status: StatusCode, code: &str) -> Response {
    (
        status,
        [("content-type", "text/plain; charset=UTF-8")],
        code.to_owned(),
    )
        .into_response()
}
fn report(run: &Run, kind: ServerKind, error: RemoteError) -> Response {
    let error = RemoteError::new(error.code, error.message);
    run.diagnostic(kind, error.clone());
    let status = if error.code == "cover_error_rendering_unsupported" {
        StatusCode::NOT_IMPLEMENTED
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    failure(status, error.code)
}
pub(crate) fn rejected(run: &Run, kind: ServerKind, error: http::Error) -> Response {
    run.diagnostic(kind, RemoteError::new(error.code, error.message));
    failure(StatusCode::BAD_REQUEST, error.code)
}
pub(crate) fn options() -> Response {
    response(&http::options_plan(), Body::from("GET,HEAD"))
}
fn response(plan: &Plan, body: Body) -> Response {
    let mut builder = Response::builder().status(plan.status);
    for (name, value) in &plan.headers {
        builder = builder.header(name, value);
    }
    // Plan's names/values are generated/validated by the pure planner; no
    // request headers or arbitrary native response-header map is forwarded.
    builder.body(body).expect("validated cover plan")
}

pub(crate) async fn serve(
    request: Request,
    kind: ServerKind,
    run: Arc<Run>,
    sender: PoolSender,
    method: Method,
) -> Response {
    let headers = match node_headers(request.headers()) {
        Ok(headers) => headers,
        Err(error) => return rejected(&run, kind, error),
    };
    let Some(abort) = request.extensions().get::<ConnectionAbort>().cloned() else {
        return report(
            &run,
            kind,
            RemoteError::new(
                "cover_connection_authority",
                "Trusted connection cancellation is unavailable",
            ),
        );
    };
    drop(request);
    let permit = match run.static_requests.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "cover_capacity"),
    };
    let control = CoverControl {
        cancel: run.cancel.child_token(),
        deadline: Instant::now() + run.config.body_timeout,
    };
    let mut guard = RequestCancel {
        token: control.cancel.clone(),
        armed: true,
    };
    let (reply, receive) = oneshot::channel();
    let work = Work {
        source: run.cover.as_ref().expect("configured source").clone(),
        control: control.clone(),
        abort,
        method,
        headers,
        reply: Some(reply),
        run: run.clone(),
        kind,
        lease: Arc::new(Lease { _permit: permit }),
    };
    let admitted = sender.lock().unwrap().as_ref().is_some_and(|sender| {
        sender
            .try_send(crate::static_files::Work::Cover(work))
            .is_ok()
    });
    if !admitted {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "cover_queue_unavailable");
    }
    tokio::select! {
        biased;
        _ = run.cancel.cancelled() => failure(StatusCode::SERVICE_UNAVAILABLE, "cover_cancelled"),
        result = tokio::time::timeout_at(control.deadline.into(), receive) => {
            match result {
                Ok(Ok(response)) => { guard.armed = false; response },
                _ => failure(StatusCode::SERVICE_UNAVAILABLE, "cover_preparation_unavailable"),
            }
        },
    }
}

pub(crate) fn handle(mut work: Work) {
    if work.control.cancel.is_cancelled()
        || Instant::now() >= work.control.deadline
        || work.reply.as_ref().unwrap().is_closed()
    {
        return;
    }
    let runtime = work
        .run
        .runtime
        .get()
        .expect("owned runtime installed")
        .clone();
    // No select/timeout drops these native futures. Their cancellation token and
    // deadline are cooperative inputs; actual entered ownership stays here.
    let prepared = runtime.block_on(work.source.prepare(work.control.clone()));
    let mut prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let response = report(&work.run, work.kind, error);
            let _ = work.reply.take().unwrap().send(response);
            return;
        }
    };
    let result = if work.control.cancel.is_cancelled() || work.reply.as_ref().unwrap().is_closed() {
        Err(RemoteError::new(
            "cover_cancelled",
            "HTTP caller retired during actual native preparation",
        ))
    } else {
        prepare_response(&work, prepared.as_mut(), &runtime)
    };
    match result {
        Ok(plan)
            if !work.control.cancel.is_cancelled()
                && !work.reply.as_ref().unwrap().is_closed()
                && Instant::now() < work.control.deadline =>
        {
            let (chunks, receiver) = mpsc::channel(2);
            let (disposed, mut disposition) = oneshot::channel();
            let body = Body::from_stream(CoverBody {
                receiver,
                disposed: Some(disposed),
                cancel: work.control.cancel.clone(),
                _lease: work.lease.clone(),
            });
            let mut response = response(&plan, body);
            if work.method == Method::Head
                && plan.status == 404
                && !plan.headers.contains_key("content-length")
            {
                response.extensions_mut().insert(MissingHead);
            }
            if work.reply.take().unwrap().send(response).is_ok() {
                stream(&work, prepared.as_ref(), &plan.body, &chunks);
                drop(chunks);
                runtime.block_on(async {
                    tokio::select! {
                        _ = &mut disposition => {},
                        _ = work.run.cancel.cancelled() => { work.abort.0.cancel(); let _ = disposition.await; },
                        _ = tokio::time::sleep_until(work.control.deadline.into()) => {
                            work.run.diagnostic(work.kind, RemoteError::new("cover_body_deadline", "Cover body deadline closed its native HTTP connection"));
                            work.abort.0.cancel(); let _ = disposition.await;
                        },
                    }
                });
            }
        }
        Ok(_) => {}
        Err(error) => {
            let response = report(&work.run, work.kind, error);
            let _ = work.reply.take().unwrap().send(response);
        }
    }
    if let Err(error) = runtime.block_on(prepared.finish()) {
        let error = RemoteError::new(error.code, error.message);
        work.run
            .file_failure
            .lock()
            .unwrap()
            .get_or_insert(error.clone());
        work.run.diagnostic(work.kind, error);
    }
}
fn prepare_response(
    work: &Work,
    prepared: &mut dyn PreparedCover,
    runtime: &tokio::runtime::Handle,
) -> Result<Plan> {
    let plan = http::plan(
        prepared.state(),
        work.method,
        &work.headers,
        work.source.error_rendering(),
    )
    .map_err(|e| RemoteError::new(e.code, e.message))?;
    if matches!(plan.body, http::Body::DeferredError { .. }) {
        return Err(RemoteError::new(
            "cover_error_rendering_unsupported",
            "Development finalhandler body requires explicit native rendering policy",
        ));
    }
    runtime.block_on(prepared.before_headers())?;
    Ok(plan)
}
fn stream(
    work: &Work,
    prepared: &dyn PreparedCover,
    body: &http::Body,
    sender: &mpsc::Sender<std::result::Result<Vec<u8>, io::Error>>,
) {
    let (mut offset, mut remaining) = match body {
        http::Body::File { offset, length } => (*offset, *length),
        http::Body::ProductionError { html } => {
            send_chunk(work, sender, Ok(html.as_bytes().to_vec()));
            return;
        }
        _ => return,
    };
    while remaining > 0
        && !work.control.cancel.is_cancelled()
        && Instant::now() < work.control.deadline
    {
        let mut chunk = vec![0; remaining.min(32768) as usize];
        match prepared.read_at(offset, &mut chunk) {
            Ok(0) => {
                work.run.diagnostic(
                    work.kind,
                    RemoteError::new("cover_read", "Held cover ended before planned body length"),
                );
                send_chunk(work, sender, Err(io::ErrorKind::UnexpectedEof.into()));
                return;
            }
            Ok(length) if length <= chunk.len() => {
                chunk.truncate(length);
                offset += length as u64;
                remaining -= length as u64;
                if !send_chunk(work, sender, Ok(chunk)) {
                    return;
                }
            }
            Ok(_) => {
                work.run.diagnostic(
                    work.kind,
                    RemoteError::new(
                        "cover_read",
                        "Native cover reader exceeded the supplied bounded buffer",
                    ),
                );
                send_chunk(work, sender, Err(io::ErrorKind::InvalidData.into()));
                return;
            }
            Err(error) => {
                work.run.diagnostic(
                    work.kind,
                    RemoteError::new("cover_read", format!("Held cover read failed: {error}")),
                );
                send_chunk(work, sender, Err(error));
                return;
            }
        }
    }
}
fn send_chunk(
    work: &Work,
    sender: &mpsc::Sender<std::result::Result<Vec<u8>, io::Error>>,
    mut chunk: std::result::Result<Vec<u8>, io::Error>,
) -> bool {
    loop {
        if work.control.cancel.is_cancelled() || Instant::now() >= work.control.deadline {
            return false;
        }
        match sender.try_send(chunk) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
            Err(mpsc::error::TrySendError::Full(value)) => {
                chunk = value;
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}

// Exact relevant Node HTTP/1 projection: singleton duplicates keep the first,
// cookie joins with '; ', other scalar headers with ', '. Set-Cookie is a Node
// array even on requests and is explicitly unsupported by this scalar contract.
fn node_headers(headers: &HeaderMap) -> std::result::Result<Headers, http::Error> {
    let mut projected = BTreeMap::<String, String>::new();
    let mut count = 0usize;
    let mut bytes = 0usize;
    for name in headers.keys() {
        for value in headers.get_all(name) {
            count += 1;
            bytes = bytes
                .saturating_add(name.as_str().len())
                .saturating_add(value.as_bytes().len());
            if count > http::MAX_HEADERS
                || bytes > http::MAX_HEADER_BYTES
                || value.as_bytes().len() > http::MAX_VALUE_BYTES
            {
                return Err(http::Error {
                    code: "cover_header_limit",
                    message: "raw incoming header count or bytes exceeds limit",
                });
            }
            if name == "set-cookie" {
                return Err(http::Error {
                    code: "cover_header_unsupported",
                    message: "Node array-valued Set-Cookie request header is unsupported",
                });
            }
            let raw = value.as_bytes();
            let start = raw
                .iter()
                .position(|b| !matches!(b, b' ' | b'\t'))
                .unwrap_or(raw.len());
            let end = raw
                .iter()
                .rposition(|b| !matches!(b, b' ' | b'\t'))
                .map_or(start, |i| i + 1);
            let value: String = raw[start..end].iter().map(|b| char::from(*b)).collect();
            let singleton = matches!(
                name.as_str(),
                "age"
                    | "authorization"
                    | "content-length"
                    | "content-type"
                    | "etag"
                    | "expires"
                    | "from"
                    | "host"
                    | "if-modified-since"
                    | "if-unmodified-since"
                    | "last-modified"
                    | "location"
                    | "max-forwards"
                    | "proxy-authorization"
                    | "referer"
                    | "retry-after"
                    | "server"
                    | "user-agent"
            );
            match projected.get_mut(name.as_str()) {
                Some(_) if singleton => {}
                Some(previous) => {
                    previous.push_str(if name == "cookie" { "; " } else { ", " });
                    previous.push_str(&value);
                }
                None => {
                    projected.insert(name.as_str().to_owned(), value);
                }
            }
        }
    }
    Headers::new(projected)
}
