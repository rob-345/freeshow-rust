use crate::{
    semantics::{self, Target},
    state::{Client, Command, Lease, Run, SendMode, SessionLease},
    static_files::FilePool,
    BoundServers, RemoteConfig, RemoteError, RemoteEventKind, RemoteEvents, RemoteGeneration,
    RemoteOrigin, RemoteStats, Result, ServerKind,
};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use serde_json::{json, Map, Value};
use socketioxide::{
    adapter::LocalAdapter, extract::SocketRef, handler::MessageHandler, socket::Socket, SocketIo,
};
use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex,
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, Mutex, OwnedSemaphorePermit, Semaphore},
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::WaitForCancellationFutureOwned;
use tower::ServiceExt;
use uuid::Uuid;

// Contain a transport poll panic while preserving the separately owned file
// pool and runtime. Never repoll the failed future; actual file work drains next.
async fn contain<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::pin!(future);
    std::future::poll_fn(|cx| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx)))
            .unwrap_or_else(|_| {
                Poll::Ready(Err(RemoteError::new(
                    "worker_panicked",
                    "Remote transport panicked",
                )))
            })
    })
    .await
}
async fn injected_fault(_run: &Run) -> Result<()> {
    #[cfg(test)]
    {
        _run.transport_fault.1.notified().await;
        if _run.transport_fault.0.lock().unwrap().take().unwrap() {
            panic!("controlled transport poll panic");
        }
        Err(RemoteError::new(
            "controlled_transport_error",
            "Controlled transport error",
        ))
    }
    #[cfg(not(test))]
    std::future::pending().await
}

