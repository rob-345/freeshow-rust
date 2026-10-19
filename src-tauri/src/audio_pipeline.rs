//! Ordered native AUDIO composition. No renderer binary bridge or hardware sink.
use freeshow_audio_pipeline as pipe;
use freeshow_core::{
    protocol::{Event, RuntimeContext},
    service::LibraryService,
};
use freeshow_icecast as ice;
use freeshow_opus as opus;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc as std_mpsc, Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
static OWNER: AtomicBool = AtomicBool::new(false);
static EPOCH: AtomicU64 = AtomicU64::new(1);
type Result<T> = std::result::Result<T, AudioError>;
#[derive(Clone, Debug, Serialize)]
pub struct AudioError {
    pub code: &'static str,
    pub message: String,
}
impl AudioError {
    fn new(code: &'static str, text: impl ToString) -> Self {
        let text = text.to_string();
        let mut end = text.len().min(2048);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            code,
            message: text[..end].into(),
        }
    }
}
impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for AudioError {}
impl From<AudioError> for freeshow_core::WireError {
    fn from(e: AudioError) -> Self {
        Self::new(e.code, e.message)
    }
}
fn pe(e: pipe::Error) -> AudioError {
    AudioError::new(e.code, e.message)
}
fn oe(e: opus::Error) -> AudioError {
    AudioError::new(e.code, e.message)
}
fn ie(e: ice::Error) -> AudioError {
    AudioError::new(e.code, e.message)
}
#[derive(Clone, Debug)]
pub struct AudioLimits {
    pub queued: usize,
    pub retained: usize,
    pub input_bytes: usize,
    pub metadata_bytes: usize,
    pub frames: usize,
    pub shutdown_timeout: Duration,
    pub delivery_timeout: Duration,
    pub opus: opus::Limits,
    pub icecast: ice::Limits,
}
impl Default for AudioLimits {
    fn default() -> Self {
        Self {
            queued: 4,
            retained: 4,
            input_bytes: 32768,
            metadata_bytes: 8192,
            frames: 8,
            shutdown_timeout: Duration::from_secs(15),
            delivery_timeout: Duration::from_secs(5),
            opus: opus::Limits::default(),
            icecast: ice::Limits {
                events: 32,
                write_timeout: Duration::from_millis(250),
                ..ice::Limits::default()
            },
        }
    }
}
impl AudioLimits {
    fn validate(&self) -> Result<()> {
        if !(1..=8).contains(&self.queued)
            || !(1..=8).contains(&self.retained)
            || !(1..=32768).contains(&self.input_bytes)
            || !(256..=8192).contains(&self.metadata_bytes)
            || !(1..=8).contains(&self.frames)
            || self.shutdown_timeout.is_zero()
            || self.shutdown_timeout > Duration::from_secs(60)
            || self.delivery_timeout.is_zero()
            || self.delivery_timeout > Duration::from_secs(10)
            || self.opus.call_budget > Duration::from_millis(250)
            || self.icecast.write_timeout > Duration::from_millis(250)
            || self.icecast.events > 32
        {
            return Err(AudioError::new(
                "audio_limits",
                "Native AUDIO limits exceed supported bounds",
            ));
        }
        Ok(())
    }
}
/// Native registry/activity authority, not renderer data or hardware proof.
#[derive(Clone, Debug, Default)]
pub struct SinkAvailability {
    pub ndi: Vec<String>,
    pub omt: Vec<String>,
    pub blackmagic: Vec<String>,
    pub output_stream: bool,
    pub webrtc: bool,
    pub rtmp: bool,
}
impl SinkAvailability {
    fn validate(&self) -> Result<()> {
        let mut bytes = 0usize;
        for list in [&self.ndi, &self.omt, &self.blackmagic] {
            if list.len() > 64 {
                return Err(AudioError::new(
                    "audio_input_limit",
                    "Sink registration count",
                ));
            }
            for key in list {
                bytes = bytes.saturating_add(key.len());
                if key.len() > 1024 {
                    return Err(AudioError::new(
                        "audio_input_limit",
                        "Sink registration length",
                    ));
                }
            }
        }
        if bytes > 8192 {
            return Err(AudioError::new(
                "audio_input_limit",
                "Sink registration bytes",
            ));
        }
        Ok(())
    }
    fn snapshot(self, epoch: u64) -> Result<pipe::SinkSnapshot> {
        pipe::SinkSnapshot::new(
            pipe::ContextId(epoch, 0),
            self.ndi,
            self.omt,
            self.blackmagic,
            self.output_stream,
            self.webrtc,
            self.rtmp,
        )
        .map_err(pe)
    }
}
#[derive(Debug, Serialize)]
pub enum Effect {
    SourceNoOp {
        sink: pipe::SinkKind,
    },
    Unsupported {
        sink: pipe::SinkKind,
    },
    Encoded {
        index: usize,
        sequence: u64,
        bytes: usize,
    },
    Icecast {
        index: usize,
        disposition: ice::Disposition,
        run: u64,
    },
    MetadataApplied {
        run: u64,
    },
    Failure {
        index: usize,
        stage: pipe::FailureStage,
        error: AudioError,
    },
}
/// Private native observation; never an original AUDIO success ACK.
pub struct AudioReceipt {
    context: RuntimeContext,
    sequence: u64,
    effects: Vec<Effect>,
    diagnostics: Vec<AudioError>,
    failure: Option<AudioError>,
    _lease: OwnedSemaphorePermit,
}
impl AudioReceipt {
    pub fn context(&self) -> &RuntimeContext {
        &self.context
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }
    pub fn diagnostics(&self) -> &[AudioError] {
        &self.diagnostics
    }
    pub fn failure(&self) -> Option<&AudioError> {
        self.failure.as_ref()
    }
}
impl std::fmt::Debug for AudioReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioReceipt")
            .field("context", &self.context)
            .field("sequence", &self.sequence)
            .field("effects", &self.effects)
            .field("failure", &self.failure)
            .finish()
    }
}
/// Native cleanup evidence, never an original AUDIO success acknowledgement.
/// Keeping an uncollected receipt retains the single reserved control lease.
pub struct AudioRetirementReceipt {
    pub context: RuntimeContext,
    pub run: Option<u64>,
    pub disconnected: bool,
    _lease: OwnedSemaphorePermit,
}
impl std::fmt::Debug for AudioRetirementReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioRetirementReceipt")
            .field("context", &self.context)
            .field("run", &self.run)
            .field("disconnected", &self.disconnected)
            .finish()
    }
}
pub struct AudioStatusDelivery {
    event: Event,
    id: String,
    run: u64,
    own_undefined_error: bool,
    diagnostic: Option<ice::Error>,
    _lease: Arc<OwnedSemaphorePermit>,
}
impl AudioStatusDelivery {
    pub fn event(&self) -> &Event {
        &self.event
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn run(&self) -> u64 {
        self.run
    }
    pub fn own_undefined_error(&self) -> bool {
        self.own_undefined_error
    }
    pub fn diagnostic(&self) -> Option<&ice::Error> {
        self.diagnostic.as_ref()
    }
}
type Sink = Arc<dyn Fn(AudioStatusDelivery) -> std::result::Result<(), String> + Send + Sync>;
struct PendingDelivery {
    context: RuntimeContext,
    id: String,
    reply: oneshot::Sender<bool>,
}
#[derive(Clone)]
struct StreamBinding {
    context: RuntimeContext,
    run: u64,
}
struct Shared {
    library: LibraryService,
    closed: AtomicBool,
    fault: CancellationToken,
    stop: CancellationToken,
    done: AtomicBool,
    joined: AtomicBool,
    changed: Notify,
    join_changed: Notify,
    ready: Notify,
    capability: Mutex<Option<opus::Capability>>,
    terminal: Mutex<Option<AudioError>>,
    pending: Mutex<Option<PendingDelivery>>,
    runs: Mutex<BTreeMap<u64, RuntimeContext>>,
    stream: Mutex<Option<StreamBinding>>,
    publication: tokio::sync::Mutex<()>,
    stale: AtomicU64,
    delivered: AtomicU64,
    drained: AtomicU64,
    #[cfg(test)]
    before_accept: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    #[cfg(test)]
    after_accept: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
}
impl Shared {
    fn fail(&self, e: AudioError) {
        let mut error = self.terminal.lock().unwrap_or_else(|e| e.into_inner());
        if error.is_none() {
            *error = Some(e);
        }
        self.closed.store(true, Ordering::Release);
        self.fault.cancel();
    }
    fn error(&self) -> Option<AudioError> {
        self.terminal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn current(&self, c: &RuntimeContext) -> Result<()> {
        if self
            .library
            .context()
            .map_err(|e| AudioError::new("audio_context_unavailable", e))?
            != *c
        {
            return Err(AudioError::new(
                "stale_library",
                "AUDIO belongs to an obsolete full runtime context",
            ));
        }
        Ok(())
    }
}
enum Operation {
    Pcm {
        bytes: Vec<u8>,
        meta: Box<pipe::AudioMetadata>,
        sinks: SinkAvailability,
    },
    Metadata(String),
    Disconnect,
}
struct Job {
    context: RuntimeContext,
    operation: Operation,
    cancel: CancellationToken,
    reply: oneshot::Sender<Result<AudioReceipt>>,
    lease: OwnedSemaphorePermit,
}
struct RetirementJob {
    context: RuntimeContext,
    reply: oneshot::Sender<Result<AudioRetirementReceipt>>,
    lease: OwnedSemaphorePermit,
}
struct Handle {
    shared: Arc<Shared>,
    sender: Mutex<Option<mpsc::Sender<Job>>>,
    retire_sender: Mutex<Option<mpsc::Sender<RetirementJob>>>,
    slots: Arc<Semaphore>,
    retire_slot: Arc<Semaphore>,
    limits: AudioLimits,
    worker: Mutex<Option<JoinHandle<()>>>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.sender.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.retire_sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.shared.stop.cancel();
    }
}
#[derive(Clone)]
pub struct NativeAudioPipeline {
    handle: Arc<Handle>,
}
struct Exit(Arc<Shared>);
impl Drop for Exit {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.fail(AudioError::new(
                "audio_host_panic",
                "Native AUDIO owner panicked",
            ));
        }
        self.0.closed.store(true, Ordering::Release);
        self.0.done.store(true, Ordering::Release);
        OWNER.store(false, Ordering::Release);
        self.0.changed.notify_waiters();
        self.0.ready.notify_waiters();
    }
}
impl NativeAudioPipeline {
    pub fn start(
        library: LibraryService,
        source: opus::LibrarySource,
        limits: AudioLimits,
        sink: impl Fn(AudioStatusDelivery) -> std::result::Result<(), String> + Send + Sync + 'static,
    ) -> Result<Self> {
        limits.validate()?;
        if OWNER
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(AudioError::new(
                "audio_owner_exists",
                "One process AUDIO host is already live",
            ));
        }
        let epoch =
            match EPOCH.try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1)) {
                Ok(n) => n,
                Err(_) => {
                    OWNER.store(false, Ordering::Release);
                    return Err(AudioError::new(
                        "audio_epoch_limit",
                        "AUDIO epochs exhausted",
                    ));
                }
            };
        let shared = Arc::new(Shared {
            library,
            closed: AtomicBool::new(false),
            fault: CancellationToken::new(),
            stop: CancellationToken::new(),
            done: AtomicBool::new(false),
            joined: AtomicBool::new(false),
            changed: Notify::new(),
            join_changed: Notify::new(),
            ready: Notify::new(),
            capability: Mutex::new(None),
            terminal: Mutex::new(None),
            pending: Mutex::new(None),
            runs: Mutex::new(BTreeMap::new()),
            stream: Mutex::new(None),
            publication: tokio::sync::Mutex::new(()),
            stale: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
            drained: AtomicU64::new(0),
            #[cfg(test)]
            before_accept: Mutex::new(None),
            #[cfg(test)]
            after_accept: Mutex::new(None),
        });
        let (sender, receiver) = mpsc::channel(limits.queued);
        let (retire_sender, retire_receiver) = mpsc::channel(1);
        let state = shared.clone();
        let config = limits.clone();
        let sink = Arc::new(sink);
        let worker = std::thread::Builder::new()
            .name("freeshow-audio-host".into())
            .spawn(move || {
                let _exit = Exit(state.clone());
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(owner(
                        state,
                        receiver,
                        retire_receiver,
                        source,
                        config,
                        epoch,
                        sink,
                    )),
                    Err(e) => state.fail(AudioError::new("audio_host_runtime", e)),
                }
            })
            .map_err(|e| {
                OWNER.store(false, Ordering::Release);
                AudioError::new("audio_host_worker", e)
            })?;
        Ok(Self {
            handle: Arc::new(Handle {
                shared,
                sender: Mutex::new(Some(sender)),
                retire_sender: Mutex::new(Some(retire_sender)),
                slots: Arc::new(Semaphore::new(limits.retained)),
                retire_slot: Arc::new(Semaphore::new(1)),
                limits,
                worker: Mutex::new(Some(worker)),
            }),
        })
    }
    pub async fn capability(&self) -> Result<opus::Capability> {
        loop {
            if self.handle.shared.closed.load(Ordering::Acquire) {
                return Err(self.handle.shared.error().unwrap_or_else(|| {
                    AudioError::new("audio_host_closed", "AUDIO capability owner is closing")
                }));
            }
            let ready = self.handle.shared.ready.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            if let Some(c) = self
                .handle
                .shared
                .capability
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                return Ok(c);
            }
            if self.handle.shared.done.load(Ordering::Acquire) {
                return Err(self.handle.shared.error().unwrap_or_else(|| {
                    AudioError::new(
                        "audio_host_closed",
                        "AUDIO host stopped before initialization",
                    )
                }));
            }
            ready.await;
        }
    }
    fn check(&self, context: &RuntimeContext, cancel: &CancellationToken) -> Result<()> {
        context_bounds(context)?;
        if self.handle.shared.closed.load(Ordering::Acquire) {
            return Err(self.handle.shared.error().unwrap_or_else(|| {
                AudioError::new("audio_host_closed", "Native AUDIO admission is closed")
            }));
        }
        if cancel.is_cancelled() {
            return Err(AudioError::new(
                "cancelled",
                "AUDIO cancelled before acceptance",
            ));
        }
        self.handle.shared.current(context)
    }
    async fn submit(
        &self,
        context: &RuntimeContext,
        cancel: CancellationToken,
        operation: impl FnOnce() -> Operation,
    ) -> Result<AudioReceipt> {
        self.check(context, &cancel)?;
        let receive = {
            let sender = self.handle.sender.lock().unwrap_or_else(|e| e.into_inner());
            self.check(context, &cancel)?;
            let sender = sender
                .as_ref()
                .ok_or_else(|| AudioError::new("audio_host_closed", "AUDIO queue closed"))?;
            let lease = self.handle.slots.clone().try_acquire_owned().map_err(|_| {
                AudioError::new(
                    "audio_host_busy",
                    "AUDIO whole-packet retained quota is full",
                )
            })?;
            let slot = sender
                .try_reserve()
                .map_err(|_| AudioError::new("audio_host_busy", "AUDIO FIFO is full or closed"))?;
            self.check(context, &cancel)?;
            let (reply, receive) = oneshot::channel();
            slot.send(Job {
                context: context.clone(),
                operation: operation(),
                cancel,
                reply,
                lease,
            });
            receive
        };
        receive.await.map_err(|_| {
            self.handle.shared.error().unwrap_or_else(|| {
                AudioError::new("audio_host_worker", "AUDIO owner ended without observation")
            })
        })?
    }
    pub async fn pcm(
        &self,
        context: &RuntimeContext,
        bytes: &[u8],
        announced_bytes: usize,
        meta: &pipe::AudioMetadata,
        sinks: &SinkAvailability,
        cancel: CancellationToken,
    ) -> Result<AudioReceipt> {
        self.check(context, &cancel)?;
        if bytes.len() != announced_bytes {
            return Err(AudioError::new(
                "audio_raw_length",
                "Announced AUDIO byte length differs from raw view",
            ));
        }
        if bytes.len() > self.handle.limits.input_bytes {
            return Err(AudioError::new(
                "audio_input_limit",
                "Raw AUDIO bytes exceed limit",
            ));
        }
        bounded_meta(meta, self.handle.limits.metadata_bytes)?;
        sinks.validate()?;
        self.submit(context, cancel, || Operation::Pcm {
            bytes: bytes.to_vec(),
            meta: Box::new(meta.clone()),
            sinks: sinks.clone(),
        })
        .await
    }
    #[allow(
        dead_code,
        reason = "Tested host operation awaiting the separately prepared NOW_PLAYING integration"
    )]
    pub async fn metadata(
        &self,
        context: &RuntimeContext,
        title: &str,
        cancel: CancellationToken,
    ) -> Result<AudioReceipt> {
        self.check(context, &cancel)?;
        if title.len() > self.handle.limits.metadata_bytes
            || title.bytes().any(|b| matches!(b, 0 | b'\r' | b'\n'))
        {
            return Err(AudioError::new(
                "audio_input_limit",
                "AUDIO metadata title exceeds supported limits",
            ));
        }
        self.submit(context, cancel, || Operation::Metadata(title.into()))
            .await
    }
    pub async fn disconnect_icecast(
        &self,
        context: &RuntimeContext,
        cancel: CancellationToken,
    ) -> Result<AudioReceipt> {
        self.submit(context, cancel, || Operation::Disconnect).await
    }
    /// Call after this context's accepted ingress guards drain. Admission is
    /// synchronous and independent of ordinary AUDIO quotas. Dropping/timing out
    /// the returned future never cancels an accepted socket cleanup.
    pub fn retire_context(
        &self,
        context: &RuntimeContext,
    ) -> impl Future<Output = Result<AudioRetirementReceipt>> + Send + 'static {
        let admitted = (|| {
            context_bounds(context)?;
            let current = self
                .handle
                .shared
                .library
                .context()
                .map_err(|e| AudioError::new("audio_context_unavailable", e))?;
            if context.runtime_id != current.runtime_id
                || context.library_generation > current.library_generation
                || (context.library_generation == current.library_generation && *context != current)
            {
                return Err(AudioError::new(
                    "stale_library",
                    "Retirement belongs to a foreign runtime context",
                ));
            }
            let sender = self
                .handle
                .retire_sender
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if self.handle.shared.closed.load(Ordering::Acquire) {
                return Err(self.handle.shared.error().unwrap_or_else(|| {
                    AudioError::new(
                        "audio_host_closed",
                        "AUDIO retirement admission closed; terminal drain owns cleanup",
                    )
                }));
            }
            let sender = sender.as_ref().ok_or_else(|| {
                AudioError::new("audio_host_closed", "AUDIO retirement queue closed")
            })?;
            let lease = self
                .handle
                .retire_slot
                .clone()
                .try_acquire_owned()
                .map_err(|_| {
                    AudioError::new(
                        "audio_retirement_busy",
                        "One accepted or uncollected AUDIO retirement remains owned",
                    )
                })?;
            let slot = sender.try_reserve().map_err(|_| {
                AudioError::new(
                    "audio_retirement_busy",
                    "AUDIO retirement queue full or closed",
                )
            })?;
            let (reply, receive) = oneshot::channel();
            slot.send(RetirementJob {
                context: context.clone(),
                reply,
                lease,
            });
            Ok(receive)
        })();
        let handle = self.handle.clone();
        async move {
            let receive = admitted?;
            tokio::time::timeout(handle.limits.shutdown_timeout, receive)
                .await
                .map_err(|_| AudioError::new("audio_retirement_timeout", "Actual socket cleanup remains owned; retry after the reserved control lease drains"))?
                .map_err(|_| handle.shared.error().unwrap_or_else(|| AudioError::new("audio_host_worker", "AUDIO owner ended without retirement evidence")))?
        }
    }
    /// Full-context retirement gates callbacks; it does not reset source-global DSP/codec.
    pub fn context_changed(&self) -> Result<()> {
        let current = self
            .handle
            .shared
            .library
            .context()
            .map_err(|e| AudioError::new("audio_context_unavailable", e))?;
        let mut pending = self
            .handle
            .shared
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if pending.as_ref().is_some_and(|p| p.context != current) {
            if let Some(p) = pending.take() {
                let _ = p.reply.send(false);
            }
        }
        Ok(())
    }
    pub fn complete_status(
        &self,
        window: &str,
        context: &RuntimeContext,
        id: &str,
        accepted: bool,
    ) -> Result<()> {
        if window != "main" || id.len() > 80 {
            return Err(AudioError::new(
                "audio_status_completion",
                "Only bounded main-window status completions are accepted",
            ));
        }
        let mut pending = self
            .handle
            .shared
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if pending
            .as_ref()
            .is_none_or(|p| p.id != id || p.context != *context)
        {
            return Err(AudioError::new(
                "audio_status_completion",
                "Unknown, foreign or consumed AUDIO status completion",
            ));
        }
        let current = self.handle.shared.current(context);
        let _ = pending
            .take()
            .unwrap()
            .reply
            .send(accepted && current.is_ok());
        current
    }
    pub fn status(&self) -> Value {
        serde_json::json!({"closing":self.handle.shared.closed.load(Ordering::Acquire),"done":self.handle.shared.done.load(Ordering::Acquire),"joined":self.handle.shared.joined.load(Ordering::Acquire),"retained":self.handle.limits.retained-self.handle.slots.available_permits(),"retiring":1-self.handle.retire_slot.available_permits(),"staleStatuses":self.handle.shared.stale.load(Ordering::Acquire),"deliveredStatuses":self.handle.shared.delivered.load(Ordering::Acquire),"drainedOwners":self.handle.shared.drained.load(Ordering::Acquire),"terminal":self.handle.shared.error()})
    }
    #[cfg(test)]
    #[allow(dead_code)] // Used when this module is included by the focused integration test.
    pub(crate) fn test_gate(
        &self,
        accepted: bool,
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    ) {
        let gate = if accepted {
            &self.handle.shared.after_accept
        } else {
            &self.handle.shared.before_accept
        };
        *gate.lock().unwrap() = Some((entered, release));
    }
    pub async fn shutdown(&self) -> Result<()> {
        {
            let mut sender = self.handle.sender.lock().unwrap_or_else(|e| e.into_inner());
            self.handle.shared.closed.store(true, Ordering::Release);
            sender.take();
            self.handle
                .retire_sender
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            self.handle.shared.stop.cancel();
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(AudioError::new(
                "audio_runtime_required",
                "Shutdown closes admission but needs a Tokio runtime to await the retained drain",
            ));
        }
        let wait = async {
            loop {
                let changed = self.handle.shared.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.handle.shared.done.load(Ordering::Acquire) {
                    break;
                }
                changed.await;
            }
            let worker = self
                .handle
                .worker
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(worker) = worker {
                if worker.join().is_err() {
                    self.handle.shared.fail(AudioError::new(
                        "audio_host_panic",
                        "AUDIO thread join failed",
                    ));
                }
                self.handle.shared.joined.store(true, Ordering::Release);
                self.handle.shared.join_changed.notify_waiters();
            } else {
                loop {
                    let joined = self.handle.shared.join_changed.notified();
                    tokio::pin!(joined);
                    joined.as_mut().enable();
                    if self.handle.shared.joined.load(Ordering::Acquire) {
                        break;
                    }
                    joined.await;
                }
            }
        };
        tokio::time::timeout(self.handle.limits.shutdown_timeout, wait)
            .await
            .map_err(|_| {
                AudioError::new(
                    "audio_shutdown_timeout",
                    "Real AUDIO owners remain retained; shutdown can be retried",
                )
            })?;
        self.handle.shared.error().map_or(Ok(()), Err)
    }
}

