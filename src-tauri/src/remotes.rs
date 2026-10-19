//! Bounded remote lifecycle, frozen context and completed renderer delivery.
use crate::discovery::{
    DiscoveryRun, NativeDiscovery, NativeDiscoveryEvent, NativeDiscoveryEvents,
};
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::LibraryService,
    WireError,
};
use freeshow_discovery::DiscoveryName;
use freeshow_remotes::{
    BoundServers, CoverSource, RemoteConfig, RemoteEventKind, RemoteEvents, RemoteGeneration,
    RemoteOrigin, RemoteService, ServerKind,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    future::Future,
    io::{self, Write},
    pin::Pin,
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
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
type Sink = Arc<dyn Fn(Event) -> Result<(), String> + Send + Sync>;
type NativeFuture<T> = Pin<Box<dyn Future<Output = Result<T, WireError>> + Send>>;

enum DiscoveryPhase {
    Starting(NativeFuture<DiscoveryRun>),
    Ready(DiscoveryRun),
    Stopping(DiscoveryRun, NativeFuture<()>),
}
#[derive(Default)]
struct DiscoveryEvidence {
    accepted: u64,
    stale: u64,
    warnings: u64,
    last: Option<Value>,
    last_warning: Option<Value>,
}
struct DiscoveryOwner {
    service: NativeDiscovery,
    phase: AsyncMutex<Option<DiscoveryPhase>>,
    drain: AsyncMutex<Option<NativeFuture<()>>>,
    joined: AtomicBool,
    evidence: Mutex<DiscoveryEvidence>,
    #[cfg(test)]
    timeout: Mutex<Duration>,
    #[cfg(test)]
    drain_hook: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
}
impl DiscoveryOwner {
    fn timeout(&self) -> Duration {
        #[cfg(test)]
        return *self.timeout.lock().unwrap();
        #[cfg(not(test))]
        DISCOVERY_TIMEOUT
    }
    fn warning(&self, stage: &str, error: &WireError) {
        let mut message = error.message.clone();
        if message.len() > 2048 {
            let mut end = 2030;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            message.push_str(" [truncated]");
        }
        eprintln!("Discovery {stage} {}: {message}", error.code);
        let mut evidence = self.evidence.lock().unwrap_or_else(|e| e.into_inner());
        evidence.warnings = evidence.warnings.saturating_add(1);
        evidence.last_warning = Some(json!({"stage":stage,"code":error.code,"message":message}));
    }
}

struct Binding {
    context: RuntimeContext,
    bound: BoundServers,
    retiring: bool,
    connections: BTreeMap<(ServerKind, String), bool>,
    discovery: Option<DiscoveryRun>,
}
struct Delivery {
    id: String,
    reply: oneshot::Sender<bool>,
}
struct Shared {
    library: LibraryService,
    service: RemoteService,
    discovery: Option<DiscoveryOwner>,
    binding: Mutex<Option<Binding>>,
    publication: AsyncMutex<()>,
    pending: Mutex<Option<Delivery>>,
    closing: AtomicBool,
    terminal: CancellationToken,
    fault: CancellationToken,
    failure: Mutex<Option<(&'static str, String)>>,
}
pub struct CoverStart {
    source: Arc<dyn CoverSource>,
    ready: Box<dyn FnOnce() -> Result<(), WireError> + Send>,
}
impl CoverStart {
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "Production START supplies the authority; Linux integration exercises this constructor"
        )
    )]
    pub fn new(
        source: Arc<dyn CoverSource>,
        ready: impl FnOnce() -> Result<(), WireError> + Send + 'static,
    ) -> Self {
        Self {
            source,
            ready: Box::new(ready),
        }
    }
}
struct Job {
    request: Request,
    cover: Option<CoverStart>,
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
    // Kept for transport-only users/tests alongside production discovery ownership.
    #[allow(dead_code)]
    pub fn new(
        library: LibraryService,
        config: RemoteConfig,
        sink: impl Fn(Event) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<Self, WireError> {
        Self::construct(library, config, None, sink)
    }
    /// Discovery is native-owned and must have the same frozen library owner.
    /// Its events are diagnostics only, never renderer/client success ACKs.
    pub fn new_with_discovery(
        library: LibraryService,
        config: RemoteConfig,
        discovery: NativeDiscovery,
        events: NativeDiscoveryEvents,
        sink: impl Fn(Event) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<Self, WireError> {
        Self::construct(library, config, Some((discovery, events)), sink)
    }
    fn construct(
        library: LibraryService,
        config: RemoteConfig,
        discovery: Option<(NativeDiscovery, NativeDiscoveryEvents)>,
        sink: impl Fn(Event) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<Self, WireError> {
        let (service, events) = RemoteService::new(config).map_err(error)?;
        let (discovery, discovery_events) = match discovery {
            Some((service, events)) => (
                Some(DiscoveryOwner {
                    service,
                    phase: AsyncMutex::new(None),
                    drain: AsyncMutex::new(None),
                    joined: AtomicBool::new(false),
                    evidence: Mutex::new(DiscoveryEvidence::default()),
                    #[cfg(test)]
                    timeout: Mutex::new(DISCOVERY_TIMEOUT),
                    #[cfg(test)]
                    drain_hook: Mutex::new(None),
                }),
                Some(events),
            ),
            None => (None, None),
        };
        let shared = Arc::new(Shared {
            library,
            service,
            discovery,
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
            discovery_events,
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
                "retiring":b.retiring,"connections":b.connections.len(),
                "discoveryRun":b.discovery.as_ref().map(|r|json!({"context":r.context(),"generation":r.generation(),"listenerRun":r.listener_run()}))})),
            "discovery":self.shared.discovery.as_ref().map(|d|{
                let evidence=d.evidence.lock().unwrap_or_else(|e|e.into_inner());
                json!({"joined":d.joined.load(Ordering::Acquire),"acceptedEvents":evidence.accepted,"staleEvents":evidence.stale,
                    "warnings":evidence.warnings,"lastEvent":evidence.last,"lastWarning":evidence.last_warning,"owner":d.service.status()})
            }),
            "service":self.shared.service.stats(),
            "failure":*self.shared.failure.lock().unwrap_or_else(|e|e.into_inner())})
    }
    pub async fn dispatch(&self, request: Request, window: &str) -> Result<Vec<Event>, WireError> {
        self.dispatch_with_cover(request, window, None).await
    }
    /// Native-only START authority is owned by its bounded accepted job/run.
    /// No factory or emitter is retained permanently in Shared/AppState.
    pub async fn dispatch_with_cover(
        &self,
        request: Request,
        window: &str,
        cover: Option<CoverStart>,
    ) -> Result<Vec<Event>, WireError> {
        if cover.is_some() && (request.channel != "MAIN" || request.data["channel"] != "START") {
            return Err(WireError::new(
                "cover_start",
                "Only START can configure a native cover source",
            ));
        }
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
                cover,
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
                if done.changed().await.is_err() {
                    // A lost legacy host task completion cannot skip discovery's
                    // independent owner. Preserve this error on all later calls.
                    record_failure(
                        &self.shared,
                        worker("Remote teardown stopped without completion"),
                    );
                    shutdown_discovery(&self.shared).await?;
                    return Err(worker("Remote teardown stopped without completion"));
                }
            }
        }
        // Command/pump completion is not proof that discovery descendants joined.
        // Retry the retained real drain after an earlier operation timeout.
        let discovery = shutdown_discovery(&self.shared).await;
        if discovery
            .as_ref()
            .is_err_and(|error| error.code == "remote_discovery_shutdown_timeout")
        {
            return discovery;
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
        discovery
    }
    #[cfg(all(test, unix))]
    #[allow(
        dead_code,
        reason = "Exercised by the dedicated remotes integration target; the binary unit harness does not invoke every fixture hook"
    )]
    pub(crate) fn test_discovery_drain(
        &self,
        timeout: Duration,
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    ) {
        let discovery = self.shared.discovery.as_ref().unwrap();
        *discovery.timeout.lock().unwrap() = timeout;
        *discovery.drain_hook.lock().unwrap() = Some((entered, release));
    }
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "Exercised by the dedicated remotes integration target; the binary unit harness does not invoke every fixture hook"
    )]
    pub(crate) fn test_discovery_timeout(&self, timeout: Duration) {
        *self
            .shared
            .discovery
            .as_ref()
            .unwrap()
            .timeout
            .lock()
            .unwrap() = timeout;
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
async fn next_discovery_event(
    events: &mut Option<NativeDiscoveryEvents>,
) -> Option<NativeDiscoveryEvent> {
    match events {
        Some(events) => events.recv().await,
        None => std::future::pending().await,
    }
}
fn observe_discovery(shared: &Shared, event: NativeDiscoveryEvent) {
    let Some(discovery) = &shared.discovery else {
        return;
    };
    let valid = shared
        .binding
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|binding| {
            !binding.retiring
                && binding.context == event.context
                && binding.bound.generation.run == event.listener_run
                && shared.library.context().ok().as_ref() == Some(&event.context)
                && binding.discovery.as_ref().is_some_and(|run| {
                    run.context() == &event.context
                        && run.listener_run() == event.listener_run
                        && run.generation() == &event.event.generation
                })
        });
    if !valid {
        let mut evidence = discovery.evidence.lock().unwrap_or_else(|e| e.into_inner());
        evidence.stale = evidence.stale.saturating_add(1);
        return;
    }
    if let freeshow_discovery::DiscoveryEventKind::Diagnostic { code, message } = &event.event.kind
    {
        discovery.warning("event", &WireError::new(code, message.clone()));
    }
    let mut evidence = discovery.evidence.lock().unwrap_or_else(|e| e.into_inner());
    evidence.accepted = evidence.accepted.saturating_add(1);
    evidence.last =
        Some(json!({"context":event.context,"listenerRun":event.listener_run,"event":event.event}));
}
async fn resolve_begin(phase: &mut Option<DiscoveryPhase>) -> Result<DiscoveryRun, WireError> {
    let result = match phase.as_mut() {
        Some(DiscoveryPhase::Starting(future)) => future.await,
        Some(DiscoveryPhase::Ready(run)) => return Ok(run.clone()),
        _ => return Err(worker("Discovery has an unfinished stop")),
    };
    match result {
        Ok(run) => {
            *phase = Some(DiscoveryPhase::Ready(run.clone()));
            Ok(run)
        }
        Err(error) => {
            *phase = None;
            Err(error)
        }
    }
}
async fn start_discovery(
    shared: &Shared,
    context: &RuntimeContext,
    bound: &BoundServers,
) -> Result<(), WireError> {
    let Some(discovery) = &shared.discovery else {
        return Ok(());
    };
    tokio::time::timeout(discovery.timeout(), async {
        let mut phase = discovery.phase.lock().await;
        if phase.is_some() {
            return Err(WireError::new(
                "remote_discovery_pending",
                "An earlier discovery lifecycle still owns its run",
            ));
        }
        let service = discovery.service.clone();
        let context = context.clone();
        let listener = bound.generation.run.clone();
        *phase = Some(DiscoveryPhase::Starting(Box::pin(async move {
            service.begin(&context, &listener).await
        })));
        let run = resolve_begin(&mut phase).await?;
        {
            let mut binding = shared.binding.lock().unwrap_or_else(|e| e.into_inner());
            let current = binding
                .as_mut()
                .filter(|b| b.bound.generation == bound.generation && !b.retiring)
                .ok_or_else(|| {
                    WireError::new(
                        "stale_generation",
                        "Listeners retired before discovery begin completed",
                    )
                })?;
            current.discovery = Some(run.clone());
        }
        for (kind, endpoint) in &bound.endpoints {
            let name = match kind {
                ServerKind::REMOTE => DiscoveryName::Remote,
                ServerKind::STAGE => DiscoveryName::Stage,
                ServerKind::CONTROLLER => DiscoveryName::Controller,
            };
            // Failure for one publication must not hide the other actual listeners.
            if let Err(error) = discovery.service.started_port(&run, name, *endpoint).await {
                discovery.warning("publish", &error);
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| {
        WireError::new(
            "remote_discovery_operation_timeout",
            "Discovery start/publication still owns native work",
        )
    })?
}
async fn stop_discovery(shared: &Shared) -> Result<(), WireError> {
    let Some(discovery) = &shared.discovery else {
        return Ok(());
    };
    tokio::time::timeout(discovery.timeout(), async {
        let mut phase = discovery.phase.lock().await;
        if discovery.joined.load(Ordering::Acquire) {
            phase.take();
            return Ok(());
        }
        if phase.is_none() {
            return Ok(());
        }
        if matches!(phase.as_ref(), Some(DiscoveryPhase::Starting(_))) {
            resolve_begin(&mut phase).await?;
        }
        if let Some(DiscoveryPhase::Ready(run)) = phase.as_ref() {
            let run = run.clone();
            let service = discovery.service.clone();
            let token = run.clone();
            *phase = Some(DiscoveryPhase::Stopping(
                run,
                Box::pin(async move { service.stop(&token).await }),
            ));
        }
        let (run, result) = match phase.as_mut() {
            Some(DiscoveryPhase::Stopping(run, future)) => (run.clone(), future.await),
            _ => return Ok(()),
        };
        // A failed stop does not authorize losing the opaque token. Terminal
        // cleanup still owns the complete NativeDiscovery, including lost replies.
        *phase = if result.is_ok() {
            None
        } else {
            Some(DiscoveryPhase::Ready(run))
        };
        result
    })
    .await
    .map_err(|_| {
        WireError::new(
            "remote_discovery_operation_timeout",
            "Discovery stop still owns its lifecycle future/token",
        )
    })?
}
async fn shutdown_discovery(shared: &Shared) -> Result<(), WireError> {
    let Some(discovery) = &shared.discovery else {
        return Ok(());
    };
    tokio::time::timeout(discovery.timeout(), async {
        let mut drain = discovery.drain.lock().await;
        if discovery.joined.load(Ordering::Acquire) {
            return Ok(());
        }
        if drain.is_none() {
            let service = discovery.service.clone();
            #[cfg(test)]
            let hook = discovery.drain_hook.lock().unwrap().take();
            *drain = Some(Box::pin(async move {
                #[cfg(test)]
                if let Some((entered, release)) = hook {
                    let _ = entered.send(());
                    let _ = release.await;
                }
                service.shutdown().await
            }));
        }
        let result = drain.as_mut().unwrap().await;
        drain.take();
        if result.is_ok() {
            discovery.joined.store(true, Ordering::Release);
        } else if let Err(error) = &result {
            record_failure(shared, WireError::new(error.code, error.message.clone()));
        }
        result
    })
    .await
    .map_err(|_| {
        WireError::new(
            "remote_discovery_shutdown_timeout",
            "Discovery descendants are still owned; shutdown must be retried",
        )
    })?
}
async fn owner(
    shared: Arc<Shared>,
    mut jobs: mpsc::Receiver<Job>,
    mut changes: mpsc::Receiver<ContextJob>,
    pump: mpsc::Sender<PumpControl>,
    complete: watch::Sender<bool>,
    mut discovery_events: Option<NativeDiscoveryEvents>,
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
                    result=dispatch(&shared,&pump,job.request,job.cover)=>result,
                };
                if let Err(Err(error))=job.reply.send(result){eprintln!("Detached remote request failed: {}",error.message);}
            },
            event=next_discovery_event(&mut discovery_events)=>match event {
                Some(event)=>observe_discovery(&shared,event),
                None=>discovery_events=None,
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
    // Terminal discovery cancellation also owns accepted token-less begins.
    // Its bounded wait may expire; public shutdown retains and retries the drain.
    if let Err(error) = shutdown_discovery(&shared).await {
        if let Some(discovery) = &shared.discovery {
            discovery.warning("shutdown", &error);
        }
    }
    if let Err(error) = retire(&shared, &pump, true).await {
        record_failure(&shared, error);
    }
    // Listener/runtime and renderer cleanup proceed even if discovery failed.
    if let Err(error) = shared.service.shutdown().await.map_err(error) {
        record_failure(&shared, error);
    }
    if let Some(events) = discovery_events.as_mut() {
        while let Ok(event) = events.try_recv() {
            observe_discovery(&shared, event);
        }
    }
    let _ = pump.send(PumpControl::Stop).await;
    complete.send_replace(true);
}
async fn dispatch(
    shared: &Shared,
    pump: &mpsc::Sender<PumpControl>,
    request: Request,
    cover: Option<CoverStart>,
) -> Result<Vec<Event>, WireError> {
    validate_context(shared, &request)?;
    if request.channel == "MAIN" {
        match request.data["channel"].as_str().unwrap_or("") {
            "START" => {
                retire(shared, pump, true).await?;
                validate_context(shared, &request)?;
                start(
                    shared,
                    pump,
                    request.context.unwrap(),
                    request.data["data"].clone(),
                    cover,
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
async fn start(
    shared: &Shared,
    pump: &mpsc::Sender<PumpControl>,
    context: RuntimeContext,
    options: Value,
    cover: Option<CoverStart>,
) -> Result<(), WireError> {
    // Early connections wait until their complete frozen binding is installed.
    let _publication = shared.publication.lock().await;
    let source = match cover {
        Some(cover) => {
            // dispatch already awaited actual old transport/body/file drain.
            (cover.ready)()?;
            Some(cover.source)
        }
        None => None,
    };
    let bound = shared
        .service
        .start_with_cover(&options, context.library_generation, source)
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
        context: context.clone(),
        bound: bound.clone(),
        retiring: false,
        connections: BTreeMap::new(),
        discovery: None,
    });
    if !bound.endpoints.is_empty() {
        if let Err(error) = start_discovery(shared, &context, &bound).await {
            if let Some(discovery) = &shared.discovery {
                discovery.warning("start", &error);
            }
        }
    }
    drop(_publication);
    if shared.library.context().map_err(WireError::from)? != context {
        // No new connection can pass the pump's own frozen-context check.
        retire(shared, pump, true).await?;
        return Err(WireError::new(
            "stale_library",
            "Library changed while discovery/listeners started",
        ));
    }
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
    let retirement = if changed {
        retire(shared, pump, true).await
    } else {
        Ok(())
    };
    // A timed-out begin/stop can still own discovery work after the listener
    // binding is gone. Its native reserved/coalesced control retains cleanup
    // across timeout/caller Drop and cannot retire a fresh current binding.
    if let Some(discovery) = &shared.discovery {
        let result = tokio::time::timeout(discovery.timeout(), discovery.service.context_changed())
            .await
            .unwrap_or_else(|_| {
                Err(WireError::new(
                    "remote_discovery_operation_timeout",
                    "Obsolete discovery retirement remains natively owned",
                ))
            });
        if let Err(error) = result {
            discovery.warning("context", &error);
        }
    }
    retirement
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
    // Never short-circuit listener and retained renderer cleanup on a discovery
    // transport/operation error. Its exact token/future remains natively owned.
    if let Err(error) = stop_discovery(shared).await {
        if let Some(discovery) = &shared.discovery {
            discovery.warning("stop", &error);
        }
    }
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
