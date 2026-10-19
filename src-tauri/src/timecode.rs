//! Owned timecode composition. Source notifications are not success acknowledgments.
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::LibraryService,
    WireError,
};
use freeshow_ltc as ltc;
use freeshow_midi::{Hydration, Number, NumberField};
use freeshow_midi_ports as ports;
use freeshow_timecode as route;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    future::{poll_fn, Future},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    task::Poll,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
pub const COMMANDS: &[&str] = route::COMMANDS;
const JOBS: usize = 16;
const PORTS: usize = 16;
const PUBLICATIONS: usize = 16;
const PUBLICATION_BYTES: usize = 1024 * 1024;
type Result<T> = std::result::Result<T, WireError>;
type Failure = (&'static str, String);
type Factory = Arc<dyn Fn() -> ports::Result<Box<dyn ports::Backend>> + Send + Sync>;
type Publisher =
    Arc<dyn Fn(NativeTimecodePublication) -> std::result::Result<(), String> + Send + Sync>;
type NativeFuture = Pin<Box<dyn Future<Output = NativeResult> + Send>>;
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
fn error(code: &'static str, text: impl ToString) -> WireError {
    let text = text.to_string();
    let mut end = text.len().min(2048);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    WireError::new(code, &text[..end])
}
fn re(e: route::Error) -> WireError {
    error(e.code, e.message)
}
fn pe(e: ports::Error) -> WireError {
    error(e.code, e.message)
}
fn le(e: ltc::Error) -> WireError {
    error(e.code, e.message)
}
fn routed(e: &WireError) -> route::Error {
    route::Error {
        code: e.code,
        message: e.message.clone(),
    }
}
#[derive(Clone)]
pub struct TimecodeHostConfig {
    pub ports: ports::Config,
    pub ltc: ltc::Config,
    pub operation_timeout: Duration,
    pub lifecycle_timeout: Duration,
    pub delivery_timeout: Duration,
}
impl Default for TimecodeHostConfig {
    fn default() -> Self {
        Self {
            ports: ports::Config::default(),
            ltc: ltc::Config::default(),
            operation_timeout: Duration::from_secs(5),
            lifecycle_timeout: Duration::from_secs(10),
            delivery_timeout: Duration::from_secs(5),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct NativeTimecodePublication {
    pub event: Event,
    pub hydration: Hydration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<Vec<u8>>,
    pub delivery: NativeTimecodeDelivery,
}
#[derive(Debug, Clone, Serialize)]
pub struct NativeTimecodeDelivery {
    pub id: String,
    pub context: RuntimeContext,
    pub run: u64,
}
#[derive(Debug, Serialize)]
pub enum Observation {
    Scheduled { operation: u64 },
    LogStatus { data: route::Data },
    Diagnostic { code: &'static str, message: String },
}
pub struct TimecodeReceipt {
    context: RuntimeContext,
    observations: Vec<Observation>,
    _lease: Arc<OwnedSemaphorePermit>,
}
impl TimecodeReceipt {
    pub fn context(&self) -> &RuntimeContext {
        &self.context
    }
    pub fn observations(&self) -> &[Observation] {
        &self.observations
    }
}
impl std::fmt::Debug for TimecodeReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimecodeReceipt")
            .field("context", &self.context)
            .field("observations", &self.observations)
            .finish()
    }
}
struct Delivery {
    context: RuntimeContext,
    id: String,
    reply: oneshot::Sender<bool>,
}
struct Shared {
    library: LibraryService,
    config: TimecodeHostConfig,
    factory: Factory,
    library_source: ltc::TrustedLibrary,
    publisher: Publisher,
    closing: AtomicBool,
    stop: CancellationToken,
    pending: Mutex<Option<Delivery>>,
    failure: Mutex<Option<Failure>>,
    state: Mutex<Value>,
    next_run: AtomicU64,
}
impl Shared {
    fn current(&self, c: &RuntimeContext) -> Result<()> {
        if self
            .library
            .context()
            .map_err(|e| error("timecode_context", e))?
            == *c
        {
            Ok(())
        } else {
            Err(error(
                "stale_library",
                "Timecode belongs to an obsolete full runtime context",
            ))
        }
    }
    fn fail(&self, e: WireError) {
        let mut first = lock(&self.failure);
        if first.is_none() {
            *first = Some((e.code, e.message));
        }
    }
    fn terminal_error(&self) -> Option<WireError> {
        lock(&self.failure).as_ref().map(|(c, m)| error(c, m))
    }
}
struct Job {
    context: RuntimeContext,
    command: route::Command,
    cancel: CancellationToken,
    reply: oneshot::Sender<Result<TimecodeReceipt>>,
    lease: Arc<OwnedSemaphorePermit>,
}
struct Change {
    reply: oneshot::Sender<Result<()>>,
}
#[derive(Clone, Default)]
struct Progress {
    attempt: u64,
    complete: bool,
    error: Option<Failure>,
}
struct Inner {
    shared: Arc<Shared>,
    jobs: mpsc::Sender<Job>,
    control: mpsc::Sender<Change>,
    slots: Arc<Semaphore>,
    done: watch::Receiver<Progress>,
    worker: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        self.shared.stop.cancel();
        lock(&self.shared.pending).take();
    }
}
#[derive(Clone)]
pub struct NativeTimecode {
    inner: Arc<Inner>,
}
impl NativeTimecode {
    pub fn new(
        library: LibraryService,
        config: TimecodeHostConfig,
        source: ltc::TrustedLibrary,
        factory: impl Fn() -> ports::Result<Box<dyn ports::Backend>> + Send + Sync + 'static,
        publisher: impl Fn(NativeTimecodePublication) -> std::result::Result<(), String>
            + Send
            + Sync
            + 'static,
    ) -> Result<Self> {
        for d in [
            config.operation_timeout,
            config.lifecycle_timeout,
            config.delivery_timeout,
        ] {
            if d.is_zero() || d > Duration::from_secs(30) {
                return Err(error("timecode_config", "Invalid host timeout"));
            }
        }
        let context = library
            .context()
            .map_err(|e| error("timecode_context", e))?;
        validate_context(&context)?;
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            error(
                "timecode_runtime",
                "Native timecode host needs a Tokio runtime",
            )
        })?;
        let shared = Arc::new(Shared {
            library,
            config,
            factory: Arc::new(factory),
            library_source: source,
            publisher: Arc::new(publisher),
            closing: AtomicBool::new(false),
            stop: CancellationToken::new(),
            pending: Mutex::new(None),
            failure: Mutex::new(None),
            state: Mutex::new(json!({})),
            next_run: AtomicU64::new(1),
        });
        let (jobs, receive) = mpsc::channel(JOBS);
        let (control, changes) = mpsc::channel(1);
        let (progress, done) = watch::channel(Progress::default());
        let state = shared.clone();
        let worker = runtime.spawn(async move {
            supervise(state, receive, changes, progress, context).await;
        });
        Ok(Self {
            inner: Arc::new(Inner {
                shared,
                jobs,
                control,
                slots: Arc::new(Semaphore::new(JOBS)),
                done,
                worker: tokio::sync::Mutex::new(Some(worker)),
            }),
        })
    }
    fn check(&self, c: &RuntimeContext, cancel: &CancellationToken) -> Result<()> {
        validate_context(c)?;
        if self.inner.shared.closing.load(Ordering::Acquire) {
            return Err(error(
                "timecode_shutdown",
                "Native timecode admission is closed",
            ));
        }
        if cancel.is_cancelled() {
            return Err(error(
                "cancelled",
                "Timecode canceled before host admission",
            ));
        }
        self.inner.shared.current(c)
    }
    async fn submit(
        &self,
        c: &RuntimeContext,
        cancel: CancellationToken,
        make: impl FnOnce() -> Result<route::Command>,
    ) -> Result<TimecodeReceipt> {
        self.check(c, &cancel)?;
        let lease =
            Arc::new(
                self.inner.slots.clone().try_acquire_owned().map_err(|_| {
                    error("timecode_busy", "Retained timecode jobs/results are full")
                })?,
            );
        let slot = self
            .inner
            .jobs
            .try_reserve()
            .map_err(|_| error("timecode_busy", "Timecode host FIFO is full"))?;
        self.check(c, &cancel)?;
        let command = make()?;
        let (reply, receive) = oneshot::channel();
        slot.send(Job {
            context: c.clone(),
            command,
            cancel,
            reply,
            lease,
        });
        receive.await.map_err(|_| {
            if self.inner.shared.closing.load(Ordering::Acquire) {
                error(
                    "timecode_shutdown",
                    "Timecode owner closed racing admission",
                )
            } else {
                error("timecode_worker", "Timecode owner ended before observation")
            }
        })?
    }
    pub async fn command(
        &self,
        c: &RuntimeContext,
        command: route::Command,
        cancel: CancellationToken,
    ) -> Result<TimecodeReceipt> {
        validate_command(&command)?;
        self.submit(c, cancel, || Ok(command)).await
    }
    pub async fn audio(
        &self,
        c: &RuntimeContext,
        mode: &str,
        bytes: &[u8],
        announced: usize,
        cancel: CancellationToken,
    ) -> Result<TimecodeReceipt> {
        if mode.len() > 4096 || bytes.len() > route::MAX_AUDIO_BYTES {
            return Err(error("timecode_limit", "Raw timecode input exceeds bounds"));
        }
        if announced != bytes.len() {
            return Err(error(
                "timecode_raw_length",
                "Announced/raw byte lengths differ",
            ));
        }
        self.submit(c, cancel, || {
            Ok(route::Command::Audio {
                mode: mode.into(),
                bytes: bytes.to_vec(),
            })
        })
        .await
    }
    pub async fn value(
        &self,
        c: &RuntimeContext,
        value: Number,
        cancel: CancellationToken,
    ) -> Result<TimecodeReceipt> {
        self.command(c, route::Command::Value(value), cancel).await
    }
    pub async fn dispatch(
        &self,
        request: &Request,
        window: &str,
        cancel: CancellationToken,
    ) -> Result<TimecodeReceipt> {
        if window != "main"
            || request.channel != "MAIN"
            || !COMMANDS.contains(
                &request
                    .data
                    .get("channel")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            )
        {
            return Err(error(
                "timecode_unsupported",
                "Unsupported native timecode request/window",
            ));
        }
        let c = request
            .context
            .as_ref()
            .ok_or_else(|| error("stale_library", "Full timecode context required"))?;
        bound_json(&request.data)?;
        self.submit(c, cancel, || parse(request)).await
    }
    pub fn complete(
        &self,
        window: &str,
        c: &RuntimeContext,
        id: &str,
        accepted: bool,
    ) -> Result<()> {
        if window != "main" || id.len() > 80 {
            return Err(error(
                "timecode_completion",
                "Only bounded main-window completions are accepted",
            ));
        }
        let mut pending = lock(&self.inner.shared.pending);
        if pending
            .as_ref()
            .is_none_or(|p| p.id != id || p.context != *c)
        {
            return Err(error(
                "timecode_completion",
                "Unknown, foreign or consumed completion",
            ));
        }
        let current = self.inner.shared.current(c);
        let _ = pending
            .take()
            .unwrap()
            .reply
            .send(accepted && current.is_ok());
        current
    }
    pub fn status(&self) -> Value {
        let mut state = lock(&self.inner.shared.state).clone();
        state["retained"] = json!(JOBS - self.inner.slots.available_permits());
        state["closing"] = json!(self.inner.shared.closing.load(Ordering::Acquire));
        state["terminal"] = self
            .inner
            .shared
            .terminal_error()
            .map(|e| json!({"code":e.code,"message":e.message}))
            .unwrap_or(Value::Null);
        state
    }
    pub async fn context_changed(&self) -> Result<()> {
        let current = self
            .inner
            .shared
            .library
            .context()
            .map_err(|e| error("timecode_context", e))?;
        {
            let mut p = lock(&self.inner.shared.pending);
            if p.as_ref().is_some_and(|p| p.context != current) {
                p.take();
            }
        }
        let (reply, receive) = oneshot::channel();
        self.inner
            .control
            .try_send(Change { reply })
            .map_err(|_| error("timecode_busy", "Lifecycle slot is occupied"))?;
        tokio::time::timeout(self.inner.shared.config.lifecycle_timeout, receive)
            .await
            .map_err(|_| {
                error(
                    "timecode_retirement_timeout",
                    "Actual owners remain retained; retry retirement",
                )
            })?
            .map_err(|_| error("timecode_worker", "Retirement owner ended"))?
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.shared.closing.store(true, Ordering::Release);
        self.inner.shared.stop.cancel();
        lock(&self.inner.shared.pending).take();
        let before = self.inner.done.borrow().attempt;
        let (reply, _) = oneshot::channel();
        let _ = self.inner.control.try_send(Change { reply });
        let mut done = self.inner.done.clone();
        tokio::time::timeout(self.inner.shared.config.lifecycle_timeout, async {
            loop {
                let p = done.borrow().clone();
                if p.complete {
                    if let Some(worker) = self.inner.worker.lock().await.take() {
                        worker.await.map_err(|_| {
                            error("timecode_worker", "Timecode supervisor panicked")
                        })?;
                    }
                    return p.error.map_or(Ok(()), |(c, m)| Err(error(c, m)));
                }
                if p.attempt > before {
                    if let Some((c, m)) = p.error {
                        return Err(error(c, m));
                    }
                }
                done.changed().await.map_err(|_| {
                    error("timecode_worker", "Timecode supervisor ended without drain")
                })?;
            }
        })
        .await
        .map_err(|_| {
            error(
                "timecode_shutdown_timeout",
                "Actual timecode owners remain retained; retry shutdown",
            )
        })?
    }
}
fn validate_context(c: &RuntimeContext) -> Result<()> {
    if c.runtime_id.len() > 256
        || c.data_path.as_os_str().len() > 32768
        || c.data_path.to_str().is_none()
    {
        Err(error(
            "timecode_context",
            "Unsupported context path or size",
        ))
    } else {
        Ok(())
    }
}
fn bound_json(v: &Value) -> Result<()> {
    let mut stack = vec![(v, 0)];
    let (mut nodes, mut bytes) = (0usize, 0usize);
    while let Some((v, d)) = stack.pop() {
        nodes += 1;
        if nodes > 1024 || d > 8 {
            return Err(error("timecode_limit", "JSON depth/node bound"));
        }
        match v {
            Value::String(s) => bytes = bytes.saturating_add(s.len()),
            Value::Array(a) => {
                if a.len() > 1024usize.saturating_sub(nodes + stack.len()) {
                    return Err(error("timecode_limit", "JSON array bound"));
                }
                stack.extend(a.iter().map(|v| (v, d + 1)));
            }
            Value::Object(o) => {
                if o.len() > 1024usize.saturating_sub(nodes + stack.len()) {
                    return Err(error("timecode_limit", "JSON object bound"));
                }
                for (k, v) in o {
                    bytes = bytes.saturating_add(k.len());
                    stack.push((v, d + 1));
                }
            }
            _ => bytes = bytes.saturating_add(16),
        }
        if bytes > 65536 {
            return Err(error("timecode_limit", "JSON byte bound"));
        }
    }
    Ok(())
}
fn scalar(v: Option<&Value>) -> Result<route::Scalar> {
    Ok(match v {
        None => route::Scalar::Undefined,
        Some(Value::Null) => route::Scalar::Null,
        Some(Value::Bool(b)) => route::Scalar::Bool(*b),
        Some(Value::Number(n)) => route::Scalar::number(n.as_f64().unwrap()),
        Some(Value::String(s)) if s.len() <= 4096 => route::Scalar::Text(s.clone()),
        _ => {
            return Err(error(
                "timecode_unsupported_scalar",
                "Only bounded source primitive framerates are represented",
            ))
        }
    })
}
fn parse(r: &Request) -> Result<route::Command> {
    let d = r.data.get("data");
    Ok(
        match r.data.get("channel").and_then(Value::as_str).unwrap_or("") {
            "TIMECODE_START" => {
                let v = d
                    .filter(|v| v.is_object())
                    .ok_or_else(|| error("timecode_payload", "START requires original object"))?;
                route::Command::Start {
                    kind: v["type"]
                        .as_str()
                        .ok_or_else(|| error("timecode_payload", "START type must be a string"))?
                        .into(),
                    mode: v["mode"]
                        .as_str()
                        .ok_or_else(|| error("timecode_payload", "START mode must be a string"))?
                        .into(),
                    framerate: scalar(v.get("framerate"))?,
                    data: v
                        .get("data")
                        .cloned()
                        .map(route::Data::Json)
                        .unwrap_or(route::Data::Undefined),
                }
            }
            "TIMECODE_STOP" => route::Command::Stop,
            "TIMECODE_VALUE" => {
                route::Command::Value(Number::new(d.and_then(Value::as_f64).ok_or_else(|| {
                    error(
                        "timecode_payload",
                        "JSON VALUE requires a number; typed API carries special numbers",
                    )
                })?))
            }
            "TIMECODE_STATUS" => route::Command::Status(
                d.cloned()
                    .map(route::Data::Json)
                    .unwrap_or(route::Data::Undefined),
            ),
            "TIMECODE_AUDIO_DATA" => {
                return Err(error(
                    "timecode_raw_required",
                    "Use the bounded raw audio entry point",
                ))
            }
            _ => return Err(error("timecode_unsupported", "Unknown timecode command")),
        },
    )
}
fn validate_command(c: &route::Command) -> Result<()> {
    match c {
        route::Command::Start {
            kind,
            mode,
            framerate,
            data,
        } => {
            if kind.len() > 32
                || mode.len() > 4096
                || matches!(framerate,route::Scalar::Text(s)if s.len()>4096)
            {
                return Err(error("timecode_limit", "START field bound"));
            }
            if let route::Data::Json(v) = data {
                bound_json(v)?;
            }
        }
        route::Command::Status(route::Data::Json(v)) => bound_json(v)?,
        route::Command::Audio { mode, bytes }
            if mode.len() > 4096 || bytes.len() > route::MAX_AUDIO_BYTES =>
        {
            return Err(error("timecode_limit", "Raw AUDIO field bound"));
        }
        _ => (),
    }
    Ok(())
}

