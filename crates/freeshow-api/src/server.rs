use crate::{
    broker::Run,
    semantics::{authorized, rest_data, socket_query, trim, truthy},
    ApiConfig, ApiError, ApiEvent, ApiEventKind, ApiGeneration, BoundEndpoints,
};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use serde::Serialize;
use serde_json::{json, Value};
use socketioxide::{
    adapter::LocalAdapter,
    extract::{SocketRef, TryData},
    handler::{FromMessageParts, MessageHandler},
    socket::Socket,
    SocketIo,
};
use std::{
    collections::HashSet,
    future::Future,
    io::{self, Read},
    net::{IpAddr, Ipv4Addr},
    pin::Pin,
    sync::{Arc, Mutex as StdMutex, Weak},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, Mutex, OwnedSemaphorePermit, Semaphore},
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Clone)]
pub struct ApiService {
    inner: Arc<Inner>,
}
struct Inner {
    config: ApiConfig,
    events: mpsc::Sender<ApiEvent>,
    active: Mutex<Option<Active>>,
    current: StdMutex<Option<Arc<Run>>>,
    lifecycle: Semaphore,
}
struct Active {
    run: Arc<Run>,
    done: Option<tokio::sync::oneshot::Receiver<Result<(), ApiError>>>,
    thread: Option<std::thread::JoinHandle<()>>,
    bound: BoundEndpoints,
}
struct Transport {
    run: Arc<Run>,
    io: SocketIo,
    raw: Router,
    tasks: Vec<JoinHandle<()>>,
}
#[derive(Debug, Serialize)]
pub struct ApiStats {
    pub runtime_tasks: usize,
    pub pending_replies: usize,
    pub active_requests: usize,
    pub engine_sessions: usize,
    pub tcp_connections: usize,
    pub dropped_diagnostics: u64,
}
impl ApiService {
    pub fn new(config: ApiConfig) -> Result<(Self, mpsc::Receiver<ApiEvent>), ApiError> {
        config.validate()?;
        let (events, receiver) = mpsc::channel(config.event_capacity);
        Ok((
            Self {
                inner: Arc::new(Inner {
                    config,
                    events,
                    active: Mutex::new(None),
                    current: StdMutex::new(None),
                    lifecycle: Semaphore::new(32),
                }),
            },
            receiver,
        ))
    }
    pub async fn start(&self, options: &Value, context: u64) -> Result<BoundEndpoints, ApiError> {
        let _admission = self.inner.lifecycle.try_acquire().map_err(|_| {
            ApiError::new(
                "lifecycle_queue_full",
                "API lifecycle command capacity reached",
            )
        })?;
        let mut active = self.inner.active.lock().await;
        self.inner.current.lock().unwrap().take();
        if let Some(previous) = active.as_mut() {
            let result = previous.stop().await;
            active.take();
            result?;
        }
        let port = match options.get("port").filter(|v| truthy(v)) {
            None => 5505,
            Some(value) => value
                .as_f64()
                .filter(|v| v.is_finite() && v.fract() == 0.0 && *v > 0.0 && *v < 65535.0)
                .ok_or_else(|| {
                    ApiError::new(
                        "invalid_port",
                        "API base port must be an integer in 1..65535",
                    )
                })? as u16,
        };
        let password = match options.get("password").filter(|v| truthy(v)) {
            None => String::new(),
            Some(Value::String(value)) => trim(value).to_owned(),
            Some(_) => {
                return Err(ApiError::new(
                    "invalid_password",
                    "API password must be a string",
                ))
            }
        };
        let config = self.inner.config.clone();
        // Bind every endpoint before publishing success or spawning workers.
        let web = bind_tcp(config.bind_ip, port)?;
        let rest = bind_tcp(config.bind_ip, port + 1)?;
        let udp_ip = match config.bind_ip {
            IpAddr::V4(ip) => ip,
            IpAddr::V6(ip) if ip.is_unspecified() => Ipv4Addr::UNSPECIFIED,
            IpAddr::V6(ip) if ip.is_loopback() => Ipv4Addr::LOCALHOST,
            _ => {
                return Err(ApiError::new(
                    "invalid_bind",
                    "OSC requires IPv4 or wildcard/loopback binding",
                ))
            }
        };
        let osc = std::net::UdpSocket::bind((udp_ip, port))
            .map_err(|e| ApiError::new("bind_failed", format!("OSC bind failed: {e}")))?;
        let generation = ApiGeneration {
            context,
            run: Uuid::new_v4().simple().to_string(),
        };
        let bound = BoundEndpoints {
            generation: generation.clone(),
            websocket: web.local_addr().map_err(io_error)?,
            rest: rest.local_addr().map_err(io_error)?,
            osc: osc.local_addr().map_err(io_error)?,
        };
        let run = Run::new(generation, config, password, self.inner.events.clone());
        web.set_nonblocking(true).map_err(io_error)?;
        rest.set_nonblocking(true).map_err(io_error)?;
        osc.set_nonblocking(true).map_err(io_error)?;
        let (ready, started) = tokio::sync::oneshot::channel();
        let (done, finished) = tokio::sync::oneshot::channel();
        let task_run = run.clone();
        let thread = std::thread::Builder::new()
            .name("freeshow-api".into())
            .spawn(move || {
                let mut ready = Some(ready);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                    || -> Result<(), ApiError> {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .map_err(io_error)?;
                        let _ = task_run.runtime.set(runtime.handle().clone());
                        let result = runtime.block_on(async {
                            let web = TcpListener::from_std(web).map_err(io_error)?;
                            let rest = TcpListener::from_std(rest).map_err(io_error)?;
                            let osc = UdpSocket::from_std(osc).map_err(io_error)?;
                            let transport = Transport::start(web, rest, osc, task_run.clone());
                            let _ = ready.take().unwrap().send(Ok(()));
                            task_run.cancel.cancelled().await;
                            transport.stop().await
                        });
                        // The isolated runtime owns every dependency-internal task.
                        // Dropping it cancels engine heartbeat/connect-timeout tasks
                        // which the library's public close API does not expose.
                        drop(runtime);
                        if task_run.engine_sessions.available_permits()
                            != task_run.config.max_engine_sessions
                            || task_run.tcp_connections.available_permits()
                                != task_run.config.max_tcp_connections
                        {
                            return Err(ApiError::new(
                                "shutdown_incomplete",
                                "API runtime retained connection leases after teardown",
                            ));
                        }
                        result
                    },
                ))
                .unwrap_or_else(|_| {
                    Err(ApiError::new(
                        "worker_panicked",
                        "API listener runtime panicked",
                    ))
                });
                if let Some(ready) = ready {
                    let _ = ready.send(result.clone());
                }
                let _ = done.send(result);
            })
            .map_err(|e| ApiError::new("worker_unavailable", e.to_string()))?;
        let candidate = Active {
            run: run.clone(),
            done: Some(finished),
            thread: Some(thread),
            bound: bound.clone(),
        };
        *self.inner.current.lock().unwrap() = Some(run.clone());
        *active = Some(candidate);
        let mut guard = StartGuard { run, armed: true };
        match started.await {
            Ok(Ok(())) => {
                guard.armed = false;
                Ok(bound)
            }
            Ok(Err(error)) => {
                self.inner.current.lock().unwrap().take();
                if let Some(candidate) = active.as_mut() {
                    let _ = candidate.stop().await;
                }
                active.take();
                Err(error)
            }
            Err(_) => {
                self.inner.current.lock().unwrap().take();
                if let Some(candidate) = active.as_mut() {
                    let _ = candidate.stop().await;
                }
                active.take();
                Err(ApiError::new(
                    "worker_unavailable",
                    "API startup worker closed",
                ))
            }
        }
    }
    pub async fn stop(&self) -> Result<(), ApiError> {
        let _admission = self.inner.lifecycle.try_acquire().map_err(|_| {
            ApiError::new(
                "lifecycle_queue_full",
                "API lifecycle command capacity reached",
            )
        })?;
        let mut active = self.inner.active.lock().await;
        self.inner.current.lock().unwrap().take();
        if let Some(previous) = active.as_mut() {
            let result = previous.stop().await;
            active.take();
            result?;
        }
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<(), ApiError> {
        self.stop().await
    }
    pub fn api_trigger(&self, generation: &ApiGeneration, payload: &Value) -> Result<(), ApiError> {
        let run = self.current()?;
        run.complete_action(generation, payload)
    }
    pub fn dynamic_values_reply(
        &self,
        generation: &ApiGeneration,
        listener_id: &str,
        value: Value,
    ) -> Result<(), ApiError> {
        let run = self.current()?;
        run.complete_dynamic(generation, listener_id, value)
    }
    pub fn cancel_pending(&self) {
        if let Ok(run) = self.current() {
            run.cancel_pending();
        }
    }
    fn current(&self) -> Result<Arc<Run>, ApiError> {
        self.inner
            .current
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| ApiError::new("stale_generation", "API service is stopped"))
    }
    pub async fn endpoints(&self) -> Option<BoundEndpoints> {
        self.inner
            .active
            .lock()
            .await
            .as_ref()
            .map(|a| a.bound.clone())
    }
    pub async fn stats(&self) -> Option<ApiStats> {
        self.inner.active.lock().await.as_ref().map(|active| {
            let r = &active.run;
            ApiStats {
                runtime_tasks: r.runtime.get().map_or(0, |h| h.metrics().num_alive_tasks()),
                pending_replies: r.pending_count(),
                active_requests: r.config.max_inflight - r.inflight.available_permits(),
                engine_sessions: r.config.max_engine_sessions
                    - r.engine_sessions.available_permits(),
                tcp_connections: r.config.max_tcp_connections
                    - r.tcp_connections.available_permits(),
                dropped_diagnostics: r
                    .dropped_diagnostics
                    .load(std::sync::atomic::Ordering::Relaxed),
            }
        })
    }
}
impl Active {
    async fn stop(&mut self) -> Result<(), ApiError> {
        self.run.close_admission();
        self.run.cancel_pending();
        let result = match self.done.as_mut() {
            Some(done) => done.await.unwrap_or_else(|_| {
                Err(ApiError::new(
                    "worker_unavailable",
                    "API runtime completion channel closed",
                ))
            }),
            None => Ok(()),
        };
        self.done.take();
        // Notification is sent after runtime destruction. Only thread return /
        // native thread-local destructors remain when join is called.
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| ApiError::new("worker_panicked", "API listener thread panicked"))?;
        }
        result
    }
}
struct StartGuard {
    run: Arc<Run>,
    armed: bool,
}
impl Drop for StartGuard {
    fn drop(&mut self) {
        if self.armed {
            self.run.close_admission();
        }
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        self.run.close_admission();
        self.run.cancel_pending();
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(active) = self.active.get_mut().take() {
            drop(active);
        }
    }
}
impl Transport {
    fn start(web: TcpListener, rest: TcpListener, osc: UdpSocket, run: Arc<Run>) -> Self {
        let parser = socketioxide::ParserConfig::common()
            .max_incoming_binary_buf_size(run.config.max_socket_payload)
            .max_incoming_binaries(run.config.socket_buffer_packets)
            .packet_completion_timeout(run.config.body_timeout);
        let (layer, io) = SocketIo::builder()
            .with_parser(parser)
            .req_path("/socket.io/")
            .max_payload(run.config.max_socket_payload as u64)
            .ws_max_message_size(run.config.max_socket_payload)
            .ws_max_frame_size(run.config.max_socket_payload)
            .max_buffer_size(run.config.socket_buffer_packets)
            .connect_timeout(run.config.handshake_timeout)
            .build_layer();
        connect(&io, run.clone());
        let rest_router = Router::new()
            .fallback(any(rest_handler))
            .with_state(run.clone());
        let raw = rest_router.clone().layer(layer);
        let web_router = raw.clone().layer(middleware::from_fn_with_state(
            run.clone(),
            engine_admission,
        ));
        let tasks = vec![
            tokio::spawn(serve_tcp(web, web_router, run.clone())),
            tokio::spawn(serve_tcp(rest, rest_router, run.clone())),
            tokio::spawn(crate::osc::serve(osc, run.clone())),
        ];
        Self {
            run,
            io,
            raw,
            tasks,
        }
    }
    async fn stop(mut self) -> Result<(), ApiError> {
        self.run.close_admission();
        self.run.cancel_pending();
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
        self.run.tracker.close();
        self.run.tracker.wait().await;
        // Explicit namespace disconnects remove namespace/socket reference
        // cycles before SocketIo::close takes its namespace map away.
        for socket in self.io.sockets() {
            let _ = socket.disconnect();
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while !self.io.sockets().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| {
            ApiError::new(
                "shutdown_incomplete",
                "Socket.IO namespaces did not disconnect",
            )
        })?;
        let sids = self
            .run
            .polling_sids
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for sid in sids {
            let request = Request::builder()
                .method(Method::POST)
                .uri(format!("/socket.io/?EIO=4&transport=polling&sid={sid}"))
                .header("content-type", "text/plain;charset=UTF-8")
                .body(Body::from("1"))
                .unwrap();
            let _ = self.raw.clone().oneshot(request).await;
        }
        self.io.close().await;
        Ok(())
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.run.close_admission();
        self.run.cancel_pending();
        for task in &self.tasks {
            task.abort();
        }
    }
}
fn io_error(error: io::Error) -> ApiError {
    ApiError::new("io_error", error.to_string())
}
fn bind_tcp(ip: IpAddr, port: u16) -> Result<std::net::TcpListener, ApiError> {
    match std::net::TcpListener::bind((ip, port)) {
        Ok(listener) => Ok(listener),
        Err(error)
            if matches!(ip,IpAddr::V6(ip) if ip.is_unspecified())
                && matches!(
                    error.kind(),
                    io::ErrorKind::AddrNotAvailable | io::ErrorKind::Unsupported
                ) =>
        {
            std::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).map_err(io_error)
        }
        Err(error) => Err(ApiError::new(
            "bind_failed",
            format!("TCP {port} bind failed: {error}"),
        )),
    }
}

