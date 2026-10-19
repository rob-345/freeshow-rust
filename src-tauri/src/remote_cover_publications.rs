//! Private MAIN folder notifications owned independently of HTTP body delivery.
//! A sealed ordered observation is consumed once; no request supplies its path.
use crate::now_playing_cover_file::Observation;
use freeshow_core::protocol::{Event, RuntimeContext};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Instant,
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

const CAPACITY: usize = 4;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
// Shared across replacement owners: sequence allocation and the actual synchronous
// trusted enqueue form one ordered lane. Never retain an emitter/AppHandle here.
static EMISSION: Mutex<()> = Mutex::new(());
static REGISTRY: OnceLock<Arc<Shared>> = OnceLock::new();
pub type Result<T> = std::result::Result<T, Failure>;
#[derive(Debug, Clone)]
pub struct Failure {
    pub code: &'static str,
    pub message: &'static str,
}
fn failure(code: &'static str, message: &'static str) -> Failure {
    Failure { code, message }
}
struct Pending {
    context: RuntimeContext,
    state: AtomicU8,
    receipt_live: AtomicBool,
    retired: AtomicBool,
    changed: Notify,
    // Last: admission survives all other pending-state destruction.
    _permit: OwnedSemaphorePermit,
}
struct State {
    context: Option<RuntimeContext>,
    closed: bool,
    pending: HashMap<String, Arc<Pending>>,
}
struct Shared {
    state: Mutex<State>,
    slots: Arc<Semaphore>,
}
#[derive(Clone)]
pub struct Publications {
    shared: Arc<Shared>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Metadata {
    pub id: String,
    pub sequence: String,
    pub folder: String,
}
#[derive(Clone)]
pub struct Delivery {
    pub event: Event,
    pub metadata: Metadata,
}
pub struct Receipt {
    // Drop the actual native observation/FD lease before publication admission.
    observation: Observation,
    id: String,
    pending: Arc<Pending>,
}
impl Publications {
    pub fn new() -> Self {
        Self {
            shared: REGISTRY
                .get_or_init(|| {
                    Arc::new(Shared {
                        state: Mutex::new(State {
                            context: None,
                            closed: false,
                            pending: HashMap::with_capacity(CAPACITY),
                        }),
                        slots: Arc::new(Semaphore::new(CAPACITY)),
                    })
                })
                .clone(),
        }
    }
    pub fn retained(&self) -> usize {
        CAPACITY - self.shared.slots.available_permits()
    }
    pub fn ready(&self, context: RuntimeContext) -> Result<()> {
        let _emission = EMISSION.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Err(failure(
                "cover_publication_closed",
                "Cover publications closed",
            ));
        }
        state.pending.retain(|_, pending| {
            if pending.context != context {
                revoke(pending);
            }
            pending.receipt_live.load(Ordering::Acquire)
                || pending.state.load(Ordering::Acquire) == 0
        });
        state.context = Some(context);
        Ok(())
    }
    pub fn retire(&self, context: &RuntimeContext, terminal: bool) {
        let _emission = EMISSION.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if terminal || state.context.as_ref() == Some(context) {
            state.context = None;
        }
        state.closed |= terminal;
        state.pending.retain(|_, pending| {
            if terminal || &pending.context == context {
                revoke(pending);
            }
            pending.receipt_live.load(Ordering::Acquire)
                || (!terminal && pending.state.load(Ordering::Acquire) == 0)
        });
    }
    /// `emit` must synchronously enqueue to the trusted main-window transport;
    /// it must not retain a delayed emission or reenter ready/retire/publish.
    /// Its failure rejects delivery but the returned receipt still owns the
    /// actual observation until the caller disposes it after HTTP work.
    pub fn publish(
        &self,
        observation: Observation,
        cancel: &CancellationToken,
        deadline: Instant,
        emit: impl FnOnce(&Delivery) -> bool,
    ) -> Result<Receipt> {
        let context = observation.context();
        let folder = observation
            .folder_failure()
            .ok_or_else(|| failure("cover_publication_source", "Actual folder failure required"))?
            .folder();
        let context_path = context
            .data_path
            .to_str()
            .ok_or_else(|| failure("cover_publication_unsupported", "Non-Unicode library path"))?;
        let path = folder
            .to_str()
            .ok_or_else(|| failure("cover_publication_unsupported", "Non-Unicode Audio path"))?;
        if folder != context.data_path.join("Audio")
            || context.runtime_id.is_empty()
            || context.runtime_id.len() > 256
            || context.runtime_id.contains('\0')
            || context.library_generation > 9_007_199_254_740_991
            || context_path.is_empty()
            || context_path.len() > 32768
            || context_path.contains('\0')
            || path.len() > 32774
            || path.contains('\0')
        {
            return Err(failure(
                "cover_publication_source",
                "Invalid captured fixed Audio path",
            ));
        }
        let _emission = EMISSION.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || state.context.as_ref() != Some(context) || cancel.is_cancelled() {
            return Err(failure("cover_publication_retired", "Cover source retired"));
        }
        if Instant::now() >= deadline {
            return Err(failure(
                "cover_publication_deadline",
                "Cover source deadline",
            ));
        }
        let permit = self.shared.slots.clone().try_acquire_owned().map_err(|_| {
            failure(
                "cover_publication_busy",
                "Four actual cover publications remain owned",
            )
        })?;
        let sequence = SEQUENCE
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_err(|_| failure("cover_publication_limit", "Cover sequence exhausted"))?
            + 1;
        let id = format!("native-cover-{}", uuid::Uuid::new_v4().simple());
        let metadata = Metadata {
            id: id.clone(),
            sequence: sequence.to_string(),
            folder: path.to_owned(),
        };
        let mut event = Event::reply(
            "MAIN",
            "ALERT",
            serde_json::Value::String(format!("Error: Could not create folder at: {path}!")),
            None,
        );
        event.context = Some(context.clone());
        let pending = Arc::new(Pending {
            context: context.clone(),
            state: AtomicU8::new(0),
            receipt_live: AtomicBool::new(true),
            retired: AtomicBool::new(false),
            changed: Notify::new(),
            _permit: permit,
        });
        state.pending.insert(id.clone(), pending.clone());
        let receipt = Receipt {
            observation,
            id,
            pending,
        };
        drop(state);
        if !emit(&Delivery { event, metadata }) {
            receipt.reject();
        }
        Ok(receipt)
    }
    pub fn complete(&self, context: &RuntimeContext, id: &str, accepted: bool) -> Result<()> {
        if id.len() != 45
            || !id.starts_with("native-cover-")
            || !id.as_bytes()[13..]
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
        {
            return Err(failure("cover_completion", "Invalid private cover ID"));
        }
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let pending = state
            .pending
            .get(id)
            .ok_or_else(|| failure("cover_completion", "Unknown private cover ID"))?;
        if &pending.context != context
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
            return Err(failure(
                "cover_completion",
                "Foreign or consumed cover completion",
            ));
        }
        pending.changed.notify_waiters();
        if !pending.receipt_live.load(Ordering::Acquire) {
            state.pending.remove(id);
        }
        Ok(())
    }
}
fn revoke(pending: &Pending) {
    pending.retired.store(true, Ordering::Release);
    pending.changed.notify_waiters();
}
impl Receipt {
    pub fn observation(&self) -> &Observation {
        &self.observation
    }
    pub async fn observe(&self, cancel: &CancellationToken, deadline: Instant) -> Result<()> {
        loop {
            let changed = self.pending.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.settled_result(|| {}) {
                return result;
            }
            tokio::select! {
                _ = changed => {},
                _ = cancel.cancelled() => return Err(failure("cover_delivery_cancelled", "Cover delivery observation cancelled")),
                _ = tokio::time::sleep_until(deadline.into()) => return Err(failure("cover_delivery_timeout", "Cover completion remains owned")),
            }
        }
    }
    // Read completion first. If a late ACK follows retirement, its acquire
    // observation must carry retirement-before evidence into the second read.
    fn settled_result(&self, between: impl FnOnce()) -> Option<Result<()>> {
        let state = self.pending.state.load(Ordering::Acquire);
        between();
        if self.pending.retired.load(Ordering::Acquire) {
            return Some(Err(failure(
                "cover_delivery_retired",
                "Cover delivery retired",
            )));
        }
        match state {
            1 => Some(Ok(())),
            2 => Some(Err(failure(
                "cover_delivery_rejected",
                "Main rejected the private cover notification",
            ))),
            _ => None,
        }
    }
    #[cfg(test)]
    pub fn observe_state_interleaved(&self, between: impl FnOnce()) -> Option<Result<()>> {
        self.settled_result(between)
    }
    pub fn reject(&self) {
        let _ = self
            .pending
            .state
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
        self.pending.changed.notify_waiters();
    }
}
impl Drop for Receipt {
    fn drop(&mut self) {
        let registry = REGISTRY.get().expect("Publication registry owns receipts");
        let mut state = registry.state.lock().unwrap_or_else(|e| e.into_inner());
        self.pending.receipt_live.store(false, Ordering::Release);
        // A timeout or dropped HTTP owner does not prove a queued WebView event
        // was disposed. Keep one of the four process-lifetime slots until its
        // genuine late ACK, or terminal closure with admission permanently sealed.
        if state.closed || self.pending.state.load(Ordering::Acquire) != 0 {
            state.pending.remove(&self.id);
        }
    }
}
impl Default for Publications {
    fn default() -> Self {
        Self::new()
    }
}
