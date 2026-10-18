//! Native-only discovery ownership. This module deliberately has no IPC handler.
use crate::network_snapshot::{NetworkSnapshots, SnapshotControl};
use freeshow_core::{protocol::RuntimeContext, service::LibraryService, WireError};
use freeshow_discovery::{
    DiscoveryConfig, DiscoveryEvent, DiscoveryEvents, DiscoveryGeneration, DiscoveryName,
    DiscoveryPlatform, DiscoveryService, NetworkSnapshot,
};
use serde_json::{json, Value};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
const CAPACITY: usize = 16;
const EVENT_CAPACITY: usize = 64;
type Failure = (&'static str, String);
type Result<T> = std::result::Result<T, WireError>;

/// Captured once during native setup. No Deserialize or renderer constructor.
#[derive(Clone)]
pub struct FrozenDiscoveryInputs {
    snapshot: NetworkSnapshot,
    suffix: [u8; 3],
}
impl FrozenDiscoveryInputs {
    /// Capture startup inputs on the shared fixed OS owner, once per process.
    /// Snapshot work survives a dropped/expired waiter until the syscall ends.
    pub async fn capture(snapshots: &NetworkSnapshots) -> Result<Self> {
        let snapshot = snapshots
            .snapshot(SnapshotControl::default())
            .await?
            .into_value();
        freeshow_discovery::get_local_ips(&snapshot).map_err(service_error)?;
        let random = uuid::Uuid::new_v4();
        let suffix = random.as_bytes()[..3].try_into().unwrap();
        Ok(Self { snapshot, suffix })
    }
    pub fn snapshot(&self) -> &NetworkSnapshot {
        &self.snapshot
    }
}
/// Native listener owner provenance, never deserialize from IPC. Retain the
/// returned token beside that owner's actual BoundServers/RuntimeContext.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryRun {
    context: RuntimeContext,
    generation: DiscoveryGeneration,
    listener_run: String,
}
impl DiscoveryRun {
    pub fn context(&self) -> &RuntimeContext {
        &self.context
    }
    pub fn generation(&self) -> &DiscoveryGeneration {
        &self.generation
    }
    pub fn listener_run(&self) -> &str {
        &self.listener_run
    }
}
#[derive(Debug)]
pub struct NativeDiscoveryEvent {
    pub context: RuntimeContext,
    pub listener_run: String,
    pub event: DiscoveryEvent,
}
pub type NativeDiscoveryEvents = mpsc::Receiver<NativeDiscoveryEvent>;
struct Binding {
    run: DiscoveryRun,
    retiring: bool,
    ports: std::collections::BTreeMap<DiscoveryName, SocketAddr>,
    endpoint: Option<std::net::SocketAddrV4>,
}
struct Shared {
    library: LibraryService,
    service: DiscoveryService,
    inputs: FrozenDiscoveryInputs,
    binding: Mutex<Option<Binding>>,
    closing: AtomicBool,
    terminal: CancellationToken,
    pump_stop: CancellationToken,
    failure: Mutex<Option<Failure>>,
    dropped_events: AtomicU64,
    stale_events: AtomicU64,
    #[cfg(test)]
    refresh_hook: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
}
enum Operation {
    Begin(RuntimeContext, String),
    Started(DiscoveryRun, DiscoveryName, SocketAddr),
    Stop(DiscoveryRun),
    #[cfg(test)]
    #[allow(dead_code, reason = "Integration-only controlled owner pause")]
    Pause(oneshot::Sender<()>, oneshot::Receiver<()>),
    #[cfg(test)]
    #[allow(dead_code, reason = "Integration-only controlled owner failure")]
    Panic,
}
enum Reply {
    Run(DiscoveryRun),
    Unit,
}
struct Job {
    operation: Operation,
    reply: oneshot::Sender<Result<Reply>>,
    _permit: OwnedSemaphorePermit,
}
struct PendingRefresh {
    receiver: watch::Receiver<Option<std::result::Result<(), Failure>>>,
    revision: u64,
}
struct Refresh {
    state: Arc<Mutex<Option<PendingRefresh>>>,
    reply: watch::Sender<Option<std::result::Result<(), Failure>>>,
    _permit: OwnedSemaphorePermit,
}
struct Inner {
    shared: Arc<Shared>,
    jobs: mpsc::Sender<Job>,
    refresh: mpsc::Sender<Refresh>,
    slots: Arc<Semaphore>,
    control_slot: Arc<Semaphore>,
    control_waiters: Arc<Semaphore>,
    pending_refresh: Arc<Mutex<Option<PendingRefresh>>>,
    done: watch::Receiver<Option<std::result::Result<(), Failure>>>,
    supervisor: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        self.shared.terminal.cancel();
    }
}
#[derive(Clone)]
pub struct NativeDiscovery {
    inner: Arc<Inner>,
}
fn bounded(mut text: String) -> String {
    if text.len() > 2048 {
        let mut at = 2030;
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        text.truncate(at);
        text.push_str(" [truncated]");
    }
    text
}
fn error(code: &'static str, message: impl Into<String>) -> WireError {
    WireError::new(code, bounded(message.into()))
}
fn service_error(e: freeshow_discovery::DiscoveryError) -> WireError {
    error(e.code, e.message)
}
fn closed() -> WireError {
    error("discovery_closed", "Native discovery admission is closed")
}
fn stale() -> WireError {
    error(
        "stale_generation",
        "Discovery request belongs to a retired listener run",
    )
}
fn current(shared: &Shared, context: &RuntimeContext) -> Result<()> {
    if context.runtime_id.len() > 256 || context.data_path.as_os_str().len() > 32768 {
        return Err(error(
            "discovery_context_limit",
            "Native discovery context exceeds limits",
        ));
    }
    if shared.library.context().map_err(WireError::from)? != *context {
        return Err(error(
            "stale_library",
            "Discovery context is no longer current",
        ));
    }
    Ok(())
}
fn retained(shared: &Shared) -> Option<WireError> {
    shared
        .failure
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|(code, message)| error(code, message.clone()))
}
fn retain(shared: &Shared, e: WireError) {
    shared.closing.store(true, Ordering::Release);
    let mut failure = shared.failure.lock().unwrap_or_else(|e| e.into_inner());
    if failure.is_none() {
        *failure = Some((e.code, bounded(e.message)));
    }
    shared.terminal.cancel();
}
impl NativeDiscovery {
    /// Config is trusted setup transport/executable authority. Its instance ID is
    /// replaced with the captured process suffix, never selected by renderer JSON.
    /// On macOS use production_config() to select the fixed system executable.
    pub fn new(
        library: LibraryService,
        config: DiscoveryConfig,
        inputs: FrozenDiscoveryInputs,
    ) -> Result<(Self, NativeDiscoveryEvents)> {
        Self::construct(library, config, inputs)
    }
    pub fn production_config() -> DiscoveryConfig {
        let mut config = DiscoveryConfig::new([0; 3]);
        if cfg!(target_os = "macos") {
            config.platform = DiscoveryPlatform::Darwin {
                dns_sd: Some("/usr/bin/dns-sd".into()),
            };
        }
        config
    }
    fn construct(
        library: LibraryService,
        mut config: DiscoveryConfig,
        inputs: FrozenDiscoveryInputs,
    ) -> Result<(Self, NativeDiscoveryEvents)> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(error(
                "discovery_runtime",
                "Native discovery requires an entered Tokio runtime",
            ));
        }
        freeshow_discovery::get_local_ips(inputs.snapshot()).map_err(service_error)?;
        config.instance_id = inputs.suffix;
        let (service, service_events) = DiscoveryService::new(config).map_err(service_error)?;
        let shared = Arc::new(Shared {
            library,
            service,
            inputs,
            binding: Mutex::new(None),
            closing: AtomicBool::new(false),
            terminal: CancellationToken::new(),
            pump_stop: CancellationToken::new(),
            failure: Mutex::new(None),
            dropped_events: AtomicU64::new(0),
            stale_events: AtomicU64::new(0),
            #[cfg(test)]
            refresh_hook: Mutex::new(None),
        });
        let (jobs, receive) = mpsc::channel(CAPACITY);
        let (refresh, changes) = mpsc::channel(1);
        let (events, event_rx) = mpsc::channel(EVENT_CAPACITY);
        let (finished, done) = watch::channel(None);
        let pump = tokio::spawn(pump(shared.clone(), service_events, events));
        let owner = tokio::spawn(owner(shared.clone(), receive, changes));
        let cleanup = shared.clone();
        let supervisor = tokio::spawn(async move {
            match owner.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => retain(&cleanup, e),
                Err(e) => retain(&cleanup, error("discovery_owner", e.to_string())),
            }
            cleanup.closing.store(true, Ordering::Release);
            if let Err(e) = cleanup.service.shutdown().await {
                retain(&cleanup, service_error(e));
            }
            cleanup.pump_stop.cancel();
            if let Err(e) = pump.await {
                retain(&cleanup, error("discovery_pump", e.to_string()));
            }
            cleanup
                .binding
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            finished.send_replace(Some(
                retained(&cleanup).map_or(Ok(()), |e| Err((e.code, e.message))),
            ));
        });
        Ok((
            Self {
                inner: Arc::new(Inner {
                    shared,
                    jobs,
                    refresh,
                    slots: Arc::new(Semaphore::new(CAPACITY)),
                    control_slot: Arc::new(Semaphore::new(1)),
                    control_waiters: Arc::new(Semaphore::new(4)),
                    pending_refresh: Arc::new(Mutex::new(None)),
                    done,
                    supervisor: Mutex::new(Some(supervisor)),
                }),
            },
            event_rx,
        ))
    }
    fn open(&self) -> Result<()> {
        if let Some(e) = retained(&self.inner.shared) {
            return Err(e);
        }
        if self.inner.shared.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        Ok(())
    }
    async fn request(&self, operation: Operation) -> Result<Reply> {
        self.open()?;
        let permit = self.inner.slots.clone().try_acquire_owned().map_err(|_| {
            error(
                "discovery_busy",
                "Native discovery requests are at capacity",
            )
        })?;
        let (reply, response) = oneshot::channel();
        self.inner
            .jobs
            .try_send(Job {
                operation,
                reply,
                _permit: permit,
            })
            .map_err(|_| closed())?;
        response
            .await
            .map_err(|_| retained(&self.inner.shared).unwrap_or_else(closed))?
    }
    /// Call only after the trusted listener owner has successfully bound at least
    /// one endpoint. This begins transport ownership, not successful publication.
    pub async fn begin(
        &self,
        context: &RuntimeContext,
        listener_run: &str,
    ) -> Result<DiscoveryRun> {
        if listener_run.len() != 32 || !listener_run.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(error(
                "invalid_listener_run",
                "Listener run must be a native 32-hex nonce",
            ));
        }
        current(&self.inner.shared, context)?;
        match self
            .request(Operation::Begin(context.clone(), listener_run.into()))
            .await?
        {
            Reply::Run(run) => Ok(run),
            _ => unreachable!(),
        }
    }
    /// Native-only actual bound endpoint evidence, never requested config ports.
    /// This does not prove multicast delivery or native dns-sd registration.
    pub async fn started_port(
        &self,
        run: &DiscoveryRun,
        name: DiscoveryName,
        bound_endpoint: SocketAddr,
    ) -> Result<()> {
        if bound_endpoint.port() == 0 {
            return Err(error(
                "invalid_discovery_port",
                "Actual listener port must be nonzero",
            ));
        }
        current(&self.inner.shared, &run.context)?;
        self.request(Operation::Started(run.clone(), name, bound_endpoint))
            .await
            .map(|_| ())
    }
    /// Exact retiring token may stop its old context after library selection.
    pub async fn stop(&self, run: &DiscoveryRun) -> Result<()> {
        self.request(Operation::Stop(run.clone())).await.map(|_| ())
    }
    /// One reserved owned retirement, with at most four coalesced native waiters.
    /// A dropped waiter never cancels the accepted cleanup. Stops only obsolete
    /// bindings; a delayed hook cannot retire a freshly started current context.
    pub async fn context_changed(&self) -> Result<()> {
        self.open()?;
        let _waiter = self
            .inner
            .control_waiters
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                error(
                    "discovery_busy",
                    "Discovery retirement waiters are at capacity",
                )
            })?;
        let mut response = {
            let mut pending = self
                .inner
                .pending_refresh
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(work) = pending
                .as_mut()
                .filter(|p| p.receiver.borrow().is_none() && p.receiver.has_changed().is_ok())
            {
                work.revision = work.revision.checked_add(1).ok_or_else(|| {
                    error(
                        "discovery_refresh_limit",
                        "Discovery retirement revision overflow",
                    )
                })?;
                work.receiver.clone()
            } else {
                let permit = self
                    .inner
                    .control_slot
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| {
                        error(
                            "discovery_busy",
                            "Discovery retirement completion is still owned",
                        )
                    })?;
                let (reply, response) = watch::channel(None);
                self.inner
                    .refresh
                    .try_send(Refresh {
                        state: self.inner.pending_refresh.clone(),
                        reply,
                        _permit: permit,
                    })
                    .map_err(|_| closed())?;
                *pending = Some(PendingRefresh {
                    receiver: response.clone(),
                    revision: 0,
                });
                response
            }
        };
        loop {
            if let Some(result) = response.borrow_and_update().clone() {
                return result.map_err(|(code, message)| error(code, message));
            }
            response
                .changed()
                .await
                .map_err(|_| retained(&self.inner.shared).unwrap_or_else(closed))?;
        }
    }
    pub fn status(&self) -> Value {
        let shared = &self.inner.shared;
        let binding = shared.binding.lock().unwrap_or_else(|e| e.into_inner());
        json!({"closing":shared.closing.load(Ordering::Acquire), "admittedRequests":CAPACITY-self.inner.slots.available_permits(), "admittedControls":1-self.inner.control_slot.available_permits(),"waitingControls":4-self.inner.control_waiters.available_permits(), "binding":binding.as_ref().map(|b|json!({"context":b.run.context,"generation":b.run.generation,"listenerRun":b.run.listener_run,"retiring":b.retiring,"actualPorts":b.ports,"discoveryEndpoint":b.endpoint})), "service":shared.service.stats(), "droppedEvents":shared.dropped_events.load(Ordering::Acquire), "staleEvents":shared.stale_events.load(Ordering::Acquire), "failure":shared.failure.lock().unwrap_or_else(|e|e.into_inner()).as_ref().map(|(code,message)|json!({"code":code,"message":message}))})
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.shared.closing.store(true, Ordering::Release);
        self.inner.shared.terminal.cancel();
        let mut done = self.inner.done.clone();
        let result = loop {
            if let Some(result) = done.borrow_and_update().clone() {
                break result;
            }
            if done.changed().await.is_err() {
                retain(
                    &self.inner.shared,
                    error(
                        "discovery_owner",
                        "Discovery supervisor completion channel closed",
                    ),
                );
                // A cancelled enclosing runtime/supervisor cannot report success.
                // Still reclaim the independent service runtime and request pump
                // cancellation; lost host task joins remain a retained failure.
                if let Err(e) = self.inner.shared.service.shutdown().await {
                    retain(&self.inner.shared, service_error(e));
                }
                self.inner.shared.pump_stop.cancel();
                self.inner
                    .shared
                    .binding
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                let e = retained(&self.inner.shared).unwrap();
                break Err((e.code, e.message));
            }
        };
        // The supervisor has already joined owner, pump, runtime/thread/children.
        // Keep its handle owned across caller cancellation until actually finished.
        loop {
            let finished = self
                .inner
                .supervisor
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_none_or(|task| task.is_finished());
            if finished {
                break;
            }
            tokio::task::yield_now().await;
        }
        let task = self
            .inner
            .supervisor
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(task) = task {
            if let Err(e) = task.await {
                retain(&self.inner.shared, error("discovery_owner", e.to_string()));
            }
        }
        if let Some(e) = retained(&self.inner.shared) {
            return Err(e);
        }
        result.map_err(|(code, message)| error(code, message))
    }
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "Exercised by the dedicated discovery/remotes integration targets; the binary unit harness does not invoke every fixture hook"
    )]
    pub(crate) fn test_inputs(snapshot: NetworkSnapshot, suffix: [u8; 3]) -> FrozenDiscoveryInputs {
        FrozenDiscoveryInputs { snapshot, suffix }
    }
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "Exercised by the dedicated discovery/remotes integration targets; the binary unit harness does not invoke every fixture hook"
    )]
    pub(crate) fn abort_supervisor(&self) {
        self.inner
            .supervisor
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .abort();
    }
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "Exercised by the dedicated discovery/remotes integration targets; the binary unit harness does not invoke every fixture hook"
    )]
    pub(crate) fn pause_refresh_completion(
        &self,
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    ) {
        *self.inner.shared.refresh_hook.lock().unwrap() = Some((entered, release));
    }
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "Exercised by the dedicated discovery/remotes integration targets; the binary unit harness does not invoke every fixture hook"
    )]
    pub(crate) async fn panic_owner(&self) -> Result<()> {
        self.request(Operation::Panic).await.map(|_| ())
    }
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "Exercised by the dedicated discovery/remotes integration targets; the binary unit harness does not invoke every fixture hook"
    )]
    pub(crate) async fn pause(
        &self,
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    ) -> Result<()> {
        self.request(Operation::Pause(entered, release))
            .await
            .map(|_| ())
    }
}
async fn retire(shared: &Shared) -> Result<()> {
    let run = {
        let mut binding = shared.binding.lock().unwrap_or_else(|e| e.into_inner());
        binding.as_mut().map(|b| {
            b.retiring = true;
            b.run.clone()
        })
    };
    if let Some(run) = run {
        if let Err(e) = shared.service.stop(&run.generation).await {
            let e = service_error(e);
            retain(shared, error(e.code, e.message.clone()));
            return Err(e);
        }
        shared
            .binding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
    Ok(())
}
async fn retire_obsolete(shared: &Shared) -> Result<()> {
    let context = shared.library.context().map_err(WireError::from)?;
    let obsolete = shared
        .binding
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|b| b.run.context != context);
    if obsolete {
        retire(shared).await?;
    }
    Ok(())
}
async fn execute(shared: &Shared, operation: Operation) -> Result<Reply> {
    match operation {
        Operation::Begin(context, listener_run) => {
            current(shared, &context)?;
            retire(shared).await?;
            current(shared, &context)?;
            let run = DiscoveryRun {
                generation: DiscoveryGeneration {
                    context: context.library_generation,
                    run: uuid::Uuid::new_v4().simple().to_string(),
                },
                context,
                listener_run,
            };
            let receipt = shared
                .service
                .begin(run.generation.clone(), shared.inputs.snapshot().clone())
                .await
                .map_err(service_error)?;
            *shared.binding.lock().unwrap_or_else(|e| e.into_inner()) = Some(Binding {
                run: run.clone(),
                retiring: false,
                ports: Default::default(),
                endpoint: receipt.endpoint,
            });
            if let Err(e) = current(shared, &run.context) {
                retire(shared).await?;
                return Err(e);
            }
            Ok(Reply::Run(run))
        }
        Operation::Started(run, name, endpoint) => {
            current(shared, &run.context)?;
            let valid = shared
                .binding
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|b| b.run == run && !b.retiring);
            if !valid {
                return Err(stale());
            }
            shared
                .service
                .publish(&run.generation, name, endpoint.port())
                .await
                .map_err(service_error)?;
            if let Some(binding) = shared
                .binding
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_mut()
            {
                binding.ports.entry(name).or_insert(endpoint);
            }
            if let Err(e) = current(shared, &run.context) {
                retire(shared).await?;
                return Err(e);
            }
            Ok(Reply::Unit)
        }
        Operation::Stop(run) => {
            let valid = shared
                .binding
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|b| b.run == run);
            if !valid {
                return Err(stale());
            }
            retire(shared).await?;
            Ok(Reply::Unit)
        }
        #[cfg(test)]
        Operation::Pause(entered, release) => {
            let _ = entered.send(());
            let _ = release.await;
            Ok(Reply::Unit)
        }
        #[cfg(test)]
        Operation::Panic => panic!("controlled discovery owner panic"),
    }
}
async fn complete_refresh(shared: &Shared, control: Refresh) {
    let Refresh {
        state,
        reply,
        _permit: permit,
    } = control;
    // Four live waiters bound ordinary coalescing. Even a sustained native flood
    // of dropped callers cannot keep this owned work in an unlimited retry loop.
    for round in 0..16 {
        let revision = state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .unwrap()
            .revision;
        let mut result = tokio::select! {biased; _=shared.terminal.cancelled()=>Err(closed()),result=retire_obsolete(shared)=>result};
        #[cfg(test)]
        {
            let hook = shared.refresh_hook.lock().unwrap().take();
            if let Some((entered, release)) = hook {
                let _ = entered.send(());
                tokio::select! {biased; _=shared.terminal.cancelled()=>result=Err(closed()),_=release=>{}}
            }
        }
        // Completion publication and the revision check share the admission
        // mutex. A later context change must get either another cleanup pass or
        // a new control job, never join a cleanup already declared complete.
        let pending = state.lock().unwrap_or_else(|e| e.into_inner());
        if result.is_ok() && pending.as_ref().unwrap().revision != revision {
            if round < 15 {
                drop(pending);
                continue;
            }
            result = Err(error(
                "discovery_refresh_limit",
                "Context churn exceeded sixteen owned retirement passes",
            ));
        }
        if let Err(e) = &result {
            if e.code != "discovery_closed" {
                retain(shared, error(e.code, e.message.clone()));
            }
        }
        drop(permit);
        reply.send_replace(Some(result.map_err(|e| (e.code, e.message))));
        return;
    }
}
async fn owner(
    shared: Arc<Shared>,
    mut jobs: mpsc::Receiver<Job>,
    mut controls: mpsc::Receiver<Refresh>,
) -> Result<()> {
    loop {
        tokio::select! { biased;
            _=shared.terminal.cancelled()=>break,
            Some(control)=controls.recv()=>complete_refresh(&shared,control).await,
            Some(job)=jobs.recv()=>{
                let result=tokio::select! { biased; _=shared.terminal.cancelled()=>Err(closed()), result=execute(&shared,job.operation)=>result };
                // Only lifecycle teardown failure is terminal. Ordinary invalid
                // input/publication failures remain visible and retryable.
                let _=job.reply.send(result);
            },
        }
    }
    jobs.close();
    controls.close();
    while let Ok(job) = jobs.try_recv() {
        let _ = job.reply.send(Err(closed()));
    }
    while let Ok(control) = controls.try_recv() {
        drop(control._permit);
        control.reply.send_replace(Some(Err((
            "discovery_closed",
            "Native discovery admission is closed".into(),
        ))));
    }
    Ok(())
}
async fn pump(
    shared: Arc<Shared>,
    mut source: DiscoveryEvents,
    events: mpsc::Sender<NativeDiscoveryEvent>,
) {
    loop {
        let event = tokio::select! { biased; _=shared.pump_stop.cancelled()=>break, event=source.recv()=>match event {Some(event)=>event,None=>break} };
        let binding = shared.binding.lock().unwrap_or_else(|e| e.into_inner());
        let Some(binding) = binding.as_ref().filter(|b| {
            !b.retiring
                && b.run.generation == event.generation
                && current(&shared, &b.run.context).is_ok()
        }) else {
            shared.stale_events.fetch_add(1, Ordering::AcqRel);
            continue;
        };
        if events
            .try_send(NativeDiscoveryEvent {
                context: binding.run.context.clone(),
                listener_run: binding.run.listener_run.clone(),
                event,
            })
            .is_err()
        {
            shared.dropped_events.fetch_add(1, Ordering::AcqRel);
        }
    }
}