#[derive(Clone)]
pub struct RemoteService {
    inner: Arc<Inner>,
}
struct Inner {
    #[cfg(test)]
    ready_pause: StdMutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
    config: RemoteConfig,
    controls: mpsc::Sender<crate::RemoteEvent>,
    events: mpsc::Sender<crate::RemoteEvent>,
    active: Mutex<Option<Active>>,
    current: StdMutex<Option<Arc<Run>>>,
    lifecycle: Semaphore,
    terminal: AtomicBool,
    data: StdMutex<BTreeMap<ServerKind, Value>>,
    gate: Arc<StdMutex<HashMap<String, bool>>>,
}
struct Active {
    run: Arc<Run>,
    bound: BoundServers,
    done: oneshot::Receiver<Result<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl RemoteService {
    pub fn new(config: RemoteConfig) -> Result<(Self, RemoteEvents)> {
        config.validate()?;
        let (controls, control_rx) = mpsc::channel(config.max_clients * 2 + 16);
        let (events, data_rx) = mpsc::channel(config.data_events);
        Ok((
            Self {
                inner: Arc::new(Inner {
                    #[cfg(test)]
                    ready_pause: StdMutex::new(None),
                    config,
                    controls,
                    events,
                    active: Mutex::new(None),
                    current: StdMutex::new(None),
                    lifecycle: Semaphore::new(8),
                    terminal: AtomicBool::new(false),
                    data: StdMutex::new(
                        ServerKind::ALL
                            .into_iter()
                            .map(|k| (k, json!({})))
                            .collect(),
                    ),
                    gate: Arc::new(StdMutex::new(HashMap::new())),
                }),
            },
            RemoteEvents::new(control_rx, data_rx),
        ))
    }
    pub async fn start(&self, payload: &Value, context: u64) -> Result<BoundServers> {
        self.start_with_cover(payload, context, None).await
    }
    /// Native per-start authority only. Source receives no request-selected path.
    pub async fn start_with_cover(
        &self,
        payload: &Value,
        context: u64,
        cover: Option<Arc<dyn crate::CoverSource>>,
    ) -> Result<BoundServers> {
        self.ensure_open()?;
        let _admission =
            self.inner.lifecycle.try_acquire().map_err(|_| {
                RemoteError::new("lifecycle_queue_full", "Remote lifecycle queue full")
            })?;
        let mut active = self.inner.active.lock().await;
        self.ensure_open()?;
        self.inner.current.lock().unwrap().take();
        if let Some(previous) = active.as_mut() {
            let result = previous.stop().await;
            active.take();
            result?;
        }
        semantics::check_size(payload, self.inner.config.max_socket_payload)?;
        let spec = semantics::prepare_start(&semantics::normalize(payload))?;
        let mut listeners = BTreeMap::new();
        for (kind, port) in &spec.ports {
            listeners.insert(*kind, bind_tcp(self.inner.config.bind_ip, *port)?);
        }
        let generation = RemoteGeneration {
            context,
            run: Uuid::new_v4().simple().to_string(),
        };
        let bound = BoundServers {
            generation: generation.clone(),
            endpoints: listeners
                .iter()
                .map(|(k, v)| v.local_addr().map(|a| (*k, a)).map_err(io_error))
                .collect::<Result<_>>()?,
        };
        for listener in listeners.values() {
            listener.set_nonblocking(true).map_err(io_error)?;
        }
        let (run, commands) = Run::new(
            self.inner.config.clone(),
            generation,
            spec,
            self.inner.controls.clone(),
            self.inner.events.clone(),
            self.inner.gate.clone(),
            cover,
        );
        let (ready, started) = oneshot::channel();
        let (done, finished) = oneshot::channel();
        let task_run = run.clone();
        #[cfg(test)]
        let ready_pause = self.inner.ready_pause.lock().unwrap().take();
        let thread = std::thread::Builder::new()
            .name("freeshow-remotes".into())
            .spawn(move || {
                let mut ready = Some(ready);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all().build().map_err(io_error)?;
                    let _ = task_run.runtime.set(runtime.handle().clone());
                    let result = runtime.block_on(async {
                        // Keep the actual file owner outside fallible/panicking
                        // transport work. Its accepted futures need this runtime
                        // alive through asynchronous drain even after a fault.
                        let files = FilePool::new(task_run.clone())?;
                        let mut transport = Transport::start(listeners, task_run.clone(), files.sender())?;
                        #[cfg(test)]
                        if let Some((entered, release)) = ready_pause {
                            entered.notify_one();
                            tokio::select! { _=release.notified()=>{}, _=task_run.cancel.cancelled()=>{} }
                        }
                        let _ = ready.take().unwrap().send(Ok(()));
                        let result = contain(transport.work(commands)).await;
                        task_run.close_admission();
                        let transport_stop = contain(transport.close()).await;
                        let files_stop = files.stop().await;
                        let file_failure = task_run.file_failure.lock().unwrap().clone();
                        result.and(transport_stop).and(files_stop).and(file_failure.map_or(Ok(()), Err))
                    });
                    drop(runtime);
                    for (name, available, expected) in [
                        ("engine", task_run.engine_sessions.available_permits(), task_run.config.max_engine_sessions),
                        ("tcp", task_run.tcp_connections.available_permits(), task_run.config.max_tcp_connections),
                        ("clients", task_run.client_limit.available_permits(), task_run.config.max_clients),
                        ("static", task_run.static_requests.available_permits(), task_run.config.static_workers + task_run.config.static_queue),
                    ] {
                        if available != expected {
                            return Err(RemoteError::new("shutdown_incomplete", format!("Remote run retained {name} leases: {available}/{expected}")));
                        }
                    }
                    result
                })).unwrap_or_else(|_| Err(RemoteError::new("worker_panicked", "Remote worker panicked")));
                if let Some(ready) = ready {
                    let _ = ready.send(result.clone());
                }
                let _ = done.send(result);
            }).map_err(io_error)?;
        *self.inner.current.lock().unwrap() = Some(run.clone());
        *active = Some(Active {
            run: run.clone(),
            bound: bound.clone(),
            done: finished,
            thread: Some(thread),
        });
        let mut guard = StartGuard { run, armed: true };
        match started.await {
            Ok(Ok(())) => {
                guard.armed = false;
                for (kind, data) in &guard.run.spec.data {
                    self.inner.data.lock().unwrap().insert(*kind, data.clone());
                }
                Ok(bound)
            }
            other => {
                self.inner.current.lock().unwrap().take();
                if let Some(candidate) = active.as_mut() {
                    let _ = candidate.stop().await;
                }
                active.take();
                match other {
                    Ok(Err(error)) => Err(error),
                    _ => Err(RemoteError::new(
                        "worker_unavailable",
                        "Remote startup channel closed",
                    )),
                }
            }
        }
    }
    pub fn update_server_data(&self, data: &Value) -> Result<()> {
        semantics::check_size(data, self.inner.config.max_socket_payload)?;
        if data.is_null() {
            return Err(RemoteError::new(
                "invalid_server_data",
                "SERVER_DATA requires non-null data",
            ));
        }
        let mut target = self.inner.data.lock().unwrap();
        for kind in ServerKind::ALL {
            target.insert(
                kind,
                data.get(kind.key())
                    .filter(|v| semantics::truthy(v))
                    .map(semantics::normalize)
                    .unwrap_or(json!({})),
            );
        }
        Ok(())
    }
    pub fn get_server_data(&self, kind: ServerKind) -> Value {
        self.inner
            .data
            .lock()
            .unwrap()
            .get(&kind)
            .cloned()
            .unwrap_or(json!({}))
    }
    fn current(&self, generation: &RemoteGeneration) -> Result<Arc<Run>> {
        let run = self
            .inner
            .current
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| RemoteError::new("stale_generation", "Remote service stopped"))?;
        if &run.generation != generation || run.cancel.is_cancelled() {
            return Err(RemoteError::new(
                "stale_generation",
                "Remote command belongs to a previous/stopped run",
            ));
        }
        Ok(run)
    }
    async fn send(
        &self,
        generation: &RemoteGeneration,
        server: ServerKind,
        message: &Value,
        mode: SendMode,
    ) -> Result<()> {
        let run = self.current(generation)?;
        semantics::check_size(message, run.config.max_socket_payload)?;
        if !run.spec.ports.contains_key(&server) {
            return Err(RemoteError::new(
                "server_disabled",
                "Remote server is disabled",
            ));
        }
        let (reply, receive) = oneshot::channel();
        run.commands
            .try_send(Command {
                server,
                message: semantics::wire(&semantics::normalize(message)),
                mode,
                reply,
            })
            .map_err(|error| {
                RemoteError::new(
                    if matches!(error, mpsc::error::TrySendError::Closed(_)) {
                        "stale_generation"
                    } else {
                        "command_queue_full"
                    },
                    "Remote output admission unavailable",
                )
            })?;
        tokio::select! {result=receive=>result.unwrap_or_else(|_|Err(RemoteError::new("cancelled","Remote output owner stopped"))),_=run.cancel.cancelled()=>Err(RemoteError::new("cancelled","Remote run stopped"))}
    }
    pub async fn renderer_send(
        &self,
        generation: &RemoteGeneration,
        server: ServerKind,
        message: &Value,
    ) -> Result<()> {
        self.send(generation, server, message, SendMode::Renderer)
            .await
    }
    pub async fn broadcast_backend(
        &self,
        generation: &RemoteGeneration,
        server: ServerKind,
        message: &Value,
    ) -> Result<()> {
        self.send(generation, server, message, SendMode::Backend)
            .await
    }
    pub async fn send_stage_subscribers(
        &self,
        generation: &RemoteGeneration,
        message: &Value,
    ) -> Result<()> {
        self.send(
            generation,
            ServerKind::STAGE,
            message,
            SendMode::Subscribers,
        )
        .await
    }
    pub fn stage_subscribers(&self, generation: &RemoteGeneration) -> Result<Vec<String>> {
        Ok(self.current(generation)?.subscriber_ids())
    }
    pub async fn endpoints(&self) -> Option<BoundServers> {
        self.inner
            .active
            .lock()
            .await
            .as_ref()
            .map(|v| v.bound.clone())
    }
    pub fn stats(&self) -> Option<RemoteStats> {
        self.inner
            .current
            .lock()
            .unwrap()
            .as_ref()
            .map(|run| run.stats())
    }
    pub async fn stop(&self) -> Result<()> {
        let _admission =
            self.inner.lifecycle.try_acquire().map_err(|_| {
                RemoteError::new("lifecycle_queue_full", "Remote lifecycle queue full")
            })?;
        let mut active = self.inner.active.lock().await;
        self.inner.current.lock().unwrap().take();
        if let Some(run) = active.as_mut() {
            let result = run.stop().await;
            active.take();
            result?;
        }
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<()> {
        // A terminal shutdown owns a separate control path: ordinary lifecycle
        // saturation cannot prevent cancellation or leave a listener behind.
        self.inner.terminal.store(true, Ordering::Release);
        if let Some(run) = self.inner.current.lock().unwrap().as_ref() {
            run.close_admission();
        }
        let mut active = self.inner.active.lock().await;
        self.inner.current.lock().unwrap().take();
        if let Some(run) = active.as_mut() {
            let result = run.stop().await;
            active.take();
            result?;
        }
        Ok(())
    }
    fn ensure_open(&self) -> Result<()> {
        if self.inner.terminal.load(Ordering::Acquire) {
            Err(RemoteError::new("shutdown", "Remote service has shut down"))
        } else {
            Ok(())
        }
    }
}
impl Active {
    async fn stop(&mut self) -> Result<()> {
        self.run.close_admission();
        let result = (&mut self.done).await.unwrap_or_else(|_| {
            Err(RemoteError::new(
                "worker_unavailable",
                "Remote completion channel closed",
            ))
        });
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| RemoteError::new("worker_panicked", "Remote worker join failed"))?;
        }
        result
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        self.run.close_admission();
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(active) = self.active.get_mut().take() {
            drop(active);
        }
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

