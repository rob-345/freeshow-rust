//! Native capabilities for a bounded continuation on the existing IO owner.
use super::{unavailable, LibraryCommit};
use crate::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    Error, Result,
};
use serde::Serialize;
use serde_json::Value;
use std::{
    io::{self, Write},
    sync::{atomic::Ordering, Arc, Mutex, Weak},
};
use tokio::sync::{mpsc, oneshot, watch, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};

pub const PHASE_CAPACITY: usize = 4;
pub const PHASE_JOB_CAPACITY: usize = 4;
pub const PHASE_JOB_LIMIT: usize = 32;
pub const PHASE_FINISH_CAPACITY: usize = 4;
pub const PHASE_REQUEST_BYTES: usize = 64 * 1024;
pub const PHASE_REPLY_BYTES: usize = 1024 * 1024;
const JSON_DEPTH: usize = 32;
const JSON_NODES: usize = 16_384;
const REPLY_EVENTS: usize = 32;

/// A native-only, non-cloneable phase capability. It cannot be deserialized.
///
/// Dropping it seals submission, but does not cancel accepted jobs. `close`
/// needs no work/result slot; `finish` additionally waits for accepted IO and
/// release of normal admission. Retained tickets/results keep their quotas
/// after that fence, without holding normal admission.
///
/// ```compile_fail
/// use freeshow_core::service::LibraryPhase;
/// let forged: LibraryPhase = serde_json::from_str("{}").unwrap();
/// ```
pub struct LibraryPhase {
    state: Arc<State>,
}

/// A single accepted operation, including its completed-but-uncollected reply.
/// Dropping the ticket never cancels accepted IO.
pub struct PhaseTicket {
    received: oneshot::Receiver<Reply>,
}

struct Reply {
    result: Result<Vec<Event>>,
    _slot: OwnedSemaphorePermit,
    _phase: Arc<State>,
}

struct Gate {
    sender: Option<mpsc::Sender<Work>>,
    admitted: usize,
    entered: bool,
    admission: Option<OwnedMutexGuard<()>>,
}

struct State {
    context: RuntimeContext,
    gate: Mutex<Gate>,
    slots: Arc<Semaphore>,
    observers: Arc<Semaphore>,
    sequence: Arc<std::sync::atomic::AtomicU64>,
    completed: watch::Receiver<bool>,
    _slot: OwnedSemaphorePermit,
}

pub(super) struct Registry {
    slots: Arc<Semaphore>,
    state: Mutex<RegistryState>,
}

struct RegistryState {
    closing: bool,
    phases: Vec<Weak<State>>,
}

pub(super) struct Owner {
    state: Option<Arc<State>>,
    pub receiver: mpsc::Receiver<Work>,
    completed: watch::Sender<bool>,
}

pub(super) struct Work {
    pub request: Request,
    pub operation: Option<LibraryCommit>,
    pub sequence: u64,
    reply: oneshot::Sender<Reply>,
    slot: OwnedSemaphorePermit,
    state: Arc<State>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(PHASE_CAPACITY)),
            state: Mutex::new(RegistryState {
                closing: false,
                phases: Vec::with_capacity(PHASE_CAPACITY),
            }),
        }
    }

    pub fn create(
        &self,
        request: &Request,
        window: &str,
        sequence: Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<(LibraryPhase, Owner)> {
        validate_request(request, window)?;
        let context = request.context.clone().ok_or_else(stale)?;
        let mut registry = self
            .state
            .lock()
            .map_err(|_| unavailable("phase registry poisoned"))?;
        if registry.closing {
            return Err(closed());
        }
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| resource("all phase leases are retained"))?;
        let (sender, receiver) = mpsc::channel(PHASE_JOB_CAPACITY);
        let (completed, progress) = watch::channel(false);
        let state = Arc::new(State {
            context,
            gate: Mutex::new(Gate {
                sender: Some(sender),
                admitted: 0,
                entered: false,
                admission: None,
            }),
            slots: Arc::new(Semaphore::new(PHASE_JOB_CAPACITY)),
            observers: Arc::new(Semaphore::new(PHASE_FINISH_CAPACITY)),
            sequence,
            completed: progress,
            _slot: slot,
        });
        registry.phases.retain(|phase| phase.strong_count() != 0);
        registry.phases.push(Arc::downgrade(&state));
        Ok((
            LibraryPhase {
                state: state.clone(),
            },
            Owner {
                state: Some(state),
                receiver,
                completed,
            },
        ))
    }

    /// Terminal control is independent of normal admission and all quotas.
    /// A host must settle its dependencies before this terminal write fence;
    /// continuations not yet submitted to a phase are explicitly rejected.
    pub fn close(&self) {
        let mut registry = self.state.lock().unwrap_or_else(|error| error.into_inner());
        registry.closing = true;
        registry.phases.retain(|weak| {
            if let Some(state) = weak.upgrade() {
                state.close();
                true
            } else {
                false
            }
        });
    }

    pub fn status(&self) -> (usize, usize, usize, bool) {
        let registry = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let jobs = registry
            .phases
            .iter()
            .filter_map(Weak::upgrade)
            .map(|state| PHASE_JOB_CAPACITY - state.slots.available_permits())
            .sum();
        let observers = registry
            .phases
            .iter()
            .filter_map(Weak::upgrade)
            .map(|state| PHASE_FINISH_CAPACITY - state.observers.available_permits())
            .sum();
        (
            PHASE_CAPACITY - self.slots.available_permits(),
            jobs,
            observers,
            registry.closing,
        )
    }
}

