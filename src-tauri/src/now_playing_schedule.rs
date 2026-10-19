//! Pure bounded source-phase ordering. Real preparation, effects, private
//! publications and cleanup remain owned by the native host, never this queue.
use freeshow_core::protocol::RuntimeContext;
use std::{
    collections::VecDeque,
    hash::{Hash, Hasher},
    sync::Arc,
};

pub const MAX_OPERATIONS: usize = 4;

struct Run {
    context: RuntimeContext,
}
/// Cloneable native identity only: cloning this is not a resource lease.
/// No Deserialize, public constructor or renderer-selected identifier.
#[derive(Clone)]
pub struct OperationId {
    run: Arc<Run>,
    sequence: u64,
}
impl OperationId {
    pub fn context(&self) -> &RuntimeContext {
        &self.run.context
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}
impl PartialEq for OperationId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.run, &other.run) && self.sequence == other.sequence
    }
}
impl Eq for OperationId {}
impl Hash for OperationId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.run).hash(state);
        self.sequence.hash(state);
    }
}
impl std::fmt::Debug for OperationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperationId")
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseKind {
    PublishMetadata,
    PostMetadata,
    Text,
    Unset,
    Open,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationState {
    PendingPreparation,
    Queued(PhaseKind),
    Active(PhaseKind),
    Finished,
}

/// Trusted dispatch/actual-completion token. A cloned OperationId cannot create
/// one. The scheduler retains its matching private phase seal until completion.
pub struct PhaseToken {
    operation: OperationId,
    kind: PhaseKind,
    seal: Arc<()>,
}
impl PhaseToken {
    pub fn operation(&self) -> &OperationId {
        &self.operation
    }
    pub fn context(&self) -> &RuntimeContext {
        self.operation.context()
    }
    pub fn kind(&self) -> PhaseKind {
        self.kind
    }
}
impl std::fmt::Debug for PhaseToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhaseToken")
            .field("operation", &self.operation)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    Full,
    Retired,
    ForeignRun,
    UnknownOperation,
    InvalidState,
    StalePhase,
    InvalidContext,
    IdentityExhausted,
}
impl std::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Full => "Four whole now-playing operation records are retained",
            Self::Retired => "Now-playing schedule admission is retired",
            Self::ForeignRun => "Operation belongs to a different native run seal",
            Self::UnknownOperation => "Operation record was released or was never admitted",
            Self::InvalidState => "Operation is not in the required state",
            Self::StalePhase => "Phase is unknown, already completed or no longer current",
            Self::InvalidContext => "Native full context is invalid or exceeds bounds",
            Self::IdentityExhausted => "Native operation identity counter exhausted",
        })
    }
}
impl std::error::Error for ScheduleError {}

struct Record {
    sequence: u64,
    state: OperationState,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub records: usize,
    pub pending_preparations: usize,
    pub queued: usize,
    pub active: usize,
    pub finished_unreleased: usize,
    pub retiring: bool,
}
impl Status {
    /// This proves zero scheduler records only. The host's explicit releases,
    /// not this method, must account for every actual native resource and future.
    pub fn drained(&self) -> bool {
        self.records == 0
    }
}

