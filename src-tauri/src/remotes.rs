//! Bounded remote lifecycle, frozen context and completed renderer delivery.
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::LibraryService,
    WireError,
};
use freeshow_remotes::{
    BoundServers, RemoteConfig, RemoteEventKind, RemoteEvents, RemoteGeneration, RemoteOrigin,
    RemoteService, ServerKind,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{
    mpsc, oneshot, watch, Mutex as AsyncMutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore,
};
use tokio_util::sync::CancellationToken;

pub const COMMANDS: &[&str] = &["START", "STOP", "SERVER_DATA"];
const MAX_REQUESTS: usize = 32;
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);
const RETIRE_TIMEOUT: Duration = Duration::from_secs(15);
type Sink = Arc<dyn Fn(Event) -> Result<(), String> + Send + Sync>;

struct Binding {
    context: RuntimeContext,
    bound: BoundServers,
    retiring: bool,
    connections: BTreeMap<(ServerKind, String), bool>,
}
struct Delivery {
    id: String,
    reply: oneshot::Sender<bool>,
}
struct Shared {
    library: LibraryService,
    service: RemoteService,
    binding: Mutex<Option<Binding>>,
    publication: AsyncMutex<()>,
    pending: Mutex<Option<Delivery>>,
    closing: AtomicBool,
    terminal: CancellationToken,
    fault: CancellationToken,
    failure: Mutex<Option<(&'static str, String)>>,
}
struct Job {
    request: Request,
    reply: oneshot::Sender<Result<Vec<Event>, WireError>>,
    _permit: OwnedSemaphorePermit,
}
struct ContextJob {
    reply: oneshot::Sender<Result<(), WireError>>,
    _guard: OwnedMutexGuard<()>,
}
enum PumpControl {
    Retire {
        generation: RemoteGeneration,
        reply: oneshot::Sender<Result<(), WireError>>,
    },
    Stop,
}
#[derive(Clone)]
pub struct NativeRemotes {
    shared: Arc<Shared>,
    jobs: mpsc::Sender<Job>,
    context: mpsc::Sender<ContextJob>,
    slots: Arc<Semaphore>,
    context_gate: Arc<AsyncMutex<()>>,
    done: watch::Receiver<bool>,
    pump_done: watch::Receiver<bool>,
}
fn error(error: freeshow_remotes::RemoteError) -> WireError {
    WireError::new(error.code, error.message)
}
fn stopped() -> WireError {
    WireError::new("remotes_shutdown", "Native remote admission is closed")
}
fn worker(message: &str) -> WireError {
    WireError::new("remotes_worker", message)
}
impl NativeRemotes {
    pub fn new(
        library: LibraryService,
        config: RemoteConfig,
        sink: impl Fn(Event) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<Self, WireError> {
        let (service, events) = RemoteService::new(config).map_err(error)?;
        let shared = Arc::new(Shared {
            library,
            service,
            binding: Mutex::new(None),
            publication: AsyncMutex::new(()),
            pending: Mutex::new(None),
            closing: AtomicBool::new(false),
            terminal: CancellationToken::new(),
            fault: CancellationToken::new(),
            failure: Mutex::new(None),
        });
        let (jobs, receive) = mpsc::channel(MAX_REQUESTS);
        let (context, changes) = mpsc::channel(1);
        let (pump_control, controls) = mpsc::channel(1);
        let (complete, done) = watch::channel(false);
        let (pump_complete, pump_done) = watch::channel(false);
        tokio::spawn(pump(
            shared.clone(),
            events,
            controls,
            Arc::new(sink),
            pump_complete,
        ));
        tokio::spawn(owner(
            shared.clone(),
            receive,
            changes,
            pump_control,
            complete,
        ));
        Ok(Self {
            shared,
            jobs,
            context,
            slots: Arc::new(Semaphore::new(MAX_REQUESTS)),
            context_gate: Arc::new(AsyncMutex::new(())),
            done,
            pump_done,
        })
    }
    pub fn status(&self) -> Value {
        let binding = self
            .shared
            .binding
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        json!({"closing":self.shared.closing.load(Ordering::Acquire),
            "admittedRequests":MAX_REQUESTS-self.slots.available_permits(),
            "pendingDeliveries":usize::from(self.shared.pending.lock().unwrap_or_else(|e|e.into_inner()).is_some()),
            "binding":binding.as_ref().map(|b|json!({"context":b.context,"bound":b.bound,
                "retiring":b.retiring,"connections":b.connections.len()})),
            "service":self.shared.service.stats(),
            "failure":*self.shared.failure.lock().unwrap_or_else(|e|e.into_inner())})
    }
    pub async fn dispatch(&self, request: Request, window: &str) -> Result<Vec<Event>, WireError> {
        if window != "main" {
            return Err(WireError::new(
                "window_error",
                "Only the main window handles remote clients",
            ));
        }
        if !(ServerKind::ALL
            .iter()
            .any(|kind| kind.channel() == request.channel)
            || request.channel == "MAIN"
                && COMMANDS.contains(&request.data["channel"].as_str().unwrap_or("")))
        {
            return Err(WireError::new(
                "unsupported_command",
                "Unknown remote command",
            ));
        }
        if self.shared.closing.load(Ordering::Acquire) {
            return Err(stopped());
        }
        if !within_limit(&request.data, 1_000_000)
            || request
                .listener_id
                .as_ref()
                .is_some_and(|id| id.len() > 256)
        {
            return Err(WireError::new(
                "remotes_input_limit",
                "Remote IPC payload exceeds its native limit",
            ));
        }
        validate_context(&self.shared, &request)?;
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new("remotes_busy", "Native remote requests are at capacity")
        })?;
        let (reply, response) = oneshot::channel();
        self.jobs
            .try_send(Job {
                request,
                reply,
                _permit: permit,
            })
            .map_err(|_| {
                if self.shared.closing.load(Ordering::Acquire) {
                    stopped()
                } else {
                    worker("Remote owner is unavailable")
                }
            })?;
        response.await.map_err(|_| stopped())?
    }
    /// Direct private completion never waits behind lifecycle or public IPC work.
    pub fn complete(&self, window: &str, id: &str, accepted: bool) -> Result<(), WireError> {
        if window != "main" {
            return Err(WireError::new(
                "window_error",
                "Only main completes remote delivery",
            ));
        }
        if id.len() != 46 || !id.starts_with("native-remote-") {
            return Err(WireError::new(
                "invalid_remote_completion",
                "Invalid private delivery ID",
            ));
        }
        let mut pending = self
            .shared
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if pending.as_ref().is_none_or(|delivery| delivery.id != id) {
            return Err(WireError::new(
                "invalid_remote_completion",
                "Unknown or consumed delivery ID",
            ));
        }
        let _ = pending.take().unwrap().reply.send(accepted);
        Ok(())
    }
    pub async fn context_changed(&self) -> Result<(), WireError> {
        let guard = self.context_gate.clone().lock_owned().await;
        if self.shared.closing.load(Ordering::Acquire) {
            return Err(stopped());
        }
        let (reply, response) = oneshot::channel();
        self.context
            .try_send(ContextJob {
                reply,
                _guard: guard,
            })
            .map_err(|_| worker("Remote context control is unavailable"))?;
        response.await.map_err(|_| stopped())?
    }
    pub async fn shutdown(&self) -> Result<(), WireError> {
        self.shared.closing.store(true, Ordering::Release);
        self.shared.terminal.cancel();
        for mut done in [self.done.clone(), self.pump_done.clone()] {
            while !*done.borrow_and_update() {
                done.changed()
                    .await
                    .map_err(|_| worker("Remote teardown stopped without completion"))?;
            }
        }
        if let Some((code, message)) = self
            .shared
            .failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return Err(WireError::new(code, message.clone()));
        }
        Ok(())
    }
}
fn validate_context(shared: &Shared, request: &Request) -> Result<(), WireError> {
    let context = request
        .context
        .as_ref()
        .ok_or_else(|| WireError::new("stale_library", "Remote IPC requires a frozen context"))?;
    if &shared.library.context().map_err(WireError::from)? != context {
        return Err(WireError::new(
            "stale_library",
            "Remote IPC belongs to an obsolete library",
        ));
    }
    Ok(())
}
fn record_failure(shared: &Shared, error: WireError) {
    let mut failure = shared.failure.lock().unwrap_or_else(|e| e.into_inner());
    if failure.is_none() {
        *failure = Some((error.code, error.message));
    }
}
async fn owner(
    shared: Arc<Shared>,
    mut jobs: mpsc::Receiver<Job>,
    mut changes: mpsc::Receiver<ContextJob>,
    pump: mpsc::Sender<PumpControl>,
    complete: watch::Sender<bool>,
) {
    loop {
        tokio::select! {biased;
            _=shared.terminal.cancelled()=>break,
            _=shared.fault.cancelled()=>break,
            Some(change)=changes.recv()=>{
                let result=tokio::select!{biased;
                    _=shared.terminal.cancelled()=>Err(stopped()),
                    _=shared.fault.cancelled()=>Err(stopped()),
                    result=refresh(&shared,&pump)=>result,
                };
                if let Err(Err(error))=change.reply.send(result){eprintln!("Detached remote context refresh failed: {}",error.message);}
            },
            Some(job)=jobs.recv()=>{
                let result=tokio::select!{biased;
                    _=shared.terminal.cancelled()=>Err(stopped()),
                    _=shared.fault.cancelled()=>Err(stopped()),
                    result=dispatch(&shared,&pump,job.request)=>result,
                };
                if let Err(Err(error))=job.reply.send(result){eprintln!("Detached remote request failed: {}",error.message);}
            },
            else=>break,
        }
    }
    shared.closing.store(true, Ordering::Release);
    jobs.close();
    changes.close();
    while let Ok(job) = jobs.try_recv() {
        let _ = job.reply.send(Err(stopped()));
    }
    while let Ok(change) = changes.try_recv() {
        let _ = change.reply.send(Err(stopped()));
    }
    // Terminal cancellation bypasses ordinary work. The service cancels its
    // current run before waiting for lifecycle ownership, then joins its thread.
    if let Err(error) = shared.service.shutdown().await.map_err(error) {
        record_failure(&shared, error);
    }
    if let Err(error) = retire(&shared, &pump, false).await {
        record_failure(&shared, error);
    }
    let _ = pump.send(PumpControl::Stop).await;
    complete.send_replace(true);
}
async fn dispatch(
    shared: &Shared,
    pump: &mpsc::Sender<PumpControl>,
    request: Request,
) -> Result<Vec<Event>, WireError> {
    validate_context(shared, &request)?;
    if request.channel == "MAIN" {
        match request.data["channel"].as_str().unwrap_or("") {
            "START" => {
                retire(shared, pump, true).await?;
                validate_context(shared, &request)?;
                start(
                    shared,
                    request.context.unwrap(),
                    request.data["data"].clone(),
                )
                .await?;
            }
            "STOP" => retire(shared, pump, true).await?,
            "SERVER_DATA" => shared
                .service
                .update_server_data(&request.data["data"])
                .map_err(error)?,
            _ => return Err(worker("Unknown remote lifecycle command")),
        }
    } else {
        let generation = {
            let binding = shared.binding.lock().unwrap_or_else(|e| e.into_inner());
            let current = binding
                .as_ref()
                .filter(|b| !b.retiring && Some(&b.context) == request.context.as_ref())
                .ok_or_else(|| {
                    WireError::new("stale_generation", "Remote listener is absent or obsolete")
                })?;
            current.bound.generation.clone()
        };
        let kind = ServerKind::ALL
            .into_iter()
            .find(|kind| kind.channel() == request.channel)
            .unwrap();
        shared
            .service
            .renderer_send(&generation, kind, &request.data)
            .await
            .map_err(error)?;
    }
    Ok(Vec::new())
}
async fn start(shared: &Shared, context: RuntimeContext, options: Value) -> Result<(), WireError> {
    // Early connections wait until their complete frozen binding is installed.
    let _publication = shared.publication.lock().await;
    let bound = shared
        .service
        .start(&options, context.library_generation)
        .await
        .map_err(error)?;
    if shared.library.context().map_err(WireError::from)? != context {
        shared.service.stop().await.map_err(error)?;
        return Err(WireError::new(
            "stale_library",
            "Library changed while remote listeners started",
        ));
    }
    *shared.binding.lock().unwrap_or_else(|e| e.into_inner()) = Some(Binding {
        context,
        bound,
        retiring: false,
        connections: BTreeMap::new(),
    });
    Ok(())
}
async fn refresh(shared: &Shared, pump: &mpsc::Sender<PumpControl>) -> Result<(), WireError> {
    let context = shared.library.context().map_err(WireError::from)?;
    let changed = shared
        .binding
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|previous| previous.context != context);
    // The retained SETTINGS handler starts the replacement only after its
    // DATA_PATH response has advanced the bridge. Starting here could lose a
    // new connection before the renderer knows its library context.
    if changed {
        retire(shared, pump, true).await?;
    }
    Ok(())
}
async fn retire(
    shared: &Shared,
    pump: &mpsc::Sender<PumpControl>,
    stop: bool,
) -> Result<(), WireError> {
    let publication = shared.publication.lock().await;
    let generation = {
        let mut binding = shared.binding.lock().unwrap_or_else(|e| e.into_inner());
        binding.as_mut().map(|current| {
            current.retiring = true;
            current.bound.generation.clone()
        })
    };
    drop(publication);
    let stopped = if stop {
        shared.service.stop().await.map_err(error)
    } else {
        Ok(())
    };
    if let Some(generation) = generation {
        let (reply, response) = oneshot::channel();
        pump.send(PumpControl::Retire {
            generation: generation.clone(),
            reply,
        })
        .await
        .map_err(|_| worker("Remote cleanup pump is unavailable"))?;
        response
            .await
            .map_err(|_| worker("Remote cleanup stopped before completion"))??;
        let mut binding = shared.binding.lock().unwrap_or_else(|e| e.into_inner());
        if binding
            .as_ref()
            .is_some_and(|b| b.bound.generation == generation)
        {
            binding.take();
        }
    }
    stopped
}
struct Publication {
    context: RuntimeContext,
    generation: RemoteGeneration,
    server: ServerKind,
    socket: String,
    origin: RemoteOrigin,
    message: Value,
}
fn begin_delivery(
    shared: &Shared,
    sink: &Sink,
    publication: Publication,
) -> Result<(String, oneshot::Receiver<bool>), WireError> {
    let id = format!("native-remote-{}", uuid::Uuid::new_v4().simple());
    let (reply, response) = oneshot::channel();
    *shared.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(Delivery {
        id: id.clone(),
        reply,
    });
    let mut wire = Event::reply(
        publication.server.channel(),
        "NATIVE_REMOTE",
        Value::Null,
        None,
    );
    wire.context = Some(publication.context);
    wire.native_remote = Some(
        json!({"deliveryId":id,"run":publication.generation.run,"server":publication.server,
        "sourceSocket":publication.socket,"origin":publication.origin,"message":publication.message}),
    );
    if let Err(message) = sink(wire) {
        shared
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        return Err(WireError::new("remote_publication", message));
    }
    Ok((id, response))
}
async fn finish_delivery(
    shared: &Shared,
    delivery: Result<(String, oneshot::Receiver<bool>), WireError>,
) -> Result<bool, WireError> {
    let (id, response) = delivery?;
    struct PendingGuard<'a> {
        shared: &'a Shared,
        id: String,
    }
    impl Drop for PendingGuard<'_> {
        fn drop(&mut self) {
            let mut pending = self
                .shared
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if pending
                .as_ref()
                .is_some_and(|delivery| delivery.id == self.id)
            {
                pending.take();
            }
        }
    }
    let _pending = PendingGuard { shared, id };
    match tokio::time::timeout(DELIVERY_TIMEOUT, response).await {
        Ok(Ok(accepted)) => Ok(accepted),
        _ => Err(WireError::new(
            "remote_delivery_timeout",
            "Main window did not complete remote delivery",
        )),
    }
}

