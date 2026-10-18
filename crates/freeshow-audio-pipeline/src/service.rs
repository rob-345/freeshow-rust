use super::*;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Condvar, Mutex, Weak,
    },
    thread::JoinHandle,
    time::Instant,
};
use tokio::sync::{oneshot, Notify, OwnedSemaphorePermit, Semaphore};
pub(super) struct Payload {
    pub(super) context: ContextId,
    pub(super) sinks: SinkSnapshot,
    pub(super) pre: Vec<SinkIntent>,
    pub(super) legacy: Vec<SinkIntent>,
    frames: Vec<OpusPcmFrame>,
    config: Option<Arc<Value>>,
    _retained: OwnedSemaphorePermit,
}
enum Phase {
    Waiting,
    Accepted,
    Finishing,
    Finished,
    Abort(Error),
}
struct Completion {
    outcome: EncodingOutcome,
    response: oneshot::Sender<Result<FinishedAudio>>,
}
struct PendingState {
    phase: Phase,
    completion: Option<Completion>,
}
struct Pending {
    state: Mutex<PendingState>,
    changed: Condvar,
    deadline: Instant,
}
impl Pending {
    fn abort(&self, error: Error, accepted: bool) {
        let mut s = self.state.lock().unwrap();
        if matches!(s.phase, Phase::Waiting) || (accepted && matches!(s.phase, Phase::Accepted)) {
            s.phase = Phase::Abort(error);
            self.changed.notify_all();
        }
    }
}
pub struct PreparedAudio {
    pending: Option<Arc<Pending>>,
    payload: Arc<Payload>,
    owner: Arc<Inner>,
    request_cancel: CancellationToken,
    sequence: u64,
}
impl PreparedAudio {
    pub fn sinks(&self) -> &SinkSnapshot {
        &self.payload.sinks
    }
    pub fn context(&self) -> ContextId {
        self.payload.context
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn planar_intents(&self) -> &[SinkIntent] {
        &self.payload.pre
    }
    pub fn opus_frames(&self) -> &[OpusPcmFrame] {
        &self.payload.frames
    }
    pub fn icecast_config(&self) -> Option<&Value> {
        self.payload.config.as_deref()
    }
    pub fn accept(mut self, context: ContextId) -> Result<AcceptedAudio> {
        if context != self.payload.context {
            return Err(Error::new(
                "stale_audio_context",
                "Audio context differs from frozen packet",
            ));
        }
        let pending = self.pending.as_ref().unwrap();
        let mut state = pending.state.lock().unwrap();
        if self.owner.closing.load(Ordering::Acquire) {
            return Err(Error::shutdown());
        }
        if self.request_cancel.is_cancelled() {
            return Err(Error::cancelled());
        }
        if Instant::now() >= pending.deadline {
            return Err(Error::new("audio_plan_expired", "Prepared audio expired"));
        }
        if !matches!(state.phase, Phase::Waiting) {
            return Err(phase_error(&state.phase));
        }
        state.phase = Phase::Accepted;
        pending.changed.notify_all();
        drop(state);
        Ok(AcceptedAudio {
            pending: self.pending.take(),
            payload: self.payload.clone(),
            owner: self.owner.clone(),
            sequence: self.sequence,
        })
    }
}
impl Drop for PreparedAudio {
    fn drop(&mut self) {
        if let Some(p) = self.pending.take() {
            p.abort(Error::cancelled(), false);
        }
    }
}
pub struct AcceptedAudio {
    pending: Option<Arc<Pending>>,
    payload: Arc<Payload>,
    owner: Arc<Inner>,
    sequence: u64,
}
impl AcceptedAudio {
    pub fn sinks(&self) -> &SinkSnapshot {
        &self.payload.sinks
    }
    pub fn context(&self) -> ContextId {
        self.payload.context
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn planar_intents(&self) -> &[SinkIntent] {
        &self.payload.pre
    }
    pub fn opus_frames(&self) -> &[OpusPcmFrame] {
        &self.payload.frames
    }
    pub fn icecast_config(&self) -> Option<&Value> {
        self.payload.config.as_deref()
    }
    pub fn ensure_current(&self, context: ContextId) -> Result<()> {
        if context != self.payload.context {
            return Err(Error::new(
                "stale_audio_context",
                "Accepted audio context differs",
            ));
        }
        let p = self
            .pending
            .as_ref()
            .ok_or_else(|| Error::new("duplicate_completion", "Audio already completed"))?;
        let state = p.state.lock().unwrap();
        if !matches!(state.phase, Phase::Accepted) {
            return Err(phase_error(&state.phase));
        }
        if Instant::now() >= p.deadline {
            return Err(Error::new(
                "audio_owner_timeout",
                "Accepted audio deadline exceeded; partial effects require host reconciliation",
            ));
        }
        Ok(())
    }
}
impl Drop for AcceptedAudio {
    fn drop(&mut self) {
        if let Some(p) = self.pending.take() {
            p.abort(
                Error::new(
                    "audio_owner_abandoned",
                    "Accepted audio dropped with unknown partial sink effects",
                ),
                true,
            );
        }
    }
}
fn phase_error(phase: &Phase) -> Error {
    match phase {
        Phase::Abort(e) => e.clone(),
        Phase::Waiting => Error::new("audio_not_accepted", "Audio owner acceptance required"),
        _ => Error::new(
            "duplicate_completion",
            "Audio is already accepted/completing/completed",
        ),
    }
}
struct Work {
    bytes: Arc<[u8]>,
    meta: AudioMetadata,
    sinks: SinkSnapshot,
    cancel: CancellationToken,
    wake: Arc<Pending>,
    response: oneshot::Sender<Result<PreparedAudio>>,
    _job: OwnedSemaphorePermit,
}
struct Inner {
    config: PipelineConfig,
    opus: bool,
    closing: AtomicBool,
    cancel: CancellationToken,
    jobs: Arc<Semaphore>,
    retained: Arc<Semaphore>,
    sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    pending: Mutex<Option<Weak<Pending>>>,
    done: AtomicBool,
    notify: Notify,
    terminal: Mutex<Option<Error>>,
    #[cfg(test)]
    hooks: Hooks,
}
#[cfg(test)]
#[derive(Default)]
struct Hooks {
    before: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    after: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    worker: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
struct WorkerExit(Weak<Inner>);
impl Drop for WorkerExit {
    fn drop(&mut self) {
        if let Some(inner) = self.0.upgrade() {
            if std::thread::panicking() || !inner.closing.load(Ordering::Acquire) {
                let error = Error::new(
                    "audio_worker_error",
                    "Audio owner exited unexpectedly; partial effects require reconciliation",
                );
                *inner
                    .terminal
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.clone());
                inner.terminal.clear_poison();
                inner.closing.store(true, Ordering::Release);
                inner.cancel.cancel();
                inner
                    .sender
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                inner.sender.clear_poison();
                let pending = inner
                    .pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .and_then(Weak::upgrade);
                inner.pending.clear_poison();
                if let Some(p) = pending {
                    let mut state = p
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.completion.take();
                    state.phase = Phase::Abort(error);
                    p.state.clear_poison();
                    p.changed.notify_all();
                }
            }
            inner.done.store(true, Ordering::Release);
            inner.notify.notify_waiters();
        }
    }
}
#[derive(Clone)]
pub struct AudioPipelineService {
    inner: Arc<Inner>,
}
struct RequestDrop {
    wake: Arc<Pending>,
    cancel: CancellationToken,
    armed: bool,
}
impl Drop for RequestDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancel.cancel();
            self.wake.abort(Error::cancelled(), false);
        }
    }
}
impl AudioPipelineService {
    /// The host must report an actual available encoder before setting this
    /// capability. This crate cannot discover or encode Opus itself.
    pub fn new(config: PipelineConfig, opus_available: bool) -> Result<Self> {
        config.validate()?;
        let (sender, receive) = mpsc::sync_channel(config.queued_jobs);
        let inner = Arc::new(Inner {
            jobs: Arc::new(Semaphore::new(config.queued_jobs + 1)),
            retained: Arc::new(Semaphore::new(config.retained_plans)),
            config,
            opus: opus_available,
            closing: AtomicBool::new(false),
            cancel: CancellationToken::new(),
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(None),
            pending: Mutex::new(None),
            done: AtomicBool::new(false),
            notify: Notify::new(),
            terminal: Mutex::new(None),
            #[cfg(test)]
            hooks: Hooks::default(),
        });
        let weak = Arc::downgrade(&inner);
        let worker = std::thread::Builder::new()
            .name("freeshow-audio-pipeline".into())
            .spawn(move || {
                let _exit = WorkerExit(weak.clone());
                run(weak, receive)
            })
            .map_err(|e| Error::new("audio_worker_error", e.to_string()))?;
        *inner.worker.lock().unwrap() = Some(worker);
        Ok(Self { inner })
    }
    pub fn is_audio_enabled(&self) -> bool {
        self.inner.opus
    }
    pub async fn prepare(
        &self,
        bytes: &[u8],
        metadata: &AudioMetadata,
        sinks: &SinkSnapshot,
        cancel: CancellationToken,
    ) -> Result<PreparedAudio> {
        self.check_open()?;
        if bytes.len() > self.inner.config.input_bytes {
            return Err(Error::resource("Audio input limit"));
        }
        js::metadata(metadata, &self.inner.config)?;
        if cancel.is_cancelled() {
            return Err(Error::cancelled());
        }
        #[cfg(test)]
        if let Some(hook) = self.inner.hooks.before.lock().unwrap().take() {
            hook();
        }
        let job = self.inner.jobs.clone().try_acquire_owned().map_err(|_| {
            if self.inner.closing.load(Ordering::Acquire) {
                Error::shutdown()
            } else {
                Error::new("audio_pipeline_busy", "Audio admission capacity exhausted")
            }
        })?;
        #[cfg(test)]
        if let Some(hook) = self.inner.hooks.after.lock().unwrap().take() {
            hook();
        }
        let (response, mut guard) = {
            let mut sender = self.inner.sender.lock().unwrap();
            self.check_open()?;
            let send = sender.as_mut().ok_or_else(Error::shutdown)?;
            let wake = Arc::new(Pending {
                state: Mutex::new(PendingState {
                    phase: Phase::Waiting,
                    completion: None,
                }),
                changed: Condvar::new(),
                deadline: Instant::now() + self.inner.config.prepared_ttl,
            });
            let (request, response) = oneshot::channel();
            let local = cancel.child_token();
            let guard = RequestDrop {
                wake: wake.clone(),
                cancel: local.clone(),
                armed: true,
            };
            let work = Work {
                bytes: Arc::from(bytes),
                meta: metadata.clone(),
                sinks: sinks.clone(),
                cancel: local,
                wake,
                response: request,
                _job: job,
            };
            send.try_send(work).map_err(|e| match e {
                mpsc::TrySendError::Full(_) => {
                    Error::new("audio_pipeline_busy", "Audio FIFO exhausted")
                }
                mpsc::TrySendError::Disconnected(_) => Error::shutdown(),
            })?;
            (response, guard)
        };
        let result = tokio::select! {biased;_ = cancel.cancelled()=>Err(Error::cancelled()),_ = self.inner.cancel.cancelled()=>Err(self.check_open().err().unwrap_or_else(Error::shutdown)),v=response=>v.map_err(|_|Error::new("audio_worker_error","Audio worker ended without reply"))?};
        if result.is_ok() {
            guard.armed = false;
        }
        result
    }
    fn check_open(&self) -> Result<()> {
        if let Some(e) = self.inner.terminal.lock().unwrap().as_ref() {
            return Err(e.clone());
        }
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(Error::shutdown());
        }
        Ok(())
    }
    pub async fn finish(
        &self,
        accepted: &mut AcceptedAudio,
        context: ContextId,
        outcome: EncodingOutcome,
    ) -> Result<FinishedAudio> {
        if !Arc::ptr_eq(&self.inner, &accepted.owner) {
            return Err(Error::new(
                "foreign_audio_plan",
                "Audio belongs to another owner",
            ));
        }
        accepted.ensure_current(context)?;
        if let EncodingOutcome::Failed { index, message, .. } = &outcome {
            if *index >= accepted.payload.frames.len() {
                return Err(Error::new(
                    "invalid_completion",
                    "Failure index is outside prepared frames",
                ));
            }
            if message.len() > 2048 {
                return Err(Error::resource("Encoder failure message"));
            }
        }
        let receive = {
            let p = accepted.pending.as_ref().unwrap();
            let mut state = p.state.lock().unwrap();
            if !matches!(state.phase, Phase::Accepted) {
                return Err(phase_error(&state.phase));
            }
            if Instant::now() >= p.deadline {
                return Err(Error::new(
                    "audio_owner_timeout",
                    "Completion arrived after accepted audio deadline",
                ));
            }
            let (send, receive) = oneshot::channel();
            state.phase = Phase::Finishing;
            state.completion = Some(Completion {
                outcome,
                response: send,
            });
            p.changed.notify_all();
            receive
        };
        accepted.pending.take();
        // Once submitted this transition survives a dropped awaiting caller.
        receive
            .await
            .map_err(|_| Error::new("audio_worker_error", "Completion owner ended"))?
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.closing.store(true, Ordering::Release);
        self.inner.sender.lock().unwrap().take();
        self.inner.cancel.cancel();
        if let Some(p) = self
            .inner
            .pending
            .lock()
            .unwrap()
            .as_ref()
            .and_then(Weak::upgrade)
        {
            let mut state = p.state.lock().unwrap();
            if matches!(state.phase, Phase::Accepted | Phase::Finishing) {
                p.changed.notify_all();
                return Err(Error::new(
                    "audio_commit_in_progress",
                    "Drain accepted audio owner work before shutdown",
                ));
            }
            if matches!(state.phase, Phase::Waiting) {
                state.phase = Phase::Abort(Error::shutdown());
            }
            p.changed.notify_all();
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(Error::new(
                "runtime_required",
                "Async worker drain requires a Tokio runtime",
            ));
        }
        loop {
            let done = self.inner.notify.notified();
            tokio::pin!(done);
            done.as_mut().enable();
            if self.inner.done.load(Ordering::Acquire) {
                break;
            }
            done.await;
        }
        // Worker signals just before returning. Joining can include final TLS /
        // destructors; no zero-blocking guarantee is made for that final step.
        if let Some(worker) = self.inner.worker.lock().unwrap().take() {
            worker
                .join()
                .map_err(|_| Error::new("audio_worker_error", "Audio owner panicked"))?;
        }
        Ok(())
    }
    pub fn terminal_error(&self) -> Option<Error> {
        self.inner.terminal.lock().unwrap().clone()
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Release);
        self.cancel.cancel();
        self.sender.get_mut().unwrap().take();
        if let Some(p) = self
            .pending
            .get_mut()
            .unwrap()
            .as_ref()
            .and_then(Weak::upgrade)
        {
            p.abort(Error::shutdown(), true);
            p.changed.notify_all();
        }
    }
}
fn run(owner: Weak<Inner>, receive: mpsc::Receiver<Work>) {
    let mut state = engine::State::default();
    let mut sequence = 0u64;
    while let Ok(work) = receive.recv() {
        let Some(inner) = owner.upgrade() else {
            break;
        };
        #[cfg(test)]
        if let Some(hook) = inner.hooks.worker.lock().unwrap().take() {
            hook();
        }
        if inner.closing.load(Ordering::Acquire) {
            let _ = work.response.send(Err(Error::shutdown()));
            continue;
        }
        if Instant::now() >= work.wake.deadline {
            let _ = work.response.send(Err(Error::new(
                "audio_plan_expired",
                "Queued audio deadline exceeded",
            )));
            continue;
        }
        let retained = match inner.retained.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                let _ = work.response.send(Err(Error::new(
                    "audio_pipeline_busy",
                    "Retained audio buffers are still leased",
                )));
                continue;
            }
        };
        let mut budget = engine::Budget::new(&inner.config, &work.cancel, &inner.cancel);
        let result = engine::prepare(
            work.bytes,
            &work.meta,
            &work.sinks,
            inner.opus,
            &state,
            &mut budget,
        );
        let transition = match result {
            Ok(t) => t,
            Err(e) => {
                let _ = work.response.send(Err(e));
                continue;
            }
        };
        if work.cancel.is_cancelled() {
            let _ = work.response.send(Err(Error::cancelled()));
            continue;
        }
        if Instant::now() >= work.wake.deadline {
            let _ = work.response.send(Err(Error::new(
                "audio_plan_expired",
                "Audio preparation deadline exceeded",
            )));
            continue;
        }
        sequence = match sequence.checked_add(1) {
            Some(n) => n,
            None => {
                let _ = work
                    .response
                    .send(Err(Error::resource("Audio sequence exhausted")));
                continue;
            }
        };
        let payload = Arc::new(Payload {
            context: work.sinks.context,
            sinks: work.sinks,
            pre: transition.pre,
            legacy: transition.legacy,
            frames: transition.frames,
            config: transition.config,
            _retained: retained,
        });
        let pending = work.wake;
        *inner.pending.lock().unwrap() = Some(Arc::downgrade(&pending));
        let prepared = PreparedAudio {
            pending: Some(pending.clone()),
            payload: payload.clone(),
            owner: inner.clone(),
            request_cancel: work.cancel.clone(),
            sequence,
        };
        let _ = work.response.send(Ok(prepared));
        let resolution = wait(&pending, &inner);
        *inner.pending.lock().unwrap() = None;
        match resolution {
            Ok(completion) => {
                let resolved = engine::resolve(
                    transition.next,
                    &transition.combined,
                    payload.frames.len(),
                    &completion.outcome,
                    payload.config.is_some(),
                );
                match resolved {
                    Ok((next, diagnostics)) => {
                        state = next;
                        let _ = completion.response.send(Ok(FinishedAudio {
                            payload,
                            diagnostics,
                            #[cfg(any(test, feature = "test-harness"))]
                            state: state.clone(),
                        }));
                    }
                    Err(e) => {
                        let _ = completion.response.send(Err(e));
                    }
                }
            }
            Err((error, terminal)) => {
                if terminal {
                    *inner.terminal.lock().unwrap() = Some(error);
                    inner.closing.store(true, Ordering::Release);
                    inner.sender.lock().unwrap().take();
                    inner.cancel.cancel();
                }
            }
        }
    }
}
fn wait(p: &Pending, inner: &Inner) -> std::result::Result<Completion, (Error, bool)> {
    let mut state = p.state.lock().unwrap();
    loop {
        if let Some(completion) = state.completion.take() {
            state.phase = Phase::Finished;
            return Ok(completion);
        }
        if let Phase::Abort(error) = &state.phase {
            return Err((error.clone(), error.code == "audio_owner_abandoned"));
        }
        if inner.closing.load(Ordering::Acquire) && matches!(state.phase, Phase::Waiting) {
            let error = Error::shutdown();
            state.phase = Phase::Abort(error.clone());
            return Err((error, false));
        }
        let now = Instant::now();
        if now >= p.deadline {
            let accepted = matches!(state.phase, Phase::Accepted | Phase::Finishing);
            let error=Error::new(if accepted{"audio_owner_timeout"}else{"audio_plan_expired"},"Pending audio deadline exceeded; accepted partial effects require host reconciliation");
            state.phase = Phase::Abort(error.clone());
            return Err((error, accepted));
        }
        let (left, _) = p.changed.wait_timeout(state, p.deadline - now).unwrap();
        state = left;
    }
}
#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