struct Transport {
    run: Arc<Run>,
    io: BTreeMap<ServerKind, SocketIo>,
    raw: BTreeMap<ServerKind, Router>,
    tasks: Vec<JoinHandle<()>>,
}
impl Transport {
    fn start(
        listeners: BTreeMap<ServerKind, std::net::TcpListener>,
        run: Arc<Run>,
        pool: crate::static_files::PoolSender,
    ) -> Result<Self> {
        let mut transport = Self {
            run: run.clone(),
            io: BTreeMap::new(),
            raw: BTreeMap::new(),
            tasks: vec![],
        };
        for (kind, listener) in listeners {
            let parser = socketioxide::ParserConfig::common()
                .max_incoming_binary_buf_size(run.config.max_socket_payload)
                .max_incoming_binaries(1)
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
            connect(&io, kind, run.clone());
            let static_run = run.clone();
            let pool = pool.clone();
            let raw = Router::new()
                .fallback(any(move |request: Request| {
                    crate::static_files::serve(request, kind, static_run.clone(), pool.clone())
                }))
                .with_state(())
                .layer(layer);
            let router = raw.clone().layer(middleware::from_fn_with_state(
                Endpoint {
                    run: run.clone(),
                    kind,
                },
                engine_admission,
            ));
            transport.raw.insert(kind, raw);
            transport.tasks.push(tokio::spawn(serve_tcp(
                TcpListener::from_std(listener).map_err(io_error)?,
                router,
                run.clone(),
                kind,
            )));
            transport.io.insert(kind, io);
        }
        Ok(transport)
    }
    async fn work(&mut self, mut commands: mpsc::Receiver<Command>) -> Result<()> {
        loop {
            tokio::select! {biased;_=self.run.cancel.cancelled()=>break, error=injected_fault(&self.run)=>return error,command=commands.recv()=>{let Some(command)=command else{break};let result=self.emit(&command);let _=command.reply.send(result);}}
        }
        self.run.close_admission();
        commands.close();
        while let Ok(command) = commands.try_recv() {
            let _ = command
                .reply
                .send(Err(RemoteError::new("cancelled", "Remote run stopped")));
        }
        Ok(())
    }
    async fn close(&mut self) -> Result<()> {
        self.run.close_admission();
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
        self.run.tracker.close();
        self.run.tracker.wait().await;
        for io in self.io.values() {
            for socket in io.sockets() {
                let _ = socket.disconnect();
            }
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while self.io.values().any(|io| !io.sockets().is_empty()) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| {
            RemoteError::new(
                "shutdown_incomplete",
                "Remote namespace disconnect callbacks did not drain",
            )
        })?;
        self.run.close_clients();
        let sids = self
            .run
            .polling_sids
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for tagged in sids {
            if let Some((channel, sid)) = tagged.split_once(':') {
                if let Some(kind) = ServerKind::ALL
                    .into_iter()
                    .find(|kind| kind.channel() == channel)
                {
                    if let Some(raw) = self.raw.get(&kind) {
                        let request = Request::builder()
                            .method(Method::POST)
                            .uri(format!("/socket.io/?EIO=4&transport=polling&sid={sid}"))
                            .header("content-type", "text/plain;charset=UTF-8")
                            .body(Body::from("1"))
                            .unwrap();
                        let _ = raw.clone().oneshot(request).await;
                    }
                }
            }
        }
        for io in self.io.values() {
            io.close().await;
        }
        self.io.clear();
        self.raw.clear();
        tokio::time::timeout(Duration::from_secs(1), async {
            while self.run.engine_sessions.available_permits()
                != self.run.config.max_engine_sessions
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| {
            RemoteError::new(
                "shutdown_incomplete",
                "Remote Engine.IO session references did not drain",
            )
        })?;
        Ok(())
    }
    fn emit(&self, command: &Command) -> Result<()> {
        let io = self
            .io
            .get(&command.server)
            .ok_or_else(|| RemoteError::new("server_disabled", "Remote server disabled"))?;
        if matches!(command.mode, SendMode::Backend) {
            if command.message.is_null() {
                return Err(RemoteError::new(
                    "invalid_message",
                    "Backend message must be non-null",
                ));
            }
            if command.message.get("channel") == Some(&json!("STREAM"))
                && !self.run.gate(&command.message, false)?
            {
                return Ok(());
            }
        }
        let target = match command.mode {
            SendMode::Renderer => semantics::target(&command.message),
            SendMode::Backend => Target::Broadcast,
            SendMode::Subscribers => Target::Ids(self.run.subscriber_ids().into_iter().collect()),
        };
        let mut failure = None;
        for socket in io.sockets() {
            if !self
                .run
                .clients
                .lock()
                .unwrap()
                .contains_key(&(command.server, socket.id.to_string()))
            {
                continue;
            }
            if matches!(&target,Target::Ids(ids)if !ids.contains(&socket.id.to_string())) {
                continue;
            }
            if let Err(error) = socket.emit(command.server.channel(), &command.message) {
                failure = Some(RemoteError::new("socket_output_full", error.to_string()));
            }
        }
        failure.map_or(Ok(()), Err)
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.run.close_admission();
        self.run.close_clients();
        for task in &self.tasks {
            task.abort();
        }
    }
}
fn connect(io: &SocketIo, kind: ServerKind, run: Arc<Run>) {
    io.ns("/",move|socket:SocketRef|{let run=run.clone();async move{
        if run.cancel.is_cancelled(){let _=socket.disconnect();return;}
        let count=run.clients.lock().unwrap().keys().filter(|(server,_)|*server==kind).count();
        if count as f64>=semantics::number(run.spec.max.as_ref()){
            let mut error=json!({"channel":"ERROR","id":"overLimit"});if let Some(max)=&run.spec.max{error["data"]=max.clone();}
            let _=socket.emit(kind.channel(),&error);let _=socket.broadcast().emit(kind.channel(),&error).await;let _=socket.disconnect();return;
        }
        let admission=(||{let permit=run.client_limit.clone().try_acquire_owned().map_err(|_|RemoteError::new("client_limit","Remote client limit reached"))?;let connected=run.controls.clone().try_reserve_owned().map_err(|_|RemoteError::new("control_queue_full","Connection event capacity reached"))?;let disconnect=run.controls.clone().try_reserve_owned().map_err(|_|RemoteError::new("control_queue_full","Disconnect event capacity reached"))?;Ok((permit,connected,disconnect))})();
        let(permit,connected,disconnect)=match admission{Ok(value)=>value,Err(error)=>{run.diagnostic(kind,error);let _=socket.disconnect();return;}};
        let id=socket.id.to_string();let ua=socket.req_parts().headers.get("user-agent").map(|v|String::from_utf8_lossy(v.as_bytes()).into_owned()).unwrap_or_default();
        run.clients.lock().unwrap().insert((kind,id.clone()),Client{disconnect,_permit:permit});
        connected.send(run.event(kind,RemoteEventKind::Renderer{origin:RemoteOrigin::Connection,source_socket:Some(id.clone()),message:json!({"channel":"CONNECTION","id":id,"data":{"name":semantics::get_os(&ua)}})}));
        socket.on(kind.channel(),DataHandler{run:run.clone(),kind});socket.on_disconnect(move||{run.disconnect(kind,&id);std::future::ready(())});
    }});
}
struct DataHandler {
    run: Arc<Run>,
    kind: ServerKind,
}
impl MessageHandler<LocalAdapter, ()> for DataHandler {
    fn call(&self, socket: Arc<Socket>, value: socketioxide::handler::Value, _ack: Option<i64>) {
        if self.run.cancel.is_cancelled() {
            return;
        }
        let socket = SocketRef::from(socket);
        let id = socket.id.to_string();
        let parsed = (|| {
            if matches!(&value,socketioxide::handler::Value::Str(_,Some(binary))if !binary.is_empty())
            {
                return Err(RemoteError::new(
                    "unsupported_binary",
                    "Binary Socket.IO payloads cannot cross the JSON renderer contract",
                ));
            }
            let raw = value.as_str().ok_or_else(|| {
                RemoteError::new("invalid_message", "Expected JSON Socket.IO event")
            })?;
            let args = semantics::parse_json(raw)?;
            let message = args
                .as_array()
                .and_then(|args| args.get(1))
                .ok_or_else(|| {
                    RemoteError::new("invalid_message", "Missing remote message argument")
                })?
                .clone();
            if message.is_null() {
                return Err(RemoteError::new(
                    "invalid_message",
                    "Null remote message has no channel",
                ));
            }
            Ok(message)
        })();
        let message = match parsed {
            Ok(message) => message,
            Err(error) => {
                self.run.diagnostic(self.kind, error);
                let _ = socket.disconnect();
                return;
            }
        };
        if message.get("channel") == Some(&json!("STREAM_DONE")) {
            if let Err(error) = self.run.gate(&message, true) {
                self.run.diagnostic(self.kind, error);
            }
            return;
        }
        if message.get("channel") == Some(&json!("OUTPUT_FRAME")) {
            self.run.diagnostic(
                self.kind,
                RemoteError::new(
                    "unsupported_output_frame",
                    "Output capture requires a separate host capability",
                ),
            );
            return;
        }
        if !semantics::truthy(&message) {
            return;
        }
        if self.kind == ServerKind::STAGE {
            match message.get("channel").and_then(Value::as_str) {
                Some("STREAM_SUBSCRIBE") => {
                    let expiry = (self.run.config.clock)().saturating_add(10_000);
                    let mut subscribers = self.run.subscribers.lock().unwrap();
                    if let Some((_, current)) = subscribers.iter_mut().find(|(key, _)| key == &id) {
                        *current = expiry;
                    } else {
                        subscribers.push((id.clone(), expiry));
                    }
                }
                Some("STREAM_UNSUBSCRIBE") => {
                    self.run
                        .subscribers
                        .lock()
                        .unwrap()
                        .retain(|(key, _)| key != &id);
                }
                _ => {}
            }
        }
        let event = self.run.event(
            self.kind,
            RemoteEventKind::Renderer {
                origin: RemoteOrigin::Client,
                source_socket: Some(id),
                message,
            },
        );
        if self.run.events.try_send(event).is_err() {
            self.run.diagnostic(
                self.kind,
                RemoteError::new(
                    "event_queue_full",
                    "Remote renderer event queue unavailable",
                ),
            );
            let _ = socket.disconnect();
        }
    }
}
fn io_error(error: io::Error) -> RemoteError {
    RemoteError::new("io_error", error.to_string())
}
fn bind_tcp(ip: IpAddr, port: u16) -> Result<std::net::TcpListener> {
    fn bind(ip: IpAddr, port: u16) -> io::Result<socket2::Socket> {
        let socket = socket2::Socket::new(
            socket2::Domain::for_address((ip, port).into()),
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )?;
        if ip.is_ipv6() {
            socket.set_only_v6(false)?;
        }
        #[cfg(unix)]
        socket.set_reuse_address(true)?;
        socket.bind(&std::net::SocketAddr::from((ip, port)).into())?;
        socket.listen(511)?;
        Ok(socket)
    }
    let socket = match bind(ip, port) {
        Err(error)
            if matches!(ip,IpAddr::V6(ip)if ip.is_unspecified())
                && error.kind() != io::ErrorKind::AddrInUse =>
        {
            bind(Ipv4Addr::UNSPECIFIED.into(), port)
        }
        result => result,
    }
    .map_err(|error| RemoteError::new("bind_failed", format!("TCP {port}: {error}")))?;
    Ok(socket.into())
}
fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        [("content-type", "application/json")],
        json!({"code":code,"message":message}).to_string(),
    )
        .into_response()
}
fn socket_query(raw: &str) -> Value {
    let mut map = Map::new();
    for (key, value) in form_urlencoded::parse(raw.as_bytes()).take(1000) {
        let value = Value::String(value.into_owned());
        match map.get_mut(key.as_ref()) {
            None => {
                map.insert(key.into_owned(), value);
            }
            Some(Value::Array(values)) => values.push(value),
            Some(previous) => {
                *previous = Value::Array(vec![previous.take(), value]);
            }
        }
    }
    Value::Object(map)
}
#[derive(Clone)]
struct Endpoint {
    run: Arc<Run>,
    kind: ServerKind,
}
async fn engine_admission(
    State(endpoint): State<Endpoint>,
    request: Request,
    next: Next,
) -> Response {
    let run = endpoint.run;
    let kind = endpoint.kind;
    if request.uri().path().starts_with("/socket.io/") {
        if request.method() == Method::POST {
            match tokio::time::timeout(
                run.config.body_timeout,
                engine_request(run.clone(), kind, request, next),
            )
            .await
            {
                Ok(response) => response,
                Err(_) => error_response(
                    StatusCode::REQUEST_TIMEOUT,
                    "body_timeout",
                    "Remote body deadline",
                ),
            }
        } else {
            engine_request(run, kind, request, next).await
        }
    } else {
        next.run(request).await
    }
}

include!("transport.rs");

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "cover_tests.rs"]
mod cover_tests;