fn context_bounds(context: &RuntimeContext) -> Result<()> {
    if context.runtime_id.len() > 256 || context.data_path.as_os_str().len() > 32768 {
        return Err(AudioError::new(
            "audio_input_limit",
            "Full runtime context exceeds bounds",
        ));
    }
    if context.data_path.to_str().is_none() {
        return Err(AudioError::new(
            "audio_unsupported_path",
            "Runtime context path is not Unicode",
        ));
    }
    Ok(())
}

fn bounded_meta(meta: &pipe::AudioMetadata, max: usize) -> Result<()> {
    fn visit(v: &Value, depth: usize, nodes: &mut usize, bytes: &mut usize) -> Result<()> {
        *nodes += 1;
        if depth > 8 || *nodes > 256 {
            return Err(AudioError::new(
                "audio_input_limit",
                "AUDIO metadata depth/nodes",
            ));
        }
        match v {
            Value::String(s) => *bytes = bytes.saturating_add(s.len()),
            Value::Array(a) => {
                for v in a {
                    visit(v, depth + 1, nodes, bytes)?
                }
            }
            Value::Object(o) => {
                for (k, v) in o {
                    *bytes = bytes.saturating_add(k.len());
                    visit(v, depth + 1, nodes, bytes)?
                }
            }
            _ => *bytes = bytes.saturating_add(16),
        }
        Ok(())
    }
    let mut bytes = 0;
    let mut nodes = 0;
    for input in [&meta.sample_rate, &meta.target_id] {
        if let pipe::JsInput::Json(v) = input {
            visit(v, 0, &mut nodes, &mut bytes)?;
        }
    }
    if let Some(v) = &meta.icecast {
        visit(v, 0, &mut nodes, &mut bytes)?;
    }
    if bytes > max {
        Err(AudioError::new("audio_input_limit", "AUDIO metadata bytes"))
    } else {
        Ok(())
    }
}

