//! Five ordinary MIDI commands. MTC/LTC routing deliberately lives elsewhere.
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::LibraryService,
    WireError,
};
use freeshow_midi::{Hydration, WireData};
use freeshow_midi_ports::{
    Backend, Command, Config, Direction, EventStream, MidiService, Owner, Reply,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
pub const COMMANDS: &[&str] = freeshow_midi::COMMANDS;
const CAPACITY: usize = 16;
const OUTPUT_LIMIT: usize = 128 * 1024;
type Result<T> = std::result::Result<T, WireError>;
type Failure = (&'static str, String);
type Factory = Arc<dyn Fn() -> freeshow_midi_ports::Result<Box<dyn Backend>> + Send + Sync>;
type Publisher =
    Arc<dyn Fn(NativeMidiPublication) -> std::result::Result<(), String> + Send + Sync>;
#[derive(Clone, Debug)]
pub struct MidiHostConfig {
    pub ports: Config,
    pub request_timeout: Duration,
    pub lifecycle_timeout: Duration,
    pub delivery_timeout: Duration,
}
impl Default for MidiHostConfig {
    fn default() -> Self {
        Self {
            ports: Config::default(),
            request_timeout: Duration::from_secs(5),
            lifecycle_timeout: Duration::from_secs(5),
            delivery_timeout: Duration::from_secs(5),
        }
    }
}
/// Hydration is relative to event.message.data, applied only after context acceptance.
#[derive(Debug, Serialize)]
pub struct NativeMidiPublication {
    pub event: Event,
    pub hydration: Hydration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<NativeMidiDelivery>,
}
#[derive(Debug, Clone, Serialize)]
pub struct NativeMidiDelivery {
    pub id: String,
    pub context: RuntimeContext,
    pub run: u64,
    pub listener: u64,
}
struct Delivery {
    id: String,
    reply: oneshot::Sender<bool>,
}
struct Waiting {
    id: String,
    binding: Arc<Binding>,
    reply: oneshot::Receiver<bool>,
    deadline: Instant,
}
struct Binding {
    context: RuntimeContext,
    run: u64,
    owner: Owner,
    cancel: CancellationToken,
    retiring: AtomicBool,
    service: MidiService,
}
/// Keep the consumer alive until its producer has joined, including timeout
/// retries. A receiver dropped before join can turn an in-flight send into a
/// spurious MIDI event-overflow failure during otherwise normal cancellation.
struct OwnedEvents {
    stream: EventStream,
    retained: Arc<AtomicBool>,
}
impl Drop for OwnedEvents {
    fn drop(&mut self) {
        self.retained.store(false, Ordering::Release);
    }
}
#[derive(Default)]
struct Evidence {
    published: u64,
    stale: u64,
    diagnostics: u64,
    last: Option<Failure>,
}
struct Shared {
    library: LibraryService,
    config: MidiHostConfig,
    factory: Factory,
    publisher: Publisher,
    binding: Mutex<Option<Arc<Binding>>>,
    publication: Mutex<()>,
    pending: Mutex<Option<Delivery>>,
    closing: AtomicBool,
    terminal: CancellationToken,
    failure: Mutex<Option<Failure>>,
    evidence: Mutex<Evidence>,
    next_run: AtomicU64,
    event_stream: Arc<AtomicBool>,
    retry: watch::Sender<u64>,
}
struct Completion<T> {
    result: Result<T>,
    _permit: OwnedSemaphorePermit,
}
struct Job {
    request: Request,
    reply: oneshot::Sender<Completion<Vec<NativeMidiPublication>>>,
    permit: OwnedSemaphorePermit,
    deadline: Instant,
}
struct ContextJob {
    reply: oneshot::Sender<Completion<()>>,
    permit: OwnedSemaphorePermit,
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
    changes: mpsc::Sender<ContextJob>,
    slots: Arc<Semaphore>,
    control: Arc<Semaphore>,
    terminal_slot: Arc<Semaphore>,
    done: watch::Receiver<Progress>,
    supervisor: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        close(&self.shared);
    }
}
#[derive(Clone)]
pub struct NativeMidi {
    inner: Arc<Inner>,
}
fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|e| e.into_inner())
}
fn error(code: &'static str, message: impl Into<String>) -> WireError {
    let mut message = message.into();
    if message.len() > 2048 {
        let mut end = 2030;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push_str(" [truncated]");
    }
    WireError::new(code, message)
}
fn port_error(e: freeshow_midi_ports::Error) -> WireError {
    error(e.code, e.message)
}
fn closed() -> WireError {
    error("midi_host_shutdown", "Native MIDI host admission is closed")
}
fn failure(shared: &Shared) -> Option<WireError> {
    lock(&shared.failure)
        .as_ref()
        .map(|(code, message)| error(code, message.clone()))
}
fn diagnostic(shared: &Shared, e: &WireError) {
    let mut evidence = lock(&shared.evidence);
    evidence.diagnostics = evidence.diagnostics.saturating_add(1);
    evidence.last = Some((e.code, e.message.clone()));
}
fn retain(shared: &Shared, e: WireError) {
    let mut first = lock(&shared.failure);
    if first.is_none() {
        *first = Some((e.code, e.message));
    }
    drop(first);
    close(shared);
}
fn cancel_binding(shared: &Shared) {
    let _gate = lock(&shared.publication);
    if let Some(binding) = lock(&shared.binding).as_ref() {
        binding.retiring.store(true, Ordering::Release);
        binding.cancel.cancel();
    }
    lock(&shared.pending).take();
}
fn close(shared: &Shared) {
    shared.closing.store(true, Ordering::Release);
    shared.terminal.cancel();
    cancel_binding(shared);
}
impl NativeMidi {
    /// Factory constructs a cheap native Backend value only; OS calls belong to
    /// MidiService's fixed thread. Publisher must be a nonblocking bounded sink.
    pub fn new(
        library: LibraryService,
        config: MidiHostConfig,
        factory: impl Fn() -> freeshow_midi_ports::Result<Box<dyn Backend>> + Send + Sync + 'static,
        publisher: impl Fn(NativeMidiPublication) -> std::result::Result<(), String>
            + Send
            + Sync
            + 'static,
    ) -> Result<Self> {
        for duration in [
            config.request_timeout,
            config.lifecycle_timeout,
            config.ports.shutdown_timeout,
        ] {
            if duration.is_zero() || duration > Duration::from_secs(30) {
                return Err(error(
                    "midi_host_configuration",
                    "Invalid native MIDI host timeout",
                ));
            }
        }
        if config.ports.idle_poll < Duration::from_millis(1)
            || config.ports.idle_poll > Duration::from_secs(1)
        {
            return Err(error(
                "midi_host_configuration",
                "Invalid native MIDI owner idle interval",
            ));
        }
        if config.delivery_timeout.is_zero() || config.delivery_timeout > Duration::from_secs(5) {
            return Err(error(
                "midi_host_configuration",
                "Invalid private MIDI delivery timeout",
            ));
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(error(
                "midi_host_runtime",
                "Native MIDI requires an entered Tokio runtime",
            ));
        }
        let (retry, retries) = watch::channel(0);
        let shared = Arc::new(Shared {
            library,
            config,
            factory: Arc::new(factory),
            publisher: Arc::new(publisher),
            binding: Mutex::new(None),
            publication: Mutex::new(()),
            pending: Mutex::new(None),
            closing: AtomicBool::new(false),
            terminal: CancellationToken::new(),
            failure: Mutex::new(None),
            evidence: Mutex::new(Evidence::default()),
            next_run: AtomicU64::new(0),
            event_stream: Arc::new(AtomicBool::new(false)),
            retry,
        });
        let (jobs, receive) = mpsc::channel(CAPACITY);
        let (changes, contexts) = mpsc::channel(1);
        let (progress, done) = watch::channel(Progress::default());
        let owner = tokio::spawn(owner(shared.clone(), receive, contexts));
        let cleanup = shared.clone();
        let supervisor = tokio::spawn(async move {
            let mut events = match owner.await {
                Ok(events) => events,
                Err(e) => {
                    retain(&cleanup, error("midi_host_worker", e.to_string()));
                    None
                }
            };
            close(&cleanup);
            let mut retries = retries;
            loop {
                let attempt = *retries.borrow_and_update();
                match retire(&cleanup).await {
                    Err(e) if e.code == "midi_shutdown_timeout" => {
                        diagnostic(&cleanup, &e);
                        progress.send_replace(Progress {
                            attempt,
                            complete: false,
                            error: Some((e.code, e.message)),
                        });
                        while *retries.borrow_and_update() == attempt {
                            if retries.changed().await.is_err() {
                                return;
                            }
                        }
                    }
                    result => {
                        // No successor or completed teardown may precede both
                        // the actual native join and old receiver destruction.
                        events.take();
                        if let Err(e) = result {
                            retain(&cleanup, e);
                        }
                        progress.send_replace(Progress {
                            attempt,
                            complete: true,
                            error: failure(&cleanup).map(|e| (e.code, e.message)),
                        });
                        return;
                    }
                }
            }
        });
        Ok(Self {
            inner: Arc::new(Inner {
                shared,
                jobs,
                changes,
                slots: Arc::new(Semaphore::new(CAPACITY)),
                control: Arc::new(Semaphore::new(1)),
                terminal_slot: Arc::new(Semaphore::new(1)),
                done,
                supervisor: AsyncMutex::new(Some(supervisor)),
            }),
        })
    }
    pub fn status(&self) -> Value {
        let shared = &self.inner.shared;
        let binding = lock(&shared.binding);
        let evidence = lock(&shared.evidence);
        let progress = self.inner.done.borrow();
        json!({"closing":shared.closing.load(Ordering::Acquire),"admittedRequests":CAPACITY-self.inner.slots.available_permits(),"admittedControls":1-self.inner.control.available_permits(),"pendingDeliveries":usize::from(lock(&shared.pending).is_some()),"eventStreamRetained":shared.event_stream.load(Ordering::Acquire),"binding":binding.as_ref().map(|b|json!({"context":b.context,"run":b.run,"owner":b.owner,"retiring":b.retiring.load(Ordering::Acquire),"service":b.service.status()})),"published":evidence.published,"staleEvents":evidence.stale,"diagnostics":evidence.diagnostics,"lastDiagnostic":evidence.last,"failure":*lock(&shared.failure),"teardown":{"attempt":progress.attempt,"complete":progress.complete,"error":progress.error}})
    }
    /// Private transport receipt only; never a source MIDI command success ACK.
    pub fn complete(&self, window: &str, id: &str, accepted: bool) -> Result<()> {
        if window != "main" {
            return Err(error(
                "window_error",
                "Only main completes private MIDI delivery",
            ));
        }
        if id.len() != 44
            || !id.starts_with("native-midi-")
            || !id[12..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(error(
                "invalid_midi_completion",
                "Invalid private MIDI delivery ID",
            ));
        }
        let mut pending = lock(&self.inner.shared.pending);
        if pending.as_ref().is_none_or(|p| p.id != id) {
            return Err(error(
                "invalid_midi_completion",
                "Unknown or consumed private MIDI delivery",
            ));
        }
        let _ = pending.take().unwrap().reply.send(accepted);
        Ok(())
    }
    pub async fn dispatch(
        &self,
        request: Request,
        window: &str,
    ) -> Result<Vec<NativeMidiPublication>> {
        if window != "main" {
            return Err(error("window_error", "Only main owns ordinary native MIDI"));
        }
        if request.channel != "MAIN"
            || !COMMANDS.contains(&request.data["channel"].as_str().unwrap_or(""))
        {
            return Err(error("unsupported_command", "Unknown native MIDI command"));
        }
        validate_input(&request)?;
        let shared = &self.inner.shared;
        if let Some(e) = failure(shared) {
            return Err(e);
        }
        if shared.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        validate_context(shared, request.context.as_ref())?;
        let provenance = request.context.clone();
        let permit = self.inner.slots.clone().try_acquire_owned().map_err(|_| {
            error(
                "midi_host_busy",
                "Native MIDI host requests are at capacity",
            )
        })?;
        let (reply, response) = oneshot::channel();
        let deadline = Instant::now() + shared.config.request_timeout;
        self.inner
            .jobs
            .try_send(Job {
                request,
                reply,
                permit,
                deadline,
            })
            .map_err(|_| closed())?;
        let completion = tokio::time::timeout_at(deadline.into(), response)
            .await
            .map_err(|_| {
                error(
                    "midi_deadline",
                    "Native MIDI request still owns accepted work after its reply deadline",
                )
            })?
            .map_err(|_| {
                failure(shared).unwrap_or_else(|| {
                    error("midi_host_worker", "Native MIDI owner lost the reply")
                })
            })?;
        // Completed oneshots keep admission until this waiter actually collects.
        validate_context(shared, provenance.as_ref())?;
        completion.result
    }
    /// Accepted context cleanup remains owned when its IPC caller is dropped.
    pub async fn context_changed(&self) -> Result<()> {
        let shared = &self.inner.shared;
        if let Some(binding) = lock(&shared.binding).as_ref() {
            if shared.library.context().map_err(WireError::from)? != binding.context {
                // Release the binding mutex before taking publication ordering.
                binding.retiring.store(true, Ordering::Release);
                binding.cancel.cancel();
            }
        }
        {
            let _gate = lock(&shared.publication);
            let old = lock(&shared.binding)
                .as_ref()
                .is_some_and(|b| b.retiring.load(Ordering::Acquire));
            if old {
                lock(&shared.pending).take();
            }
        }
        if let Some(e) = failure(shared) {
            return Err(e);
        }
        if shared.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        let permit = self
            .inner
            .control
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                error(
                    "midi_host_control_busy",
                    "Native MIDI context cleanup is already owned",
                )
            })?;
        let (reply, response) = oneshot::channel();
        self.inner
            .changes
            .try_send(ContextJob { reply, permit })
            .map_err(|_| closed())?;
        tokio::time::timeout(shared.config.lifecycle_timeout, response)
            .await
            .map_err(|_| {
                error(
                    "midi_shutdown_timeout",
                    "Native MIDI context cleanup is still owned; retry drain",
                )
            })?
            .map_err(|_| failure(shared).unwrap_or_else(closed))?
            .result
    }
    pub async fn shutdown(&self) -> Result<()> {
        let shared = &self.inner.shared;
        close(shared);
        let _slot = self.inner.terminal_slot.try_acquire().map_err(|_| {
            error(
                "midi_host_control_busy",
                "Native MIDI terminal drain already has a waiter",
            )
        })?;
        let mut attempt = 0;
        shared.retry.send_modify(|revision| {
            *revision = revision.saturating_add(1);
            attempt = *revision;
        });
        let mut done = self.inner.done.clone();
        let wait = async {
            loop {
                let progress = done.borrow_and_update().clone();
                if progress.complete {
                    break progress
                        .error
                        .map_or(Ok(()), |(code, message)| Err(error(code, message)));
                }
                if progress.attempt >= attempt {
                    if let Some((code, message)) = progress.error {
                        return Err(error(code, message));
                    }
                }
                done.changed().await.map_err(|_| {
                    error(
                        "midi_host_worker",
                        "Native MIDI supervisor completion was lost",
                    )
                })?;
            }
        };
        let result = tokio::time::timeout(shared.config.lifecycle_timeout, wait)
            .await
            .map_err(|_| {
                error(
                    "midi_shutdown_timeout",
                    "Native MIDI actual operations/joins are still owned; retry shutdown",
                )
            })?;
        if let Err(e) = &result {
            if e.code == "midi_host_worker" {
                // A lost supervisor must never turn a later retry into success.
                // The independent real service stays reachable for best-effort
                // drain even when the fixed host task join can no longer prove it.
                retain(shared, error(e.code, e.message.clone()));
                if let Err(drain) = retire(shared).await {
                    if drain.code == "midi_shutdown_timeout" {
                        return Err(drain);
                    }
                    retain(shared, drain);
                }
            }
        }
        if self.inner.done.borrow().complete {
            let mut supervisor = self.inner.supervisor.lock().await;
            if let Some(task) = supervisor.as_mut() {
                if let Err(e) = task.await {
                    retain(shared, error("midi_host_worker", e.to_string()));
                }
                supervisor.take();
            }
        }
        if let Some(e) = failure(shared) {
            return Err(e);
        }
        result
    }
}
fn validate_context(shared: &Shared, context: Option<&RuntimeContext>) -> Result<()> {
    let current = shared.library.context().map_err(WireError::from)?;
    if context != Some(&current) {
        return Err(error(
            "stale_library",
            "MIDI request belongs to an obsolete or missing library context",
        ));
    }
    Ok(())
}
async fn retire(shared: &Shared) -> Result<()> {
    cancel_binding(shared);
    let old = lock(&shared.binding).clone();
    if let Some(old) = old {
        match old.service.shutdown().await.map_err(port_error) {
            Err(e) if e.code == "midi_shutdown_timeout" => return Err(e),
            result => {
                let mut binding = lock(&shared.binding);
                if binding.as_ref().is_some_and(|b| b.run == old.run) {
                    binding.take();
                }
                drop(binding);
                if let Err(e) = result {
                    retain(shared, error(e.code, e.message.clone()));
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}
async fn retire_events(shared: &Shared, events: &mut Option<OwnedEvents>) -> Result<()> {
    let result = retire(shared).await;
    if result
        .as_ref()
        .err()
        .is_none_or(|e| e.code != "midi_shutdown_timeout")
    {
        events.take();
    }
    result
}
async fn ensure_binding(
    shared: &Shared,
    context: &RuntimeContext,
    events: &mut Option<OwnedEvents>,
) -> Result<Arc<Binding>> {
    let previous = lock(&shared.binding).clone();
    if let Some(previous) = previous {
        if previous.context == *context && !previous.retiring.load(Ordering::Acquire) {
            return Ok(previous);
        }
        retire_events(shared, events).await?;
    }
    validate_context(shared, Some(context))?;
    if shared.closing.load(Ordering::Acquire) {
        return Err(closed());
    }
    let run = shared
        .next_run
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < 9007199254740991).then_some(n + 1)
        })
        .map_err(|_| error("midi_host_limit", "MIDI native run identifiers exhausted"))?
        + 1;
    let path = context.data_path.to_str().ok_or_else(|| {
        error(
            "midi_host_unsupported",
            "MIDI owner path is not valid UTF-8",
        )
    })?;
    let owner = Owner::new(&context.runtime_id, context.library_generation, path, run)
        .map_err(port_error)?;
    let cancel = CancellationToken::new();
    let service = MidiService::with_backend(
        owner.clone(),
        cancel.clone(),
        shared.config.ports.clone(),
        (shared.factory)().map_err(port_error)?,
    )
    .map_err(port_error)?;
    let binding = Arc::new(Binding {
        context: context.clone(),
        run,
        owner,
        cancel,
        retiring: AtomicBool::new(false),
        service,
    });
    *lock(&shared.binding) = Some(binding.clone());
    let stream = binding.service.events().map_err(port_error)?;
    shared.event_stream.store(true, Ordering::Release);
    *events = Some(OwnedEvents {
        stream,
        retained: shared.event_stream.clone(),
    });
    if let Err(e) = validate_context(shared, Some(context)) {
        binding.cancel.cancel();
        binding.retiring.store(true, Ordering::Release);
        return Err(e);
    }
    Ok(binding)
}
fn map_command(request: &Request) -> Result<Command> {
    let payload = request.data.get("data");
    Ok(match request.data["channel"].as_str().unwrap() {
        "GET_MIDI_INPUTS" => Command::Devices(Direction::Input),
        "GET_MIDI_OUTPUTS" => Command::Devices(Direction::Output),
        "SEND_MIDI" => Command::Send(payload.cloned().unwrap_or(Value::Null)),
        "RECEIVE_MIDI" => Command::Listen(payload.cloned().unwrap_or(Value::Null)),
        "CLOSE_MIDI" => {
            if payload.is_none_or(Value::is_null) {
                return Err(error(
                    "midi_invalid",
                    format!(
                        "Cannot read properties of {} (reading 'id')",
                        if payload.is_none() {
                            "undefined"
                        } else {
                            "null"
                        }
                    ),
                ));
            }
            Command::close_inputs_field(payload.and_then(|p| p.get("id"))).map_err(port_error)?
        }
        _ => return Err(error("unsupported_command", "Unknown native MIDI command")),
    })
}
fn publication(
    context: &RuntimeContext,
    command: &str,
    listener: Option<String>,
    wire: WireData,
) -> Result<NativeMidiPublication> {
    let mut event = Event::reply("MAIN", command, wire.data, listener);
    event.context = Some(context.clone());
    let publication = NativeMidiPublication {
        event,
        hydration: wire.hydration,
        delivery: None,
    };
    if !encoded(&publication, OUTPUT_LIMIT) {
        return Err(error(
            "midi_host_limit",
            "MIDI publication exceeds encoded output bound",
        ));
    }
    Ok(publication)
}
async fn execute(
    shared: &Shared,
    request: &Request,
    deadline: Instant,
    events: &mut Option<OwnedEvents>,
) -> Result<Vec<NativeMidiPublication>> {
    validate_context(shared, request.context.as_ref())?;
    if shared.closing.load(Ordering::Acquire) {
        return Err(closed());
    }
    if Instant::now() >= deadline {
        return Err(error("midi_deadline", "Queued native MIDI request expired"));
    }
    let command = map_command(request)?;
    let binding = ensure_binding(shared, request.context.as_ref().unwrap(), events).await?;
    let ticket = binding
        .service
        .begin(&binding.owner, command, CancellationToken::new(), deadline)
        .map_err(port_error)?;
    // This future is owned by the fixed actor, never the cancellable IPC waiter.
    let result = ticket.finish().await.map_err(port_error);
    if let Some(e) = binding.service.status().failure {
        retain(shared, port_error(e));
    }
    if let Err(e) = &result {
        if e.code == "midi_worker" {
            retain(shared, error(e.code, e.message.clone()));
        }
        diagnostic(shared, e);
    }
    validate_context(shared, request.context.as_ref())?;
    if shared.closing.load(Ordering::Acquire) {
        return Err(failure(shared).unwrap_or_else(closed));
    }
    match result? {
        Reply::Devices(wire) => Ok(vec![publication(
            &binding.context,
            request.data["channel"].as_str().unwrap(),
            request.listener_id.clone(),
            wire,
        )?]),
        Reply::Diagnostics(errors) => {
            for e in errors {
                diagnostic(shared, &port_error(e));
            }
            Ok(vec![])
        }
        Reply::Done => Ok(vec![]),
        Reply::MtcListener(_) => Err(error(
            "midi_host_unsupported",
            "MTC reply cannot enter ordinary MIDI routing",
        )),
    }
}
async fn next_event(
    events: &mut Option<OwnedEvents>,
) -> freeshow_midi_ports::Result<freeshow_midi_ports::Event> {
    match events {
        Some(events) => events.stream.recv().await,
        None => std::future::pending().await,
    }
}
fn publish(shared: &Shared, event: freeshow_midi_ports::Event) -> Result<Option<Waiting>> {
    let freeshow_midi_ports::Event::Midi {
        owner,
        listener,
        data,
    } = event
    else {
        return Err(error(
            "midi_host_unsupported",
            "MTC event cannot enter ordinary MIDI routing",
        ));
    };
    let _gate = lock(&shared.publication);
    let binding = lock(&shared.binding).clone();
    let Some(binding) = binding.filter(|b| {
        !shared.closing.load(Ordering::Acquire)
            && !b.retiring.load(Ordering::Acquire)
            && !b.cancel.is_cancelled()
            && b.owner == owner
            && listener != 0
            && shared.library.context().ok().as_ref() == Some(&b.context)
    }) else {
        let mut evidence = lock(&shared.evidence);
        evidence.stale = evidence.stale.saturating_add(1);
        return Ok(None);
    };
    let mut publication = publication(&binding.context, "RECEIVE_MIDI2", None, data)?;
    let id = format!("native-midi-{}", uuid::Uuid::new_v4().simple());
    publication.delivery = Some(NativeMidiDelivery {
        id: id.clone(),
        context: binding.context.clone(),
        run: binding.run,
        listener,
    });
    if !encoded(&publication, OUTPUT_LIMIT) {
        return Err(error(
            "midi_host_limit",
            "Private MIDI publication exceeds output bound",
        ));
    }
    let (reply, response) = oneshot::channel();
    *lock(&shared.pending) = Some(Delivery {
        id: id.clone(),
        reply,
    });
    if let Err(message) = (shared.publisher)(publication) {
        lock(&shared.pending).take();
        return Err(error("midi_publication", message));
    }
    let mut evidence = lock(&shared.evidence);
    evidence.published = evidence.published.saturating_add(1);
    Ok(Some(Waiting {
        id,
        binding,
        reply: response,
        deadline: Instant::now() + shared.config.delivery_timeout,
    }))
}
async fn wait_delivery(shared: &Shared, waiting: &mut Option<Waiting>) -> Result<Option<bool>> {
    let Some(waiting) = waiting.as_mut() else {
        return std::future::pending().await;
    };
    tokio::select! {biased;
        _=shared.terminal.cancelled()=>Ok(None),
        _=waiting.binding.cancel.cancelled()=>Ok(None),
        _=tokio::time::sleep_until(waiting.deadline.into())=>Err(error("midi_delivery_timeout","Main did not complete private MIDI delivery")),
        accepted=&mut waiting.reply=>match accepted {Ok(value)=>Ok(Some(value)),Err(_)=>Err(error("midi_delivery_lost","Private MIDI delivery completion was lost"))},
    }
}
fn finish_delivery(
    shared: &Shared,
    waiting: &mut Option<Waiting>,
    result: Result<Option<bool>>,
) -> Result<()> {
    let obsolete = waiting.as_ref().is_some_and(|w| {
        shared.closing.load(Ordering::Acquire)
            || w.binding.retiring.load(Ordering::Acquire)
            || w.binding.cancel.is_cancelled()
            || shared.library.context().ok().as_ref() != Some(&w.binding.context)
    });
    if let Some(waiting) = waiting.take() {
        let mut pending = lock(&shared.pending);
        if pending.as_ref().is_some_and(|p| p.id == waiting.id) {
            pending.take();
        }
    }
    if obsolete {
        return Ok(());
    }
    match result {
        Ok(Some(false)) => Err(error(
            "midi_delivery_rejected",
            "Main rejected private MIDI delivery",
        )),
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}
async fn owner(
    shared: Arc<Shared>,
    mut jobs: mpsc::Receiver<Job>,
    mut changes: mpsc::Receiver<ContextJob>,
) -> Option<OwnedEvents> {
    let mut events = None;
    let mut waiting = None;
    loop {
        let retiring = lock(&shared.binding)
            .as_ref()
            .is_some_and(|b| b.retiring.load(Ordering::Acquire));
        let read_events = waiting.is_none() && !retiring;
        tokio::select! {biased;
            _=shared.terminal.cancelled()=>break,
            Some(change)=changes.recv()=>{
                let old=lock(&shared.binding).clone();
                let obsolete=old.as_ref().is_some_and(|b|shared.library.context().ok().as_ref()!=Some(&b.context)||b.retiring.load(Ordering::Acquire));
                let result=if obsolete {retire_events(&shared,&mut events).await}else{Ok(())};
                if let Err(e)=&result {diagnostic(&shared,e);if e.code!="midi_shutdown_timeout"{retain(&shared,error(e.code,e.message.clone()));}}
                let _=change.reply.send(Completion{result,_permit:change.permit});
            },
            receipt=wait_delivery(&shared,&mut waiting)=>{if let Err(e)=finish_delivery(&shared,&mut waiting,receipt){retain(&shared,e);}},
            Some(job)=jobs.recv()=>{
                let mut result=execute(&shared,&job.request,job.deadline,&mut events).await;
                if result.is_ok()&&job.request.data["channel"]=="CLOSE_MIDI"&&waiting.is_some(){
                    let receipt=wait_delivery(&shared,&mut waiting).await;
                    if let Err(e)=finish_delivery(&shared,&mut waiting,receipt){retain(&shared,error(e.code,e.message.clone()));result=Err(e);}
                }
                if let Err(e)=&result {if e.code!="stale_library"{diagnostic(&shared,e);}}
                let _=job.reply.send(Completion{result,_permit:job.permit});
            },
            event=next_event(&mut events),if read_events=>match event {
                Ok(event)=>match publish(&shared,event){Ok(delivery)=>waiting=delivery,Err(e)=>retain(&shared,e)},
                Err(e)=>{
                    let current=lock(&shared.binding).clone();
                    if let Some(binding)=current{if let Some(e)=binding.service.status().failure{retain(&shared,port_error(e));}else if !binding.retiring.load(Ordering::Acquire)&&!shared.closing.load(Ordering::Acquire){retain(&shared,port_error(e));}}
                },
            },
            else=>break,
        }
    }
    finish_delivery(&shared, &mut waiting, Ok(None)).ok();
    jobs.close();
    changes.close();
    while let Ok(job) = jobs.try_recv() {
        let _ = job.reply.send(Completion {
            result: Err(closed()),
            _permit: job.permit,
        });
    }
    while let Ok(change) = changes.try_recv() {
        let _ = change.reply.send(Completion {
            result: Err(closed()),
            _permit: change.permit,
        });
    }
    events
}
fn validate_input(request: &Request) -> Result<()> {
    if request
        .listener_id
        .as_ref()
        .is_some_and(|id| id.len() > 256)
        || request
            .context
            .as_ref()
            .is_some_and(|c| c.runtime_id.len() > 256 || c.data_path.as_os_str().len() > 32768)
    {
        return Err(error(
            "midi_host_limit",
            "Native MIDI provenance exceeds bounds",
        ));
    }
    let mut stack = vec![(&request.data, 0usize)];
    let mut nodes = 0usize;
    let mut bytes = 0usize;
    while let Some((value, depth)) = stack.pop() {
        nodes += 1;
        if depth > 10 || nodes > 1024 {
            return Err(error(
                "midi_host_limit",
                "Native MIDI input nesting/work exceeds bounds",
            ));
        }
        match value {
            Value::String(v) => bytes = bytes.saturating_add(v.len()),
            Value::Array(v) => {
                if v.len() > 1024usize.saturating_sub(nodes + stack.len()) {
                    return Err(error("midi_host_limit", "Native MIDI input exceeds bounds"));
                }
                stack.extend(v.iter().map(|v| (v, depth + 1)));
            }
            Value::Object(v) => {
                if v.len() > 1024usize.saturating_sub(nodes + stack.len()) {
                    return Err(error("midi_host_limit", "Native MIDI input exceeds bounds"));
                }
                for (k, v) in v {
                    bytes = bytes.saturating_add(k.len());
                    stack.push((v, depth + 1));
                }
            }
            _ => bytes = bytes.saturating_add(8),
        }
        if bytes > 65536 {
            return Err(error(
                "midi_host_limit",
                "Native MIDI input bytes exceed bounds",
            ));
        }
    }
    if !encoded(&request.data, 65536) {
        return Err(error(
            "midi_host_limit",
            "Native MIDI encoded input exceeds bounds",
        ));
    }
    Ok(())
}
fn encoded(value: &impl Serialize, maximum: usize) -> bool {
    struct Count(usize);
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::other("MIDI output limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Count(maximum), value).is_ok()
}
