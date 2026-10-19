use crate::*;
use futures_util::FutureExt;
use std::{
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
    queries: AtomicU64,
    completed: AtomicU64,
    capability: Mutex<Capability>,
    terminal: Mutex<Option<Error>>,
    notify: Notify,
    cancel: CancellationToken,
    #[cfg(test)]
    reap_gate: Mutex<Option<Gate>>,
    #[cfg(test)]
    exit_gate: Mutex<Option<Gate>>,
    #[cfg(test)]
    pub(crate) panic_after_spawn: AtomicBool,
    #[cfg(test)]
    abandon_reply: AtomicBool,
}
#[cfg(test)]
struct Gate {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
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
    fn gate(slot: &Mutex<Option<Gate>>) {
        let gate = slot.lock().unwrap().take();
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
            let _ = gate.release.recv();
        }
    }
}
struct Work {
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
}
impl Inner {
    fn close(&self) {
        self.shared.close();
        self.sender.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
    fn join(&self) {
        // No await between taking and joining: observer cancellation cannot
        // detach this handle. Final thread TLS/destructors are nonpreemptible.
        let mut handle = self.thread.lock().unwrap_or_else(|e| e.into_inner());
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
    pub fn into_reply(self, owner: &Owner) -> Result<Outcome> {
        if !self.owner.matches(owner) {
            return Err(Error::new("spotify_stale", "Foreign native result owner"));
        }
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(self
                .shared
                .error()
                .unwrap_or_else(|| Error::new("spotify_shutdown", "Native query owner retired")));
        }
        self.result
    }
}
impl SpotifyService {
    pub fn start(owner: Owner, backend: Backend, limits: Limits) -> Result<Self> {
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
        let shared = Arc::new(Shared {
            closed: AtomicBool::new(false),
            done: AtomicBool::new(false),
            joined: AtomicBool::new(false),
            pid: AtomicU32::new(0),
            spawned: AtomicU64::new(0),
            reaped: AtomicU64::new(0),
            queries: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            capability: Mutex::new(Capability::Pending),
            terminal: Mutex::new(None),
            notify: Notify::new(),
            cancel: CancellationToken::new(),
            #[cfg(test)]
            reap_gate: Mutex::new(None),
            #[cfg(test)]
            exit_gate: Mutex::new(None),
            #[cfg(test)]
            panic_after_spawn: AtomicBool::new(false),
            #[cfg(test)]
            abandon_reply: AtomicBool::new(false),
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
                        Ok(runtime) => runtime.block_on(run(
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
            .try_send(Work {
                token: token.clone(),
                deadline: deadline.min(Instant::now() + self.0.limits.query_timeout),
                reply,
                lease: lease.clone(),
            })
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
            if self.0.shared.done.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
        self.0.join();
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
            drained: joined && pid == 0 && spawned == reaped,
            retained: JOB_CAPACITY - self.0.slots.available_permits(),
            observers: OBSERVER_CAPACITY - self.0.observers.available_permits(),
            active_pid: pid,
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
async fn probe(
    backend: &Backend,
    deadline: Instant,
    cancel: &CancellationToken,
    shared: &Arc<Shared>,
    active: &mut Option<tokio::process::Child>,
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
async fn run(
    owner: Owner,
    backend: Backend,
    limits: Limits,
    shared: Arc<Shared>,
    mut receiver: mpsc::Receiver<Work>,
) {
    let mut active = None;
    let mut capability = probe(
        &backend,
        Instant::now() + limits.query_timeout,
        &shared.cancel,
        &shared,
        &mut active,
    )
    .await;
    set_capability(&shared, &capability);
    while let Some(work) = receiver.recv().await {
        #[cfg(test)]
        if shared.abandon_reply.swap(false, Ordering::AcqRel) {
            shared.fail(Error::new(
                "spotify_owner",
                "Controlled result owner failure",
            ));
            break;
        }
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
                capability =
                    probe(&backend, work.deadline, &work.token, &shared, &mut active).await;
                set_capability(&shared, &capability);
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
                capability = Err(error.clone());
                set_capability(&shared, &capability);
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
            owner: owner.clone(),
            shared: shared.clone(),
            result,
            _lease: work.lease,
        });
    }
}
async fn query(
    backend: &Backend,
    limits: &Limits,
    deadline: Instant,
    cancel: &CancellationToken,
    shared: &Arc<Shared>,
    active: &mut Option<tokio::process::Child>,
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