struct Catch<F: Future> {
    future: Pin<Box<F>>,
}
impl<F: Future> Catch<F> {
    fn new(future: F) -> Self {
        Self {
            future: Box::pin(future),
        }
    }
}
impl<F: Future> Future for Catch<F> {
    type Output = Result<F::Output>;
    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.future.as_mut().poll(cx)
        })) {
            Ok(p) => p.map(Ok),
            Err(_) => std::task::Poll::Ready(Err(AudioError::new(
                "audio_host_panic",
                "AUDIO owner future panicked",
            ))),
        }
    }
}
struct DeliveryJob {
    envelope: AudioStatusDelivery,
    reply: oneshot::Sender<Result<()>>,
}
fn delivery_thread(
    shared: Arc<Shared>,
    sink: Sink,
) -> Result<(std_mpsc::SyncSender<DeliveryJob>, JoinHandle<()>)> {
    let (sender, receiver) = std_mpsc::sync_channel::<DeliveryJob>(1);
    let thread = std::thread::Builder::new()
        .name("freeshow-audio-status".into())
        .spawn(move || {
            while let Ok(job) = receiver.recv() {
                let result = shared
                    .current(job.envelope.event.context.as_ref().unwrap())
                    .and_then(|_| {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            sink(job.envelope)
                        }))
                        .map_err(|_| {
                            AudioError::new(
                                "audio_renderer_panic",
                                "Trusted AUDIO status sink panicked",
                            )
                        })?
                        .map_err(|e| AudioError::new("audio_renderer_sink", e))
                    });
                let _ = job.reply.send(result);
            }
        })
        .map_err(|e| AudioError::new("audio_renderer_worker", e))?;
    Ok((sender, thread))
}
async fn bind_run(shared: &Shared, context: &RuntimeContext, receipt: &ice::Receipt) -> Result<()> {
    let mut runs = shared.runs.lock().unwrap_or_else(|e| e.into_inner());
    if !runs.contains_key(&receipt.run()) && runs.len() >= 33 {
        return Err(AudioError::new(
            "audio_status_limit",
            "Unconsumed Icecast run provenance exceeds bound",
        ));
    }
    runs.entry(receipt.run()).or_insert_with(|| context.clone());
    Ok(())
}
// Caller holds publication. Ownership follows the actual connect/run, never an
// arbitrary newest context or metadata-only operation.
fn bind_stream(shared: &Shared, context: &RuntimeContext, receipt: &ice::Receipt) -> Result<()> {
    let mut stream = shared.stream.lock().unwrap_or_else(|e| e.into_inner());
    match receipt.disposition() {
        ice::Disposition::ConnectingDropped | ice::Disposition::Written => {
            if stream
                .as_ref()
                .is_some_and(|s| s.run == receipt.run() && s.context != *context)
            {
                return Err(AudioError::new(
                    "audio_stream_context",
                    "An existing Icecast run cannot change full-context ownership",
                ));
            }
            *stream = Some(StreamBinding {
                context: context.clone(),
                run: receipt.run(),
            });
        }
        ice::Disposition::Disabled
        | ice::Disposition::Disconnected
        | ice::Disposition::CooldownDropped => *stream = None,
        ice::Disposition::Ignored | ice::Disposition::Updated => {}
    }
    Ok(())
}
// Caller holds publication; successful completion means EOS/shutdown was
// attempted with bounded writes and the actual old TcpStream has been dropped.
async fn disconnect_stream(
    shared: &Shared,
    icecast: &ice::IcecastService,
    epoch: u64,
    stream: &StreamBinding,
) -> Result<()> {
    let receipt = icecast
        .disconnect(ice::ContextId(epoch, 0), CancellationToken::new())
        .await
        .map_err(ie)?;
    bind_run(shared, &stream.context, &receipt).await?;
    *shared.stream.lock().unwrap_or_else(|e| e.into_inner()) = None;
    Ok(())
}
async fn retire_stream(
    shared: &Shared,
    icecast: &ice::IcecastService,
    epoch: u64,
    context: &RuntimeContext,
) -> Result<Option<u64>> {
    let _gate = shared.publication.lock().await;
    let stream = shared
        .stream
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    match stream {
        Some(stream) if stream.context == *context => {
            disconnect_stream(shared, icecast, epoch, &stream).await?;
            Ok(Some(stream.run))
        }
        _ => Ok(None),
    }
}
async fn status_pump(
    shared: Arc<Shared>,
    icecast: Arc<ice::IcecastService>,
    sender: std_mpsc::SyncSender<DeliveryJob>,
    timeout: Duration,
) -> Result<()> {
    let permits = Arc::new(Semaphore::new(2));
    loop {
        let mut batch = match icecast.next_events().await {
            Ok(events) => events,
            Err(e) if e.code == "shutdown" => break,
            Err(e) => return Err(ie(e)),
        };
        // Collect events produced while a method held the registration barrier too;
        // otherwise pruning an old run could discard its already-queued close status.
        let frozen = {
            let _gate = shared.publication.lock().await;
            batch.extend(icecast.take_events());
            let mut runs = shared.runs.lock().unwrap_or_else(|e| e.into_inner());
            let frozen = batch
                .into_iter()
                .map(|event| {
                    let context = runs.get(&event.run).cloned();
                    (event, context)
                })
                .collect::<Vec<_>>();
            if let Some(latest) = runs.keys().next_back().copied() {
                runs.retain(|run, _| *run == latest);
            }
            frozen
        };
        for (event, context) in frozen {
            let Some(context) = context else {
                shared.stale.fetch_add(1, Ordering::AcqRel);
                continue;
            };
            if shared.current(&context).is_err() {
                shared.stale.fetch_add(1, Ordering::AcqRel);
                continue;
            }
            let lease = Arc::new(permits.clone().try_acquire_owned().map_err(|_| {
                AudioError::new(
                    "audio_status_limit",
                    "Retained status envelopes exceed bound",
                )
            })?);
            let id = format!("native-audio-{}", uuid::Uuid::new_v4());
            let (reply, completion) = oneshot::channel();
            {
                let mut pending = shared.pending.lock().unwrap_or_else(|e| e.into_inner());
                if pending.is_some() {
                    return Err(AudioError::new(
                        "audio_status_limit",
                        "A status delivery is already pending",
                    ));
                }
                *pending = Some(PendingDelivery {
                    context: context.clone(),
                    id: id.clone(),
                    reply,
                });
            }
            let mut wire = Event::reply(
                "MAIN",
                "ICECAST_STATUS",
                serde_json::to_value(&event.data)
                    .map_err(|e| AudioError::new("audio_status_wire", e))?,
                None,
            );
            wire.context = Some(context.clone());
            if !bounded_wire(&wire) {
                return Err(AudioError::new(
                    "audio_status_wire_limit",
                    "Original AUDIO status plus context exceeds wire limit",
                ));
            }
            let envelope = AudioStatusDelivery {
                event: wire,
                id: id.clone(),
                run: event.run,
                own_undefined_error: event.data.error.is_none(),
                diagnostic: event.diagnostic,
                _lease: lease.clone(),
            };
            let (reply, synchronous) = oneshot::channel();
            if sender.try_send(DeliveryJob { envelope, reply }).is_err() {
                return Err(AudioError::new(
                    "audio_renderer_busy",
                    "Fixed AUDIO renderer queue is full or closed",
                ));
            }
            let result = tokio::time::timeout(timeout, async {
                let synchronous = synchronous.await.map_err(|_| {
                    AudioError::new(
                        "audio_renderer_worker",
                        "AUDIO sink ended without synchronous completion",
                    )
                })?;
                if let Err(e) = synchronous {
                    if e.code == "stale_library" {
                        return Ok(false);
                    }
                    return Err(e);
                }
                if shared.current(&context).is_err() {
                    return Ok(false);
                }
                completion.await.map_err(|_| {
                    AudioError::new(
                        "audio_status_completion",
                        "AUDIO private delivery completion was lost",
                    )
                })
            })
            .await
            .map_err(|_| {
                AudioError::new(
                    "audio_status_timeout",
                    "Real renderer status completion exceeded deadline",
                )
            });
            {
                let mut pending = shared.pending.lock().unwrap_or_else(|e| e.into_inner());
                if pending.as_ref().is_some_and(|p| p.id == id) {
                    pending.take();
                }
            }
            match result?? {
                true => {
                    shared.delivered.fetch_add(1, Ordering::AcqRel);
                }
                false => {
                    if shared.current(&context).is_ok() {
                        return Err(AudioError::new(
                            "audio_status_rejected",
                            "Current renderer rejected AUDIO status delivery",
                        ));
                    }
                    shared.stale.fetch_add(1, Ordering::AcqRel);
                }
            }
            drop(lease);
        }
    }
    Ok(())
}