async fn cleanup(
    shared: &Shared,
    sink: &Sink,
    generation: &RemoteGeneration,
) -> Result<(), WireError> {
    loop {
        let next = {
            let binding = shared.binding.lock().unwrap_or_else(|e| e.into_inner());
            let Some(current) = binding
                .as_ref()
                .filter(|b| &b.bound.generation == generation)
            else {
                return Ok(());
            };
            current
                .connections
                .first_key_value()
                .map(|((kind, id), confirmed)| {
                    (current.context.clone(), *kind, id.clone(), *confirmed)
                })
        };
        let Some((context, server, id, confirmed)) = next else {
            return Ok(());
        };
        let delivery = begin_delivery(
            shared,
            sink,
            Publication {
                context,
                generation: generation.clone(),
                server,
                socket: id.clone(),
                origin: RemoteOrigin::Disconnect,
                message: json!({"channel":"DISCONNECT","id":id}),
            },
        );
        if !finish_delivery(shared, delivery).await? && confirmed {
            return Err(WireError::new(
                "remote_cleanup_rejected",
                "Main window rejected an owned disconnect",
            ));
        }
        if let Some(current) = shared
            .binding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .filter(|b| &b.bound.generation == generation)
        {
            current.connections.remove(&(server, id));
        }
    }
}
async fn pump(
    shared: Arc<Shared>,
    mut events: RemoteEvents,
    mut controls: mpsc::Receiver<PumpControl>,
    sink: Sink,
    complete: watch::Sender<bool>,
) {
    loop {
        tokio::select! {biased;
            Some(control)=controls.recv()=>match control {
                PumpControl::Stop=>break,
                PumpControl::Retire{generation,reply}=>{
                    let result=tokio::time::timeout(RETIRE_TIMEOUT,cleanup(&shared,&sink,&generation)).await
                        .unwrap_or_else(|_|Err(WireError::new("remote_retire_timeout","Remote renderer cleanup exceeded its aggregate deadline")));
                    let _=reply.send(result);
                },
            },
            Some(event)=events.recv()=>{
                let (origin,socket,message)=match event.kind {
                    RemoteEventKind::Renderer{origin,source_socket:Some(socket),message}=>(origin,socket,message),
                    RemoteEventKind::Diagnostic{code,message}=>{eprintln!("Remotes {code}: {message}");continue;},
                    _=>continue,
                };
                let publication=shared.publication.lock().await;
                let context={
                    let binding=shared.binding.lock().unwrap_or_else(|e|e.into_inner());
                    binding.as_ref().filter(|b|b.bound.generation==event.generation)
                        .filter(|b|if origin==RemoteOrigin::Disconnect{b.connections.contains_key(&(event.server,socket.clone()))}
                            else{!b.retiring && shared.library.context().ok().as_ref()==Some(&b.context)})
                        .map(|b|b.context.clone())
                };
                let Some(context)=context else{continue;};
                if origin==RemoteOrigin::Connection {
                    if let Some(current)=shared.binding.lock().unwrap_or_else(|e|e.into_inner()).as_mut().filter(|b|b.bound.generation==event.generation){
                        current.connections.insert((event.server,socket.clone()),false);
                    }
                }
                let delivery=begin_delivery(&shared,&sink,Publication{context,generation:event.generation.clone(),server:event.server,socket:socket.clone(),origin,message});
                drop(publication);
                match finish_delivery(&shared,delivery).await {
                    Ok(accepted)=>{
                        if let Some(current)=shared.binding.lock().unwrap_or_else(|e|e.into_inner()).as_mut().filter(|b|b.bound.generation==event.generation){
                            if accepted && origin==RemoteOrigin::Connection{current.connections.insert((event.server,socket),true);}
                            else if (accepted && origin==RemoteOrigin::Disconnect) || (!accepted && origin==RemoteOrigin::Connection){current.connections.remove(&(event.server,socket));}
                        }
                    },
                    Err(error)=>{
                        record_failure(&shared,error);
                        shared.closing.store(true,Ordering::Release);
                        if let Some(current)=shared.binding.lock().unwrap_or_else(|e|e.into_inner()).as_mut(){current.retiring=true;}
                        shared.fault.cancel();
                    },
                }
            },
            else=>break,
        }
    }
    complete.send_replace(true);
}
fn within_limit(value: &Value, limit: usize) -> bool {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("Remote IPC byte limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(limit), value).is_ok()
}