/// Single-owner state machine: no task, IO, timer, mutex, async work or automatic
/// resource disposal. Each record has at most one queued/current phase. Thus the
/// FIFO contains at most four entries, and queued + active is also at most four.
pub struct Schedule {
    run: Arc<Run>,
    next: Option<u64>,
    records: Vec<Record>,
    queue: VecDeque<(u64, PhaseKind)>,
    current: Option<PhaseToken>,
    retiring: bool,
}
impl Schedule {
    pub fn new(context: RuntimeContext) -> Result<Self, ScheduleError> {
        let path = context
            .data_path
            .to_str()
            .ok_or(ScheduleError::InvalidContext)?;
        if context.runtime_id.is_empty()
            || context.runtime_id.len() > 256
            || context.runtime_id.contains('\0')
            || path.is_empty()
            || path.len() > 32768
            || path.contains('\0')
            || !context.data_path.is_absolute()
            || context.library_generation > 9_007_199_254_740_991
        {
            return Err(ScheduleError::InvalidContext);
        }
        Ok(Self {
            run: Arc::new(Run { context }),
            next: Some(1),
            records: Vec::with_capacity(MAX_OPERATIONS),
            queue: VecDeque::with_capacity(MAX_OPERATIONS),
            current: None,
            retiring: false,
        })
    }
    pub fn context(&self) -> &RuntimeContext {
        &self.run.context
    }
    fn record(&self, id: &OperationId) -> Result<usize, ScheduleError> {
        if !Arc::ptr_eq(&self.run, &id.run) {
            return Err(ScheduleError::ForeignRun);
        }
        self.records
            .iter()
            .position(|record| record.sequence == id.sequence)
            .ok_or(ScheduleError::UnknownOperation)
    }
    fn admit(&mut self, state: OperationState) -> Result<OperationId, ScheduleError> {
        if self.retiring {
            return Err(ScheduleError::Retired);
        }
        if self.records.len() == MAX_OPERATIONS {
            return Err(ScheduleError::Full);
        }
        let Some(sequence) = self.next else {
            self.retire();
            return Err(ScheduleError::IdentityExhausted);
        };
        self.next = sequence.checked_add(1);
        self.records.push(Record { sequence, state });
        if let OperationState::Queued(kind) = state {
            self.queue.push_back((sequence, kind));
        }
        Ok(OperationId {
            run: self.run.clone(),
            sequence,
        })
    }
    pub fn admit_set(&mut self) -> Result<OperationId, ScheduleError> {
        self.admit(OperationState::PendingPreparation)
    }
    pub fn admit_unset(&mut self) -> Result<OperationId, ScheduleError> {
        self.admit(OperationState::Queued(PhaseKind::Unset))
    }
    pub fn admit_open(&mut self) -> Result<OperationId, ScheduleError> {
        self.admit(OperationState::Queued(PhaseKind::Open))
    }
    pub fn state(&self, id: &OperationId) -> Result<OperationState, ScheduleError> {
        Ok(self.records[self.record(id)?].state)
    }
    /// Actual preparation has settled. `false` is ONLY source-normal parser-null;
    /// it still runs PostMetadata/Text. Unsupported/auth/native failure must use
    /// preparation_failed. A retired pending prep settles without future phases.
    pub fn ready(&mut self, id: &OperationId, has_metadata: bool) -> Result<(), ScheduleError> {
        let index = self.record(id)?;
        if self.records[index].state != OperationState::PendingPreparation {
            return Err(ScheduleError::InvalidState);
        }
        if self.retiring {
            self.records[index].state = OperationState::Finished;
        } else {
            let kind = if has_metadata {
                PhaseKind::PublishMetadata
            } else {
                PhaseKind::PostMetadata
            };
            self.records[index].state = OperationState::Queued(kind);
            self.queue.push_back((id.sequence, kind));
        }
        Ok(())
    }
    /// Host assertion that actual prep failed AND has genuinely settled. Never
    /// call merely because an observer/caller timed out or dropped.
    pub fn preparation_failed(&mut self, id: &OperationId) -> Result<(), ScheduleError> {
        let index = self.record(id)?;
        if self.records[index].state != OperationState::PendingPreparation {
            return Err(ScheduleError::InvalidState);
        }
        self.records[index].state = OperationState::Finished;
        Ok(())
    }
    /// Process all currently observed prep outcomes/source commands before this
    /// call when reproducing the documented same-turn source experiment. This
    /// queue does not infer Node/Tokio turn equivalence or actual IO completion.
    pub fn take_next(&mut self) -> Option<PhaseToken> {
        if self.current.is_some() || self.retiring {
            return None;
        }
        let (sequence, kind) = self.queue.pop_front()?;
        let index = self
            .records
            .iter()
            .position(|record| record.sequence == sequence)
            .expect("private phase queue references an admitted record");
        self.records[index].state = OperationState::Active(kind);
        let token = PhaseToken {
            operation: OperationId {
                run: self.run.clone(),
                sequence,
            },
            kind,
            seal: Arc::new(()),
        };
        self.current = Some(PhaseToken {
            operation: token.operation.clone(),
            kind,
            seal: token.seal.clone(),
        });
        Some(token)
    }
    fn actual_current(&self, token: &PhaseToken) -> Result<usize, ScheduleError> {
        let index = self.record(&token.operation)?;
        if !self.current.as_ref().is_some_and(|current| {
            current.operation == token.operation
                && current.kind == token.kind
                && Arc::ptr_eq(&current.seal, &token.seal)
        }) {
            return Err(ScheduleError::StalePhase);
        }
        Ok(index)
    }
    /// Call ONLY after actual phase effects settle. Publication is actual source
    /// enqueue, not a private renderer receipt; the latter may retain the whole
    /// slot independently. No other phase is returned during awaited effects.
    pub fn complete(&mut self, token: &PhaseToken) -> Result<(), ScheduleError> {
        let index = self.actual_current(token)?;
        self.current = None;
        let successor = if self.retiring {
            None
        } else {
            match token.kind {
                PhaseKind::PublishMetadata => Some(PhaseKind::PostMetadata),
                PhaseKind::PostMetadata => Some(PhaseKind::Text),
                PhaseKind::Text | PhaseKind::Unset | PhaseKind::Open => None,
            }
        };
        self.records[index].state = match successor {
            Some(kind) => {
                self.queue.push_back((token.operation.sequence, kind));
                OperationState::Queued(kind)
            }
            None => OperationState::Finished,
        };
        Ok(())
    }
    /// Same actual-completion obligation as complete; no successor after a
    /// genuine failed phase. Other independent operations remain scheduled.
    pub fn fail_current(&mut self, token: &PhaseToken) -> Result<(), ScheduleError> {
        let index = self.actual_current(token)?;
        self.current = None;
        self.records[index].state = OperationState::Finished;
        Ok(())
    }
    /// Synchronous admission fence, not resource drain. Unstarted queued phases
    /// stop, active tokens remain owned, and pending preps remain pending until
    /// the host reports their actual result. Repeated retirement is harmless.
    pub fn retire(&mut self) {
        self.retiring = true;
        self.queue.clear();
        for record in &mut self.records {
            if matches!(record.state, OperationState::Queued(_)) {
                record.state = OperationState::Finished;
            }
        }
    }
    /// Explicit trusted host assertion: actual image cleanup, publications and
    /// all other resources are finished. Finished-unreleased records consume a
    /// slot. Neither a timeout nor this method proves native resources drained.
    pub fn release(&mut self, id: &OperationId) -> Result<(), ScheduleError> {
        let index = self.record(id)?;
        if self.records[index].state != OperationState::Finished {
            return Err(ScheduleError::InvalidState);
        }
        self.records.remove(index);
        Ok(())
    }
    pub fn status(&self) -> Status {
        Status {
            records: self.records.len(),
            pending_preparations: self
                .records
                .iter()
                .filter(|record| record.state == OperationState::PendingPreparation)
                .count(),
            queued: self.queue.len(),
            active: usize::from(self.current.is_some()),
            finished_unreleased: self
                .records
                .iter()
                .filter(|record| record.state == OperationState::Finished)
                .count(),
            retiring: self.retiring,
        }
    }
}