impl State {
    fn close(&self) {
        let mut gate = self.gate.lock().unwrap_or_else(|error| error.into_inner());
        gate.sender.take();
        // Before the owner enters there cannot be a published LibraryPhase or
        // accepted subjob. In particular, a reserved mpsc permit can publish
        // into a closed receiver: its retained Job must never retain admission.
        if !gate.entered {
            gate.admission.take();
        }
    }

    fn release_admission(&self) {
        self.gate
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .admission
            .take();
    }
}

impl LibraryPhase {
    pub(super) fn attach_admission(&self, admission: OwnedMutexGuard<()>) -> Result<()> {
        let mut gate = self
            .state
            .gate
            .lock()
            .map_err(|_| unavailable("phase gate poisoned"))?;
        if gate.sender.is_none() {
            return Err(closed());
        }
        if gate.entered || gate.admission.is_some() {
            return Err(unavailable("phase admission already attached"));
        }
        gate.admission = Some(admission);
        Ok(())
    }

    pub fn context(&self) -> &RuntimeContext {
        &self.state.context
    }

    /// Immediate admission: accepted + ready/uncollected jobs consume four
    /// whole leases. A phase admits at most 32 operations in its lifetime.
    /// The closure is trusted native code, never a renderer-selected callback.
    /// Each accepted operation validates this exact context again on the owner.
    pub fn submit(
        &self,
        request: Request,
        operation: impl FnOnce(&mut Runtime) -> Result<Vec<Event>> + Send + 'static,
    ) -> Result<PhaseTicket> {
        validate_request(&request, "")?;
        if request.context.as_ref() != Some(&self.state.context) {
            return Err(stale());
        }
        let mut gate = self
            .state
            .gate
            .lock()
            .map_err(|_| unavailable("phase gate poisoned"))?;
        let sender = gate.sender.as_ref().ok_or_else(closed)?;
        if gate.admitted == PHASE_JOB_LIMIT {
            return Err(resource("phase lifetime job limit reached"));
        }
        // Reserving the private channel and result slot is nonblocking. No
        // permit can be held by a suspended sender after close has sealed it.
        let slot = self
            .state
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| resource("all phase job results are retained"))?;
        let permit = sender.try_reserve().map_err(|error| match error {
            mpsc::error::TrySendError::Closed(_) => unavailable("phase owner stopped"),
            mpsc::error::TrySendError::Full(_) => resource("phase job queue is full"),
        })?;
        // Normal admission is held by the Phase owner. This short gate orders
        // its own subjobs, and publishes before enqueue without a suspension.
        let sequence = self
            .state
            .sequence
            .load(Ordering::Acquire)
            .checked_add(1)
            .ok_or_else(|| unavailable("request sequence exhausted"))?;
        let (reply, received) = oneshot::channel();
        self.state.sequence.store(sequence, Ordering::Release);
        permit.send(Work {
            request,
            operation: Some(Box::new(operation)),
            sequence,
            reply,
            slot,
            state: self.state.clone(),
        });
        gate.admitted += 1;
        Ok(PhaseTicket { received })
    }

    /// Seal immediately; this never waits for or consumes a work/result slot.
    pub fn close(&self) {
        self.state.close();
    }

    pub(super) fn is_closed(&self) -> bool {
        self.state
            .gate
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sender
            .is_none()
    }

    /// Close and wait for actual accepted jobs plus admission release. Dropping
    /// this wait cannot discard the worker-owned phase, jobs, or admission.
    /// This is a phase fence, not termination/join of the process IO thread.
    pub async fn finish(&self) -> Result<()> {
        self.close();
        if *self.state.completed.borrow() {
            return Ok(());
        }
        // Closing must work even when these independent observation slots are
        // exhausted. No waiting admission queue or replacement owner is used.
        let _observer = self
            .state
            .observers
            .clone()
            .try_acquire_owned()
            .map_err(|_| resource("all phase finish observers are retained"))?;
        let mut completed = self.state.completed.clone();
        completed
            .wait_for(|done| *done)
            .await
            .map_err(|_| unavailable("phase owner stopped before completion"))?;
        Ok(())
    }
}

impl Drop for LibraryPhase {
    fn drop(&mut self) {
        self.close();
    }
}

impl PhaseTicket {
    pub async fn finish(self) -> Result<Vec<Event>> {
        let reply = self
            .received
            .await
            .map_err(|_| unavailable("phase owner stopped before replying"))?;
        reply.result
    }
}