struct CancelIo {
    stream: TcpStream,
    cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
    _permit: OwnedSemaphorePermit,
}
impl AsyncRead for CancelIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl AsyncWrite for CancelIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::ErrorKind::ConnectionAborted.into()));
        }
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
async fn serve_tcp(listener: TcpListener, router: Router, run: Arc<Run>) {
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {biased;
            _=run.cancel.cancelled()=>break,
            Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
            accepted=listener.accept()=>{let(stream,_)=match accepted{Ok(pair)=>pair,Err(error)=>{run.diagnostic("accept_failed",error.to_string());break;}};
                let permit=match run.tcp_connections.clone().try_acquire_owned(){Ok(permit)=>permit,Err(_)=>{drop(stream);run.diagnostic("connection_limit","API TCP connection limit reached");continue;}};
                let router=router.clone();let cancel=run.cancel.clone();let io=TokioIo::new(CancelIo{stream,cancelled:Box::pin(cancel.clone().cancelled_owned()),_permit:permit});
                tasks.spawn(async move{let mut builder=hyper::server::conn::http1::Builder::new();builder.timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(10)).max_buf_size(16_384).max_headers(100);let connection=builder.serve_connection(io,TowerToHyperService::new(router)).with_upgrades();tokio::select!{_=cancel.cancelled()=>{},_=connection=>{}}});
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

#[derive(Clone)]
struct SessionLease(Arc<Lease>);
struct Lease {
    _permit: OwnedSemaphorePermit,
    sid: StdMutex<Option<String>>,
    sids: Weak<StdMutex<HashSet<String>>>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let (Some(sids), Some(sid)) = (self.sids.upgrade(), self.sid.get_mut().unwrap().take()) {
            sids.lock().unwrap().remove(&sid);
        }
    }
}
async fn engine_admission(State(run): State<Arc<Run>>, request: Request, next: Next) -> Response {
    if !request.uri().path().starts_with("/socket.io/") {
        return next.run(request).await;
    }
    let requested_headers = request
        .headers()
        .get("access-control-request-headers")
        .cloned();
    let preflight = request.method() == Method::OPTIONS;
    let mut response = if preflight {
        StatusCode::NO_CONTENT.into_response()
    } else {
        engine_request(run, request, next).await
    };
    cors(&mut response, preflight, requested_headers);
    response
}
async fn engine_request(run: Arc<Run>, mut request: Request, next: Next) -> Response {
    if run.cancel.is_cancelled() {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "cancelled",
            "API service is stopping",
        );
    }
    let mut query = socket_query(request.uri().query().unwrap_or_default());
    // Engine.IO treats an empty string sid as an initial request. Remove it
    // before dependency dispatch, and acquire the same session lease as absent.
    if query.get("sid").and_then(Value::as_str) == Some("") {
        query.as_object_mut().unwrap().remove("sid");
    }
    // The dependency treats an unparseable raw sid as absent. Validate the
    // decoded Engine.IO reserved fields before canonicalizing them, so encoded,
    // duplicate or invalid sid parameters cannot create an unleased session.
    let transport = query.get("transport").and_then(Value::as_str);
    if query.get("EIO").and_then(Value::as_str) != Some("4")
        || !matches!(transport, Some("polling" | "websocket"))
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_engine_query",
            "Unsupported Engine.IO transport or protocol",
        );
    }
    if let Some(sid) = query.get("sid") {
        if !sid
            .as_str()
            .is_some_and(|sid| sid.parse::<socketioxide::socket::Sid>().is_ok())
        {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_engine_sid",
                "Invalid Engine.IO session ID",
            );
        }
    }
    let initial = request.method() == Method::GET && query.get("sid").is_none();
    let polling = transport == Some("polling");
    let canonical = {
        let mut encoded = form_urlencoded::Serializer::new(String::new());
        if let Some(values) = query.as_object() {
            for (key, value) in values {
                match value {
                    Value::Array(values) => {
                        for value in values {
                            if let Some(value) = value.as_str() {
                                encoded.append_pair(key, value);
                            }
                        }
                    }
                    Value::String(value) => {
                        encoded.append_pair(key, value);
                    }
                    _ => {}
                }
            }
        }
        format!("{}?{}", request.uri().path(), encoded.finish())
    };
    *request.uri_mut() = canonical.parse().unwrap();
    let lease = if initial {
        let permit = match run.engine_sessions.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "session_limit",
                    "API Engine.IO session limit reached",
                )
            }
        };
        let lease = SessionLease(Arc::new(Lease {
            _permit: permit,
            sid: StdMutex::new(None),
            sids: Arc::downgrade(&run.polling_sids),
        }));
        request.extensions_mut().insert(lease.clone());
        Some(lease)
    } else {
        None
    };
    let response = if initial {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task_run = run.clone();
        if run
            .spawn(async move {
                let mut response = next.run(request).await;
                #[cfg(test)]
                {
                    let pause = task_run.handshake_pause.lock().unwrap().take();
                    if let Some((entered, release)) = pause {
                        entered.notify_one();
                        release.notified().await;
                    }
                }
                if polling && response.status() == StatusCode::OK {
                    let (parts, body) = response.into_parts();
                    response = match to_bytes(body, 2048).await {
                        Ok(bytes) => {
                            if let Some(sid) = bytes
                                .first()
                                .filter(|b| **b == b'0')
                                .and_then(|_| serde_json::from_slice::<Value>(&bytes[1..]).ok())
                                .and_then(|v| {
                                    v.get("sid").and_then(Value::as_str).map(str::to_owned)
                                })
                            {
                                if let Some(lease) = lease {
                                    *lease.0.sid.lock().unwrap() = Some(sid.clone());
                                    task_run.polling_sids.lock().unwrap().insert(sid);
                                }
                            }
                            Response::from_parts(parts, Body::from(bytes))
                        }
                        Err(_) => error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "handshake_failed",
                            "Engine.IO handshake response exceeds its limit",
                        ),
                    };
                }
                let _ = sender.send(response);
            })
            .is_err()
        {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "cancelled",
                "API handshake admission is closed",
            );
        }
        match receiver.await {
            Ok(response) => response,
            Err(_) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "cancelled",
                    "API handshake was cancelled",
                )
            }
        }
    } else {
        next.run(request).await
    };
    response
}
fn cors(response: &mut Response, preflight: bool, headers: Option<HeaderValue>) {
    response
        .headers_mut()
        .insert("access-control-allow-origin", HeaderValue::from_static("*"));
    if !preflight {
        return;
    }
    response.headers_mut().insert(
        "vary",
        HeaderValue::from_static("Access-Control-Request-Headers"),
    );
    response
        .headers_mut()
        .insert("content-length", HeaderValue::from_static("0"));
    response.headers_mut().insert(
        "access-control-allow-methods",
        HeaderValue::from_static("GET,HEAD,PUT,PATCH,POST,DELETE"),
    );
    if let Some(headers) = headers {
        response
            .headers_mut()
            .insert("access-control-allow-headers", headers);
    }
}