struct Slot {
    id: u64,
    owner: ports::Owner,
    service: ports::MidiService,
    events: ports::EventStream,
    input: bool,
    listener: Option<route::ListenerToken>,
    native_listener: Option<u64>,
    opening: bool,
    router_closed: bool,
    closed: bool,
    joined: bool,
    shutdown_error: Option<WireError>,
}
enum NativeReply {
    Midi(ports::Reply),
    Ltc(ltc::Reply),
    Closed { closed: bool, joined: bool },
}
struct NativeResult {
    closed: bool,
    reply: Result<NativeReply>,
    slot: Option<u64>,
}
enum Purpose {
    Open { listener: route::ListenerToken },
    Send { fresh: bool },
    Close,
    CloseSenders,
    Ltc { opening: bool },
    LtcClose,
}
struct Pending {
    token: route::EffectToken,
    purpose: Purpose,
    slot: Option<u64>,
    future: NativeFuture,
    cancel: CancellationToken,
    _lease: Option<Arc<OwnedSemaphorePermit>>,
}
struct Joining {
    slot: u64,
    future: Pin<Box<dyn Future<Output = Result<()>> + Send>>,
}
struct Codec {
    owner: ltc::Owner,
    service: ltc::LtcService,
}
struct Active {
    job: Job,
    observations: Vec<Observation>,
    error: Option<WireError>,
}
struct Notice {
    publication: NativeTimecodePublication,
    bytes: usize,
}
struct Waiting {
    id: String,
    context: RuntimeContext,
    reply: oneshot::Receiver<bool>,
    deadline: tokio::time::Instant,
}
struct Run {
    context: RuntimeContext,
    id: u64,
    router: route::Router,
    slots: Vec<Slot>,
    next_slot: u64,
    current_sender: Option<u64>,
    sync: Option<Pending>,
    asynchronous: Vec<Pending>,
    joining: Vec<Joining>,
    codec: Option<Codec>,
    codec_join: Option<Pin<Box<dyn Future<Output = Result<()>> + Send>>>,
    codec_joined: bool,
    retiring: bool,
    active: Option<Active>,
    effects: VecDeque<route::Intent>,
    notices: VecDeque<Notice>,
    notice_bytes: usize,
    waiting: Option<Waiting>,
    diagnostics: u64,
    last: Option<Failure>,
    stale: u64,
    published: u64,
    retire_error: Option<WireError>,
    retired_runs: u64,
    last_retired: Option<Value>,
}
impl Run {
    fn new(context: RuntimeContext, id: u64) -> Self {
        Self {
            context,
            id,
            router: route::Router::new(cfg!(target_os = "windows")),
            slots: vec![],
            next_slot: 1,
            current_sender: None,
            sync: None,
            asynchronous: vec![],
            joining: vec![],
            codec: None,
            codec_join: None,
            codec_joined: false,
            retiring: false,
            active: None,
            effects: VecDeque::new(),
            notices: VecDeque::new(),
            notice_bytes: 0,
            waiting: None,
            diagnostics: 0,
            last: None,
            stale: 0,
            published: 0,
            retire_error: None,
            retired_runs: 0,
            last_retired: None,
        }
    }
    fn diagnostic(&mut self, e: WireError) {
        self.diagnostics = self.diagnostics.saturating_add(1);
        self.last = Some((e.code, e.message.clone()));
        if let Some(active) = &mut self.active {
            if active.observations.len() < 32 {
                active.observations.push(Observation::Diagnostic {
                    code: e.code,
                    message: e.message.clone(),
                });
            }
            if matches!(
                e.code,
                "timecode_limit"
                    | "timecode_unsupported_fps"
                    | "ltc_unsupported"
                    | "ltc_resource_limit"
            ) {
                active
                    .error
                    .get_or_insert_with(|| error(e.code, &e.message));
            }
        }
        if self.retiring
            && !matches!(
                e.code,
                "midi_shutdown_timeout"
                    | "ltc_shutdown_timeout"
                    | "midi_shutdown"
                    | "midi_cancelled"
                    | "ltc_shutdown"
                    | "ltc_cancelled"
            )
        {
            self.retire_error.get_or_insert(e);
        }
    }
    fn step(&mut self, step: route::Step) {
        for e in step.errors {
            self.diagnostic(re(e));
        }
        self.effects.extend(step.effects);
    }
    fn snapshot(&self, shared: &Shared) {
        *lock(&shared.state) = json!({"run":self.id,"context":self.context,"router":self.router.snapshot(),"portOwners":self.slots.len(),"inputOwners":self.slots.iter().filter(|s|s.input).count(),"senderOwners":self.slots.iter().filter(|s|!s.input).count(),"asyncEffects":self.asynchronous.len(),"syncEffect":self.sync.is_some(),"retiring":self.retiring,"queuedPublications":self.notices.len(),"publicationBytes":self.notice_bytes,"pendingDelivery":self.waiting.is_some(),"published":self.published,"stale":self.stale,"diagnostics":self.diagnostics,"last":self.last.as_ref().map(|(code,message)|json!({"code":code,"message":message})),"ltc":self.codec.as_ref().map(|c|c.service.status()),"ltcJoined":self.codec_joined,"retiredRuns":self.retired_runs,"lastRetired":self.last_retired});
    }
    fn slot(&mut self, id: u64) -> Result<&mut Slot> {
        self.slots
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| error("timecode_state", "Owned native port slot missing"))
    }
    fn lease(&self) -> Option<Arc<OwnedSemaphorePermit>> {
        self.active.as_ref().map(|a| a.job.lease.clone())
    }
    fn allocate(
        &mut self,
        shared: &Shared,
        input: bool,
        listener: Option<route::ListenerToken>,
    ) -> Result<u64> {
        if self.slots.len() >= PORTS || self.slots.iter().filter(|s| s.input == input).count() >= 8
        {
            return Err(error(
                "timecode_limit",
                "Bounded native MTC owner pool is full",
            ));
        }
        let id = self.next_slot;
        self.next_slot = id
            .checked_add(1)
            .ok_or_else(|| error("timecode_limit", "Native slot IDs exhausted"))?;
        let owner = ports::Owner::new(
            &self.context.runtime_id,
            self.context.library_generation,
            self.context.data_path.to_str().unwrap(),
            self.id,
        )
        .map_err(pe)?;
        let backend = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (shared.factory)()))
            .map_err(|_| error("timecode_backend_panic", "Native backend factory panicked"))?
            .map_err(pe)?;
        let service = ports::MidiService::with_backend(
            owner.clone(),
            CancellationToken::new(),
            shared.config.ports.clone(),
            backend,
        )
        .map_err(pe)?;
        let events = service.events().map_err(pe)?;
        self.slots.push(Slot {
            id,
            owner,
            service,
            events,
            input,
            listener,
            native_listener: None,
            opening: true,
            router_closed: false,
            closed: false,
            joined: false,
            shutdown_error: None,
        });
        Ok(id)
    }
    fn port_operation(
        &mut self,
        shared: &Shared,
        id: u64,
        command: ports::Command,
    ) -> Result<(NativeFuture, CancellationToken)> {
        let slot = self.slot(id)?;
        let cancel = CancellationToken::new();
        let ticket = slot
            .service
            .begin(
                &slot.owner,
                command,
                cancel.clone(),
                Instant::now() + shared.config.operation_timeout,
            )
            .map_err(pe)?;
        Ok((
            Box::pin(async move {
                NativeResult {
                    closed: false,
                    reply: ticket.finish().await.map(NativeReply::Midi).map_err(pe),
                    slot: Some(id),
                }
            }),
            cancel,
        ))
    }
    fn close_operation(
        &mut self,
        shared: &Shared,
        id: u64,
        terminal: bool,
    ) -> Result<(NativeFuture, CancellationToken)> {
        let slot = self.slot(id)?;
        if slot.joined {
            let result = slot
                .shutdown_error
                .as_ref()
                .map(|e| Err(error(e.code, &e.message)))
                .unwrap_or(Ok(NativeReply::Closed {
                    closed: true,
                    joined: true,
                }));
            return Ok((
                Box::pin(async move {
                    NativeResult {
                        reply: result,
                        slot: Some(id),
                        closed: true,
                    }
                }),
                CancellationToken::new(),
            ));
        }
        let service = slot.service.clone();
        let cancel = CancellationToken::new();
        let mut closed = slot.closed;
        let ticket = if closed || terminal {
            None
        } else {
            let command = if slot.input {
                ports::Command::MtcClose(
                    slot.native_listener
                        .ok_or_else(|| error("timecode_state", "Input open has not settled"))?,
                )
            } else {
                ports::Command::MtcStop
            };
            Some(
                service
                    .begin(
                        &slot.owner,
                        command,
                        cancel.clone(),
                        Instant::now() + shared.config.operation_timeout,
                    )
                    .map_err(pe)?,
            )
        };
        Ok((
            Box::pin(async move {
                let reply = async {
                    if let Some(ticket) = ticket {
                        match ticket.finish().await.map_err(pe)? {
                            ports::Reply::Done => closed = true,
                            _ => {
                                return Err(error(
                                    "timecode_receipt",
                                    "Unexpected real close receipt",
                                ))
                            }
                        }
                    }
                    service.shutdown().await.map_err(pe)?;
                    closed = true;
                    Ok(NativeReply::Closed {
                        closed: true,
                        joined: true,
                    })
                }
                .await;
                NativeResult {
                    reply,
                    slot: Some(id),
                    closed,
                }
            }),
            cancel,
        ))
    }
    fn ltc_operation(
        &mut self,
        shared: &Shared,
        operation: ltc::Operation,
    ) -> Result<(NativeFuture, CancellationToken)> {
        if self.codec.is_none() {
            let owner = ltc::Owner::new(
                &self.context.runtime_id,
                self.context.library_generation,
                self.context.data_path.to_str().unwrap(),
            )
            .map_err(le)?;
            let service = ltc::LtcService::start(
                owner.clone(),
                shared.library_source.clone(),
                shared.config.ltc.clone(),
            )
            .map_err(le)?;
            self.codec = Some(Codec { owner, service });
            self.codec_joined = false;
        }
        let codec = self.codec.as_ref().unwrap();
        let ticket = codec
            .service
            .begin(
                &codec.owner,
                operation,
                Instant::now() + shared.config.operation_timeout,
            )
            .map_err(le)?;
        Ok((
            Box::pin(async move {
                NativeResult {
                    closed: false,
                    reply: ticket
                        .finish()
                        .await
                        .and_then(|r| r.into_reply())
                        .map(NativeReply::Ltc)
                        .map_err(le),
                    slot: None,
                }
            }),
            CancellationToken::new(),
        ))
    }
    fn retired_codec_close(&self) -> (NativeFuture, CancellationToken) {
        let service = self.codec.as_ref().map(|c| c.service.clone());
        let future: NativeFuture = Box::pin(async move {
            let result = match service {
                Some(service) => service.shutdown().await.map_err(le),
                None => Ok(()),
            };
            NativeResult {
                reply: result.map(|_| NativeReply::Ltc(ltc::Reply::Done)),
                slot: None,
                closed: false,
            }
        });
        (future, CancellationToken::new())
    }
    fn selector(data: &route::Data, field: &str) -> Result<ports::Selector> {
        match data {
            route::Data::Undefined | route::Data::Json(Value::Null) => Err(error(
                "timecode_payload",
                "Cannot read selected MIDI property of null/undefined",
            )),
            route::Data::Json(v) => ports::Selector::from_field(v.get(field)).map_err(pe),
        }
    }
    fn fps(fps: &route::Scalar, ltc: bool) -> Result<f64> {
        let route::Scalar::Number(n) = fps else {
            return Err(error(
                "timecode_unsupported_fps",
                "Native timecode IO supports verified numeric framerates only",
            ));
        };
        let n = n.value();
        if !n.is_finite() || ltc && ![24.0, 25.0, 29.97, 30.0].contains(&n) {
            return Err(error(
                "timecode_unsupported_fps",
                "Unverified native framerate",
            ));
        }
        Ok(n)
    }
    fn preflight(&self, command: &route::Command) -> Result<()> {
        let snapshot = self.router.snapshot();
        match command {
            route::Command::Start {
                kind,
                mode,
                framerate,
                ..
            } if kind == "receive" && mode == "LTC" => {
                let fps = match framerate {
                    route::Scalar::Undefined | route::Scalar::Null | route::Scalar::Bool(false) => {
                        snapshot.framerate
                    }
                    route::Scalar::Number(n) if n.value() == 0.0 || n.value().is_nan() => {
                        snapshot.framerate
                    }
                    route::Scalar::Text(s) if s.is_empty() => snapshot.framerate,
                    _ => framerate.clone(),
                };
                Self::fps(&fps, true)?;
            }
            route::Command::Value(time) => match snapshot.current_mode.as_deref() {
                Some("LTC") => {
                    Self::fps(&snapshot.framerate, true)?;
                    if !time.value().is_finite() || time.value() < 0.0 {
                        return Err(error(
                            "timecode_unsupported_value",
                            "LTC encode supports finite nonnegative time only",
                        ));
                    }
                }
                Some("MTC") => {
                    Self::fps(&snapshot.framerate, false)?;
                }
                _ => (),
            },
            _ => (),
        }
        Ok(())
    }
    fn finish_job(&mut self) {
        if self.sync.is_none() && self.effects.is_empty() {
            if let Some(active) = self.active.take() {
                let result = match active.error {
                    Some(e) => Err(e),
                    None => Ok(TimecodeReceipt {
                        context: active.job.context,
                        observations: active.observations,
                        _lease: active.job.lease,
                    }),
                };
                let _ = active.job.reply.send(result);
            }
        }
    }
    fn begin_join(&mut self, id: u64) {
        if self.joining.iter().any(|j| j.slot == id) {
            return;
        }
        if let Some(slot) = self.slots.iter().find(|s| s.id == id && !s.joined) {
            let service = slot.service.clone();
            self.joining.push(Joining {
                slot: id,
                future: Box::pin(async move { service.shutdown().await.map_err(pe) }),
            });
        }
    }
    fn retire(&mut self) {
        self.retiring = true;
        self.waiting.take();
        self.notices.clear();
        self.notice_bytes = 0;
        let step = self.router.retire();
        match step {
            Ok(mut s) => {
                s.effects.retain(|intent| {
                    if let route::Effect::Cancel { operation } = &intent.effect {
                        if let Some(p) = self
                            .sync
                            .as_ref()
                            .filter(|p| p.token.serial() == operation.serial())
                        {
                            p.cancel.cancel();
                        }
                        for p in self
                            .asynchronous
                            .iter()
                            .filter(|p| p.token.serial() == operation.serial())
                        {
                            p.cancel.cancel();
                        }
                        false
                    } else {
                        true
                    }
                });
                self.step(s)
            }
            Err(e) => self.diagnostic(re(e)),
        }
        let ids = self.slots.iter().map(|s| s.id).collect::<Vec<_>>();
        for id in ids {
            self.begin_join(id);
        }
    }
}
async fn next_pending(operations: &mut Vec<Pending>) -> (Pending, NativeResult) {
    poll_fn(|cx| {
        for i in 0..operations.len() {
            if let Poll::Ready(out) = operations[i].future.as_mut().poll(cx) {
                return Poll::Ready((operations.remove(i), out));
            }
        }
        Poll::Pending
    })
    .await
}
async fn next_sync(operation: &mut Option<Pending>) -> (Pending, NativeResult) {
    poll_fn(|cx| {
        let Some(p) = operation.as_mut() else {
            return Poll::Pending;
        };
        match p.future.as_mut().poll(cx) {
            Poll::Ready(out) => Poll::Ready((operation.take().unwrap(), out)),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}
async fn next_join(operations: &mut Vec<Joining>) -> (u64, Result<()>) {
    poll_fn(|cx| {
        for i in 0..operations.len() {
            if let Poll::Ready(out) = operations[i].future.as_mut().poll(cx) {
                return Poll::Ready((operations.remove(i).slot, out));
            }
        }
        Poll::Pending
    })
    .await
}
async fn next_port(slots: &mut [Slot]) -> (u64, ports::Result<ports::Event>) {
    poll_fn(|cx| {
        for slot in slots
            .iter_mut()
            .filter(|s| s.input && s.native_listener.is_some() && !s.closed && !s.joined)
        {
            let future = slot.events.recv();
            tokio::pin!(future);
            if let Poll::Ready(out) = future.poll(cx) {
                return Poll::Ready((slot.id, out));
            }
        }
        Poll::Pending
    })
    .await
}

impl Run {
    fn enqueue_notice(&mut self, shared: &Shared, effect: route::Effect) -> Result<()> {
        if self.retiring || shared.current(&self.context).is_err() {
            self.stale = self.stale.saturating_add(1);
            return Ok(());
        }
        let mut hydration = Hydration::default();
        let mut bytes = None;
        let (channel, data) = match effect {
            route::Effect::Value { value } => {
                let n = value.value();
                if n.is_finite() && !(n == 0.0 && n.is_sign_negative()) {
                    ("TIMECODE_VALUE", json!(n))
                } else {
                    hydration.numbers.push(NumberField {
                        path: String::new(),
                        number: value,
                    });
                    ("TIMECODE_VALUE", Value::Null)
                }
            }
            route::Effect::Status { status } => (
                "TIMECODE_STATUS",
                serde_json::to_value(status).map_err(|e| error("timecode_wire", e))?,
            ),
            route::Effect::Audio { bytes: buffer } => {
                bytes = Some(buffer);
                ("TIMECODE_AUDIO_DATA", Value::Null)
            }
            route::Effect::Alert { message } => ("ALERT", json!(message)),
            _ => return Err(error("timecode_state", "Not a source notification")),
        };
        let mut event = Event::reply("MAIN", channel, data, None);
        event.context = Some(self.context.clone());
        let publication = NativeTimecodePublication {
            event,
            hydration,
            bytes,
            delivery: NativeTimecodeDelivery {
                id: format!("timecode-{}", uuid::Uuid::new_v4()),
                context: self.context.clone(),
                run: self.id,
            },
        };
        let size = wire_size(&publication)?;
        if self.notices.len() >= PUBLICATIONS
            || size > PUBLICATION_BYTES.saturating_sub(self.notice_bytes)
        {
            return Err(error(
                "timecode_publication_limit",
                "Bounded source notification queue is full",
            ));
        }
        self.notice_bytes += size;
        self.notices.push_back(Notice {
            publication,
            bytes: size,
        });
        Ok(())
    }
    fn publish(&mut self, shared: &Shared) -> Result<()> {
        if self.waiting.is_some() {
            return Ok(());
        }
        while let Some(notice) = self.notices.pop_front() {
            self.notice_bytes -= notice.bytes;
            let c = &notice.publication.delivery.context;
            if self.retiring || shared.current(c).is_err() {
                self.stale = self.stale.saturating_add(1);
                continue;
            }
            let id = notice.publication.delivery.id.clone();
            let context = c.clone();
            let (reply, receive) = oneshot::channel();
            {
                let mut pending = lock(&shared.pending);
                if pending.is_some() {
                    return Err(error("timecode_completion", "Publication already pending"));
                }
                *pending = Some(Delivery {
                    context: context.clone(),
                    id: id.clone(),
                    reply,
                });
            }
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (shared.publisher)(notice.publication)
            }))
            .map_err(|_| {
                error(
                    "timecode_publisher_panic",
                    "Trusted native publication sink panicked",
                )
            })?
            .map_err(|e| error("timecode_publisher", e))?;
            self.waiting = Some(Waiting {
                id,
                context,
                reply: receive,
                deadline: tokio::time::Instant::now() + shared.config.delivery_timeout,
            });
            break;
        }
        Ok(())
    }
    fn effects(&mut self, shared: &Shared) -> Result<()> {
        // A real synchronous source continuation blocks source commands, callbacks and
        // async source completions. Native lifecycle controls/joins remain pollable.
        while self.sync.is_none() {
            let Some(intent) = self.effects.pop_front() else {
                break;
            };
            let Some(token) = intent.token else {
                match intent.effect {
                    route::Effect::LogStatus { data } => {
                        if let Some(a) = &mut self.active {
                            a.observations.push(Observation::LogStatus { data });
                        }
                    }
                    route::Effect::Cancel { .. } => (),
                    e => self.enqueue_notice(shared, e)?,
                }
                continue;
            };
            let serial = token.serial();
            let lease = self.lease();
            let first_new_slot = self.next_slot;
            let rejected_constructor = matches!(
                intent.effect,
                route::Effect::CreateDecoder { .. } | route::Effect::CreateEncoder { .. }
            );
            let issued: Result<(Purpose, Option<u64>, NativeFuture, CancellationToken, bool)> =
                match intent.effect {
                    route::Effect::OpenMtc { data, listener, .. } => (|| {
                        let selector = Self::selector(&data, "midiInput")?;
                        let id = self.allocate(shared, true, Some(listener.clone()))?;
                        let (f, c) =
                            self.port_operation(shared, id, ports::Command::MtcListen(selector))?;
                        Ok((Purpose::Open { listener }, Some(id), f, c, false))
                    })(),
                    route::Effect::SendMtc {
                        time,
                        framerate,
                        data,
                    } => (|| {
                        let fps = Self::fps(&framerate, false)?;
                        let fresh = self.current_sender.is_none();
                        let (id, selector) = match self.current_sender {
                            Some(id) => (id, ports::Selector::Undefined),
                            None => {
                                let selector = Self::selector(&data, "midiOutput")?;
                                (self.allocate(shared, false, None)?, selector)
                            }
                        };
                        let (f, c) = self.port_operation(
                            shared,
                            id,
                            ports::Command::MtcSend {
                                selector,
                                time_ms: time.value(),
                                framerate: fps,
                            },
                        )?;
                        Ok((Purpose::Send { fresh }, Some(id), f, c, !fresh))
                    })(),
                    route::Effect::StopMtc => {
                        if self.retiring {
                            let ids = self
                                .slots
                                .iter()
                                .filter(|s| !s.input && !s.opening)
                                .map(|s| s.id)
                                .collect::<Vec<_>>();
                            let services = ids
                                .iter()
                                .map(|id| {
                                    self.slots
                                        .iter()
                                        .find(|s| s.id == *id)
                                        .unwrap()
                                        .service
                                        .clone()
                                })
                                .collect::<Vec<_>>();
                            let f: NativeFuture = Box::pin(async move {
                                let mut first = None;
                                for service in services {
                                    if let Err(e) = service.shutdown().await {
                                        first.get_or_insert_with(|| pe(e));
                                    }
                                }
                                NativeResult {
                                    closed: false,
                                    reply: first.map_or(
                                        Ok(NativeReply::Closed {
                                            closed: true,
                                            joined: true,
                                        }),
                                        Err,
                                    ),
                                    slot: None,
                                }
                            });
                            Ok((
                                Purpose::CloseSenders,
                                None,
                                f,
                                CancellationToken::new(),
                                false,
                            ))
                        } else if let Some(id) = self.current_sender {
                            self.close_operation(shared, id, false)
                                .map(|(f, c)| (Purpose::Close, Some(id), f, c, true))
                        } else {
                            let f: NativeFuture = Box::pin(async {
                                NativeResult {
                                    closed: false,
                                    reply: Ok(NativeReply::Closed {
                                        closed: true,
                                        joined: true,
                                    }),
                                    slot: None,
                                }
                            });
                            Ok((Purpose::Close, None, f, CancellationToken::new(), true))
                        }
                    }
                    route::Effect::CloseListener { listener, mode } => {
                        if mode == "LTC" {
                            self.ltc_operation(shared, ltc::Operation::StopDecoder)
                                .map(|(f, c)| (Purpose::Ltc { opening: false }, None, f, c, true))
                        } else {
                            (|| {
                                let id = self
                                    .slots
                                    .iter()
                                    .find(|s| {
                                        s.listener
                                            .as_ref()
                                            .is_some_and(|l| l.serial() == listener.serial())
                                    })
                                    .map(|s| s.id)
                                    .ok_or_else(|| {
                                        error(
                                            "timecode_state",
                                            "Accepted source listener has no native owner",
                                        )
                                    })?;
                                self.close_operation(shared, id, self.retiring)
                                    .map(|(f, c)| (Purpose::Close, Some(id), f, c, !self.retiring))
                            })()
                        }
                    }
                    route::Effect::CreateDecoder { framerate, .. } => Self::fps(&framerate, true)
                        .and_then(|fps| {
                            self.ltc_operation(shared, ltc::Operation::CreateDecoder { fps })
                        })
                        .map(|(f, c)| (Purpose::Ltc { opening: true }, None, f, c, true)),
                    route::Effect::CreateEncoder { framerate, .. } => Self::fps(&framerate, true)
                        .and_then(|fps| {
                            self.ltc_operation(shared, ltc::Operation::CreateEncoder { fps })
                        })
                        .map(|(f, c)| (Purpose::Ltc { opening: true }, None, f, c, true)),
                    route::Effect::Encode { frame } => self
                        .ltc_operation(
                            shared,
                            ltc::Operation::Encode {
                                hours: frame.hours.value(),
                                minutes: frame.minutes.value(),
                                seconds: frame.seconds.value(),
                                frames: frame.frames.value(),
                            },
                        )
                        .map(|(f, c)| (Purpose::Ltc { opening: false }, None, f, c, true)),
                    route::Effect::Decode { bytes } => self
                        .ltc_operation(shared, ltc::Operation::Decode { bytes })
                        .map(|(f, c)| (Purpose::Ltc { opening: false }, None, f, c, true)),
                    route::Effect::StopEncoder => {
                        if self.retiring {
                            let (f, c) = self.retired_codec_close();
                            Ok((Purpose::LtcClose, None, f, c, true))
                        } else if self.codec_joined || !self.router.snapshot().encoder {
                            let f: NativeFuture = Box::pin(async {
                                NativeResult {
                                    closed: false,
                                    reply: Ok(NativeReply::Ltc(ltc::Reply::Done)),
                                    slot: None,
                                }
                            });
                            Ok((
                                Purpose::Ltc { opening: false },
                                None,
                                f,
                                CancellationToken::new(),
                                !self.retiring,
                            ))
                        } else {
                            self.ltc_operation(shared, ltc::Operation::StopEncoder)
                                .map(|(f, c)| {
                                    (Purpose::Ltc { opening: false }, None, f, c, !self.retiring)
                                })
                        }
                    }
                    _ => Err(error(
                        "timecode_state",
                        "Unexpected token-bearing source effect",
                    )),
                };
            match issued {
                Ok((purpose, slot, future, cancel, synchronous)) => {
                    if let Some(active) = &mut self.active {
                        active
                            .observations
                            .push(Observation::Scheduled { operation: serial });
                    }
                    let pending = Pending {
                        token,
                        purpose,
                        slot,
                        future,
                        cancel,
                        _lease: lease,
                    };
                    if synchronous {
                        self.sync = Some(pending);
                    } else {
                        if self.asynchronous.len() >= route::MAX_PENDING {
                            return Err(error(
                                "timecode_limit",
                                "Host asynchronous effect set exceeded Router slots",
                            ));
                        }
                        self.asynchronous.push(pending);
                    }
                }
                Err(e) => {
                    let unadmitted = self
                        .slots
                        .iter_mut()
                        .filter(|s| s.id >= first_new_slot)
                        .map(|s| {
                            s.opening = false;
                            s.router_closed = true;
                            s.id
                        })
                        .collect::<Vec<_>>();
                    for id in unadmitted {
                        self.begin_join(id);
                    }
                    if rejected_constructor {
                        self.retire();
                    }
                    let completion = if rejected_constructor {
                        route::Completion::Abandoned(routed(&e))
                    } else {
                        route::Completion::Failed(routed(&e))
                    };
                    let next = self.router.complete(&token, completion).map_err(re)?;
                    self.step(next);
                }
            }
        }
        self.finish_job();
        Ok(())
    }
    fn settled(&mut self, pending: Pending, result: NativeResult) -> Result<()> {
        let id = pending.slot.or(result.slot);
        if result.closed {
            if let Some(id) = id {
                self.slot(id)?.closed = true;
            }
        }
        let completion = match result.reply {
            Ok(NativeReply::Midi(ports::Reply::MtcListener(listener))) => {
                let Purpose::Open { listener: source } = pending.purpose else {
                    return Err(error("timecode_receipt", "Wrong open receipt"));
                };
                let slot = self.slot(id.unwrap())?;
                slot.native_listener = Some(listener);
                slot.opening = false;
                debug_assert_eq!(slot.listener.as_ref().unwrap().serial(), source.serial());
                self.router.accept_listener(&pending.token).map_err(re)?;
                route::Completion::Opened
            }
            Ok(NativeReply::Midi(ports::Reply::Done)) => {
                if let Purpose::Send { fresh } = pending.purpose {
                    let id = id.unwrap();
                    self.slot(id)?.opening = false;
                    self.router.accept_sender(&pending.token).map_err(re)?;
                    if fresh {
                        self.current_sender = Some(id);
                    }
                }
                route::Completion::Done
            }
            Ok(NativeReply::Closed { closed, joined }) => {
                match pending.purpose {
                    Purpose::CloseSenders => {
                        for slot in self.slots.iter_mut().filter(|s| !s.input && !s.opening) {
                            slot.closed = closed;
                            slot.joined = joined;
                            slot.router_closed = true;
                        }
                        self.current_sender = None;
                    }
                    _ => {
                        if let Some(id) = id {
                            let slot = self.slot(id)?;
                            slot.closed = closed;
                            slot.joined = joined;
                            slot.router_closed = true;
                            if self.current_sender == Some(id) {
                                self.current_sender = None;
                            }
                        }
                    }
                }
                route::Completion::Done
            }
            Ok(NativeReply::Ltc(reply)) => match reply {
                ltc::Reply::Done => {
                    if matches!(pending.purpose, Purpose::LtcClose) {
                        self.codec_joined = true;
                    }
                    if matches!(pending.purpose, Purpose::Ltc { opening: true }) {
                        route::Completion::Opened
                    } else {
                        route::Completion::Done
                    }
                }
                ltc::Reply::Audio(bytes) => route::Completion::Encoded(bytes),
                ltc::Reply::Frame(frame) => route::Completion::Decoded(frame.map(|f| {
                    route::LtcFrame::new(
                        f.hours as f64,
                        f.minutes as f64,
                        f.seconds as f64,
                        f.frames as f64,
                    )
                })),
                ltc::Reply::Capability(_) => {
                    return Err(error("timecode_receipt", "Unexpected capability receipt"))
                }
            },
            Ok(_) => return Err(error("timecode_receipt", "Unexpected real MIDI receipt")),
            Err(e) => {
                if matches!(pending.purpose, Purpose::Ltc { opening: true })
                    && matches!(
                        e.code,
                        "ltc_resource_limit" | "ltc_cancelled" | "ltc_busy" | "ltc_unsupported"
                    )
                {
                    self.retire();
                    let step = self
                        .router
                        .complete(&pending.token, route::Completion::Abandoned(routed(&e)))
                        .map_err(re)?;
                    self.step(step);
                    return Ok(());
                }
                if let Some(id) = id {
                    let slot = self.slot(id)?;
                    slot.opening = false;
                    match pending.purpose {
                        Purpose::Open { .. } => {
                            slot.router_closed = true;
                            self.begin_join(id);
                        }
                        Purpose::Send { fresh } => {
                            if slot.service.status().outputs > 0 {
                                self.router.accept_sender(&pending.token).map_err(re)?;
                                if fresh {
                                    self.current_sender = Some(id);
                                }
                            } else {
                                slot.router_closed = true;
                                self.begin_join(id);
                            }
                        }
                        _ => (),
                    }
                }
                route::Completion::Failed(routed(&e))
            }
        };
        let step = self
            .router
            .complete(&pending.token, completion)
            .map_err(re)?;
        self.step(step);
        self.slots.retain(|s| !(s.router_closed && s.joined));
        Ok(())
    }
}
fn wire_size<T: Serialize>(value: &T) -> Result<usize> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if b.len() > (512 * 1024usize).saturating_sub(self.0) {
                return Err(std::io::Error::other("Timecode publication bound"));
            }
            self.0 += b.len();
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut c = Counter(0);
    serde_json::to_writer(&mut c, value).map_err(|e| error("timecode_wire_limit", e))?;
    Ok(c.0)
}

