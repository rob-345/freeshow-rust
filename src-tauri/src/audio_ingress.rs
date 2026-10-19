//! Native raw AUDIO admission/ownership. Never an original renderer success ACK.
use crate::audio_pipeline::{AudioError, AudioReceipt, NativeAudioPipeline, SinkAvailability};
use freeshow_audio_pipeline::{AudioMetadata, JsInput};
use freeshow_core::{protocol::RuntimeContext, service::LibraryService};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::VecDeque,
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
    time::Duration,
};
use tokio::{
    sync::{Notify, OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub const CAPACITY: usize = 4;
pub const MAX_PCM_BYTES: usize = 32768;
static INGRESS_OWNER: AtomicBool = AtomicBool::new(false);
type Result<T> = std::result::Result<T, IngressError>;
#[derive(Clone, Debug, Serialize)]
pub struct IngressError {
    pub code: &'static str,
    pub message: String,
}
impl IngressError {
    fn new(code: &'static str, text: impl AsRef<str>) -> Self {
        let text = text.as_ref();
        let mut end = text.len().min(2048);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            code,
            message: text[..end].into(),
        }
    }
    fn cancelled() -> Self {
        Self::new(
            "audio_ingress_cancelled",
            "Native AUDIO input/result was retired",
        )
    }
}
impl std::fmt::Display for IngressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for IngressError {}
impl From<AudioError> for IngressError {
    fn from(e: AudioError) -> Self {
        Self::new(e.code, e.message)
    }
}
impl From<IngressError> for freeshow_core::WireError {
    fn from(e: IngressError) -> Self {
        Self::new(e.code, e.message)
    }
}
fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|e| e.into_inner())
}
struct Group {
    context: RuntimeContext,
    closed: AtomicBool,
    cancel: CancellationToken,
    guards: AtomicUsize,
    changed: Notify,
}
impl Group {
    fn new(context: RuntimeContext) -> Arc<Self> {
        Arc::new(Self {
            context,
            closed: AtomicBool::new(false),
            cancel: CancellationToken::new(),
            guards: AtomicUsize::new(0),
            changed: Notify::new(),
        })
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.cancel.cancel();
    }
}
struct GroupGuard(Arc<Group>);
impl Drop for GroupGuard {
    fn drop(&mut self) {
        self.0.guards.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}
enum Input {
    Pcm {
        bytes: Vec<u8>,
        meta: Box<AudioMetadata>,
        sinks: SinkAvailability,
    },
    Disconnect,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Queued,
    Active,
    Ready,
    Released,
}
struct CellState {
    phase: Phase,
    input: Option<Input>,
    outcome: Option<Result<AudioReceipt>>,
    guard: Option<GroupGuard>,
    lease: Option<OwnedSemaphorePermit>,
    cancelled: Option<IngressError>,
}
impl CellState {
    // Dispose of the real native receipt before claiming its group/lease drained.
    fn dispose(&mut self, reason: Option<IngressError>) {
        self.input.take();
        self.outcome.take();
        self.guard.take();
        self.lease.take();
        self.phase = Phase::Released;
        self.cancelled = reason;
    }
}
struct Cell {
    group: Arc<Group>,
    shared: Weak<Shared>,
    state: Mutex<CellState>,
    caller_cancel: CancellationToken,
    explicit_cancel: CancellationToken,
    abandoned: AtomicBool,
    changed: Notify,
}
struct State {
    group: Option<Arc<Group>>,
    previous: Option<Arc<Group>>,
    queue: VecDeque<Arc<Cell>>,
    active: Option<Arc<Cell>>,
    cells: Vec<Weak<Cell>>,
    closed: bool,
    failure: Option<IngressError>,
}
struct Shared {
    library: LibraryService,
    audio: NativeAudioPipeline,
    state: Mutex<State>,
    slots: Arc<Semaphore>,
    controls: Arc<Semaphore>,
    wake: Notify,
    joined: AtomicBool,
    changed: Notify,
    supervisor: Mutex<Option<JoinHandle<()>>>,
    timeout: Duration,
    accepted_packets: AtomicU64,
    completed_packets: AtomicU64,
    #[cfg(test)]
    panic: CancellationToken,
}
impl Drop for Shared {
    fn drop(&mut self) {
        // A forgotten failed owner must not admit a replacement while an actual
        // accepted pipeline job is still outstanding. Normal shutdown clears
        // active before this final drop; failed owners require native join proof.
        if lock(&self.state).active.is_none() || self.audio.status()["joined"] == true {
            INGRESS_OWNER.store(false, Ordering::Release);
        }
    }
}
impl Shared {
    fn current(&self, context: &RuntimeContext) -> Result<()> {
        if self
            .library
            .context()
            .map_err(|e| IngressError::new("audio_ingress_context", e.to_string()))?
            != *context
        {
            return Err(IngressError::new(
                "stale_library",
                "Raw AUDIO full runtime context is obsolete",
            ));
        }
        Ok(())
    }
    fn retire_cells(&self, state: &mut State, group: Option<&Arc<Group>>) {
        state
            .queue
            .retain(|cell| group.is_some_and(|g| !Arc::ptr_eq(g, &cell.group)));
        for cell in state.cells.iter().filter_map(Weak::upgrade) {
            if group.is_some_and(|g| !Arc::ptr_eq(g, &cell.group)) {
                continue;
            }
            let mut value = lock(&cell.state);
            if matches!(value.phase, Phase::Queued | Phase::Ready) {
                value.dispose(Some(IngressError::cancelled()));
                cell.changed.notify_waiters();
            }
        }
        state.cells.retain(|c| c.strong_count() > 0);
    }
    fn close_all(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        for group in [&state.group, &state.previous].into_iter().flatten() {
            group.close();
        }
        self.retire_cells(&mut state, None);
        self.wake.notify_waiters();
    }
    fn close_group(&self, context: &RuntimeContext) -> Result<Arc<Group>> {
        bounded_context(context)?;
        let mut state = lock(&self.state);
        let existing = [&state.group, &state.previous]
            .into_iter()
            .flatten()
            .find(|g| g.context == *context)
            .cloned();
        let group = match existing {
            Some(g) => g,
            None => {
                let current = self
                    .library
                    .context()
                    .map_err(|e| IngressError::new("audio_ingress_context", e.to_string()))?;
                if context.runtime_id != current.runtime_id
                    || context.library_generation > current.library_generation
                    || (context.library_generation == current.library_generation
                        && *context != current)
                {
                    return Err(IngressError::new(
                        "stale_library",
                        "Foreign AUDIO ingress cleanup context",
                    ));
                }
                let g = Group::new(context.clone());
                if *context == current {
                    if state.group.is_some() {
                        return Err(IngressError::new(
                            "audio_ingress_context_drain",
                            "The previous AUDIO context still needs retirement",
                        ));
                    }
                    state.group = Some(g.clone());
                }
                // An older context absent from both retained groups cannot own
                // queued/active/ready data: replacement required its actual
                // guard count to reach zero. Retain no synthetic history entry.
                g
            }
        };
        group.close();
        self.retire_cells(&mut state, Some(&group));
        self.wake.notify_waiters();
        Ok(group)
    }
    fn cancel_ticket(&self, cell: &Arc<Cell>, abandoned: bool) {
        if abandoned {
            cell.abandoned.store(true, Ordering::Release);
        }
        let mut state = lock(&self.state);
        let mut value = lock(&cell.state);
        if value.phase == Phase::Queued {
            state.queue.retain(|c| !Arc::ptr_eq(c, cell));
            if abandoned {
                value.dispose(Some(IngressError::cancelled()));
            } else {
                value.input.take();
                value.outcome = Some(Err(IngressError::cancelled()));
                value.phase = Phase::Ready;
            }
            cell.changed.notify_waiters();
        } else if abandoned && value.phase == Phase::Ready {
            value.dispose(Some(IngressError::cancelled()));
        } else if !abandoned {
            cell.explicit_cancel.cancel();
        }
    }
    // A panicked pump cannot fabricate an active-job receipt. Only the real
    // underlying owner's terminal join can release that retained guard.
    fn recover_after_native_join(&self) {
        if self.audio.status()["joined"] != true {
            return;
        }
        let mut state = lock(&self.state);
        if state.failure.is_some() && self.joined.load(Ordering::Acquire) {
            if let Some(cell) = state.active.take() {
                lock(&cell.state).dispose(state.failure.clone());
                cell.changed.notify_waiters();
            }
        }
    }
}
struct Handle {
    shared: Arc<Shared>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.shared.close_all();
    }
}
#[derive(Clone)]
pub struct AudioIngress {
    handle: Arc<Handle>,
}
pub struct Ticket {
    cell: Arc<Cell>,
    armed: bool,
}
impl Ticket {
    #[cfg(test)]
    #[allow(dead_code)] // Explicit cancellation is exercised by the separate ingress integration test.
    pub fn cancel(&self) {
        if let Some(shared) = self.cell.shared.upgrade() {
            shared.cancel_ticket(&self.cell, false);
        }
    }
    pub async fn finish(mut self) -> Result<Observation> {
        loop {
            let changed = self.cell.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let value = lock(&self.cell.state);
                if matches!(value.phase, Phase::Ready | Phase::Released) {
                    self.armed = false;
                    return Ok(Observation {
                        cell: self.cell.clone(),
                    });
                }
            }
            changed.await;
        }
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if self.armed {
            if let Some(shared) = self.cell.shared.upgrade() {
                shared.cancel_ticket(&self.cell, true);
            }
        }
    }
}
/// Native-only private observation. It has no Deserialize/source ACK surface.
/// Even after Ticket::finish, the quota/guard remains held until consume/drop or
/// actual disposal by explicit context retirement.
pub struct Observation {
    cell: Arc<Cell>,
}
impl Observation {
    /// Bounded native inspection only; callback must not block or reenter ingress.
    pub fn inspect<T>(&self, read: impl FnOnce(&AudioReceipt) -> T) -> Result<T> {
        let shared = self
            .cell
            .shared
            .upgrade()
            .ok_or_else(IngressError::cancelled)?;
        shared.current(&self.cell.group.context)?;
        let value = lock(&self.cell.state);
        if let Some(e) = &value.cancelled {
            return Err(e.clone());
        }
        if self.cell.group.closed.load(Ordering::Acquire) {
            return Err(IngressError::cancelled());
        }
        match value.outcome.as_ref() {
            Some(Ok(receipt)) => Ok(read(receipt)),
            Some(Err(e)) => Err(e.clone()),
            None => Err(IngressError::cancelled()),
        }
    }
    pub fn into_void(self) -> Result<()> {
        self.inspect(|_| ())
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        lock(&self.cell.state).dispose(None);
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngressStatus {
    pub closed: bool,
    pub retained: usize,
    pub queued: usize,
    pub active: bool,
    pub group_closed: bool,
    pub group_outstanding: usize,
    pub pump_joined: bool,
    pub terminal: Option<IngressError>,
    pub accepted_packets: u64,
    pub completed_packets: u64,
    pub retained_controls: usize,
}
impl AudioIngress {
    pub fn new(library: LibraryService, audio: NativeAudioPipeline) -> Result<Self> {
        Self::with_timeout(library, audio, Duration::from_secs(15))
    }
    pub fn with_timeout(
        library: LibraryService,
        audio: NativeAudioPipeline,
        timeout: Duration,
    ) -> Result<Self> {
        if timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(IngressError::new(
                "audio_ingress_limits",
                "Drain timeout must be positive and at most 30 seconds",
            ));
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            IngressError::new(
                "audio_ingress_runtime",
                "AUDIO ingress requires the retained host Tokio runtime",
            )
        })?;
        INGRESS_OWNER
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                IngressError::new(
                    "audio_ingress_owner_exists",
                    "One process AUDIO ingress owner is already retained",
                )
            })?;
        let shared = Arc::new(Shared {
            library,
            audio,
            state: Mutex::new(State {
                group: None,
                previous: None,
                queue: VecDeque::with_capacity(CAPACITY),
                active: None,
                cells: Vec::with_capacity(CAPACITY),
                closed: false,
                failure: None,
            }),
            slots: Arc::new(Semaphore::new(CAPACITY)),
            controls: Arc::new(Semaphore::new(CAPACITY)),
            wake: Notify::new(),
            joined: AtomicBool::new(false),
            changed: Notify::new(),
            supervisor: Mutex::new(None),
            timeout,
            accepted_packets: AtomicU64::new(0),
            completed_packets: AtomicU64::new(0),
            #[cfg(test)]
            panic: CancellationToken::new(),
        });
        let pump_state = shared.clone();
        let pump = runtime.spawn(async move { pump(pump_state).await });
        let state = shared.clone();
        let supervisor = runtime.spawn(async move {
            if let Err(e) = pump.await {
                lock(&state.state).failure = Some(IngressError::new(
                    "audio_ingress_pump",
                    format!("Fixed AUDIO pump failed: {e}"),
                ));
                state.close_all();
            }
            state.joined.store(true, Ordering::Release);
            state.changed.notify_waiters();
        });
        // The supervisor retains the actual pump JoinHandle through caller Drop.
        // Its bounded self-retention ends when that pump has really returned.
        *lock(&shared.supervisor) = Some(supervisor);
        Ok(Self {
            handle: Arc::new(Handle { shared }),
        })
    }
    pub fn submit(
        &self,
        context: &RuntimeContext,
        bytes: &[u8],
        meta: &AudioMetadata,
        sinks: &SinkAvailability,
        cancel: CancellationToken,
    ) -> Result<Ticket> {
        if bytes.len() > MAX_PCM_BYTES {
            return Err(IngressError::new(
                "audio_ingress_limit",
                "Raw PCM exceeds 32768 bytes",
            ));
        }
        bounded_inputs(meta, sinks)?;
        self.enqueue(context, cancel, || Input::Pcm {
            bytes: bytes.to_vec(),
            meta: Box::new(meta.clone()),
            sinks: sinks.clone(),
        })
    }
    pub fn disconnect_icecast(
        &self,
        context: &RuntimeContext,
        cancel: CancellationToken,
    ) -> Result<Ticket> {
        self.enqueue(context, cancel, || Input::Disconnect)
    }
    fn enqueue(
        &self,
        context: &RuntimeContext,
        cancel: CancellationToken,
        input: impl FnOnce() -> Input,
    ) -> Result<Ticket> {
        bounded_context(context)?;
        let shared = &self.handle.shared;
        shared.current(context)?;
        if cancel.is_cancelled() {
            return Err(IngressError::cancelled());
        }
        let mut state = lock(&shared.state);
        if state.closed {
            return Err(state.failure.clone().unwrap_or_else(|| {
                IngressError::new("audio_ingress_closed", "AUDIO ingress is closed")
            }));
        }
        shared.current(context)?;
        let group = if let Some(group) = &state.group {
            if group.context == *context {
                if group.closed.load(Ordering::Acquire) {
                    return Err(IngressError::new(
                        "audio_ingress_context_closed",
                        "This full AUDIO context is terminally closed",
                    ));
                }
                group.clone()
            } else {
                if !group.closed.load(Ordering::Acquire)
                    || group.guards.load(Ordering::Acquire) != 0
                {
                    return Err(IngressError::new(
                        "audio_ingress_context_drain",
                        "The previous full AUDIO context must really drain first",
                    ));
                }
                let group = Group::new(context.clone());
                state.previous = state.group.replace(group.clone());
                group
            }
        } else {
            let group = Group::new(context.clone());
            state.group = Some(group.clone());
            group
        };
        let lease = shared.slots.clone().try_acquire_owned().map_err(|_| {
            IngressError::new(
                "audio_ingress_busy",
                "Four whole raw jobs/results are already retained",
            )
        })?;
        // Reserve quota and group authority before copying PCM/metadata/sinks.
        group.guards.fetch_add(1, Ordering::AcqRel);
        let guard = GroupGuard(group.clone());
        let input = input();
        if matches!(&input, Input::Pcm { .. }) {
            let _ = shared
                .accepted_packets
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    Some(n.saturating_add(1))
                });
        }
        let cell = Arc::new(Cell {
            group,
            shared: Arc::downgrade(shared),
            state: Mutex::new(CellState {
                phase: Phase::Queued,
                input: Some(input),
                outcome: None,
                guard: Some(guard),
                lease: Some(lease),
                cancelled: None,
            }),
            caller_cancel: cancel,
            explicit_cancel: CancellationToken::new(),
            abandoned: AtomicBool::new(false),
            changed: Notify::new(),
        });
        state.cells.retain(|c| {
            c.upgrade()
                .is_some_and(|c| lock(&c.state).phase != Phase::Released)
        });
        state.cells.push(Arc::downgrade(&cell));
        state.queue.push_back(cell.clone());
        shared.wake.notify_one();
        Ok(Ticket { cell, armed: true })
    }
    /// Closes admission/cancels synchronously at this call, even if the returned
    /// future is never polled or is later dropped. It never queues a FIFO barrier.
    pub fn close_context(
        &self,
        context: &RuntimeContext,
    ) -> impl Future<Output = Result<()>> + Send + 'static {
        let group = self.handle.shared.close_group(context);
        let owner = self.clone();
        let lease = self.control_lease();
        async move {
            let _lease = lease?;
            owner.wait_group(&group?).await
        }
    }
    /// Retire only the actually obsolete native group, including a late hook.
    pub fn context_changed(&self) -> impl Future<Output = Result<()>> + Send + 'static {
        let group = self
            .handle
            .shared
            .library
            .context()
            .map_err(|e| IngressError::new("audio_ingress_context", e.to_string()))
            .and_then(|current| {
                let old = lock(&self.handle.shared.state)
                    .group
                    .as_ref()
                    .filter(|group| group.context != current)
                    .map(|group| group.context.clone());
                old.map(|old| self.handle.shared.close_group(&old))
                    .transpose()
            });
        let lease = self.control_lease();
        let owner = self.clone();
        async move {
            let _lease = lease?;
            if let Some(group) = group? {
                owner.wait_group(&group).await?;
            }
            Ok(())
        }
    }
    fn control_lease(&self) -> Result<OwnedSemaphorePermit> {
        self.handle.shared.controls.clone().try_acquire_owned().map_err(|_| {
            IngressError::new("audio_ingress_control_busy", "At most four AUDIO drain callers may be retained; synchronous retirement still applies")
        })
    }
    async fn wait_group(&self, group: &Arc<Group>) -> Result<()> {
        let wait = async {
            loop {
                let changed = group.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                self.handle.shared.recover_after_native_join();
                if group.guards.load(Ordering::Acquire) == 0 {
                    return Ok(());
                }
                tokio::select! { _=changed=>{}, _=tokio::time::sleep(Duration::from_millis(5))=>{} }
            }
        };
        tokio::time::timeout(self.handle.shared.timeout, wait)
            .await
            .map_err(|_| {
                IngressError::new(
                    "audio_ingress_drain_timeout",
                    "Actual AUDIO group guards remain retained; keep this owner and retry",
                )
            })?
    }
    pub fn status(&self) -> IngressStatus {
        let shared = &self.handle.shared;
        let state = lock(&shared.state);
        IngressStatus {
            closed: state.closed,
            retained: CAPACITY - shared.slots.available_permits(),
            queued: state.queue.len(),
            active: state.active.is_some(),
            group_closed: state
                .group
                .as_ref()
                .is_some_and(|g| g.closed.load(Ordering::Acquire)),
            group_outstanding: state
                .group
                .as_ref()
                .map_or(0, |g| g.guards.load(Ordering::Acquire)),
            pump_joined: shared.joined.load(Ordering::Acquire),
            terminal: state.failure.clone(),
            accepted_packets: shared.accepted_packets.load(Ordering::Acquire),
            completed_packets: shared.completed_packets.load(Ordering::Acquire),
            retained_controls: CAPACITY - shared.controls.available_permits(),
        }
    }
    /// Stops/drains only this ingress. The host separately shuts down the real
    /// pipeline and attempts every other owner even if one returns an error.
    pub fn shutdown(&self) -> impl Future<Output = Result<()>> + Send + 'static {
        self.handle.shared.close_all();
        let lease = self.control_lease();
        let owner = self.clone();
        async move {
            let _lease = lease?;
            let shared = &owner.handle.shared;
            tokio::time::timeout(shared.timeout, async {
                loop {
                    let changed = shared.changed.notified();
                    tokio::pin!(changed); changed.as_mut().enable();
                    shared.recover_after_native_join();
                    let clear = { let state=lock(&shared.state); state.active.is_none() && [&state.group,&state.previous].into_iter().flatten().all(|g|g.guards.load(Ordering::Acquire)==0) };
                    if shared.joined.load(Ordering::Acquire) && clear { break; }
                    tokio::select! { _=changed=>{}, _=tokio::time::sleep(Duration::from_millis(5))=>{} }
                }
                // Only take an already-finished supervisor handle: cancellation
                // of this caller cannot lose ownership of a live task.
                loop {
                    let ready=lock(&shared.supervisor).as_ref().is_none_or(JoinHandle::is_finished);
                    if ready { break; }
                    tokio::task::yield_now().await;
                }
                let joined=lock(&shared.supervisor).take();
                if let Some(joined)=joined { joined.await.map_err(|e|IngressError::new("audio_ingress_supervisor",e.to_string()))?; }
                lock(&shared.state).failure.clone().map_or(Ok(()),Err)
            }).await.map_err(|_|IngressError::new("audio_ingress_drain_timeout","Real AUDIO pump/group ownership remains retained; retry shutdown"))?
        }
    }
    #[cfg(test)]
    #[allow(dead_code)] // Called by the separate composed integration test.
    pub(crate) fn test_panic_pump(&self) {
        self.handle.shared.panic.cancel();
    }
}
async fn pump(shared: Arc<Shared>) {
    loop {
        let wake = shared.wake.notified();
        tokio::pin!(wake);
        wake.as_mut().enable();
        let next = {
            let mut state = lock(&shared.state);
            if let Some(cell) = state.queue.pop_front() {
                let mut value = lock(&cell.state);
                let input = value.input.take();
                value.phase = Phase::Active;
                drop(value);
                state.active = Some(cell.clone());
                Some((cell, input))
            } else if state.closed {
                return;
            } else {
                None
            }
        };
        let Some((cell, input)) = next else {
            wake.await;
            continue;
        };
        let outcome = if cell.group.cancel.is_cancelled()
            || cell.caller_cancel.is_cancelled()
            || cell.explicit_cancel.is_cancelled()
        {
            Err(IngressError::cancelled())
        } else if let Err(e) = shared.current(&cell.group.context) {
            Err(e)
        } else {
            let cancel = cell.group.cancel.child_token();
            let call = async {
                match input.expect("an active AUDIO cell owns exactly one input") {
                    Input::Pcm { bytes, meta, sinks } => {
                        let result = shared
                            .audio
                            .pcm(
                                &cell.group.context,
                                &bytes,
                                bytes.len(),
                                &meta,
                                &sinks,
                                cancel.clone(),
                            )
                            .await;
                        let _ = shared.completed_packets.try_update(
                            Ordering::AcqRel,
                            Ordering::Acquire,
                            |n| Some(n.saturating_add(1)),
                        );
                        result
                    }
                    Input::Disconnect => {
                        shared
                            .audio
                            .disconnect_icecast(&cell.group.context, cancel.clone())
                            .await
                    }
                }
                .map_err(IngressError::from)
            };
            tokio::pin!(call);
            #[cfg(not(test))]
            let outcome = tokio::select! {
                result=&mut call=>result,
                _=cell.caller_cancel.cancelled()=>{cancel.cancel();call.await},
                _=cell.explicit_cancel.cancelled()=>{cancel.cancel();call.await},
            };
            #[cfg(test)]
            let outcome = tokio::select! {
                result=&mut call=>result,
                _=cell.caller_cancel.cancelled()=>{cancel.cancel();call.await},
                _=cell.explicit_cancel.cancelled()=>{cancel.cancel();call.await},
                _=shared.panic.cancelled()=>panic!("controlled fixed AUDIO pump failure"),
            };
            outcome
        };
        let mut state = lock(&shared.state);
        let mut value = lock(&cell.state);
        value.outcome = Some(outcome);
        value.phase = Phase::Ready;
        // Active future returned for real. Only now may explicit retirement or
        // caller abandonment dispose its actual receipt and group guard.
        if cell.group.closed.load(Ordering::Acquire) || cell.abandoned.load(Ordering::Acquire) {
            value.dispose(Some(IngressError::cancelled()));
        }
        state.active.take();
        cell.changed.notify_waiters();
        shared.changed.notify_waiters();
    }
}
fn bounded_context(context: &RuntimeContext) -> Result<()> {
    if context.runtime_id.len() > 256
        || context.data_path.as_os_str().len() > 32768
        || context.data_path.to_str().is_none()
    {
        Err(IngressError::new(
            "audio_ingress_limit",
            "Full runtime context exceeds native AUDIO limits",
        ))
    } else {
        Ok(())
    }
}
fn bounded_inputs(meta: &AudioMetadata, sinks: &SinkAvailability) -> Result<()> {
    fn visit(value: &Value, depth: usize, nodes: &mut usize, bytes: &mut usize) -> Result<()> {
        *nodes += 1;
        if depth > 8 || *nodes > 256 || *bytes > 8192 {
            return Err(IngressError::new(
                "audio_ingress_limit",
                "AUDIO metadata exceeds depth/count/byte limits",
            ));
        }
        match value {
            Value::String(s) => *bytes = bytes.saturating_add(s.len()),
            Value::Array(a) => {
                for v in a {
                    visit(v, depth + 1, nodes, bytes)?;
                }
            }
            Value::Object(o) => {
                for (k, v) in o {
                    *bytes = bytes.saturating_add(k.len());
                    visit(v, depth + 1, nodes, bytes)?;
                }
            }
            _ => *bytes = bytes.saturating_add(16),
        }
        Ok(())
    }
    let (mut nodes, mut bytes) = (0, 0);
    for input in [&meta.sample_rate, &meta.target_id] {
        if let JsInput::Json(value) = input {
            visit(value, 0, &mut nodes, &mut bytes)?;
        }
    }
    if let Some(value) = &meta.icecast {
        visit(value, 0, &mut nodes, &mut bytes)?;
    }
    if bytes > 8192 {
        return Err(IngressError::new(
            "audio_ingress_limit",
            "AUDIO metadata bytes exceed8192",
        ));
    }
    let mut bytes = 0usize;
    for list in [&sinks.ndi, &sinks.omt, &sinks.blackmagic] {
        if list.len() > 64 {
            return Err(IngressError::new(
                "audio_ingress_limit",
                "Too many native sink registrations",
            ));
        }
        for key in list {
            bytes = bytes.saturating_add(key.len());
            if key.len() > 1024 || bytes > 8192 {
                return Err(IngressError::new(
                    "audio_ingress_limit",
                    "Native sink registrations exceed byte limits",
                ));
            }
        }
    }
    Ok(())
}
