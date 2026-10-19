//! Native nowPlaying composition. The single supervisor owns accepted futures;
//! private receipts and renderer completions are never original IPC replies.
use crate::{
    assets::NativeAssets,
    audio_pipeline::NativeAudioPipeline,
    now_playing_files::{self as files, Intent, Leaf},
    now_playing_prepare::{FailureKind, PreparationControl, PreparationServices},
    now_playing_schedule::{OperationId, OperationState, PhaseKind, PhaseToken, Schedule},
};
use freeshow_assets::{Control, MediaAuthority};
use freeshow_audio::{AudioService, CommonValue};
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::{LibraryPhase, LibraryService},
};
use freeshow_cover_images::{
    Cancel, Context as CoverContext, CoverImageService, ImagePolicy, Reply, Session,
};
use freeshow_metadata_wire::{Hydration, PictureRef};
use freeshow_now_playing as planner;
use futures_util::{stream::FuturesUnordered, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
static PUBLICATION_SEQUENCE: AtomicU64 = AtomicU64::new(1);
pub type Result<T> = std::result::Result<T, HostError>;
pub type CallbackFuture = Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;
type OwnedFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
#[derive(Debug, Clone, Serialize)]
pub struct HostError {
    pub code: &'static str,
    pub message: String,
}
impl HostError {
    pub fn new(code: &'static str, message: impl ToString) -> Self {
        let message = message.to_string();
        let mut n = message.len().min(2048);
        while !message.is_char_boundary(n) {
            n -= 1;
        }
        Self {
            code,
            message: message[..n].into(),
        }
    }
}
impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for HostError {}
fn dependency(e: impl ToString) -> HostError {
    HostError::new("now_playing_dependency", e)
}
fn busy() -> HostError {
    HostError::new(
        "now_playing_busy",
        "Four whole native now-playing operations/results are retained",
    )
}
#[derive(Clone)]
pub struct Dependencies {
    pub library: LibraryService,
    pub assets: NativeAssets,
    pub audio: Arc<AudioService>,
    pub covers: Arc<CoverImageService>,
    pub pipeline: NativeAudioPipeline,
}
#[derive(Clone)]
pub struct Config {
    pub operation_timeout: Duration,
    pub shutdown_timeout: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            operation_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(30),
        }
    }
}
/// Trusted native callbacks. No renderer-selected callback or destination.
/// These futures must settle; a timeout keeps the same owner/future for retry.
#[derive(Clone)]
pub struct Callbacks {
    pub publish: Arc<dyn Fn(Publication) -> CallbackFuture + Send + Sync>,
    pub reveal: Arc<dyn Fn(PathBuf, RuntimeContext) -> CallbackFuture + Send + Sync>,
}
pub struct MetadataDelivery {
    event: Event,
    hydration: Hydration,
    id: String,
    sequence: String,
    context: RuntimeContext,
}
impl MetadataDelivery {
    pub fn event(&self) -> &Event {
        &self.event
    }
    pub fn hydration(&self) -> &Hydration {
        &self.hydration
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn sequence(&self) -> &str {
        &self.sequence
    }
    pub fn context(&self) -> &RuntimeContext {
        &self.context
    }
}
/// Native-only source reason. Delivery construction is private; no renderer
/// message/path can manufacture a folder-failure observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AlertKind {
    MissingText,
    FolderCreationFailed { folder: PathBuf },
}
impl AlertKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MissingText => "missingText",
            Self::FolderCreationFailed { .. } => "folderCreationFailed",
        }
    }
    fn message(&self) -> Result<String> {
        match self {
            Self::MissingText => Ok("This does not exist!".into()),
            Self::FolderCreationFailed { folder } => {
                let path = folder.to_str().ok_or_else(|| {
                    HostError::new("now_playing_unsupported", "NonUnicode folder-failure path")
                })?;
                if path.len() > 32774 || path.contains('\0') {
                    return Err(HostError::new(
                        "now_playing_limit",
                        "Invalid folder-failure path",
                    ));
                }
                Ok(format!("Error: Could not create folder at: {path}!"))
            }
        }
    }
}
impl Serialize for AlertKind {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
pub struct AlertDelivery {
    kind: AlertKind,
    event: Event,
    id: String,
    sequence: String,
    context: RuntimeContext,
}
impl AlertDelivery {
    pub fn kind(&self) -> &AlertKind {
        &self.kind
    }
    pub fn folder_path(&self) -> Option<&Path> {
        match &self.kind {
            AlertKind::FolderCreationFailed { folder } => Some(folder),
            AlertKind::MissingText => None,
        }
    }
    pub fn event(&self) -> &Event {
        &self.event
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn sequence(&self) -> &str {
        &self.sequence
    }
    pub fn context(&self) -> &RuntimeContext {
        &self.context
    }
}
pub enum Publication {
    Metadata(MetadataDelivery),
    Alert(AlertDelivery),
}
impl Publication {
    pub fn id(&self) -> &str {
        match self {
            Self::Metadata(d) => d.id(),
            Self::Alert(d) => d.id(),
        }
    }
    pub fn context(&self) -> &RuntimeContext {
        match self {
            Self::Metadata(d) => d.context(),
            Self::Alert(d) => d.context(),
        }
    }
    pub fn sequence(&self) -> &str {
        match self {
            Self::Metadata(d) => d.sequence(),
            Self::Alert(d) => d.sequence(),
        }
    }
    pub fn event(&self) -> &Event {
        match self {
            Self::Metadata(d) => d.event(),
            Self::Alert(d) => d.event(),
        }
    }
}
pub enum Command {
    Set {
        payload: Value,
        duration_number: Option<f64>,
    },
    Unset,
    Open,
}
fn check_payload(value: &Value) -> Result<()> {
    fn walk(v: &Value, depth: usize, nodes: &mut usize, raw: &mut usize) -> Result<()> {
        *nodes += 1;
        if depth > 8 || *nodes > 1024 {
            return Err(HostError::new(
                "now_playing_input_limit",
                "Payload depth/node cap exceeded",
            ));
        }
        match v {
            Value::Array(a) => {
                for v in a {
                    walk(v, depth + 1, nodes, raw)?
                }
            }
            Value::Object(o) => {
                for (key, v) in o {
                    *raw = raw.saturating_add(key.len());
                    walk(v, depth + 1, nodes, raw)?;
                }
            }
            Value::String(v) => *raw = raw.saturating_add(v.len()),
            _ => *raw = raw.saturating_add(16),
        }
        if *raw > 65536 {
            return Err(HostError::new(
                "now_playing_input_limit",
                "Planner raw input budget exceeded",
            ));
        }
        Ok(())
    }
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > 65536 {
                Err(std::io::Error::other("64KiB payload limit"))
            } else {
                Ok(bytes.len())
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    walk(value, 0, &mut 0, &mut 0)?;
    serde_json::to_writer(Count(0), value)
        .map_err(|_| HostError::new("now_playing_input_limit", "64KiB payload limit"))?;
    if !value.is_object() || !value.get("filePath").is_some_and(Value::is_string) {
        return Err(HostError::new(
            "now_playing_payload",
            "Expected source object with string filePath",
        ));
    }
    let path = value["filePath"].as_str().unwrap();
    if path.len() > 32768 || path.contains('\0') {
        return Err(HostError::new(
            "now_playing_path",
            "Source path exceeds the native supported path bound",
        ));
    }
    Ok(())
}
/// Sealed fixed four whole-operation/result leases. The trusted controller
/// retains this same budget across scoped host/context replacement.
#[derive(Clone)]
pub struct OperationBudget(Arc<Semaphore>);
impl OperationBudget {
    pub fn retained(&self) -> usize {
        4 - self.0.available_permits()
    }
}
impl Default for OperationBudget {
    fn default() -> Self {
        Self(Arc::new(Semaphore::new(4)))
    }
}
struct Lease {
    _permit: OwnedSemaphorePermit,
}
/// Whole result quota remains charged until this native observation is disposed.
pub struct Receipt {
    context: RuntimeContext,
    operation: u64,
    #[allow(
        dead_code,
        reason = "Private received-object mutation observation for source parity; never an IPC callback"
    )]
    mutated_input: Option<Value>,
    diagnostics: Vec<HostError>,
    failure: Option<HostError>,
    _lease: Arc<Lease>,
}
impl Receipt {
    pub fn context(&self) -> &RuntimeContext {
        &self.context
    }
    pub fn operation(&self) -> u64 {
        self.operation
    }
    #[allow(
        dead_code,
        reason = "Private received-object mutation observation for source parity; never an IPC callback"
    )]
    pub fn mutated_input(&self) -> Option<&Value> {
        self.mutated_input.as_ref()
    }
    pub fn diagnostics(&self) -> &[HostError] {
        &self.diagnostics
    }
    pub fn failure(&self) -> Option<&HostError> {
        self.failure.as_ref()
    }
}
pub struct Ticket {
    receive: oneshot::Receiver<Result<Receipt>>,
}
impl Ticket {
    pub async fn finish(self) -> Result<Receipt> {
        self.receive.await.map_err(|_| {
            HostError::new(
                "now_playing_worker",
                "Supervisor stopped before the actual operation receipt",
            )
        })?
    }
}
struct Pending {
    context: RuntimeContext,
    state: AtomicU8,
}
// state0 live,1 accepted,2 renderer rejected,3 authoritatively retired.
#[derive(Clone, Default)]
struct Progress {
    complete: bool,
    error: Option<HostError>,
}
struct Shared {
    context: RuntimeContext,
    dependencies: Dependencies,
    config: Config,
    closing: AtomicBool,
    stop: CancellationToken,
    changed: Notify,
    publications: Mutex<HashMap<String, Arc<Pending>>>,
    error: Mutex<Option<HostError>>,
    retained: AtomicU64,
    admitted: AtomicU64,
    completed: AtomicU64,
    reveal_calls: AtomicU64,
    joined: AtomicBool,
    phase_active: AtomicBool,
    preparing: AtomicU64,
    cleaning: AtomicU64,
    #[cfg(test)]
    hooks: TestHooks,
}
impl Shared {
    fn fail(&self, e: HostError) {
        let mut first = self.error.lock().unwrap_or_else(|e| e.into_inner());
        if first.is_none() {
            *first = Some(e);
        }
    }
    fn error(&self) -> Option<HostError> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn close(&self) {
        self.closing.store(true, Ordering::Release);
        self.stop.cancel();
        for pending in self
            .publications
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            let _ = pending
                .state
                .compare_exchange(0, 3, Ordering::AcqRel, Ordering::Acquire);
        }
        self.changed.notify_one();
    }
    fn current(&self, captured: &RuntimeContext) -> Result<()> {
        if self.closing.load(Ordering::Acquire) {
            return Err(self.error().unwrap_or_else(|| {
                HostError::new(
                    "now_playing_shutdown",
                    "Native now-playing admission is retired",
                )
            }));
        }
        if captured != &self.context
            || self.dependencies.library.context().map_err(dependency)? != *captured
        {
            return Err(HostError::new(
                "stale_library",
                "Native now-playing full context is obsolete",
            ));
        }
        Ok(())
    }
}
struct Handle {
    shared: Arc<Shared>,
    jobs: mpsc::Sender<Work>,
    slots: Arc<Semaphore>,
    observers: Arc<Semaphore>,
    progress: watch::Receiver<Progress>,
    worker: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.shared.close();
    }
}
#[derive(Clone)]
pub struct NativeNowPlaying {
    handle: Arc<Handle>,
}
struct Work {
    command: Command,
    cancel: CancellationToken,
    reply: oneshot::Sender<Result<Receipt>>,
    lease: Arc<Lease>,
}
impl NativeNowPlaying {
    #[allow(
        dead_code,
        reason = "Isolated native host harness constructor; desktop uses the shared process-lifetime budget"
    )]
    pub fn start(dependencies: Dependencies, config: Config, callbacks: Callbacks) -> Result<Self> {
        Self::start_with_budget(dependencies, config, callbacks, OperationBudget::default())
    }
    pub fn start_with_budget(
        dependencies: Dependencies,
        config: Config,
        callbacks: Callbacks,
        budget: OperationBudget,
    ) -> Result<Self> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(HostError::new(
                "now_playing_runtime",
                "Start requires an entered Tokio runtime",
            ));
        }
        if config.operation_timeout.is_zero()
            || config.operation_timeout > Duration::from_secs(300)
            || config.shutdown_timeout.is_zero()
            || config.shutdown_timeout > Duration::from_secs(300)
        {
            return Err(HostError::new(
                "now_playing_limit",
                "Invalid host time limits",
            ));
        }
        let context = dependencies.library.context().map_err(dependency)?;
        let schedule = Schedule::new(context.clone()).map_err(dependency)?;
        let shared = Arc::new(Shared {
            context,
            dependencies,
            config,
            closing: AtomicBool::new(false),
            stop: CancellationToken::new(),
            changed: Notify::new(),
            publications: Mutex::new(HashMap::new()),
            error: Mutex::new(None),
            retained: AtomicU64::new(0),
            admitted: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            reveal_calls: AtomicU64::new(0),
            joined: AtomicBool::new(false),
            phase_active: AtomicBool::new(false),
            preparing: AtomicU64::new(0),
            cleaning: AtomicU64::new(0),
            #[cfg(test)]
            hooks: TestHooks::default(),
        });
        let (jobs, receive) = mpsc::channel(4);
        let (progress, observed) = watch::channel(Progress::default());
        let worker = tokio::spawn(supervise(
            shared.clone(),
            callbacks,
            schedule,
            receive,
            progress,
        ));
        Ok(Self {
            handle: Arc::new(Handle {
                shared,
                jobs,
                slots: budget.0,
                observers: Arc::new(Semaphore::new(4)),
                progress: observed,
                worker: tokio::sync::Mutex::new(Some(worker)),
            }),
        })
    }
    pub fn submit(
        &self,
        captured: &RuntimeContext,
        command: Command,
        cancel: CancellationToken,
    ) -> Result<Ticket> {
        self.handle.shared.current(captured)?;
        if cancel.is_cancelled() {
            return Err(HostError::new(
                "cancelled",
                "Native request canceled before admission",
            ));
        }
        if let Command::Set { payload, .. } = &command {
            check_payload(payload)?;
        }
        let permit = self
            .handle
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy())?;
        self.handle.shared.current(captured)?;
        let (reply, receive) = oneshot::channel();
        self.handle
            .jobs
            .try_send(Work {
                command,
                cancel,
                reply,
                lease: Arc::new(Lease { _permit: permit }),
            })
            .map_err(|_| busy())?;
        if self
            .handle
            .shared
            .admitted
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .is_err()
        {
            self.handle.shared.fail(HostError::new(
                "now_playing_limit",
                "Native operation counter exhausted",
            ));
            self.handle.shared.close();
        }
        Ok(Ticket { receive })
    }
    /// Private one-use renderer completion; accepted=false is an observation,
    /// never a rollback or a source success ACK.
    pub fn complete(&self, id: &str, captured: &RuntimeContext, accepted: bool) -> Result<()> {
        self.handle.shared.current(captured)?;
        if id.len() != 51
            || !id.starts_with("native-now-playing-")
            || !id.as_bytes()[19..].iter().all(u8::is_ascii_hexdigit)
        {
            return Err(HostError::new(
                "now_playing_completion",
                "Invalid bounded private completion ID",
            ));
        }
        let pending = self
            .handle
            .shared
            .publications
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| {
                HostError::new("now_playing_completion", "Unknown private completion")
            })?;
        if &pending.context != captured
            || pending
                .state
                .compare_exchange(
                    0,
                    if accepted { 1 } else { 2 },
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
        {
            return Err(HostError::new(
                "now_playing_completion",
                "Foreign, retired or already consumed completion",
            ));
        }
        self.handle.shared.changed.notify_one();
        Ok(())
    }
    pub fn status(&self) -> Value {
        json!({"context":self.handle.shared.context,"closing":self.handle.shared.closing.load(Ordering::Acquire),"retained":4-self.handle.slots.available_permits(),"admitted":self.handle.shared.admitted.load(Ordering::Acquire).to_string(),"completed":self.handle.shared.completed.load(Ordering::Acquire).to_string(),"revealCalls":self.handle.shared.reveal_calls.load(Ordering::Acquire).to_string(),"activeOperations":self.handle.shared.retained.load(Ordering::Acquire),"publications":self.handle.shared.publications.lock().unwrap_or_else(|e|e.into_inner()).len(),"lifecycleObservers":4-self.handle.observers.available_permits(),"complete":self.handle.progress.borrow().complete,"supervisorJoined":self.handle.shared.joined.load(Ordering::Acquire),"drained":self.handle.progress.borrow().complete && self.handle.shared.joined.load(Ordering::Acquire),"currentPhase":self.handle.shared.phase_active.load(Ordering::Acquire),"preparations":self.handle.shared.preparing.load(Ordering::Acquire),"cleanups":self.handle.shared.cleaning.load(Ordering::Acquire),"error":self.handle.shared.error()})
    }
    /// Called before shared dependency retirement. This host is context-scoped;
    /// process-global audio state belongs to NativeAudioPipeline and is not reset.
    pub async fn context_changed(&self) -> Result<()> {
        self.shutdown().await
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.handle.shared.close();
        let _observer = self
            .handle
            .observers
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                HostError::new(
                    "now_playing_lifecycle_busy",
                    "Four lifecycle observers are retained",
                )
            })?;
        let deadline = tokio::time::Instant::now() + self.handle.shared.config.shutdown_timeout;
        tokio::time::timeout_at(deadline, self.wait_shutdown())
            .await
            .map_err(|_| {
                HostError::new(
                    "now_playing_drain_timeout",
                    "Same actual supervisor and accepted cleanup retained for retry",
                )
            })?
    }
    async fn wait_shutdown(&self) -> Result<()> {
        let mut progress = self.handle.progress.clone();
        loop {
            let state = progress.borrow().clone();
            if state.complete {
                let mut retained = self.handle.worker.lock().await;
                if let Some(worker) = retained.as_mut() {
                    let joined = worker.await;
                    retained.take();
                    self.handle.shared.joined.store(true, Ordering::Release);
                    if let Err(e) = joined {
                        self.handle
                            .shared
                            .fail(HostError::new("now_playing_worker", e));
                    }
                }
                return self
                    .handle
                    .shared
                    .error()
                    .or(state.error)
                    .map_or(Ok(()), Err);
            }
            if progress.changed().await.is_err() {
                let mut retained = self.handle.worker.lock().await;
                if let Some(worker) = retained.as_mut() {
                    let joined = worker.await;
                    retained.take();
                    self.handle.shared.joined.store(true, Ordering::Release);
                    if let Err(e) = joined {
                        self.handle
                            .shared
                            .fail(HostError::new("now_playing_worker", e));
                    }
                }
                let e = HostError::new(
                    "now_playing_worker",
                    "Supervisor exited without drain proof",
                );
                self.handle.shared.fail(e);
                return Err(self.handle.shared.error().unwrap());
            }
        }
    }
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "Controlled native integration-test boundary; binary unit target does not install it"
    )]
    pub fn test_hooks(&self) -> TestHooks {
        self.handle.shared.hooks.clone()
    }
}
struct Operation {
    id: OperationId,
    router: planner::Router,
    router_id: u64,
    next: Option<planner::Issued>,
    control: PreparationControl,
    deadline: Instant,
    cancel: CancellationToken,
    authority: Option<MediaAuthority>,
    session: Option<Session>,
    publications: Vec<(String, Arc<Pending>)>,
    reply: Option<oneshot::Sender<Result<Receipt>>>,
    lease: Arc<Lease>,
    diagnostics: Vec<HostError>,
    failure: Option<HostError>,
    result: Option<planner::Finished>,
    cleaned: bool,
}
impl Operation {
    fn diagnostic(&mut self, e: HostError) {
        if self.diagnostics.len() < 16 {
            self.diagnostics.push(e)
        }
    }
    fn fail(&mut self, e: HostError) {
        if self.failure.is_none() {
            self.failure = Some(e);
        }
    }
}
fn request(context: &RuntimeContext) -> Request {
    Request {
        channel: "MAIN".into(),
        data: json!({"channel":"NOW_PLAYING_NATIVE_EFFECT"}),
        context: Some(context.clone()),
        listener_id: None,
    }
}
fn build_planner(
    shared: &Shared,
    command: &Command,
) -> Result<(planner::Router, planner::Started)> {
    let captured = &shared.context;
    let data_path = captured
        .data_path
        .to_str()
        .ok_or_else(|| HostError::new("now_playing_unsupported", "NonUnicode library path"))?;
    let context =
        planner::Context::from_host(&captured.runtime_id, captured.library_generation, data_path)
            .map_err(dependency)?;
    let folder = captured.data_path.join("Audio");
    let paths = planner::TrustedPaths::from_host(
        &context,
        if cfg!(windows) {
            planner::Platform::Windows
        } else {
            planner::Platform::Posix
        },
        folder
            .to_str()
            .ok_or_else(|| dependency("NonUnicode audio folder"))?,
    )
    .map_err(dependency)?;
    let mut router = planner::Router::new(context.clone(), paths).map_err(dependency)?;
    let started = match command {
        Command::Set {
            payload,
            duration_number,
        } => {
            let path = payload["filePath"].as_str().unwrap();
            let trusted =
                planner::TrustedMediaPath::from_host(&context, path).map_err(dependency)?;
            let mut data =
                planner::SourceData::from_payload(payload.clone()).map_err(dependency)?;
            if let Some(n) = duration_number {
                data.set_duration_number(*n)
            }
            router.begin_set(data, trusted)
        }
        Command::Unset => router.begin_unset(),
        Command::Open => router.begin_open(),
    }
    .map_err(dependency)?;
    Ok((router, started))
}
fn make_operation(
    shared: &Shared,
    id: OperationId,
    work: Work,
    router: planner::Router,
    started: planner::Started,
) -> Operation {
    let deadline = Instant::now() + shared.config.operation_timeout;
    Operation {
        id,
        router,
        router_id: started.operation,
        next: started.effect,
        control: PreparationControl::new(deadline),
        deadline,
        cancel: work.cancel,
        authority: None,
        session: None,
        publications: Vec::new(),
        reply: Some(work.reply),
        lease: work.lease,
        diagnostics: vec![],
        failure: None,
        result: None,
        cleaned: false,
    }
}
async fn revalidate(shared: &Shared, op: &Operation) -> Result<()> {
    shared.current(op.id.context())?;
    if op.cancel.is_cancelled() {
        return Err(HostError::new(
            "cancelled",
            "Explicit operation cancellation",
        ));
    }
    if Instant::now() >= op.deadline {
        return Err(HostError::new(
            "now_playing_deadline",
            "Cooperative operation deadline exceeded",
        ));
    }
    if let Some(authority) = &op.authority {
        authority
            .revalidate(Control::new(op.deadline, op.cancel.clone()))
            .await
            .map_err(dependency)?;
    }
    shared.current(op.id.context())
}
fn native_picture(p: &freeshow_cover_images::Picture) -> Result<planner::NativePicture> {
    let mut fields = vec![];
    for key in p.field_order.iter().copied().filter(|k| *k != "data") {
        let value = if p.undefined_fields.contains(&key) {
            CommonValue::Undefined
        } else {
            from_json(
                p.fields
                    .get(key)
                    .ok_or_else(|| dependency("Cover field order has missing value"))?,
            )
        };
        fields.push((key.to_owned(), value));
    }
    planner::NativePicture::from_host(CommonValue::Object(fields), p.data.clone())
        .map_err(dependency)
}
fn from_json(value: &Value) -> CommonValue {
    match value {
        Value::Null => CommonValue::Null,
        Value::Bool(v) => CommonValue::Bool(*v),
        Value::Number(v) => CommonValue::Number(v.as_f64().unwrap()),
        Value::String(v) => CommonValue::String(v.clone()),
        Value::Array(v) => CommonValue::Array(v.iter().map(from_json).collect()),
        Value::Object(v) => {
            CommonValue::Object(v.iter().map(|(k, v)| (k.clone(), from_json(v))).collect())
        }
    }
}
async fn prepare(shared: Arc<Shared>, mut op: Operation) -> Operation {
    #[cfg(test)]
    shared.hooks.pause(TestStage::Preparation).await;
    let Some(issued) = op.next.take() else {
        return op;
    };
    let planner::Effect::ParseMetadata { path } = &issued.effect else {
        op.next = Some(issued);
        return op;
    };
    let path = path.clone();
    let accepted = match op.router.accept(issued.token) {
        Ok(v) => v,
        Err(e) => {
            op.fail(dependency(e));
            return op;
        }
    };
    let cover_context = CoverContext::from_host(
        &op.id.context().runtime_id,
        op.id.context().library_generation,
        op.id.context().data_path.to_str().unwrap(),
    );
    let result = match cover_context {
        Ok(ctx) => {
            let services = PreparationServices::new(
                &shared.dependencies.library,
                &shared.dependencies.assets,
                &shared.dependencies.audio,
                &shared.dependencies.covers,
            );
            let preparation = services.prepare_retained(op.id.context(), &ctx, &path, &op.control);
            tokio::pin!(preparation);
            tokio::select! {biased; result=&mut preparation=>result,
                _=op.cancel.cancelled()=>{op.control.cancel();preparation.await},
                _=shared.stop.cancelled()=>{op.control.cancel();preparation.await},
            }
        }
        Err(e) => {
            op.fail(dependency(e));
            return op;
        }
    };
    let receipt = match result {
        Ok(prepared) => {
            let (common, pictures, authority, session, _) = prepared.into_parts();
            op.authority = Some(authority);
            op.session = Some(session);
            let metadata = pictures
                .iter()
                .map(native_picture)
                .collect::<Result<Vec<_>>>()
                .and_then(|pictures| {
                    planner::Metadata::from_native_common(
                        common,
                        planner::Covers::NativeComplete(pictures),
                    )
                    .map_err(dependency)
                });
            match metadata {
                Ok(m) => planner::Receipt::Metadata(Some(m)),
                Err(e) => {
                    op.fail(e.clone());
                    planner::Receipt::Failed(planner::Error::native(e.code, &e.message))
                }
            }
        }
        Err(failure) => {
            let (error, session) = failure.into_parts();
            op.session = session;
            if error.code == "audio_input_rejected" {
                op.diagnostic(HostError::new("audio_input_rejected", error));
                planner::Receipt::Metadata(None)
            } else {
                let code = match error.kind {
                    FailureKind::Authorization => "now_playing_authorization",
                    FailureKind::Context => "stale_library",
                    FailureKind::Cancelled => "cancelled",
                    FailureKind::Unsupported => "now_playing_unsupported",
                    FailureKind::Resource => "now_playing_resource",
                    FailureKind::Input => "now_playing_input",
                    FailureKind::Dependency => "now_playing_dependency",
                };
                let e = HostError::new(code, error);
                op.fail(e.clone());
                planner::Receipt::Failed(planner::Error::native(e.code, &e.message))
            }
        }
    };
    match op.router.complete(&accepted, receipt) {
        Ok(next) => op.next = next,
        Err(e) => op.fail(dependency(e)),
    }
    op
}
async fn register_publication(
    shared: &Shared,
    op: &mut Operation,
    metadata: Option<&planner::NativeMetadataPublication>,
    alert: Option<AlertKind>,
) -> Result<Publication> {
    if op.publications.len() == 5 {
        return Err(HostError::new(
            "now_playing_publication_limit",
            "Five whole source publications are retained for this operation",
        ));
    }
    shared.current(op.id.context())?;
    let sequence = PUBLICATION_SEQUENCE
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
        .map_err(|_| {
            HostError::new(
                "now_playing_limit",
                "Process publication sequence exhausted",
            )
        })?
        .to_string();
    let id = format!("native-now-playing-{}", uuid::Uuid::new_v4().simple());
    let context = op.id.context().clone();
    let publication = if let Some(metadata) = metadata {
        let pictures = metadata
            .pictures()
            .iter()
            .map(|p| PictureRef {
                fields: p.fields(),
                bytes: p.bytes(),
            })
            .collect::<Vec<_>>();
        let wire =
            freeshow_metadata_wire::build(metadata.file_path(), metadata.common(), &pictures)
                .map_err(|e| HostError::new(e.code(), e))?;
        let mut event = Event::reply("MAIN", "AUDIO_METADATA", wire.data, None);
        event.context = Some(context.clone());
        Publication::Metadata(MetadataDelivery {
            event,
            hydration: wire.hydration,
            id: id.clone(),
            sequence,
            context: context.clone(),
        })
    } else {
        let kind = alert.ok_or_else(|| dependency("Missing typed native alert reason"))?;
        let mut event = Event::reply("MAIN", "ALERT", Value::String(kind.message()?), None);
        event.context = Some(context.clone());
        Publication::Alert(AlertDelivery {
            kind,
            event,
            id: id.clone(),
            sequence,
            context: context.clone(),
        })
    };
    let pending = Arc::new(Pending {
        context,
        state: AtomicU8::new(0),
    });
    #[cfg(test)]
    shared.hooks.pause(TestStage::PublicationAdmission).await;
    let mut map = shared
        .publications
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // The same map lock fences close's retirement scan. Never install a new
    // unackable live Pending after close already retired the earlier entries.
    if shared.closing.load(Ordering::Acquire) {
        return Err(shared.error().unwrap_or_else(|| {
            HostError::new(
                "now_playing_shutdown",
                "Publication admission retired before registration",
            )
        }));
    }
    if map.len() >= 20 {
        return Err(busy());
    }
    map.insert(id.clone(), pending.clone());
    op.publications.push((id, pending));
    Ok(publication)
}
async fn image(
    shared: &Shared,
    op: &Operation,
    effect: &planner::Effect,
) -> Result<planner::Receipt> {
    let covers = &shared.dependencies.covers;
    let session = op
        .session
        .as_ref()
        .ok_or_else(|| dependency("No real image Session"))?;
    let cancel = Cancel::default();
    let accepted_cancel = cancel.clone();
    let ticket = match effect {
        planner::Effect::ImageCreate { bytes } => covers.create(
            session,
            bytes.clone(),
            ImagePolicy::VerifiedPngPixels,
            cancel,
        ),
        planner::Effect::ImagePng { .. } => covers.png(session, cancel),
        planner::Effect::ImageSize { .. } => covers.size(session, cancel),
        planner::Effect::ImageResizeJpeg { .. } => covers.resize_jpeg90(session, cancel),
        planner::Effect::ReleaseImage { .. } => covers.release(session),
        _ => return Err(dependency("Unknown image effect")),
    }
    .map_err(|e| HostError::new(e.code, e.message))?;
    let finish = ticket.finish();
    tokio::pin!(finish);
    let observed = if matches!(effect, planner::Effect::ReleaseImage { .. }) {
        finish.await
    } else {
        tokio::select! {biased; result=&mut finish=>result,
            _=op.cancel.cancelled()=>{accepted_cancel.cancel();finish.await},
            _=shared.stop.cancelled()=>{accepted_cancel.cancel();finish.await},
        }
    }
    .map_err(|e| HostError::new(e.code, e.message))?;
    observed
        .inspect(|reply| match reply {
            Reply::Image { width, height } => {
                if matches!(effect, planner::Effect::ImageSize { .. }) {
                    Ok(planner::Receipt::Size {
                        width: *width,
                        height: *height,
                    })
                } else {
                    Ok(planner::Receipt::Image(1))
                }
            }
            Reply::Bytes { data, .. } => Ok(planner::Receipt::Bytes(data.clone())),
            Reply::Released => Ok(planner::Receipt::Done { success: true }),
            _ => Err(dependency("Wrong real image reply")),
        })
        .map_err(|e| HostError::new(e.code, e.message))?
}
async fn publish(callbacks: &Callbacks, op: &Operation, delivery: Publication) -> Result<()> {
    let id = delivery.id().to_owned();
    let result = AssertUnwindSafe(async { (callbacks.publish)(delivery).await })
        .catch_unwind()
        .await
        .map_err(|_| {
            HostError::new(
                "now_playing_callback_panic",
                "Trusted publication callback panicked",
            )
        })
        .and_then(|r| r);
    if result.is_err() {
        if let Some((_, pending)) = op.publications.iter().find(|(saved, _)| saved == &id) {
            let _ = pending
                .state
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
        }
    }
    result
}
async fn execute(
    shared: &Shared,
    callbacks: &Callbacks,
    op: &mut Operation,
    effect: &planner::Effect,
    phase: Option<&LibraryPhase>,
) -> Result<planner::Receipt> {
    use planner::Effect as E;
    if matches!(effect, E::ReleaseImage { .. }) {
        return image(shared, op, effect).await;
    }
    revalidate(shared, op).await?;
    let intent = match effect {
        E::EnsureAudioFolder { .. } => Some(Intent::EnsureFolder),
        E::Exists { path } => Some(Intent::Exists(if path.ends_with("NowPlaying.txt") {
            Leaf::Text
        } else {
            Leaf::Cover
        })),
        E::Delete { .. } => Some(Intent::DeleteCover),
        E::WriteCover { bytes, .. } => Some(Intent::WriteCover(bytes.clone())),
        E::WriteText { text, .. } => Some(Intent::WriteText(text.clone())),
        _ => None,
    };
    if let Some(intent) = intent {
        let observed = files::submit(
            phase.ok_or_else(|| dependency("File effect outside a real LibraryPhase"))?,
            intent,
        )
        .map_err(dependency)?
        .finish()
        .await
        .map_err(dependency)?;
        for error in &observed.diagnostics {
            op.diagnostic(HostError::new("now_playing_file_caught", error));
        }
        if observed.folder_creation_failed {
            if !matches!(effect, E::EnsureAudioFolder { .. })
                || observed.paths.folder != op.id.context().data_path.join("Audio")
            {
                return Err(dependency(
                    "Folder-failure observation is not the captured fixed Audio path",
                ));
            }
            let delivery = register_publication(
                shared,
                op,
                None,
                Some(AlertKind::FolderCreationFailed {
                    folder: observed.paths.folder.clone(),
                }),
            )
            .await?;
            publish(callbacks, op, delivery).await?;
        }
        return Ok(if matches!(effect, E::Exists { .. }) {
            planner::Receipt::Exists(observed.success.unwrap_or(false))
        } else {
            planner::Receipt::Done {
                success: observed.success.unwrap_or(false),
            }
        });
    }
    match effect {
        E::PublishMetadata { .. } => {
            let metadata = effect
                .native_metadata()
                .ok_or_else(|| dependency("JSON projection cannot publish native metadata"))?;
            let delivery = register_publication(shared, op, Some(metadata), None).await?;
            publish(callbacks, op, delivery).await?;
            Ok(planner::Receipt::Done { success: true })
        }
        E::Alert { message } => {
            if message != "This does not exist!" {
                return Err(dependency("Unexpected sealed planner alert"));
            }
            let delivery =
                register_publication(shared, op, None, Some(AlertKind::MissingText)).await?;
            publish(callbacks, op, delivery).await?;
            Ok(planner::Receipt::Done { success: true })
        }
        E::IcecastMetadata { title } => {
            let receipt = shared
                .dependencies
                .pipeline
                .metadata(op.id.context(), title, op.cancel.clone())
                .await
                .map_err(dependency)?;
            for e in receipt.diagnostics() {
                op.diagnostic(HostError::new("now_playing_icecast_caught", e));
            }
            if let Some(e) = receipt.failure() {
                return Err(dependency(e));
            }
            if !receipt
                .effects()
                .iter()
                .any(|e| matches!(e, crate::audio_pipeline::Effect::MetadataApplied { .. }))
            {
                return Err(dependency("No actual metadata-applied observation"));
            }
            Ok(planner::Receipt::Done { success: true })
        }
        E::ImageCreate { .. }
        | E::ImagePng { .. }
        | E::ImageSize { .. }
        | E::ImageResizeJpeg { .. } => image(shared, op, effect).await,
        E::Reveal { path } => {
            AssertUnwindSafe(async {
                let path = PathBuf::from(path);
                let context = op.id.context().clone();
                // Count only actual trusted callback invocation after authority
                // revalidation, never an intent or successful OS reveal claim.
                shared
                    .reveal_calls
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
                    .map_err(|_| {
                        HostError::new(
                            "now_playing_limit",
                            "Native reveal invocation counter exhausted",
                        )
                    })?;
                (callbacks.reveal)(path, context).await
            })
            .catch_unwind()
            .await
            .map_err(|_| {
                HostError::new(
                    "now_playing_callback_panic",
                    "Trusted reveal callback panicked",
                )
            })??;
            Ok(planner::Receipt::Done { success: true })
        }
        _ => Err(dependency(
            "Unexpected preparation effect in source continuation",
        )),
    }
}
fn belongs(source: planner::SourcePhase, kind: PhaseKind) -> bool {
    source == planner::SourcePhase::Cleanup
        || matches!(
            (source, kind),
            (
                planner::SourcePhase::PublishMetadata,
                PhaseKind::PublishMetadata
            ) | (planner::SourcePhase::PostMetadata, PhaseKind::PostMetadata)
                | (planner::SourcePhase::Text, PhaseKind::Text)
                | (planner::SourcePhase::Unset, PhaseKind::Unset)
                | (planner::SourcePhase::Open, PhaseKind::Open)
        )
}
async fn run_phase(
    shared: Arc<Shared>,
    callbacks: Callbacks,
    mut op: Operation,
    token: PhaseToken,
) -> (Operation, PhaseToken) {
    #[cfg(test)]
    shared.hooks.pause(TestStage::Phase(token.kind())).await;
    let phase = if token.kind() != PhaseKind::PublishMetadata {
        match shared
            .dependencies
            .library
            .begin_phase(request(op.id.context()), "main")
            .await
        {
            Ok(p) => Some(p),
            Err(e) => {
                op.fail(dependency(e));
                None
            }
        }
    } else {
        None
    };
    if op.failure.is_none() {
        while let Some(issued) = op.next.take() {
            if !belongs(issued.source_phase(), token.kind()) {
                op.next = Some(issued);
                break;
            }
            let accepted = match op.router.accept(issued.token) {
                Ok(a) => a,
                Err(e) => {
                    op.fail(dependency(e));
                    break;
                }
            };
            let receipt =
                match execute(&shared, &callbacks, &mut op, &issued.effect, phase.as_ref()).await {
                    Ok(r) => r,
                    Err(e) => {
                        op.fail(e.clone());
                        planner::Receipt::Failed(planner::Error::native(e.code, &e.message))
                    }
                };
            match op.router.complete(&accepted, receipt) {
                Ok(next) => op.next = next,
                Err(e) => {
                    op.fail(dependency(e));
                    break;
                }
            }
            if op.failure.is_some()
                && op
                    .next
                    .as_ref()
                    .is_some_and(|i| i.source_phase() != planner::SourcePhase::Cleanup)
            {
                break;
            }
        }
    }
    if let Some(phase) = phase {
        phase.close();
        if let Err(e) = phase.finish().await {
            op.fail(dependency(e));
        }
    }
    (op, token)
}
async fn cleanup(shared: Arc<Shared>, mut op: Operation) -> Operation {
    // This future never discards a timed-out Session. It retries the same real
    // retirement until actual completion; callers may time out independently.
    if let Some(session) = op.session.as_ref() {
        loop {
            match shared
                .dependencies
                .covers
                .retire(session, shared.config.shutdown_timeout)
                .await
            {
                Ok(()) => break,
                Err(e) => {
                    shared.fail(dependency(e));
                    continue;
                }
            }
        }
    }
    op.session.take();
    op.authority.take();
    op.cleaned = true;
    if op.result.is_none() {
        match op.router.take_result(op.router_id) {
            Ok(result) => {
                if let Some(e) = &result.error {
                    op.fail(dependency(e));
                }
                op.result = Some(result);
            }
            Err(e) => op.fail(dependency(e)),
        }
    }
    op
}
fn publications_done(op: &mut Operation) -> bool {
    // Inspect all first: a settled sibling never disposes a still-live metadata
    // or folder notification, and repeated polls do not duplicate diagnostics.
    if op
        .publications
        .iter()
        .any(|(_, p)| p.state.load(Ordering::Acquire) == 0)
    {
        return false;
    }
    let states = op
        .publications
        .iter()
        .map(|(_, p)| p.state.load(Ordering::Acquire))
        .collect::<Vec<_>>();
    for state in states {
        match state {
            2 => op.diagnostic(HostError::new(
                "now_playing_delivery_rejected",
                "Renderer rejected actual private publication",
            )),
            3 => op.diagnostic(HostError::new(
                "now_playing_delivery_retired",
                "Publication authority retired; renderer delivery is unconfirmed",
            )),
            _ => {}
        }
    }
    true
}
fn finish(shared: &Shared, mut op: Operation, schedule: &mut Schedule) {
    if op
        .publications
        .iter()
        .any(|(_, pending)| pending.state.load(Ordering::Acquire) == 3)
    {
        if let Some(error) = shared.error() {
            op.fail(error);
        }
    }
    for (id, _) in op.publications.drain(..) {
        shared
            .publications
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }
    if let Err(e) = schedule.release(&op.id) {
        shared.fail(dependency(e));
    }
    let receipt = Receipt {
        context: op.id.context().clone(),
        operation: op.id.sequence(),
        mutated_input: op.result.take().and_then(|r| r.mutated_input),
        diagnostics: op.diagnostics,
        failure: op.failure,
        _lease: op.lease,
    };
    if let Some(reply) = op.reply.take() {
        let _ = reply.send(Ok(receipt));
    }
    if shared
        .completed
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
        .is_err()
    {
        shared.fail(HostError::new(
            "now_playing_limit",
            "Native completion counter exhausted",
        ));
        shared.close();
    }
}
async fn supervise(
    shared: Arc<Shared>,
    callbacks: Callbacks,
    mut schedule: Schedule,
    mut incoming: mpsc::Receiver<Work>,
    progress: watch::Sender<Progress>,
) {
    let mut preparations: FuturesUnordered<OwnedFuture<Operation>> = FuturesUnordered::new();
    let mut cleanups: FuturesUnordered<OwnedFuture<Operation>> = FuturesUnordered::new();
    let mut active: Option<OwnedFuture<(Operation, PhaseToken)>> = None;
    let mut ready: HashMap<OperationId, Operation> = HashMap::new();
    let mut retired = false;
    loop {
        let notified = shared.changed.notified();
        tokio::pin!(notified);
        if shared.closing.load(Ordering::Acquire) && !retired {
            retired = true;
            schedule.retire();
            incoming.close();
            for op in ready.values() {
                op.control.cancel();
            }
        }
        // Source ingress observed in this turn precedes observed parse outcomes.
        while let Ok(work) = incoming.try_recv() {
            admit(&shared, &mut schedule, &mut preparations, &mut ready, work);
        }
        // Poll all immediately ready preparations before choosing the next phase.
        while let Some(Some(op)) = preparations.next().now_or_never() {
            prepared(&shared, &mut schedule, &mut ready, op);
        }
        let done = ready
            .iter()
            .filter(|(_, op)| schedule.state(&op.id) == Ok(OperationState::Finished))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in done {
            let mut op = ready.remove(&id).unwrap();
            if !op.cleaned {
                cleanups.push(Box::pin(cleanup(shared.clone(), op)));
            } else if publications_done(&mut op) {
                finish(&shared, op, &mut schedule);
            } else {
                ready.insert(id, op);
            }
        }
        if active.is_none() {
            if let Some(token) = schedule.take_next() {
                if let Some(op) = ready.remove(token.operation()) {
                    active = Some(Box::pin(run_phase(
                        shared.clone(),
                        callbacks.clone(),
                        op,
                        token,
                    )));
                } else {
                    shared.fail(dependency("Scheduler exposed a missing native operation"));
                    shared.close();
                }
            }
        }
        shared
            .phase_active
            .store(active.is_some(), Ordering::Release);
        shared
            .preparing
            .store(preparations.len() as u64, Ordering::Release);
        shared
            .cleaning
            .store(cleanups.len() as u64, Ordering::Release);
        shared
            .retained
            .store(schedule.status().records as u64, Ordering::Release);
        if retired
            && incoming.is_empty()
            && schedule.status().drained()
            && preparations.is_empty()
            && cleanups.is_empty()
            && active.is_none()
        {
            break;
        }
        let deadline = ready
            .values()
            .filter(|op| {
                op.publications
                    .iter()
                    .any(|(_, p)| p.state.load(Ordering::Acquire) == 0)
            })
            .map(|op| op.deadline)
            .min();
        tokio::select! {biased;
            work=incoming.recv(),if !incoming.is_closed()=>{if let Some(work)=work{admit(&shared,&mut schedule,&mut preparations,&mut ready,work);}},
            value=preparations.next(),if !preparations.is_empty()=>{if let Some(op)=value{prepared(&shared,&mut schedule,&mut ready,op);}},
            value=async {active.as_mut().unwrap().await},if active.is_some()=>{
                active.take();let (mut op,token)=value;
                let result=if op.failure.is_some(){schedule.fail_current(&token)}else{schedule.complete(&token)};
                if let Err(e)=result{op.fail(dependency(e));shared.fail(dependency(e));shared.close();}
                ready.insert(op.id.clone(),op);
            },
            value=cleanups.next(),if !cleanups.is_empty()=>{if let Some(op)=value{ready.insert(op.id.clone(),op);}},
            _=&mut notified=>{},
            _=shared.stop.cancelled(),if !retired=>{},
            _=async{tokio::time::sleep_until(tokio::time::Instant::from_std(deadline.unwrap())).await},if deadline.is_some()=>{shared.fail(HostError::new("now_playing_delivery_timeout","Publication completion timed out; authoritative host retirement retains actual effects"));shared.close();},
        }
    }
    // Callback strong references end here. Capturing the host strongly inside a
    // callback would create a cycle; construction must use weak native handles.
    drop(callbacks);
    let _ = progress.send(Progress {
        complete: true,
        error: shared.error(),
    });
    #[cfg(test)]
    shared.hooks.pause(TestStage::SupervisorExit).await;
}
use futures_util::FutureExt;
fn admit(
    shared: &Arc<Shared>,
    schedule: &mut Schedule,
    preparations: &mut FuturesUnordered<OwnedFuture<Operation>>,
    ready: &mut HashMap<OperationId, Operation>,
    work: Work,
) {
    if shared.closing.load(Ordering::Acquire) {
        let _ = work.reply.send(Err(shared.error().unwrap_or_else(|| {
            HostError::new(
                "now_playing_shutdown",
                "Host retired before native acceptance",
            )
        })));
        return;
    }
    let (router, started) = match build_planner(shared, &work.command) {
        Ok(plan) => plan,
        Err(error) => {
            let _ = work.reply.send(Err(error));
            return;
        }
    };
    let admitted = match &work.command {
        Command::Set { .. } => schedule.admit_set(),
        Command::Unset => schedule.admit_unset(),
        Command::Open => schedule.admit_open(),
    };
    match admitted {
        Ok(id) => {
            let op = make_operation(shared, id.clone(), work, router, started);
            if op
                .next
                .as_ref()
                .is_some_and(|i| i.source_phase() == planner::SourcePhase::Preparation)
            {
                preparations.push(Box::pin(prepare(shared.clone(), op)));
            } else {
                if matches!(schedule.state(&id), Ok(OperationState::PendingPreparation)) {
                    let _ = schedule.ready(&id, false);
                }
                ready.insert(id, op);
            }
        }
        Err(e) => {
            let _ = work.reply.send(Err(dependency(e)));
        }
    }
}
fn prepared(
    _shared: &Shared,
    schedule: &mut Schedule,
    ready: &mut HashMap<OperationId, Operation>,
    mut op: Operation,
) {
    let result = if op.failure.is_some() {
        schedule.preparation_failed(&op.id)
    } else {
        schedule.ready(
            &op.id,
            op.next
                .as_ref()
                .is_some_and(|i| i.source_phase() == planner::SourcePhase::PublishMetadata),
        )
    };
    if let Err(e) = result {
        op.fail(dependency(e));
    }
    ready.insert(op.id.clone(), op);
}
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TestStage {
    Preparation,
    PublicationAdmission,
    Phase(PhaseKind),
    SupervisorExit,
}
#[cfg(test)]
#[derive(Clone, Default)]
pub struct TestHooks(Arc<Mutex<HeldGate>>);
#[cfg(test)]
type HeldGate = Option<(TestStage, Arc<TestGate>)>;
#[cfg(test)]
pub struct TestGate {
    pub entered: Notify,
    pub resume: Notify,
}
#[cfg(test)]
impl TestHooks {
    #[allow(
        dead_code,
        reason = "Controlled native integration-test boundary; binary unit target does not install it"
    )]
    pub fn hold(&self, stage: TestStage) -> Arc<TestGate> {
        let gate = Arc::new(TestGate {
            entered: Notify::new(),
            resume: Notify::new(),
        });
        *self.0.lock().unwrap() = Some((stage, gate.clone()));
        gate
    }
    async fn pause(&self, stage: TestStage) {
        let gate = {
            let mut gate = self.0.lock().unwrap();
            if gate.as_ref().is_some_and(|(s, _)| *s == stage) {
                gate.take().map(|(_, g)| g)
            } else {
                None
            }
        };
        if let Some(g) = gate {
            g.entered.notify_one();
            g.resume.notified().await;
        }
    }
}