impl Owner {
    pub fn enter(&self) -> bool {
        let mut gate = self
            .state
            .as_ref()
            .expect("phase owner consumed once")
            .gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if gate.sender.is_none() || gate.admission.is_none() || gate.entered {
            return false;
        }
        gate.entered = true;
        true
    }

    pub fn complete(mut self) {
        let state = self.state.take().expect("phase owner consumed once");
        state.release_admission();
        // The completion also retires the owner's whole-phase lease. Only
        // genuine host-retained handles/work/results may keep it charged now.
        drop(state);
        self.completed.send_replace(true);
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        // Unwind/receiver teardown cannot strand normal admission. Unlike
        // complete(), this does not certify success: the watch sender closes.
        if let Some(state) = &self.state {
            state.release_admission();
        }
    }
}

impl Work {
    pub fn finish(self, result: Result<Vec<Event>>) {
        let _ = self.reply.send(Reply {
            result,
            _slot: self.slot,
            _phase: self.state,
        });
    }
}

fn stale() -> Error {
    Error::StaleLibrary {
        recovery_path: None,
        recovery_error: None,
    }
}

pub(super) fn closed() -> Error {
    Error::NativeOperation {
        code: "library_phase_closed",
        message: "library phase is closed".into(),
    }
}

fn resource(message: &str) -> Error {
    Error::NativeOperation {
        code: "library_phase_resource",
        message: message.into(),
    }
}

fn validate_request(request: &Request, window: &str) -> Result<()> {
    let mut limits = JsonLimits::new(PHASE_REQUEST_BYTES);
    limits.text(window)?;
    limits.text(&request.channel)?;
    if let Some(listener) = &request.listener_id {
        limits.text(listener)?;
    }
    if let Some(context) = &request.context {
        limits.context(context)?;
    }
    limits.value(&request.data)?;
    bounded_json(request, PHASE_REQUEST_BYTES - window.len())
}

pub(super) fn validate_events(events: &[Event]) -> Result<()> {
    if events.len() > REPLY_EVENTS {
        return Err(resource("too many phase reply events after operation"));
    }
    let mut limits = JsonLimits::new(PHASE_REPLY_BYTES);
    for event in events {
        limits.text(&event.channel)?;
        limits.text(&event.message.channel)?;
        if let Some(profile) = &event.message.auto_profile {
            limits.text(profile)?;
        }
        if let Some(listener) = &event.listener_id {
            limits.text(listener)?;
        }
        if let Some(context) = &event.context {
            limits.context(context)?;
        }
        for value in [
            event.message.data.as_ref(),
            event.native_assets.as_ref(),
            event.native_capture.as_ref(),
            event.native_exif.as_ref(),
            event.native_remote.as_ref(),
            event.native_midi.as_ref(),
            event.native_audio.as_ref(),
            event.native_timecode.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            limits.value(value)?;
        }
    }
    bounded_json(events, PHASE_REPLY_BYTES)
}

struct JsonLimits {
    bytes: usize,
    nodes: usize,
}

impl JsonLimits {
    fn new(bytes: usize) -> Self {
        Self {
            bytes,
            nodes: JSON_NODES,
        }
    }

    fn text(&mut self, text: &str) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_sub(text.len())
            .ok_or_else(|| resource("phase JSON text limit exceeded"))?;
        Ok(())
    }

    fn context(&mut self, context: &RuntimeContext) -> Result<()> {
        self.text(&context.runtime_id)?;
        self.text(
            context
                .data_path
                .to_str()
                .ok_or_else(|| resource("phase context path is not Unicode"))?,
        )
    }

    fn value(&mut self, value: &Value) -> Result<()> {
        let mut stack = vec![(value, 0)];
        while let Some((value, depth)) = stack.pop() {
            self.nodes = self
                .nodes
                .checked_sub(1)
                .ok_or_else(|| resource("phase JSON node limit exceeded"))?;
            if depth > JSON_DEPTH {
                return Err(resource("phase JSON depth limit exceeded"));
            }
            let children = match value {
                Value::Array(values) => values.len(),
                Value::Object(values) => values.len(),
                _ => 0,
            };
            if children > self.nodes || stack.len() > self.nodes - children {
                return Err(resource("phase JSON node limit exceeded"));
            }
            match value {
                Value::String(text) => self.text(text)?,
                Value::Array(values) => stack.extend(values.iter().map(|value| (value, depth + 1))),
                Value::Object(values) => {
                    for key in values.keys() {
                        self.text(key)?;
                    }
                    stack.extend(values.values().map(|value| (value, depth + 1)));
                }
                _ => (),
            }
        }
        Ok(())
    }
}

struct Budget(usize);

impl Write for Budget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.0 {
            return Err(io::Error::other("phase JSON byte limit exceeded"));
        }
        self.0 -= bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn bounded_json(value: &(impl Serialize + ?Sized), limit: usize) -> Result<()> {
    serde_json::to_writer(&mut Budget(limit), value)
        .map_err(|_| resource("phase JSON byte/encoding limit exceeded"))
}
