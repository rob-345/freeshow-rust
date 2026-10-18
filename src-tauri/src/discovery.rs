//! Native-only discovery ownership. This module deliberately has no IPC handler.
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
    /// Synchronous native OS calls, like source module initialization. Invoke
    /// during setup, outside hot request paths; syscalls are not preemptible.
    pub fn capture() -> Result<Self> {
        let snapshot = native_snapshot()?;
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
    Pause(oneshot::Sender<()>, oneshot::Receiver<()>),
    #[cfg(test)]
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
        freeshow_discovery::get_local_ips(&inputs.snapshot).map_err(service_error)?;
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
    pub(crate) fn test_inputs(snapshot: NetworkSnapshot, suffix: [u8; 3]) -> FrozenDiscoveryInputs {
        FrozenDiscoveryInputs { snapshot, suffix }
    }
    #[cfg(test)]
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
    pub(crate) fn pause_refresh_completion(
        &self,
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    ) {
        *self.inner.shared.refresh_hook.lock().unwrap() = Some((entered, release));
    }
    #[cfg(test)]
    pub(crate) async fn panic_owner(&self) -> Result<()> {
        self.request(Operation::Panic).await.map(|_| ())
    }
    #[cfg(test)]
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
                .begin(run.generation.clone(), shared.inputs.snapshot.clone())
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

fn snapshot_limit(message: &str) -> WireError {
    error("discovery_snapshot_limit", message)
}
fn native_error(message: impl Into<String>) -> WireError {
    error("discovery_snapshot_os", message)
}
fn mac_text(bytes: [u8; 6]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}
fn add_address(
    interfaces: &mut Vec<freeshow_discovery::NetworkInterface>,
    name: String,
    address: freeshow_discovery::InterfaceAddress,
) -> Result<()> {
    if name.len() > 128 {
        return Err(snapshot_limit("Native interface name exceeds 128 bytes"));
    }
    let index = if let Some(index) = interfaces.iter().position(|i| i.name == name) {
        index
    } else {
        if interfaces.len() == 32 {
            return Err(snapshot_limit("More than 32 native interfaces"));
        }
        interfaces.push(freeshow_discovery::NetworkInterface {
            name,
            addresses: vec![],
        });
        interfaces.len() - 1
    };
    if interfaces[index].addresses.len() == 16 {
        return Err(snapshot_limit(
            "More than 16 addresses per native interface",
        ));
    }
    interfaces[index].addresses.push(address);
    Ok(())
}
fn key_order(snapshot: &mut NetworkSnapshot) {
    snapshot.interfaces.sort_by_key(|i| {
        let n = i
            .name
            .parse::<u32>()
            .ok()
            .filter(|n| *n != u32::MAX && n.to_string() == i.name);
        (n.is_none(), n.unwrap_or(0))
    });
}
/// Native ordered candidates used by source os.networkInterfaces(), restricted to
/// bounded IPv4/IPv6 data required by Bonjour. No subprocess or LAN operations.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn native_snapshot() -> Result<NetworkSnapshot> {
    use freeshow_discovery::InterfaceAddress;
    use std::{
        ffi::CStr,
        net::{Ipv4Addr, Ipv6Addr},
    };
    let mut head = std::ptr::null_mut();
    // SAFETY: OS writes an owned list, freed exactly once by List below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Err(native_error(std::io::Error::last_os_error().to_string()));
    }
    struct List(*mut libc::ifaddrs);
    impl Drop for List {
        fn drop(&mut self) {
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
    let _list = List(head);
    let mut entries = Vec::new();
    let mut cursor = head;
    while !cursor.is_null() {
        if entries.len() == 1024 {
            return Err(snapshot_limit("Native getifaddrs exceeded 1024 entries"));
        }
        entries.push(cursor);
        // SAFETY: owned getifaddrs list remains alive throughout traversal.
        cursor = unsafe { (*cursor).ifa_next };
    }
    let mut interfaces = Vec::new();
    for ptr in &entries {
        let entry = unsafe { &**ptr };
        let enabled = libc::IFF_UP as u32 | libc::IFF_RUNNING as u32;
        if entry.ifa_addr.is_null() || entry.ifa_flags & enabled != enabled {
            continue;
        }
        let family = unsafe { (*entry.ifa_addr).sa_family as i32 };
        let (address, family) = match family {
            libc::AF_INET => {
                let addr = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in>() };
                (
                    Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes()).to_string(),
                    "IPv4",
                )
            }
            libc::AF_INET6 => {
                let addr = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in6>() };
                (Ipv6Addr::from(addr.sin6_addr.s6_addr).to_string(), "IPv6")
            }
            #[cfg(target_os = "linux")]
            libc::AF_PACKET => continue,
            #[cfg(target_os = "macos")]
            libc::AF_LINK => continue,
            _ => {
                return Err(error(
                    "discovery_snapshot_unsupported",
                    "Native interface family is not IPv4/IPv6/link metadata",
                ))
            }
        };
        if entry.ifa_name.is_null() {
            return Err(native_error("Native interface name is null"));
        }
        let name_bytes = unsafe { CStr::from_ptr(entry.ifa_name) }.to_bytes();
        let name = String::from_utf8_lossy(name_bytes).into_owned();
        if name.len() > 128 {
            return Err(snapshot_limit("Native interface name exceeds bound"));
        }
        let mut mac = [0u8; 6];
        for physical in &entries {
            let physical = unsafe { &**physical };
            if physical.ifa_addr.is_null()
                || physical.ifa_name.is_null()
                || physical.ifa_flags & enabled != enabled
            {
                continue;
            }
            let physical_family = unsafe { (*physical.ifa_addr).sa_family as i32 };
            let physical_name = unsafe { CStr::from_ptr(physical.ifa_name) }.to_bytes();
            #[cfg(target_os = "linux")]
            if physical_family == libc::AF_PACKET
                && (name_bytes == physical_name
                    || name_bytes
                        .strip_prefix(physical_name)
                        .is_some_and(|tail| tail.starts_with(b":")))
            {
                let link = unsafe { &*physical.ifa_addr.cast::<libc::sockaddr_ll>() };
                mac.copy_from_slice(&link.sll_addr[..6]);
            }
            #[cfg(target_os = "macos")]
            if physical_family == libc::AF_LINK && name_bytes == physical_name {
                let link = unsafe { &*physical.ifa_addr.cast::<libc::sockaddr_dl>() };
                let start =
                    std::mem::offset_of!(libc::sockaddr_dl, sdl_data) + usize::from(link.sdl_nlen);
                if start + 6 <= usize::from(link.sdl_len) {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            physical.ifa_addr.cast::<u8>().add(start),
                            mac.as_mut_ptr(),
                            6,
                        )
                    };
                }
            }
        }
        add_address(
            &mut interfaces,
            name,
            InterfaceAddress {
                address,
                family: json!(family),
                internal: entry.ifa_flags & libc::IFF_LOOPBACK as u32 != 0,
                mac: mac_text(mac),
            },
        )?;
    }
    #[cfg(target_os = "linux")]
    let mut host = [0u8; 65];
    #[cfg(target_os = "macos")]
    let mut host = [0u8; 257];
    if unsafe { libc::gethostname(host.as_mut_ptr().cast(), host.len()) } != 0 {
        return Err(native_error(std::io::Error::last_os_error().to_string()));
    }
    *host.last_mut().unwrap() = 0;
    let end = host.iter().position(|b| *b == 0).unwrap();
    let mut snapshot = NetworkSnapshot {
        hostname: String::from_utf8_lossy(&host[..end]).into_owned(),
        interfaces,
    };
    key_order(&mut snapshot);
    Ok(snapshot)
}
#[cfg(windows)]
fn native_snapshot() -> Result<NetworkSnapshot> {
    use freeshow_discovery::InterfaceAddress;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use windows_sys::Win32::{
        Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA},
        NetworkManagement::{IpHelper::*, Ndis::IfOperStatusUp},
        Networking::WinSock::*,
    };
    let mut wsa = std::mem::MaybeUninit::<WSADATA>::uninit();
    let status = unsafe { WSAStartup(0x0202, wsa.as_mut_ptr()) };
    if status != 0 {
        return Err(native_error(format!("WSAStartup: {status}")));
    }
    struct Winsock;
    impl Drop for Winsock {
        fn drop(&mut self) {
            unsafe {
                WSACleanup();
            }
        }
    }
    let _winsock = Winsock;
    let mut host = [0u16; 256];
    if unsafe { GetHostNameW(host.as_mut_ptr(), host.len() as i32) } != 0 {
        return Err(native_error(format!("GetHostNameW: {}", unsafe {
            WSAGetLastError()
        })));
    }
    let end = host
        .iter()
        .position(|u| *u == 0)
        .ok_or_else(|| snapshot_limit("Hostname not terminated"))?;
    let hostname = String::from_utf16(&host[..end]).map_err(|_| {
        error(
            "discovery_snapshot_unsupported",
            "Hostname contains lone UTF-16 surrogate",
        )
    })?;
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 0u32;
    let mut buffer = Vec::<u64>::new(); // alignment for all native adapter structs
    let mut ready = false;
    for _ in 0..3 {
        let pointer = if buffer.is_empty() {
            std::ptr::null_mut()
        } else {
            buffer.as_mut_ptr().cast()
        };
        let code = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC as u32,
                flags,
                std::ptr::null(),
                pointer,
                &mut size,
            )
        };
        if code == ERROR_NO_DATA {
            return Ok(NetworkSnapshot {
                hostname,
                interfaces: vec![],
            });
        }
        if code == 0 {
            ready = true;
            break;
        }
        if code != ERROR_BUFFER_OVERFLOW {
            return Err(native_error(format!("GetAdaptersAddresses: {code}")));
        }
        if size as usize > 256 * 1024 {
            return Err(snapshot_limit("Native adapter buffer exceeds 256 KiB"));
        }
        buffer.resize((size as usize).div_ceil(8), 0);
    }
    if !ready || buffer.is_empty() {
        return Err(native_error(
            "Native adapter list changed across three bounded calls",
        ));
    }
    let mut interfaces = Vec::new();
    let mut adapter = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    let mut count = 0;
    while !adapter.is_null() {
        count += 1;
        if count > 1024 {
            return Err(snapshot_limit("More than 1024 native adapters"));
        }
        // SAFETY: the successful OS call owns linked records within this aligned
        // buffer for the whole traversal. No pointer escapes the function.
        let data = unsafe { &*adapter };
        if data.OperStatus == IfOperStatusUp && !data.FirstUnicastAddress.is_null() {
            let mut units = Vec::new();
            if data.FriendlyName.is_null() {
                return Err(native_error("Null native adapter name"));
            }
            for index in 0..=128 {
                let unit = unsafe { *data.FriendlyName.add(index) };
                if unit == 0 {
                    break;
                }
                if index == 128 {
                    return Err(snapshot_limit(
                        "Native adapter name exceeds 128 UTF-16 units",
                    ));
                }
                units.push(unit);
            }
            let name = String::from_utf16(&units).map_err(|_| {
                error(
                    "discovery_snapshot_unsupported",
                    "Adapter name contains lone UTF-16 surrogate",
                )
            })?;
            let mut mac = [0; 6];
            if data.PhysicalAddressLength == 6 {
                mac.copy_from_slice(&data.PhysicalAddress[..6]);
            }
            let mut address = data.FirstUnicastAddress;
            let mut address_count = 0;
            while !address.is_null() {
                address_count += 1;
                if address_count > 16 {
                    return Err(snapshot_limit("More than 16 native adapter addresses"));
                }
                let unicast = unsafe { &*address };
                let socket = unicast.Address.lpSockaddr;
                if socket.is_null() {
                    return Err(native_error("Null native adapter address"));
                }
                let (ip, family) = match unsafe { (*socket).sa_family } {
                    AF_INET => {
                        let ip = unsafe { &*socket.cast::<SOCKADDR_IN>() };
                        (
                            Ipv4Addr::from(unsafe { ip.sin_addr.S_un.S_addr }.to_ne_bytes())
                                .to_string(),
                            "IPv4",
                        )
                    }
                    AF_INET6 => {
                        let ip = unsafe { &*socket.cast::<SOCKADDR_IN6>() };
                        (
                            Ipv6Addr::from(unsafe { ip.sin6_addr.u.Byte }).to_string(),
                            "IPv6",
                        )
                    }
                    _ => {
                        return Err(error(
                            "discovery_snapshot_unsupported",
                            "Native adapter address family unsupported",
                        ))
                    }
                };
                add_address(
                    &mut interfaces,
                    name.clone(),
                    InterfaceAddress {
                        address: ip,
                        family: json!(family),
                        internal: data.IfType == 24, /* IF_TYPE_SOFTWARE_LOOPBACK */
                        mac: mac_text(mac),
                    },
                )?;
                address = unicast.Next;
            }
        }
        adapter = data.Next;
    }
    let mut snapshot = NetworkSnapshot {
        hostname,
        interfaces,
    };
    key_order(&mut snapshot);
    Ok(snapshot)
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn native_snapshot() -> Result<NetworkSnapshot> {
    Err(error(
        "discovery_snapshot_unsupported",
        "Native interface capture supports Linux, macOS and Windows",
    ))
}