struct Client {
    auth: Value,
    headers: Value,
    authorization: Value,
    query: Value,
    cancel: CancellationToken,
    inflight: Arc<Semaphore>,
}
fn connect(io: &SocketIo, run: Arc<Run>) {
    io.ns(
        "/",
        move |socket: SocketRef, TryData(auth): TryData<Box<serde_json::value::RawValue>>| {
            let run = run.clone();
            if run.cancel.is_cancelled() || run.event(ApiEventKind::Connected).is_err() {
                let _ = socket.disconnect();
                return std::future::ready(());
            }
            let auth = match auth
                .map_err(|error| ApiError::new("invalid_socket_auth", error.to_string()))
                .and_then(|raw| crate::semantics::parse_json(raw.get()))
            {
                Ok(auth) => auth,
                Err(error) => {
                    run.diagnostic(error.code, error.message);
                    let _ = socket.disconnect();
                    return std::future::ready(());
                }
            };
            let headers = headers_json(&socket.req_parts().headers);
            let client = Arc::new(Client {
                auth,
                authorization: headers.get("authorization").cloned().unwrap_or(Value::Null),
                headers,
                query: socket_query(socket.req_parts().uri.query().unwrap_or_default()),
                cancel: CancellationToken::new(),
                inflight: Arc::new(Semaphore::new(run.config.max_client_inflight)),
            });
            socket.on(
                "data",
                DataHandler {
                    run: run.clone(),
                    client: client.clone(),
                },
            );
            socket.on_disconnect(move || {
                client.cancel.cancel();
                std::future::ready(())
            });
            std::future::ready(())
        },
    );
}
struct DataHandler {
    run: Arc<Run>,
    client: Arc<Client>,
}
impl MessageHandler<LocalAdapter, ()> for DataHandler {
    fn call(&self, socket: Arc<Socket>, mut value: socketioxide::handler::Value, ack: Option<i64>) {
        // Custom synchronous handler: the library's async function adapter
        // otherwise spawns a task before our admission check for every packet.
        let exact = match value
            .as_str()
            .map(|raw| crate::semantics::parse_json(raw))
            .transpose()
        {
            Ok(Some(Value::Array(mut args))) => {
                if args.len() > 1 {
                    args.swap_remove(1)
                } else {
                    Value::Null
                }
            }
            Ok(_) => {
                self.run.diagnostic(
                    "invalid_socket_data",
                    "Socket.IO event payload must be a JSON argument array",
                );
                return;
            }
            Err(error) => {
                self.run.diagnostic(error.code, error.message);
                return;
            }
        };
        let payload = if matches!(&value,socketioxide::handler::Value::Str(_,Some(binaries)) if !binaries.is_empty())
        {
            let mut projected =
                match TryData::<Value>::from_message_parts(&socket, &mut value, &ack)
                    .unwrap()
                    .0
                {
                    Ok(value) => value,
                    Err(error) => {
                        self.run
                            .diagnostic("invalid_socket_data", error.to_string());
                        return;
                    }
                };
            crate::json::correct_numbers(exact, &mut projected);
            projected
        } else {
            exact
        };
        let socket = SocketRef::from(socket);
        if self.run.cancel.is_cancelled() || self.client.cancel.is_cancelled() {
            return;
        }
        let global = match self.run.inflight.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                emit(
                    &self.run,
                    &socket,
                    "error",
                    &json!("API request concurrency limit reached"),
                );
                emit(
                    &self.run,
                    &socket,
                    "data",
                    &json!({"status":503,"error":"Busy"}),
                );
                return;
            }
        };
        let local = match self.client.inflight.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                emit(
                    &self.run,
                    &socket,
                    "error",
                    &json!("API client concurrency limit reached"),
                );
                emit(
                    &self.run,
                    &socket,
                    "data",
                    &json!({"status":503,"error":"Busy"}),
                );
                return;
            }
        };
        let run = self.run.clone();
        let client = self.client.clone();
        let _=self.run.spawn(async move{let(_global,_local)=(global,local);tokio::select!{biased;_=run.cancel.cancelled()=>{},_=client.cancel.cancelled()=>{},result=socket_data(run.clone(),client.clone(),socket.clone(),payload)=>if let Err(error)=result{run.diagnostic(error.code,error.message.clone());if error.code!="cancelled"{emit(&run,&socket,"error",&json!(error.message));emit(&run,&socket,"data",&json!({"status":503,"error":error.code}));}}}});
    }
}
fn emit(run: &Run, socket: &SocketRef, event: &str, value: &Value) {
    if !run.cancel.is_cancelled() && socket.connected() {
        if let Err(error) = socket.emit(event, &crate::semantics::wire(value)) {
            run.diagnostic("socket_emit_failed", error.to_string());
        }
    }
}
async fn socket_data(
    run: Arc<Run>,
    client: Arc<Client>,
    socket: SocketRef,
    payload: Value,
) -> Result<(), ApiError> {
    let parsed = match payload {
        Value::String(text) if !text.is_empty() => match crate::semantics::parse_json(&text) {
            Ok(value) => value,
            Err(error) if error.code == "json_number_overflow" => return Err(error),
            Err(_) => {
                run.diagnostic("invalid_socket_json", "Could not parse socket data");
                return Ok(());
            }
        },
        other => {
            if truthy(&other) {
                other
            } else {
                json!({})
            }
        }
    };
    if !authorized(
        &run.password,
        &[
            client.auth.clone(),
            client.authorization.clone(),
            client.headers.clone(),
            client.query.clone(),
            parsed.clone(),
        ],
    ) {
        emit(&run, &socket, "error", &json!("Unauthorized request (401)"));
        emit(
            &run,
            &socket,
            "data",
            &json!({"status":401,"error":"Unauthorized"}),
        );
        return Ok(());
    }
    if parsed.is_null() {
        return Err(ApiError::new(
            "invalid_socket_data",
            "Socket JSON null cannot carry an API action",
        ));
    }
    if parsed.get("isVariable").is_some_and(truthy) {
        let keys = parsed
            .get("keys")
            .filter(|v| truthy(v))
            .cloned()
            .unwrap_or(json!([]));
        let value = run.dynamic(keys, &client.cancel).await?;
        emit(&run, &socket, "data", &value);
    } else {
        if !parsed.get("action").is_some_and(truthy) {
            emit(
                &run,
                &socket,
                "error",
                &json!("Received message from client, but missing 'action' key."),
            );
            return Ok(());
        }
        if let Some(value) = run.received(parsed, &client.cancel).await? {
            emit(&run, &socket, "data", &value);
        }
    }
    Ok(())
}
fn headers_json(headers: &HeaderMap) -> Value {
    let mut values = serde_json::Map::new();
    for name in headers.keys() {
        let raw = headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect::<Vec<_>>();
        let value = if name == "set-cookie" {
            json!(raw)
        } else if name == "authorization" {
            json!(raw.first().copied().unwrap_or_default())
        } else {
            json!(raw.join(if name == "cookie" { "; " } else { ", " }))
        };
        values.insert(name.as_str().into(), value);
    }
    Value::Object(values)
}
fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        [("content-type", "application/json; charset=utf-8")],
        json!({"status":status.as_u16(),"error":message,"code":code}).to_string(),
    )
        .into_response()
}
async fn rest_handler(State(run): State<Arc<Run>>, request: Request) -> Response {
    let method = request.method().clone();
    let response = rest_request(run, request).await;
    if method == Method::HEAD {
        let (parts, _) = response.into_parts();
        Response::from_parts(parts, Body::empty())
    } else {
        response
    }
}
async fn rest_request(run: Arc<Run>, request: Request) -> Response {
    let permit = match run.inflight.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "request_limit",
                "API request concurrency limit reached",
            )
        }
    };
    let _permit = permit;
    let path = request.uri().path().to_owned();
    let query = crate::parse_query(request.uri().query().unwrap_or_default());
    let headers = headers_json(request.headers());
    let content_type = request
        .headers()
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let encoding = request
        .headers()
        .get("content-encoding")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("identity")
        .to_ascii_lowercase();
    let json_body = content_type
        .split(';')
        .next()
        .is_some_and(|v| v.trim() == "application/json");
    let has_body = request.headers().contains_key("transfer-encoding")
        || request
            .headers()
            .get("content-length")
            .is_some_and(|h| h != "0");
    let bytes = match tokio::time::timeout(
        run.config.body_timeout,
        to_bytes(request.into_body(), run.config.max_http_body),
    )
    .await
    {
        Err(_) => {
            return error_response(
                StatusCode::REQUEST_TIMEOUT,
                "body_timeout",
                "Request body timed out",
            )
        }
        Ok(Err(_)) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_too_large",
                "request entity too large",
            )
        }
        Ok(Ok(bytes)) => bytes,
    };
    let mut body = json!({});
    if json_body && has_body {
        let charset = content_type
            .split(';')
            .skip(1)
            .filter_map(|p| p.trim().split_once('='))
            .find(|(k, _)| *k == "charset")
            .map(|(_, v)| v.trim_matches('"'))
            .unwrap_or("utf-8");
        if !charset.starts_with("utf-") {
            return error_response(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_charset",
                "unsupported charset",
            );
        }
        let decompressed = match inflate(&bytes, &encoding, run.config.max_http_body) {
            Ok(bytes) => bytes,
            Err((status, code)) => {
                return error_response(status, code, "Could not decode JSON request body")
            }
        };
        let text = match decode_text(&decompressed, charset) {
            Some(text) => text,
            None => {
                return error_response(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "unsupported_charset",
                    "unsupported charset",
                )
            }
        };
        if !text.is_empty() {
            if !text
                .trim_start_matches([' ', '\t', '\r', '\n'])
                .starts_with(['{', '['])
            {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_json",
                    "JSON body must be an object or array",
                );
            }
            body = match crate::semantics::parse_json(&text) {
                Ok(body) => body,
                Err(error) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        error.code,
                        "Could not parse JSON body",
                    )
                }
            };
        }
    }
    if path != "/" {
        return (
            StatusCode::NOT_FOUND,
            [("content-type", "text/html; charset=utf-8")],
            "Not Found",
        )
            .into_response();
    }
    if !authorized(
        &run.password,
        &[
            headers.get("authorization").cloned().unwrap_or(Value::Null),
            headers,
            query.clone(),
            body.clone(),
        ],
    ) {
        return (
            StatusCode::UNAUTHORIZED,
            [("content-type", "application/json; charset=utf-8")],
            "{\"status\":401,\"error\":\"Unauthorized\"}",
        )
            .into_response();
    }
    let data = match rest_data(body, &query) {
        Ok(data) => data,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error.code, &error.message),
    };
    let client = CancellationToken::new();
    match run.received(data, &client).await {
        Ok(Some(value)) => (
            [("content-type", "application/json; charset=utf-8")],
            crate::semantics::wire(&value).to_string(),
        )
            .into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            let status = if error.code == "invalid_action" {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            error_response(status, error.code, &error.message)
        }
    }
}
fn inflate(
    bytes: &[u8],
    encoding: &str,
    limit: usize,
) -> Result<Vec<u8>, (StatusCode, &'static str)> {
    let reader: Box<dyn Read + '_> = match encoding {
        "identity" => return Ok(bytes.to_vec()),
        "gzip" => Box::new(flate2::read::GzDecoder::new(bytes)),
        "deflate" => Box::new(flate2::read::ZlibDecoder::new(bytes)),
        _ => return Err((StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_encoding")),
    };
    let mut output = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut output)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid_compression"))?;
    if output.len() > limit {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    }
    Ok(output)
}
fn decode_text(bytes: &[u8], charset: &str) -> Option<String> {
    let name = charset
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>();
    let text = match name.as_str() {
        "utf7" | "utf7imap" => decode_utf7(bytes, name == "utf7imap"),
        "utf16" | "utf16le" | "utf16be" => {
            let big = if name == "utf16be" {
                true
            } else if name == "utf16le" {
                false
            } else if bytes.starts_with(&[0xfe, 0xff]) {
                true
            } else if bytes.starts_with(&[0xff, 0xfe]) {
                false
            } else {
                let mut le = 0;
                let mut be = 0;
                for pair in bytes[..bytes.len().min(64)].as_chunks::<2>().0 {
                    if pair[0] != 0 && pair[1] == 0 {
                        le += 1;
                    }
                    if pair[0] == 0 && pair[1] != 0 {
                        be += 1;
                    }
                }
                be > le
            };
            decode_utf16(bytes, big)
        }
        _ => {
            let encoding = encoding_rs::Encoding::for_label(charset.as_bytes())?;
            let (text, _) = encoding.decode_without_bom_handling(bytes);
            text.into_owned()
        }
    };
    Some(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
}
fn decode_utf16(bytes: &[u8], big: bool) -> String {
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            if big {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16_lossy(&units)
}
fn decode_utf7(bytes: &[u8], imap: bool) -> String {
    let marker = if imap { b'&' } else { b'+' };
    let mut result = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != marker {
            result.push(char::from(bytes[i] & 0x7f));
            i += 1;
            continue;
        }
        i += 1;
        if bytes.get(i) == Some(&b'-') {
            result.push(char::from(marker));
            i += 1;
            continue;
        }
        let mut decoded = Vec::new();
        let mut buffer = 0u32;
        let mut bits = 0;
        while let Some(&byte) = bytes.get(i) {
            let digit = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b',' if imap => 63,
                _ => break,
            };
            buffer = (buffer << 6) | u32::from(digit);
            bits += 6;
            i += 1;
            if bits >= 8 {
                bits -= 8;
                decoded.push((buffer >> bits) as u8);
                buffer &= (1 << bits) - 1;
            }
        }
        let shifted = decode_utf16(&decoded, true);
        result.push_str(shifted.strip_prefix('\u{feff}').unwrap_or(&shifted));
        if bytes.get(i) == Some(&b'-') {
            i += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    fn port() -> u16 {
        for _ in 0..100 {
            let first = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let p = first.local_addr().unwrap().port();
            if p == 65535 {
                continue;
            }
            if let (Ok(_second), Ok(_udp)) = (
                std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, p + 1)),
                std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, p)),
            ) {
                return p;
            }
        }
        panic!("loopback ports")
    }
    #[tokio::test]
    async fn stop_waits_for_handshake_created_before_sid_registration() {
        let config = ApiConfig {
            bind_ip: Ipv4Addr::LOCALHOST.into(),
            ..ApiConfig::default()
        };
        let (service, _events) = ApiService::new(config).unwrap();
        let p = port();
        service
            .start(&json!({"port":f64::from(p)}), 7)
            .await
            .unwrap();
        let run = service.current().unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *run.handshake_pause.lock().unwrap() = Some((entered.clone(), release.clone()));
        let request = tokio::spawn(async move {
            let mut socket = TcpStream::connect((Ipv4Addr::LOCALHOST, p)).await.unwrap();
            socket.write_all(b"GET /socket.io/?EIO=4&transport=polling HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
            let mut bytes = Vec::new();
            let _ = socket.read_to_end(&mut bytes).await;
        });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        assert_eq!(
            run.engine_sessions.available_permits(),
            run.config.max_engine_sessions - 1
        );
        assert!(run.polling_sids.lock().unwrap().is_empty());
        let stopping_service = service.clone();
        let stop = tokio::spawn(async move { stopping_service.stop().await });
        while !run.cancel.is_cancelled() {
            tokio::task::yield_now().await;
        }
        assert!(
            !stop.is_finished(),
            "stop must await admitted handshake registration"
        );
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), stop)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        request.await.unwrap();
        assert!(run.polling_sids.lock().unwrap().is_empty());
        assert_eq!(
            run.engine_sessions.available_permits(),
            run.config.max_engine_sessions
        );
        assert_eq!(
            run.tcp_connections.available_permits(),
            run.config.max_tcp_connections
        );
        assert_eq!(run.runtime.get().unwrap().metrics().num_alive_tasks(), 0);
        service.start(&json!({"port":p}), 7).await.unwrap();
        service.stop().await.unwrap();
    }
    #[tokio::test]
    async fn cancelled_stop_retains_run_until_its_runtime_is_reclaimed() {
        let config = ApiConfig {
            bind_ip: Ipv4Addr::LOCALHOST.into(),
            ..ApiConfig::default()
        };
        let (service, _) = ApiService::new(config).unwrap();
        let p = port();
        service.start(&json!({"port":p}), 1).await.unwrap();
        let run = service.current().unwrap();
        // A tracked admitted task deterministically holds the teardown boundary.
        let release = Arc::new(tokio::sync::Notify::new());
        let waiting = release.clone();
        let barrier = run.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        run.runtime.get().unwrap().spawn(async move {
            barrier
                .spawn(async move {
                    waiting.notified().await;
                })
                .unwrap();
            let _ = tx.send(());
        });
        rx.await.unwrap();
        let copy = service.clone();
        let stop = tokio::spawn(async move { copy.stop().await });
        while !run.cancel.is_cancelled() {
            tokio::task::yield_now().await;
        }
        stop.abort();
        assert!(stop.await.unwrap_err().is_cancelled());
        let copy = service.clone();
        let restarted = tokio::spawn(async move { copy.start(&json!({"port":p}), 2).await });
        tokio::task::yield_now().await;
        assert!(!restarted.is_finished());
        release.notify_one();
        let bound = tokio::time::timeout(Duration::from_secs(2), restarted)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(bound.generation.context, 2);
        assert_eq!(run.runtime.get().unwrap().metrics().num_alive_tasks(), 0);
        service.stop().await.unwrap();
    }
    #[tokio::test]
    async fn lifecycle_waiter_capacity_is_bounded_and_cancellation_reclaims_it() {
        let (service, _) = ApiService::new(ApiConfig::default()).unwrap();
        let held = service.inner.active.lock().await;
        let mut tasks = Vec::new();
        for _ in 0..32 {
            let copy = service.clone();
            tasks.push(tokio::spawn(async move { copy.stop().await }));
        }
        while service.inner.lifecycle.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            service.stop().await.unwrap_err().code,
            "lifecycle_queue_full"
        );
        for task in tasks {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
        assert_eq!(service.inner.lifecycle.available_permits(), 32);
        drop(held);
        service.stop().await.unwrap();
    }
}