fn observe_sink(intent: &pipe::SinkIntent, sinks: &pipe::SinkSnapshot) -> Effect {
    let no_op = match intent.kind() {
        pipe::SinkKind::NdiBroadcast | pipe::SinkKind::NdiTarget => sinks.ndi_keys().is_empty(),
        pipe::SinkKind::OmtBroadcast => sinks.omt_keys().is_empty(),
        pipe::SinkKind::BlackmagicBroadcast => sinks.blackmagic_keys().is_empty(),
        _ => false,
    };
    if no_op {
        Effect::SourceNoOp {
            sink: intent.kind(),
        }
    } else {
        Effect::Unsupported {
            sink: intent.kind(),
        }
    }
}
async fn process(
    shared: &Shared,
    pipeline: &pipe::AudioPipelineService,
    codec: &opus::OpusService,
    icecast: &ice::IcecastService,
    epoch: u64,
    job: &Job,
) -> Result<(Vec<Effect>, Vec<AudioError>, Option<AudioError>)> {
    shared.current(&job.context)?;
    if job.cancel.is_cancelled() {
        return Err(AudioError::new(
            "cancelled",
            "AUDIO cancelled before effect acceptance",
        ));
    }
    match &job.operation {
        Operation::Metadata(title) => {
            let _gate = shared.publication.lock().await;
            shared.current(&job.context)?;
            let observed = icecast
                .update_metadata_observed(ice::ContextId(epoch, 0), title, CancellationToken::new())
                .await
                .map_err(ie)?;
            let (receipt, diagnostic) = observed.into_parts();
            bind_run(shared, &job.context, &receipt).await?;
            Ok((
                vec![Effect::MetadataApplied { run: receipt.run() }],
                diagnostic.into_iter().map(ie).collect(),
                None,
            ))
        }
        Operation::Disconnect => {
            let _gate = shared.publication.lock().await;
            shared.current(&job.context)?;
            let receipt = icecast
                .disconnect(ice::ContextId(epoch, 0), CancellationToken::new())
                .await
                .map_err(ie)?;
            bind_run(shared, &job.context, &receipt).await?;
            bind_stream(shared, &job.context, &receipt)?;
            Ok((vec![], vec![], None))
        }
        Operation::Pcm { bytes, meta, sinks } => {
            let snapshot = sinks.clone().snapshot(epoch)?;
            let prepared = pipeline
                .prepare(bytes, meta, &snapshot, job.cancel.clone())
                .await
                .map_err(pe)?;
            #[cfg(test)]
            checkpoint(&shared.before_accept).await;
            shared.current(&job.context)?;
            let mut accepted = prepared.accept(pipe::ContextId(epoch, 0)).map_err(pe)?;
            #[cfg(test)]
            checkpoint(&shared.after_accept).await;
            let mut effects = accepted
                .planar_intents()
                .iter()
                .map(|intent| observe_sink(intent, accepted.sinks()))
                .collect::<Vec<_>>();
            let mut outcome = pipe::EncodingOutcome::AllSucceeded;
            for (index, frame) in accepted.opus_frames().iter().enumerate() {
                let encoded = codec
                    .encode(
                        opus::ContextId(epoch, 0),
                        frame.bytes(),
                        CancellationToken::new(),
                    )
                    .await;
                let packet = match encoded.and_then(|packet| {
                    packet.bytes(opus::ContextId(epoch, 0))?;
                    Ok(packet)
                }) {
                    Ok(packet) => packet,
                    Err(e) => {
                        let error = oe(e);
                        effects.push(Effect::Failure {
                            index,
                            stage: pipe::FailureStage::Encode,
                            error: error.clone(),
                        });
                        outcome = pipe::EncodingOutcome::Failed {
                            index,
                            stage: pipe::FailureStage::Encode,
                            message: error.message,
                        };
                        break;
                    }
                };
                // The sealed packet was checked above; its epoch cannot retire within
                // this single accepted host operation. Still reconcile a contract failure.
                let packet_bytes = match packet.bytes(opus::ContextId(epoch, 0)) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        let error = oe(e);
                        effects.push(Effect::Failure {
                            index,
                            stage: pipe::FailureStage::Encode,
                            error: error.clone(),
                        });
                        outcome = pipe::EncodingOutcome::Failed {
                            index,
                            stage: pipe::FailureStage::Encode,
                            message: error.message,
                        };
                        break;
                    }
                };
                effects.push(Effect::Encoded {
                    index,
                    sequence: packet.sequence(),
                    bytes: packet_bytes.len(),
                });
                let sent = {
                    let _gate = shared.publication.lock().await;
                    // A new group may be admitted after old guards drain but
                    // before the reserved retirement is selected. It must create
                    // a genuine new run, rather than adopt the still-live socket.
                    let old = shared
                        .stream
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    let sent = async {
                        if let Some(old) = old.filter(|s| s.context != job.context) {
                            disconnect_stream(shared, icecast, epoch, &old).await?;
                        }
                        icecast
                            .send_audio(
                                ice::ContextId(epoch, 0),
                                packet_bytes,
                                accepted.icecast_config(),
                                CancellationToken::new(),
                            )
                            .await
                            .map_err(ie)
                    }
                    .await;
                    if let Ok(receipt) = &sent {
                        if let Err(error) = async {
                            bind_run(shared, &job.context, receipt).await?;
                            bind_stream(shared, &job.context, receipt)
                        }
                        .await
                        {
                            effects.push(Effect::Icecast {
                                index,
                                disposition: receipt.disposition(),
                                run: receipt.run(),
                            });
                            shared.fail(error.clone());
                            Err(error)
                        } else {
                            sent
                        }
                    } else {
                        sent
                    }
                };
                match sent {
                    Ok(receipt) => effects.push(Effect::Icecast {
                        index,
                        disposition: receipt.disposition(),
                        run: receipt.run(),
                    }),
                    Err(error) => {
                        effects.push(Effect::Failure {
                            index,
                            stage: pipe::FailureStage::Send,
                            error: error.clone(),
                        });
                        outcome = pipe::EncodingOutcome::Failed {
                            index,
                            stage: pipe::FailureStage::Send,
                            message: error.message,
                        };
                        break;
                    }
                }
            }
            let finished = match pipeline
                .finish(&mut accepted, pipe::ContextId(epoch, 0), outcome)
                .await
            {
                Ok(finished) => finished,
                Err(e) => {
                    let error = pe(e);
                    shared.fail(error.clone());
                    return Ok((effects, vec![], Some(error)));
                }
            };
            for intent in finished.legacy_intents() {
                effects.push(observe_sink(intent, finished.sinks()));
            }
            let diagnostics = finished
                .diagnostics()
                .iter()
                .map(|e| AudioError::new(e.code, &e.message))
                .collect();
            if let Some(e) = codec.terminal_error() {
                shared.fail(oe(e));
            }
            if let Some(e) = icecast.terminal_error() {
                shared.fail(ie(e));
            }
            Ok((effects, diagnostics, None))
        }
    }
}

