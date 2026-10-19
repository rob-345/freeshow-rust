use crate::*;
use futures_util::{future::LocalBoxFuture, stream::FuturesUnordered, FutureExt, StreamExt};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread::JoinHandle,
    time::Instant,
};
use tokio::sync::{mpsc, oneshot, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

static OWNER_ACTIVE: AtomicBool = AtomicBool::new(false);
fn jobs() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(JOB_CAPACITY)))
        .clone()
}
fn observers() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(OBSERVER_CAPACITY)))
        .clone()
}
pub(crate) struct Shared {
    pub(crate) closed: AtomicBool,
    done: AtomicBool,
    joined: AtomicBool,
    pub(crate) pid: AtomicU32,
    pub(crate) spawned: AtomicU64,
    pub(crate) reaped: AtomicU64,
    commands: AtomicU64,
    commands_completed: AtomicU64,
    command_failures: AtomicU64,
    command_error: Mutex<Option<Error>>,
    pub(crate) command_pids: [AtomicU32; JOB_CAPACITY],
    queries: AtomicU64,
    completed: AtomicU64,
    capability: Mutex<Capability>,
    terminal: Mutex<Option<Error>>,
    notify: Notify,
    cancel: CancellationToken,
    #[cfg(test)]
    reap_gate: Mutex<Option<Gate>>,
    #[cfg(test)]
    after_spawn_gate: Mutex<Option<Gate>>,
    #[cfg(test)]
    exit_gate: Mutex<Option<Gate>>,
    #[cfg(test)]
    post_done_gate: Mutex<Option<Gate>>,
    #[cfg(test)]
    panic_after_done: AtomicBool,
    #[cfg(test)]
    pub(crate) panic_after_spawn: AtomicBool,
    #[cfg(test)]
    abandon_reply: AtomicBool,
    #[cfg(test)]
    panic_scheduler: AtomicBool,
}
#[cfg(test)]
struct Gate {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}
#[cfg(test)]
#[derive(Default)]
struct StartHooks {
    after_spawn: Option<Gate>,
    reap: Option<Gate>,
    panic_after_spawn: bool,
}
impl Shared {
    pub(crate) fn notify_spawn(&self) {
        self.notify.notify_waiters();
    }
    pub(crate) fn fail(&self, error: Error) {
        let mut first = self.terminal.lock().unwrap_or_else(|e| e.into_inner());
        if first.is_none() {
            *first = Some(error);
        }
        self.closed.store(true, Ordering::Release);
        self.cancel.cancel();
    }
    fn error(&self) -> Option<Error> {
        self.terminal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.cancel.cancel();
        self.notify.notify_waiters();
    }
    #[cfg(test)]
    pub(crate) fn hold_reap(&self) {
        Self::gate(&self.reap_gate);
    }
    #[cfg(test)]
    pub(crate) fn hold_after_spawn(&self) {
        Self::gate(&self.after_spawn_gate);
    }
    #[cfg(test)]
    fn gate(slot: &Mutex<Option<Gate>>) {
        let gate = slot.lock().unwrap().take();
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
            let _ = gate.release.recv();
        }
    }
}
enum Work {
    State(StateWork),
    Command(CommandWork),
}
struct CommandWork {
    command: Option<PreparedCommand>,
    token: CancellationToken,
    deadline: Instant,
    reply: Option<oneshot::Sender<Result<CommandOutcome>>>,
    settled: Arc<AtomicBool>,
    entered: bool,
    _lease: Arc<OwnedSemaphorePermit>,
}
impl Drop for CommandWork {
    fn drop(&mut self) {
        // Before entry there can be no child/effect to settle. Once entered,
        // only the outer child registry may publish actual settlement.
        if !self.entered {
            self.settled.store(true, Ordering::Release);
        }
    }
}
struct StateWork {
    token: CancellationToken,
    deadline: Instant,
    reply: oneshot::Sender<Receipt>,
    lease: Arc<OwnedSemaphorePermit>,
}
struct Inner {
    owner: Owner,
    sender: Mutex<Option<mpsc::Sender<Work>>>,
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
    observers: Arc<Semaphore>,
    slots: Arc<Semaphore>,
    limits: Limits,
    command_unavailable: Option<Error>,
}
impl Inner {
    fn close(&self) {
        self.shared.close();
        self.sender.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
    fn join(&self) {
        let mut handle = self.thread.lock().unwrap_or_else(|e| e.into_inner());
        self.join_locked(&mut handle);
    }
    fn join_finished(&self) -> bool {
        let mut handle = self.thread.lock().unwrap_or_else(|e| e.into_inner());
        if handle.as_ref().is_some_and(|thread| !thread.is_finished()) {
            return false;
        }
        self.join_locked(&mut handle);
        true
    }
    fn join_locked(&self, handle: &mut Option<JoinHandle<()>>) {
        // The named handle guard remains held through actual join. Observers
        // never see a taken-but-unjoined handle, nor take an unfinished handle.
        // Emergency last-owner Drop alone may synchronously wait for live IO.
        if let Some(thread) = handle.take() {
            if thread.join().is_err() {
                self.shared.fail(Error::new(
                    "spotify_owner_panicked",
                    "Native owner join failed",
                ));
            }
            self.shared.joined.store(true, Ordering::Release);
            OWNER_ACTIVE.store(false, Ordering::Release);
        }
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.close();
        self.join();
    }
}
#[derive(Clone)]
pub struct SpotifyService(Arc<Inner>);
pub struct Ticket {
    receiver: Option<oneshot::Receiver<Receipt>>,
    token: CancellationToken,
    inner: Arc<Inner>,
    armed: bool,
    // Retains admission even if a failed owner loses its result sender.
    _lease: Arc<OwnedSemaphorePermit>,
}
impl Ticket {
    pub fn cancel(&self) {
        self.token.cancel();
    }
    pub async fn finish(mut self) -> Result<Receipt> {
        let result =
            self.receiver
                .take()
                .expect("single ticket wait")
                .await
                .map_err(|_| {
                    self.inner.shared.error().unwrap_or_else(|| {
                        Error::new("spotify_owner", "Native result owner stopped")
                    })
                });
        self.armed = false;
        result
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if self.armed {
            self.token.cancel();
        }
    }
}
/// Native observation owns the whole work quota until consumed or dropped.
/// Its existence certifies accepted work settled, not server/media delivery.
pub struct Receipt {
    owner: Owner,
    shared: Arc<Shared>,
    result: Result<Outcome>,
    _lease: Arc<OwnedSemaphorePermit>,
}
impl Receipt {
    pub fn context(&self) -> &Context {
        self.owner.context()
    }
    fn validate(&self, owner: &Owner) -> Result<()> {
        if !self.owner.matches(owner) {
            return Err(Error::new("spotify_stale", "Foreign native result owner"));
        }
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(self
                .shared
                .error()
                .unwrap_or_else(|| Error::new("spotify_shutdown", "Native query owner retired")));
        }
        Ok(())
    }
    /// Borrow while retaining the whole native result/publication permit.
    pub fn reply(&self, owner: &Owner) -> Result<&Outcome> {
        self.validate(owner)?;
        self.result.as_ref().map_err(Clone::clone)
    }
    /// Consuming handoff: the caller must independently bound the returned data.
    pub fn into_reply(self, owner: &Owner) -> Result<Outcome> {
        self.validate(owner)?;
        self.result
    }
}
/// Accepted scheduling receipt. Dropping it NEVER cancels an accepted command.
/// Both this receipt and the actual owner independently retain the same whole lease.
pub struct CommandReceipt {
    owner: Owner,
    inner: Arc<Inner>,
    control: CommandControl,
    receiver: oneshot::Receiver<Result<CommandOutcome>>,
    lease: Arc<OwnedSemaphorePermit>,
}
/// Bounded native monitoring only: no admission permit, result, or join handle.
#[derive(Clone)]
pub struct CommandControl {
    token: CancellationToken,
    settled: Arc<AtomicBool>,
}
impl CommandControl {
    pub fn cancel(&self) {
        self.token.cancel();
    }
    /// True only after no actual child/seek continuation remains for this job.
    /// This does not certify public result disposal or native thread join.
    pub fn settled(&self) -> bool {
        self.settled.load(Ordering::Acquire)
    }
}
impl CommandReceipt {
    /// Original early `true`; certifies queue transfer, not spawn/send success.
    pub fn reply(&self, owner: &Owner) -> Result<bool> {
        if !self.owner.matches(owner) {
            return Err(Error::new("spotify_stale", "Foreign native command owner"));
        }
        if self.inner.shared.closed.load(Ordering::Acquire) {
            return Err(self.inner.shared.error().unwrap_or_else(|| {
                Error::new("spotify_shutdown", "Native command owner retired")
            }));
        }
        Ok(true)
    }
    pub fn control(&self) -> CommandControl {
        self.control.clone()
    }
    pub fn cancel(&self) {
        self.control.cancel();
    }
    pub async fn finish(self) -> Result<CommandFinished> {
        let result = self.receiver.await.map_err(|_| {
            self.inner.shared.error().unwrap_or_else(|| {
                Error::new("spotify_owner", "Native command result owner stopped")
            })
        })?;
        Ok(CommandFinished {
            owner: self.owner,
            result,
            _lease: self.lease,
        })
    }
}
pub struct CommandFinished {
    owner: Owner,
    result: Result<CommandOutcome>,
    _lease: Arc<OwnedSemaphorePermit>,
}
impl CommandFinished {
    /// Borrowed actual completion; retired owners may inspect cleanup/error evidence.
    pub fn result(&self, owner: &Owner) -> Result<&CommandOutcome> {
        if !self.owner.matches(owner) {
            return Err(Error::new(
                "spotify_stale",
                "Foreign native completion owner",
            ));
        }
        self.result.as_ref().map_err(Clone::clone)
    }
}
impl SpotifyService {
    pub fn start(owner: Owner, backend: Backend, limits: Limits) -> Result<Self> {
        Self::start_configured(
            owner,
            backend,
            limits,
            #[cfg(test)]
            StartHooks::default(),
        )
    }
    fn start_configured(
        owner: Owner,
        backend: Backend,
        limits: Limits,
        #[cfg(test)] hooks: StartHooks,
    ) -> Result<Self> {
        limits.validate()?;
        if OWNER_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Error::new(
                "spotify_owner_busy",
                "A native query owner still requires actual join",
            ));
        }
        let command_unavailable = backend.unavailable.clone();
        let shared = Arc::new(Shared {
            closed: AtomicBool::new(false),
            done: AtomicBool::new(false),
            joined: AtomicBool::new(false),
            pid: AtomicU32::new(0),
            spawned: AtomicU64::new(0),
            reaped: AtomicU64::new(0),
            commands: AtomicU64::new(0),
            commands_completed: AtomicU64::new(0),
            command_failures: AtomicU64::new(0),
            command_error: Mutex::new(None),
            command_pids: std::array::from_fn(|_| AtomicU32::new(0)),
            queries: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            capability: Mutex::new(Capability::Pending),
            terminal: Mutex::new(None),
            notify: Notify::new(),
            cancel: CancellationToken::new(),
            #[cfg(test)]
            reap_gate: Mutex::new(hooks.reap),
            #[cfg(test)]
            after_spawn_gate: Mutex::new(hooks.after_spawn),
            #[cfg(test)]
            exit_gate: Mutex::new(None),
            #[cfg(test)]
            post_done_gate: Mutex::new(None),
            #[cfg(test)]
            panic_after_done: AtomicBool::new(false),
            #[cfg(test)]
            panic_after_spawn: AtomicBool::new(hooks.panic_after_spawn),
            #[cfg(test)]
            abandon_reply: AtomicBool::new(false),
            #[cfg(test)]
            panic_scheduler: AtomicBool::new(false),
        });
        let (sender, receiver) = mpsc::channel(JOB_CAPACITY);
        let state = shared.clone();
        let thread_owner = owner.clone();
        let thread_limits = limits.clone();
        let thread = std::thread::Builder::new()
            .name("freeshow-spotify".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => runtime.block_on(run_supervised(
                            thread_owner,
                            backend,
                            thread_limits,
                            state.clone(),
                            receiver,
                        )),
                        Err(_) => state.fail(Error::new(
                            "spotify_runtime",
                            "Native runtime could not start",
                        )),
                    }
                }));
                if result.is_err() {
                    state.fail(Error::new(
                        "spotify_owner_panicked",
                        "Native query owner panicked",
                    ));
                }
                #[cfg(test)]
                Shared::gate(&state.exit_gate);
                state.closed.store(true, Ordering::Release);
                state.done.store(true, Ordering::Release);
                state.notify.notify_waiters();
                #[cfg(test)]
                {
                    Shared::gate(&state.post_done_gate);
                    assert!(
                        !state.panic_after_done.load(Ordering::Acquire),
                        "controlled native owner frame panic after done notification"
                    );
                }
            })
            .map_err(|_| {
                OWNER_ACTIVE.store(false, Ordering::Release);
                Error::new("spotify_thread", "Native owner thread could not start")
            })?;
        Ok(Self(Arc::new(Inner {
            owner,
            sender: Mutex::new(Some(sender)),
            shared,
            thread: Mutex::new(Some(thread)),
            observers: observers(),
            slots: jobs(),
            limits,
            command_unavailable,
        })))
    }
    pub fn begin_state(&self, owner: &Owner, deadline: Instant) -> Result<Ticket> {
        if !self.0.owner.matches(owner) {
            return Err(Error::new("spotify_stale", "Foreign native query owner"));
        }
        let sender = self.0.sender.lock().unwrap_or_else(|e| e.into_inner());
        if self.0.shared.closed.load(Ordering::Acquire) {
            return Err(self
                .0
                .shared
                .error()
                .unwrap_or_else(|| Error::new("spotify_shutdown", "Native query owner retired")));
        }
        let sender = sender
            .as_ref()
            .ok_or_else(|| Error::new("spotify_shutdown", "Native query admission closed"))?;
        let lease = self
            .0
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::new("spotify_busy", "Whole query quota exhausted"))?;
        let lease = Arc::new(lease);
        let token = self.0.shared.cancel.child_token();
        let (reply, receiver) = oneshot::channel();
        sender
            .try_send(Work::State(StateWork {
                token: token.clone(),
                deadline: deadline.min(Instant::now() + self.0.limits.query_timeout),
                reply,
                lease: lease.clone(),
            }))
            .map_err(|_| Error::new("spotify_shutdown", "Native queue stopped"))?;
        if self
            .0
            .shared
            .queries
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .is_err()
        {
            self.0.shared.fail(Error::new(
                "spotify_counter_limit",
                "Query counter exhausted",
            ));
            return Err(self.0.shared.error().expect("stored error"));
        }
        Ok(Ticket {
            receiver: Some(receiver),
            token,
            inner: self.0.clone(),
            armed: true,
            _lease: lease,
        })
    }
    pub fn begin_command(
        &self,
        owner: &Owner,
        command: PreparedCommand,
        deadline: Instant,
    ) -> Result<CommandReceipt> {
        if !self.0.owner.matches(owner) {
            return Err(Error::new("spotify_stale", "Foreign native command owner"));
        }
        if let Some(error) = &self.0.command_unavailable {
            return Err(error.clone());
        }
        if let Capability::Unavailable { code } = *self
            .0
            .shared
            .capability
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            return Err(Error::new(code, "Native command capability is unavailable"));
        }
        let sender = self.0.sender.lock().unwrap_or_else(|e| e.into_inner());
        if self.0.shared.closed.load(Ordering::Acquire) {
            return Err(self.0.shared.error().unwrap_or_else(|| {
                Error::new("spotify_shutdown", "Native command owner retired")
            }));
        }
        let sender = sender
            .as_ref()
            .ok_or_else(|| Error::new("spotify_shutdown", "Native command admission closed"))?;
        let lease = Arc::new(
            self.0
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::new("spotify_busy", "Whole job quota exhausted"))?,
        );
        let token = self.0.shared.cancel.child_token();
        let (reply, receiver) = oneshot::channel();
        let settled = Arc::new(AtomicBool::new(false));
        sender
            .try_send(Work::Command(CommandWork {
                command: Some(command),
                token: token.clone(),
                deadline: deadline.min(Instant::now() + self.0.limits.query_timeout),
                reply: Some(reply),
                settled: settled.clone(),
                entered: false,
                _lease: lease.clone(),
            }))
            .map_err(|_| Error::new("spotify_shutdown", "Native command queue stopped"))?;
        increment(&self.0.shared.commands, &self.0.shared);
        Ok(CommandReceipt {
            owner: owner.clone(),
            inner: self.0.clone(),
            control: CommandControl { token, settled },
            receiver,
            lease,
        })
    }
    pub fn close(&self) {
        self.0.close();
    }
    pub async fn ready(&self) -> Result<Capability> {
        let _observer = self.0.observers.clone().try_acquire_owned().map_err(|_| {
            Error::new(
                "spotify_observer_busy",
                "Lifecycle observer quota exhausted",
            )
        })?;
        loop {
            let notified = self.0.shared.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let value = self
                .0
                .shared
                .capability
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if value != Capability::Pending {
                return Ok(value);
            }
            if self.0.shared.closed.load(Ordering::Acquire) {
                return Err(self.0.shared.error().unwrap_or_else(|| {
                    Error::new("spotify_shutdown", "Native query owner retired")
                }));
            }
            notified.await;
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        // Independent sealing never needs a job/result/observer permit.
        self.close();
        let _observer = self.0.observers.clone().try_acquire_owned().map_err(|_| {
            Error::new(
                "spotify_observer_busy",
                "Lifecycle observer quota exhausted",
            )
        })?;
        loop {
            let notified = self.0.shared.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let done = self.0.shared.done.load(Ordering::Acquire);
            if done && self.0.join_finished() {
                break;
            }
            if tokio::runtime::Handle::try_current().is_err() {
                return Err(Error::new(
                    "spotify_runtime",
                    "Pending native thread exit requires Tokio; join ownership retained",
                ));
            }
            if done {
                tokio::time::sleep(Duration::from_millis(1)).await;
            } else {
                notified.await;
            }
        }
        self.0.shared.error().map_or(Ok(()), Err)
    }
    pub async fn retire(&self, owner: &Owner) -> Result<()> {
        if !self.0.owner.matches(owner) {
            return Err(Error::new("spotify_stale", "Foreign native retirement"));
        }
        self.shutdown().await
    }
    pub fn status(&self) -> Status {
        let s = &self.0.shared;
        let joined = s.joined.load(Ordering::Acquire);
        let pid = s.pid.load(Ordering::Acquire);
        let spawned = s.spawned.load(Ordering::Acquire);
        let reaped = s.reaped.load(Ordering::Acquire);
        Status {
            context: self.0.owner.context.clone(),
            closing: s.closed.load(Ordering::Acquire),
            running: !s.done.load(Ordering::Acquire),
            joined,
            drained: joined
                && pid == 0
                && s.command_pids
                    .iter()
                    .all(|p| p.load(Ordering::Acquire) == 0)
                && spawned == reaped,
            retained: JOB_CAPACITY - self.0.slots.available_permits(),
            observers: OBSERVER_CAPACITY - self.0.observers.available_permits(),
            active_pid: pid,
            command_pids: s
                .command_pids
                .iter()
                .map(|p| p.load(Ordering::Acquire))
                .filter(|p| *p != 0)
                .collect(),
            commands: s.commands.load(Ordering::Acquire),
            commands_completed: s.commands_completed.load(Ordering::Acquire),
            command_failures: s.command_failures.load(Ordering::Acquire),
            last_command_error: s
                .command_error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            spawned,
            reaped,
            queries: s.queries.load(Ordering::Acquire),
            completed: s.completed.load(Ordering::Acquire),
            capability: s
                .capability
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            terminal_error: s.error(),
        }
    }
}
fn set_capability(shared: &Shared, capability: &Result<()>) {
    *shared.capability.lock().unwrap_or_else(|e| e.into_inner()) = match capability {
        Ok(()) => Capability::Available,
        Err(error) => Capability::Unavailable { code: error.code },
    };
    shared.notify.notify_waiters();
}
fn record_capability(cached: &RefCell<Result<()>>, shared: &Shared, observed: Result<()>) {
    // Only an actual probe or binary failure changes capability. A job's older
    // snapshot may span concurrent child IO and must never overwrite a newer
    // observation merely because that job finished later. Keep both views in
    // agreement on the same fixed owner without an intervening await.
    *cached.borrow_mut() = observed.clone();
    set_capability(shared, &observed);
}
async fn probe(
    backend: &Backend,
    deadline: Instant,
    cancel: &CancellationToken,
    shared: &Arc<Shared>,
    active: &mut process::Active,
) -> Result<()> {
    match backend.unavailable.clone() {
        Some(error) => Err(error),
        None => {
            match std::panic::AssertUnwindSafe(process::run(
                backend,
                &["--help"],
                (8192, 8192),
                deadline,
                cancel,
                shared,
                active,
            ))
            .catch_unwind()
            .await
            {
                Err(_) => {
                    shared.fail(Error::new(
                        "spotify_owner_panicked",
                        "Native capability probe panicked",
                    ));
                    process::settle(active, shared, true).await;
                    Err(shared.error().expect("stored panic"))
                }
                Ok(result) => match result {
                    Ok(output)
                        if [0, 1].contains(&output.status.code().unwrap_or(-1))
                            && [output.stdout.as_slice(), output.stderr.as_slice()]
                                .iter()
                                .any(|bytes| {
                                    bytes
                                        .windows(b"[--print-reply".len())
                                        .any(|w| w == b"[--print-reply")
                                }) =>
                    {
                        Ok(())
                    }
                    Ok(_) => Err(Error::new(
                        "spotify_binary_unsupported",
                        "Native executable did not identify dbus-send",
                    )),
                    Err(error) => Err(error),
                },
            }
        }
    }
}
type ChildSlot = Rc<tokio::sync::Mutex<process::Active>>;
async fn run_supervised(
    owner: Owner,
    backend: Backend,
    limits: Limits,
    shared: Arc<Shared>,
    receiver: mpsc::Receiver<Work>,
) {
    // Registry is outside the guarded scheduler/futures. Unwinding drops borrows,
    // never the last actual Child handle before this final kill+wait pass.
    let slots: Vec<ChildSlot> = (0..=JOB_CAPACITY)
        .map(|i| {
            Rc::new(tokio::sync::Mutex::new(process::Active::new(
                i.checked_sub(1),
            )))
        })
        .collect();
    let result = std::panic::AssertUnwindSafe(run(
        owner,
        backend,
        limits,
        shared.clone(),
        receiver,
        &slots,
    ))
    .catch_unwind()
    .await;
    if result.is_err() {
        shared.fail(Error::new(
            "spotify_owner_panicked",
            "Native scheduler panicked",
        ));
    }
    for slot in slots {
        let mut active = slot.lock().await;
        process::settle(&mut active, &shared, true).await;
        active.finish_job();
    }
}
async fn run(
    owner: Owner,
    backend: Backend,
    limits: Limits,
    shared: Arc<Shared>,
    mut receiver: mpsc::Receiver<Work>,
    slots: &[ChildSlot],
) {
    let capability = probe(
        &backend,
        Instant::now() + limits.query_timeout,
        &shared.cancel,
        &shared,
        &mut *slots[0].lock().await,
    )
    .await;
    set_capability(&shared, &capability);
    let capability = Rc::new(RefCell::new(capability));
    let mut futures = FuturesUnordered::<LocalBoxFuture<'static, usize>>::new();
    let mut occupied = [false; JOB_CAPACITY + 1];
    let mut states = VecDeque::new();
    let mut ended = false;
    loop {
        if !occupied[0] {
            if let Some(work) = states.pop_front() {
                occupied[0] = true;
                let (owner, backend, limits, shared, capability, slot) = (
                    owner.clone(),
                    backend.clone(),
                    limits.clone(),
                    shared.clone(),
                    capability.clone(),
                    slots[0].clone(),
                );
                futures.push(
                    async move {
                        state_job(owner, backend, limits, shared, capability, slot, work).await;
                        0
                    }
                    .boxed_local(),
                );
            }
        }
        if ended && futures.is_empty() && states.is_empty() {
            break;
        }
        tokio::select! {
            work = receiver.recv(), if !ended => match work {
                None => ended = true,
                Some(work) => {
                    #[cfg(test)]
                    assert!(!shared.panic_scheduler.swap(false, Ordering::AcqRel), "controlled aggregate scheduler panic");
                    #[cfg(test)]
                    if shared.abandon_reply.swap(false, Ordering::AcqRel) {
                        shared.fail(Error::new("spotify_owner", "Controlled result owner failure"));
                        break;
                    }
                    match work {
                        Work::State(work) => states.push_back(work),
                        Work::Command(work) => {
                            // Shared whole-job permits guarantee a vacant command slot.
                            let index = (1..=JOB_CAPACITY).find(|i| !occupied[*i]).expect("bounded command slot");
                            occupied[index] = true;
                            let (backend, limits, shared, slot, capability) = (backend.clone(), limits.clone(), shared.clone(), slots[index].clone(), capability.clone());
                            futures.push(async move { command_job(backend, limits, shared, capability, slot, work).await; index }.boxed_local());
                        }
                    }
                }
            },
            Some(index) = futures.next(), if !futures.is_empty() => occupied[index] = false,
        }
    }
}
async fn state_job(
    owner: Owner,
    backend: Backend,
    limits: Limits,
    shared: Arc<Shared>,
    cached: Rc<RefCell<Result<()>>>,
    slot: ChildSlot,
    work: StateWork,
) {
    let mut active = slot.lock().await;
    active.lease = Some(work.lease.clone());
    let mut capability = cached.borrow().clone();
    let result = if shared.closed.load(Ordering::Acquire) {
        Err(shared
            .error()
            .unwrap_or_else(|| Error::new("spotify_shutdown", "Native owner closing")))
    } else if work.token.is_cancelled() {
        Err(Error::new(
            "spotify_cancelled",
            "Query cancelled before entry",
        ))
    } else if Instant::now() >= work.deadline {
        Err(Error::new("spotify_timeout", "Query expired before entry"))
    } else {
        // Failed capability checks retry only on an actual admitted query.
        // The selected native path/environment never changes and idle has no timer.
        if capability.is_err() {
            capability = probe(&backend, work.deadline, &work.token, &shared, &mut active).await;
            record_capability(&cached, &shared, capability.clone());
        }
        match &capability {
            Err(error) => Err(error.clone()),
            Ok(()) => {
                // Work/lease and actual child slot live outside the panicking
                // query future. Cleanup remains owned before settling receipt.
                match std::panic::AssertUnwindSafe(query(
                    &backend,
                    &limits,
                    work.deadline,
                    &work.token,
                    &shared,
                    &mut active,
                ))
                .catch_unwind()
                .await
                {
                    Ok(result) => result,
                    Err(_) => {
                        shared.fail(Error::new(
                            "spotify_owner_panicked",
                            "Native query panicked",
                        ));
                        process::settle(&mut active, &shared, true).await;
                        Err(shared.error().expect("stored panic"))
                    }
                }
            }
        }
    };
    if let Err(error) = &result {
        if error.code == "spotify_binary_unavailable" {
            record_capability(&cached, &shared, Err(error.clone()));
        }
    }
    if shared
        .completed
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
        .is_err()
    {
        shared.fail(Error::new(
            "spotify_counter_limit",
            "Completion counter exhausted",
        ));
    }
    let _ = work.reply.send(Receipt {
        owner,
        shared,
        result,
        _lease: work.lease,
    });
    active.finish_job();
}
async fn command_job(
    backend: Backend,
    limits: Limits,
    shared: Arc<Shared>,
    cached: Rc<RefCell<Result<()>>>,
    slot: ChildSlot,
    mut work: CommandWork,
) {
    let mut active = slot.lock().await;
    active.lease = Some(work._lease.clone());
    active.settlement = Some(work.settled.clone());
    work.entered = true;
    let result = std::panic::AssertUnwindSafe(async {
        if shared.closed.load(Ordering::Acquire) {
            return Err(shared
                .error()
                .unwrap_or_else(|| Error::new("spotify_shutdown", "Native owner closing")));
        }
        if work.token.is_cancelled() {
            return Err(Error::new(
                "spotify_cancelled",
                "Native command cancelled before entry",
            ));
        }
        if Instant::now() >= work.deadline {
            return Err(Error::new(
                "spotify_timeout",
                "Native command expired before entry",
            ));
        }
        let mut capability = cached.borrow().clone();
        if capability.is_err() {
            capability = probe(&backend, work.deadline, &work.token, &shared, &mut active).await;
            record_capability(&cached, &shared, capability.clone());
        }
        capability?;
        command(
            &backend,
            &limits,
            work.deadline,
            &work.token,
            &shared,
            &mut active,
            work.command.take().expect("single command entry"),
        )
        .await
    })
    .catch_unwind()
    .await;
    let result = match result {
        Ok(result) => result,
        Err(_) => {
            shared.fail(Error::new(
                "spotify_owner_panicked",
                "Native command panicked",
            ));
            process::settle(&mut active, &shared, true).await;
            Err(shared.error().expect("stored panic"))
        }
    };
    if let Err(error) = &result {
        if error.code == "spotify_binary_unavailable" {
            record_capability(&cached, &shared, Err(error.clone()));
        }
    }
    let diagnostic = match &result {
        Err(e) => Some(e.clone()),
        Ok(CommandOutcome::SourceProcessFailure {
            service_unknown: true,
        }) => Some(Error::new(
            "spotify_service_unknown",
            "Spotify service was unavailable at command completion",
        )),
        Ok(CommandOutcome::SourceProcessFailure {
            service_unknown: false,
        }) => Some(Error::new(
            "spotify_process_failure",
            "Native command process failed",
        )),
        _ => None,
    };
    if let Some(error) = diagnostic {
        increment(&shared.command_failures, &shared);
        *shared
            .command_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(error);
    }
    increment(&shared.commands_completed, &shared);
    let _ = work
        .reply
        .take()
        .expect("single command reply")
        .send(result);
    active.finish_job();
}
fn increment(counter: &AtomicU64, shared: &Shared) {
    if counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
        .is_err()
    {
        shared.fail(Error::new(
            "spotify_counter_limit",
            "Native counter exhausted",
        ));
    }
}
async fn command(
    backend: &Backend,
    limits: &Limits,
    deadline: Instant,
    cancel: &CancellationToken,
    shared: &Arc<Shared>,
    active: &mut process::Active,
    command: PreparedCommand,
) -> Result<CommandOutcome> {
    use crate::commands::Command;
    let mut stdout_used = 0;
    let mut stderr_used = 0;
    let tail: Vec<String> = match command.0 {
        Command::PlayPause => vec!["org.mpris.MediaPlayer2.Player.PlayPause".into()],
        Command::Next => vec!["org.mpris.MediaPlayer2.Player.Next".into()],
        Command::Previous => vec!["org.mpris.MediaPlayer2.Player.Previous".into()],
        Command::Pause => vec!["org.mpris.MediaPlayer2.Player.Pause".into()],
        Command::SetVolume(value) => vec![
            "org.freedesktop.DBus.Properties.Set".into(),
            "string:org.mpris.MediaPlayer2.Player".into(),
            "string:Volume".into(),
            format!("variant:double:{value}"),
        ],
        Command::Seek(value) => {
            let output = process::run(
                backend,
                &[
                    "--print-reply",
                    "--dest=org.mpris.MediaPlayer2.spotify",
                    "/org/mpris/MediaPlayer2",
                    "org.freedesktop.DBus.Properties.Get",
                    "string:org.mpris.MediaPlayer2.Player",
                    "string:Metadata",
                ],
                (limits.stdout_bytes, limits.stderr_bytes),
                deadline,
                cancel,
                shared,
                active,
            )
            .await?;
            if !output.status.success() {
                return Ok(process_failure(&output));
            }
            stdout_used = output.stdout.len();
            stderr_used = output.stderr.len();
            let text = std::str::from_utf8(&output.stdout).map_err(|_| {
                Error::new(
                    "spotify_encoding_unsupported",
                    "Native metadata is not supported UTF-8",
                )
            })?;
            let Some(id) = crate::commands::track_id(text)? else {
                return Ok(CommandOutcome::MissingTrackId);
            };
            vec![
                "org.mpris.MediaPlayer2.Player.SetPosition".into(),
                format!("objpath:{id}"),
                format!("int64:{value}"),
            ]
        }
    };
    let mut args = vec![
        "--type=method_call",
        "--dest=org.mpris.MediaPlayer2.spotify",
        "/org/mpris/MediaPlayer2",
    ];
    args.extend(tail.iter().map(String::as_str));
    let output = process::run(
        backend,
        &args,
        (
            limits.stdout_bytes - stdout_used,
            limits.stderr_bytes - stderr_used,
        ),
        deadline,
        cancel,
        shared,
        active,
    )
    .await?;
    Ok(if output.status.success() {
        CommandOutcome::Sent
    } else {
        process_failure(&output)
    })
}
fn process_failure(output: &process::Output) -> CommandOutcome {
    CommandOutcome::SourceProcessFailure {
        service_unknown: output
            .stderr
            .windows(b"ServiceUnknown".len())
            .any(|w| w == b"ServiceUnknown"),
    }
}
async fn query(
    backend: &Backend,
    limits: &Limits,
    deadline: Instant,
    cancel: &CancellationToken,
    shared: &Arc<Shared>,
    active: &mut process::Active,
) -> Result<Outcome> {
    let mut stdout = Vec::new();
    let mut stderr_used = 0;
    for property in ["Metadata", "PlaybackStatus", "Position", "Volume"] {
        let output = process::run(
            backend,
            &[
                "--print-reply",
                "--dest=org.mpris.MediaPlayer2.spotify",
                "/org/mpris/MediaPlayer2",
                "org.freedesktop.DBus.Properties.Get",
                "string:org.mpris.MediaPlayer2.Player",
                &format!("string:{property}"),
            ],
            (
                limits.stdout_bytes - stdout.len(),
                limits.stderr_bytes - stderr_used,
            ),
            deadline,
            cancel,
            shared,
            active,
        )
        .await?;
        stderr_used += output.stderr.len();
        if !output.status.success() {
            return Ok(Outcome::SourceNull(
                if output
                    .stderr
                    .windows(b"ServiceUnknown".len())
                    .any(|w| w == b"ServiceUnknown")
                {
                    NullReason::ServiceUnknown
                } else {
                    NullReason::ProcessFailure
                },
            ));
        }
        stdout.extend_from_slice(&output.stdout);
    }
    let text = std::str::from_utf8(&stdout).map_err(|_| {
        Error::new(
            "spotify_encoding_unsupported",
            "Native output is not supported UTF-8",
        )
    })?;
    let result = parse_linux(text)?;
    if cancel.is_cancelled() {
        return Err(Error::new(
            "spotify_cancelled",
            "Native query cancelled after parsing",
        ));
    }
    if Instant::now() >= deadline {
        return Err(Error::new(
            "spotify_timeout",
            "Total query deadline exceeded during parsing",
        ));
    }
    Ok(result)
}
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