async fn delivery_done(waiting: &mut Option<Waiting>) -> Result<bool> {
    let Some(waiting) = waiting.as_mut() else {
        std::future::pending::<()>().await;
        unreachable!()
    };
    tokio::time::timeout_at(waiting.deadline, &mut waiting.reply)
        .await
        .map_err(|_| {
            error(
                "timecode_delivery_timeout",
                "Real main-handler completion exceeded bound",
            )
        })?
        .map_err(|_| {
            error(
                "timecode_delivery_lost",
                "Private source notification completion was lost",
            )
        })
}
async fn codec_done(
    join: &mut Option<Pin<Box<dyn Future<Output = Result<()>> + Send>>>,
) -> Result<()> {
    match join {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}
enum Wake {
    Stop,
    Change(Option<Change>),
    Job(Option<Job>),
    Sync(Pending, NativeResult),
    Async(Pending, NativeResult),
    Joined(u64, Result<()>),
    CodecJoined(Result<()>),
    Port(u64, ports::Result<ports::Event>),
    Delivery(Result<bool>),
}
async fn supervise(
    shared: Arc<Shared>,
    mut jobs: mpsc::Receiver<Job>,
    mut changes: mpsc::Receiver<Change>,
    progress: watch::Sender<Progress>,
    context: RuntimeContext,
) {
    let mut run = Run::new(context, 1);
    let mut change: Option<Change> = None;
    let mut changes_open = true;
    let mut jobs_open = true;
    let mut attempt = 0u64;
    loop {
        if let Err(e) = run.effects(&shared) {
            shared.fail(error(e.code, &e.message));
            shared.closing.store(true, Ordering::Release);
            run.diagnostic(e);
            run.retire();
        }
        if run.retiring {
            lock(&shared.pending).take();
        }
        if let Err(e) = run.publish(&shared) {
            shared.fail(error(e.code, &e.message));
            shared.closing.store(true, Ordering::Release);
            lock(&shared.pending).take();
            run.diagnostic(e);
            run.retire();
        }
        // No synchronous codec receipt is invalidated by premature service shutdown.
        // Native port joins are independent and already run during a blocked codec call.
        if run.retiring
            && run.router.drained()
            && run.sync.is_none()
            && run.asynchronous.is_empty()
            && run.codec_join.is_none()
            && !run.codec_joined
        {
            if let Some(codec) = &run.codec {
                let service = codec.service.clone();
                run.codec_join = Some(Box::pin(
                    async move { service.shutdown().await.map_err(le) },
                ));
            } else {
                run.codec_joined = true;
            }
        }
        run.slots.retain(|s| !(s.router_closed && s.joined));
        let actual_drained = run.retiring
            && run.router.drained()
            && run.sync.is_none()
            && run.asynchronous.is_empty()
            && run.slots.iter().all(|s| s.joined)
            && run.joining.is_empty()
            && run.codec_joined
            && run.codec_join.is_none();
        if actual_drained {
            run.retired_runs = run.retired_runs.saturating_add(1);
            run.last_retired = Some(
                json!({"run":run.id,"context":run.context,"portOwners":run.slots.len(),"allPortOwnersJoined":run.slots.iter().all(|s|s.joined),"ltcJoined":run.codec_joined,"ltc":run.codec.as_ref().map(|c|c.service.status())}),
            );
            attempt = attempt.saturating_add(1);
            if shared.closing.load(Ordering::Acquire) {
                while let Ok(job) = jobs.try_recv() {
                    let _ = job.reply.send(Err(error(
                        "timecode_shutdown",
                        "Timecode terminal drain rejected queued source command",
                    )));
                }
                let first = shared.terminal_error().or_else(|| run.retire_error.take());
                progress.send_replace(Progress {
                    attempt,
                    complete: true,
                    error: first.map(|e| (e.code, e.message)),
                });
                if let Some(change) = change.take() {
                    let _ = change.reply.send(Ok(()));
                }
                run.snapshot(&shared);
                break;
            }
            let result: Result<()> = (|| {
                let context = shared
                    .library
                    .context()
                    .map_err(|e| error("timecode_context", e))?;
                validate_context(&context)?;
                run.router.renew_after_drain().map_err(re)?;
                run.context = context;
                run.id = shared
                    .next_run
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |id| {
                        (id < 9007199254740991).then_some(id + 1)
                    })
                    .map_err(|_| error("timecode_limit", "Timecode run IDs exhausted"))?
                    + 1;
                run.codec.take();
                run.codec_joined = false;
                run.slots.clear();
                run.current_sender = None;
                run.retiring = false;
                run.retire_error = None;
                Ok(())
            })();
            if let Some(change) = change.take() {
                let _ = change.reply.send(
                    result
                        .as_ref()
                        .map(|_| ())
                        .map_err(|e| error(e.code, &e.message)),
                );
            }
            if let Err(e) = result {
                shared.fail(e);
                shared.closing.store(true, Ordering::Release);
            }
            progress.send_replace(Progress {
                attempt,
                complete: false,
                error: shared.terminal_error().map(|e| (e.code, e.message)),
            });
        } else if run.retiring
            && run.sync.is_none()
            && run.asynchronous.is_empty()
            && run.effects.is_empty()
            && run.joining.is_empty()
            && run.codec_join.is_none()
        {
            if let Some(e) = run.retire_error.as_ref() {
                attempt = attempt.saturating_add(1);
                progress.send_replace(Progress {
                    attempt,
                    complete: false,
                    error: Some((e.code, e.message.clone())),
                });
                if let Some(change) = change.take() {
                    let _ = change.reply.send(Err(error(e.code, &e.message)));
                }
            }
        }
        run.snapshot(&shared);
        let source_ready = run.sync.is_none()
            && !run.retiring
            && run.notices.len() <= PUBLICATIONS - 2
            && run.notice_bytes < PUBLICATION_BYTES - 128 * 1024;
        let wake = tokio::select! {biased;
         _=shared.stop.cancelled(),if !run.retiring=>Wake::Stop,
         control=changes.recv(),if changes_open=>Wake::Change(control),
         result=next_sync(&mut run.sync),if run.sync.is_some()=>Wake::Sync(result.0,result.1),
         result=next_join(&mut run.joining),if !run.joining.is_empty()=>Wake::Joined(result.0,result.1),
         result=codec_done(&mut run.codec_join),if run.codec_join.is_some()=>Wake::CodecJoined(result),
         result=delivery_done(&mut run.waiting),if run.waiting.is_some()=>Wake::Delivery(result),
         result=next_pending(&mut run.asynchronous),if run.sync.is_none()&&!run.asynchronous.is_empty()=>Wake::Async(result.0,result.1),
         job=jobs.recv(),if source_ready&&run.active.is_none()&&jobs_open=>Wake::Job(job),
         result=next_port(&mut run.slots),if source_ready=>Wake::Port(result.0,result.1),
        };
        match wake {
            Wake::Stop => {
                lock(&shared.pending).take();
                run.retire();
            }
            Wake::Change(None) => {
                changes_open = false;
                if !jobs_open && !run.retiring {
                    shared.closing.store(true, Ordering::Release);
                    run.retire();
                }
            }
            Wake::Change(Some(next)) => {
                if change.as_ref().is_some_and(|c| !c.reply.is_closed()) {
                    let _ = next.reply.send(Err(error(
                        "timecode_busy",
                        "Retirement observer is already retained",
                    )));
                } else {
                    change = Some(next);
                    let current = shared.library.context();
                    if shared.closing.load(Ordering::Acquire)
                        || run.retiring
                        || current.as_ref().map_or(true, |c| *c != run.context)
                    {
                        lock(&shared.pending).take();
                        run.retire();
                        if run.codec_join.is_none() && !run.codec_joined && run.router.drained() {
                            if let Some(codec) = &run.codec {
                                let service = codec.service.clone();
                                run.codec_join =
                                    Some(Box::pin(
                                        async move { service.shutdown().await.map_err(le) },
                                    ));
                            }
                        }
                    } else if let Some(change) = change.take() {
                        let _ = change.reply.send(Ok(()));
                    }
                }
            }
            Wake::Job(None) => {
                jobs_open = false;
                shared.closing.store(true, Ordering::Release);
                run.retire();
            }
            Wake::Job(Some(job)) => {
                let check = shared.current(&job.context).and_then(|_| {
                    if job.cancel.is_cancelled() {
                        Err(error("cancelled", "Timecode canceled before source call"))
                    } else {
                        run.preflight(&job.command)
                    }
                });
                match check {
                    Err(e) => {
                        let _ = job.reply.send(Err(e));
                    }
                    Ok(()) => {
                        if job.context != run.context {
                            let _ = job.reply.send(Err(error(
                                "timecode_retirement_required",
                                "Retire obsolete native owners before dispatching the new context",
                            )));
                            run.retire();
                        } else {
                            let step = run.router.begin(job.command.clone()).map_err(re);
                            run.active = Some(Active {
                                job,
                                observations: vec![],
                                error: None,
                            });
                            match step {
                                Ok(step) => run.step(step),
                                Err(e) => {
                                    if let Some(active) = run.active.take() {
                                        let _ = active.job.reply.send(Err(e));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Wake::Sync(pending, result) | Wake::Async(pending, result) => {
                if let Err(e) = run.settled(pending, result) {
                    shared.fail(error(e.code, &e.message));
                    shared.closing.store(true, Ordering::Release);
                    run.diagnostic(e);
                    run.retire();
                }
            }
            Wake::Joined(id, result) => {
                if let Ok(slot) = run.slot(id) {
                    match result {
                        Ok(()) => {
                            slot.joined = true;
                            slot.closed = true;
                        }
                        Err(e) => {
                            slot.joined = slot.service.status().worker_finished;
                            if slot.joined {
                                slot.shutdown_error = Some(error(e.code, &e.message));
                            }
                            run.diagnostic(e);
                        }
                    }
                }
            }
            Wake::CodecJoined(result) => {
                run.codec_join.take();
                match result {
                    Ok(()) => run.codec_joined = true,
                    Err(e) => {
                        run.diagnostic(e);
                    }
                }
            }
            Wake::Delivery(result) => {
                let waiting = run.waiting.take().unwrap();
                {
                    let mut pending = lock(&shared.pending);
                    if pending.as_ref().is_some_and(|p| p.id == waiting.id) {
                        pending.take();
                    }
                }
                if shared.current(&waiting.context).is_err() || run.retiring {
                    run.stale = run.stale.saturating_add(1);
                } else {
                    match result {
                        Ok(true) => run.published = run.published.saturating_add(1),
                        Ok(false) => {
                            let e = error(
                                "timecode_delivery_rejected",
                                "Current main handler rejected source notification",
                            );
                            shared.fail(error(e.code, &e.message));
                            shared.closing.store(true, Ordering::Release);
                            run.diagnostic(e);
                            run.retire();
                        }
                        Err(e) => {
                            shared.fail(error(e.code, &e.message));
                            shared.closing.store(true, Ordering::Release);
                            run.diagnostic(e);
                            run.retire();
                        }
                    }
                }
            }
            Wake::Port(id, result) => match result {
                Ok(ports::Event::Mtc {
                    owner,
                    listener,
                    bytes,
                    received_unix_ms,
                    ..
                }) => {
                    let slot = run.slots.iter().find(|s| s.id == id).unwrap();
                    if owner != slot.owner
                        || Some(listener) != slot.native_listener
                        || shared.current(&run.context).is_err()
                    {
                        run.stale = run.stale.saturating_add(1);
                    } else if let Some(source) = &slot.listener {
                        match run.router.mtc_message(source, &bytes, received_unix_ms) {
                            Ok(step) => run.step(step),
                            Err(e) => run.diagnostic(re(e)),
                        }
                    }
                }
                Ok(_) => {
                    run.diagnostic(error(
                        "timecode_receipt",
                        "Unexpected ordinary MIDI callback",
                    ));
                    run.retire();
                }
                Err(e) => {
                    shared.fail(pe(e.clone()));
                    shared.closing.store(true, Ordering::Release);
                    run.diagnostic(pe(e));
                    run.retire();
                }
            },
        }
    }
}