#[cfg(test)]
async fn checkpoint(gate: &Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>) {
    let gate = gate.lock().unwrap().take();
    if let Some((entered, release)) = gate {
        let _ = entered.send(());
        let _ = release.await;
    }
}
fn bounded_wire(event: &Event) -> bool {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > (256 * 1024usize).saturating_sub(self.0) {
                return Err(std::io::Error::other("AUDIO wire limit"));
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(0), event).is_ok()
}

async fn owner(
    shared: Arc<Shared>,
    mut receiver: mpsc::Receiver<Job>,
    mut retire_receiver: mpsc::Receiver<RetirementJob>,
    source: opus::LibrarySource,
    limits: AudioLimits,
    epoch: u64,
    sink: Sink,
) {
    let codec =
        match opus::OpusService::start(opus::ContextId(epoch, 0), source, limits.opus.clone()) {
            Ok(service) => Some(Arc::new(service)),
            Err(e) => {
                shared.fail(oe(e));
                None
            }
        };
    let icecast = match ice::IcecastService::start(ice::ContextId(epoch, 0), limits.icecast.clone())
    {
        Ok(service) => Some(Arc::new(service)),
        Err(e) => {
            shared.fail(ie(e));
            None
        }
    };
    let capability = match &codec {
        Some(codec) => {
            tokio::select! {biased;_=shared.stop.cancelled()=>opus::Capability::Unavailable(opus::Error{code:"shutdown",message:"AUDIO retired during initialization".into(),native_code:None}),capability=codec.capability()=>capability}
        }
        None => opus::Capability::Unavailable(opus::Error {
            code: "audio_codec_owner",
            message: "Actual Opus owner unavailable".into(),
            native_code: None,
        }),
    };
    *shared.capability.lock().unwrap_or_else(|e| e.into_inner()) = Some(capability.clone());
    shared.ready.notify_waiters();
    let pipeline = if shared.stop.is_cancelled() {
        None
    } else {
        match pipe::AudioPipelineService::new(
            pipe::PipelineConfig {
                queued_jobs: 1,
                retained_plans: 2,
                input_bytes: limits.input_bytes,
                buffer_bytes: 256 * 1024,
                max_frames: limits.frames,
                metadata_bytes: limits.metadata_bytes,
                prepared_ttl: Duration::from_secs(10),
                ..pipe::PipelineConfig::default()
            },
            capability.is_available(),
        ) {
            Ok(service) => Some(Arc::new(service)),
            Err(e) => {
                shared.fail(pe(e));
                None
            }
        }
    };
    let delivery = delivery_thread(shared.clone(), sink);
    let (mut delivery_sender, delivery_worker) = match delivery {
        Ok((sender, worker)) => (Some(sender), Some(worker)),
        Err(e) => {
            shared.fail(e);
            (None, None)
        }
    };
    let pump = match (&icecast, &delivery_sender) {
        (Some(icecast), Some(sender)) => {
            let state = shared.clone();
            let icecast = icecast.clone();
            let sender = sender.clone();
            let timeout = limits.delivery_timeout;
            Some(tokio::spawn(async move {
                let result = status_pump(state.clone(), icecast, sender, timeout).await;
                if let Err(e) = &result {
                    state.fail(e.clone());
                }
                result
            }))
        }
        _ => None,
    };
    let mut draining = false;
    let mut ordinary_done = false;
    let mut retire_done = false;
    let mut sequence = 0u64;
    while !ordinary_done || !retire_done {
        let job = tokio::select! {
            biased;
            _=shared.fault.cancelled(),if !draining=>{draining=true;receiver.close();retire_receiver.close();continue;},
            retirement=retire_receiver.recv(),if !retire_done=>{
                let Some(job) = retirement else {retire_done=true;continue};
                // Even a faulted ordinary pipeline must attempt accepted socket
                // cleanup; terminal drain subsequently joins every owner too.
                let result = if let Some(icecast) = &icecast {
                    match Catch::new(retire_stream(&shared, icecast, epoch, &job.context)).await {
                        Ok(Ok(run)) => Ok(AudioRetirementReceipt {context:job.context,run,disconnected:run.is_some(),_lease:job.lease}),
                        Ok(Err(e)) | Err(e) => {shared.fail(e.clone());Err(e)},
                    }
                } else {Err(shared.error().unwrap_or_else(||AudioError::new("audio_host_closed","Actual Icecast owner unavailable")))};
                let _ = job.reply.send(result);
                continue;
            },
            job=receiver.recv(),if !ordinary_done=>job
        };
        let Some(job) = job else {
            ordinary_done = true;
            continue;
        };
        let result = if let Some(e) = shared.error() {
            Err(e)
        } else if let (Some(pipeline), Some(codec), Some(icecast)) = (&pipeline, &codec, &icecast) {
            sequence = match sequence.checked_add(1) {
                Some(n) => n,
                None => {
                    shared.fail(AudioError::new(
                        "audio_sequence_limit",
                        "AUDIO sequence exhausted",
                    ));
                    let _ = job.reply.send(Err(shared.error().unwrap()));
                    continue;
                }
            };
            match Catch::new(process(&shared, pipeline, codec, icecast, epoch, &job)).await {
                Ok(result) => result.map(|(effects, diagnostics, failure)| AudioReceipt {
                    context: job.context,
                    sequence,
                    effects,
                    diagnostics,
                    failure,
                    _lease: job.lease,
                }),
                Err(e) => {
                    shared.fail(e.clone());
                    Err(e)
                }
            }
        } else {
            Err(AudioError::new(
                "audio_host_closed",
                "AUDIO initialization retired or unavailable",
            ))
        };
        let _ = job.reply.send(result);
    }
    async fn drain(shared: &Shared, index: u64, future: impl Future<Output = Result<()>>) {
        match Catch::new(future).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) | Err(e) => shared.fail(e),
        }
        shared.drained.fetch_or(index, Ordering::AcqRel);
    }
    tokio::join!(
        drain(&shared, 1, async {
            match &pipeline {
                Some(p) => p.shutdown().await.map_err(pe),
                None => Ok(()),
            }
        }),
        drain(&shared, 2, async {
            match &codec {
                Some(c) => c.shutdown().await.map_err(oe),
                None => Ok(()),
            }
        }),
        drain(&shared, 4, async {
            match &icecast {
                Some(i) => i.shutdown().await.map_err(ie),
                None => Ok(()),
            }
        })
    );
    if let Some(pump) = pump {
        match pump.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => shared.fail(e),
            Err(_) => shared.fail(AudioError::new(
                "audio_status_panic",
                "AUDIO status pump ended unexpectedly",
            )),
        }
    }
    delivery_sender.take();
    if let Some(worker) = delivery_worker {
        if worker.join().is_err() {
            shared.fail(AudioError::new(
                "audio_renderer_panic",
                "AUDIO delivery thread join failed",
            ));
        }
    }
    shared.drained.fetch_or(8, Ordering::AcqRel);
}
