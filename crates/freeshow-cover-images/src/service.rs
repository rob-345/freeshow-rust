use crate::*;
use std::{
    collections::VecDeque,
    sync::{atomic::AtomicUsize, Condvar, Mutex, Weak},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, Notify};
const DEFAULT_DEADLINE: Duration = Duration::from_secs(10);
pub(crate) struct Check {
    session: Arc<SessionInner>,
    cancel: Cancel,
    deadline: Instant,
}
impl Check {
    pub(crate) fn check(&self) -> Result<()> {
        if self.session.closed.load(Ordering::Acquire) || self.cancel.is_cancelled() {
            Err(Error::new(
                "cover_cancelled",
                "Native operation canceled/retired",
            ))
        } else if Instant::now() >= self.deadline {
            Err(Error::new(
                "cover_deadline",
                "Cooperative work deadline exceeded",
            ))
        } else {
            Ok(())
        }
    }
}
enum Command {
    Covers(TrustedMediaFile),
    Create(Vec<u8>, ImagePolicy),
    Png,
    Size,
    ResizeJpeg90,
    Release,
    #[cfg(test)]
    Gate(Arc<crate::tests::Gate>),
    #[cfg(test)]
    Panic,
    #[cfg(test)]
    PanicOutside,
}
struct Job {
    cell: Arc<Cell>,
    command: Command,
    cancel: Cancel,
    deadline: Instant,
    notify: oneshot::Sender<()>,
}
struct Queue {
    closing: bool,
    jobs: VecDeque<Job>,
    sessions: Vec<Weak<SessionInner>>,
    failures: Vec<Error>,
}
struct Core {
    operation_timeout: Duration,
    queue: Mutex<Queue>,
    wake: Condvar,
    changed: Notify,
    finished: AtomicUsize,
    joins: Mutex<Vec<JoinHandle<()>>>,
}
struct SessionInner {
    core: Weak<Core>,
    context: Context,
    closed: AtomicBool,
    disposed: AtomicBool,
    busy: AtomicBool,
    active: AtomicBool,
    pending: AtomicBool,
    operations: AtomicUsize,
    image: Mutex<Option<codec::Raster>>,
    cell: Mutex<Weak<Cell>>,
    changed: Notify,
}
impl SessionInner {
    fn dispose(&self) {
        if self.pending.load(Ordering::Acquire) {
            return;
        }
        self.image.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(cell) = self
            .cell
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade()
        {
            cell.result.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
        self.disposed.store(true, Ordering::Release);
        self.changed.notify_waiters();
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if !self.pending.load(Ordering::Acquire) {
            self.dispose()
        }
    }
}
struct Cell {
    cancel: Cancel,
    session: Arc<SessionInner>,
    result: Mutex<Option<Result<Reply>>>,
}
impl Drop for Cell {
    fn drop(&mut self) {
        // Release actual retained result buffers before re-opening this session.
        drop(
            self.result
                .get_mut()
                .unwrap_or_else(|e| e.into_inner())
                .take(),
        );
        self.session.busy.store(false, Ordering::Release);
        self.session.changed.notify_waiters();
    }
}
/// One whole-operation lease, including retained image and completed observations.
pub struct Session {
    inner: Arc<SessionInner>,
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(core) = self.inner.core.upgrade() {
            let mut queue = core.queue.lock().unwrap_or_else(|e| e.into_inner());
            self.inner.close(); // serialized with admission's pending/lease publication
            cancel_queued(&mut queue);
            core.wake.notify_all();
        } else {
            self.inner.close();
        }
    }
}

pub struct Ticket {
    cell: Arc<Cell>,
    notify: oneshot::Receiver<()>,
}
impl Ticket {
    pub async fn finish(self) -> Result<Observation> {
        let Self { cell, notify } = self;
        notify
            .await
            .map_err(|_| Error::new("cover_worker_failed", "Owner returned no receipt"))?;
        if cell.session.closed.load(Ordering::Acquire) || cell.cancel.is_cancelled() {
            return Err(Error::new(
                "cover_cancelled",
                "Result retired before collection",
            ));
        }
        Ok(Observation { cell })
    }
}
/// Borrowed inspection keeps result bytes under the session's reserved ownership.
pub struct Observation {
    cell: Arc<Cell>,
}
impl Observation {
    pub fn inspect<T>(&self, f: impl FnOnce(&Reply) -> T) -> Result<T> {
        if self.cell.session.closed.load(Ordering::Acquire) || self.cell.cancel.is_cancelled() {
            return Err(Error::new("cover_cancelled", "Observation retired"));
        }
        let result = self.cell.result.lock().unwrap_or_else(|e| e.into_inner());
        match result.as_ref() {
            Some(Ok(reply)) => Ok(f(reply)),
            Some(Err(e)) => Err(e.clone()),
            None => Err(Error::new("cover_cancelled", "Observation disposed")),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct Status {
    pub sessions: usize,
    pub reserved_bytes: usize,
    pub queued: usize,
    pub active: usize,
    pub finished_workers: usize,
    pub closing: bool,
    pub failures: Vec<Error>,
}
pub struct CoverImageService {
    core: Arc<Core>,
}
impl CoverImageService {
    pub fn new() -> Result<Self> {
        Self::with_operation_timeout(DEFAULT_DEADLINE)
    }
    /// Trusted native configuration; deadlines remain cooperative around actual OS/codec calls.
    pub fn with_operation_timeout(operation_timeout: Duration) -> Result<Self> {
        if operation_timeout.is_zero() || operation_timeout > DEFAULT_DEADLINE {
            return Err(resource("Operation timeout must be >0 and <=10s"));
        }
        let core = Arc::new(Core {
            operation_timeout,
            queue: Mutex::new(Queue {
                closing: false,
                jobs: VecDeque::new(),
                sessions: vec![],
                failures: vec![],
            }),
            wake: Condvar::new(),
            changed: Notify::new(),
            finished: AtomicUsize::new(0),
            joins: Mutex::new(vec![]),
        });
        for index in 0..WORKERS {
            let c = core.clone();
            match thread::Builder::new()
                .name(format!("freeshow-covers-{index}"))
                .spawn(move || worker(c))
            {
                Ok(join) => core
                    .joins
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(join),
                Err(e) => {
                    core.queue.lock().unwrap_or_else(|e| e.into_inner()).closing = true;
                    core.wake.notify_all();
                    for join in core
                        .joins
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .drain(..)
                    {
                        let _ = join.join();
                    }
                    return Err(Error::new("cover_worker_unavailable", e.to_string()));
                }
            }
        }
        Ok(Self { core })
    }
    pub fn begin_session(&self, context: &Context) -> Result<Session> {
        let mut q = self.core.queue.lock().unwrap_or_else(|e| e.into_inner());
        if q.closing {
            return Err(Error::new("cover_closed", "Service admission closed"));
        }
        q.sessions.retain(|s| {
            s.upgrade()
                .is_some_and(|s| !s.disposed.load(Ordering::Acquire))
        });
        if q.sessions.len() >= MAX_SESSIONS {
            return Err(Error::new(
                "cover_busy",
                "Four complete sessions already retained",
            ));
        }
        let inner = Arc::new(SessionInner {
            core: Arc::downgrade(&self.core),
            context: context.clone(),
            closed: AtomicBool::new(false),
            disposed: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            active: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            operations: AtomicUsize::new(0),
            image: Mutex::new(None),
            cell: Mutex::new(Weak::new()),
            changed: Notify::new(),
        });
        q.sessions.push(Arc::downgrade(&inner));
        Ok(Session { inner })
    }
    fn submit(&self, session: &Session, command: Command, cancel: Cancel) -> Result<Ticket> {
        let inner = &session.inner;
        let mut q = self.core.queue.lock().unwrap_or_else(|e| e.into_inner());
        if !inner
            .core
            .upgrade()
            .is_some_and(|c| Arc::ptr_eq(&c, &self.core))
        {
            return Err(Error::new("cover_authority", "Foreign service session"));
        }
        if q.closing || inner.closed.load(Ordering::Acquire) {
            return Err(Error::new("cover_closed", "Session admission closed"));
        }
        if inner.busy.swap(true, Ordering::AcqRel) {
            return Err(Error::new(
                "cover_session_busy",
                "Collect/drop prior native observation before next operation",
            ));
        }
        if inner.operations.load(Ordering::Acquire) >= MAX_OPERATIONS {
            inner.busy.store(false, Ordering::Release);
            return Err(resource("Session operation budget"));
        }
        inner.operations.fetch_add(1, Ordering::AcqRel);
        inner.pending.store(true, Ordering::Release);
        let cell = Arc::new(Cell {
            cancel: cancel.clone(),
            session: inner.clone(),
            result: Mutex::new(None),
        });
        *inner.cell.lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(&cell);
        let (tx, rx) = oneshot::channel();
        q.jobs.push_back(Job {
            cell: cell.clone(),
            command,
            cancel,
            deadline: Instant::now() + self.core.operation_timeout,
            notify: tx,
        });
        self.core.wake.notify_one();
        Ok(Ticket { cell, notify: rx })
    }
    pub fn covers(
        &self,
        session: &Session,
        file: TrustedMediaFile,
        cancel: Cancel,
    ) -> Result<Ticket> {
        if !session.inner.context.same(&file.context) {
            return Err(Error::new("cover_context", "Stale/foreign file authority"));
        }
        self.submit(session, Command::Covers(file), cancel)
    }
    pub fn create(
        &self,
        session: &Session,
        bytes: Vec<u8>,
        policy: ImagePolicy,
        cancel: Cancel,
    ) -> Result<Ticket> {
        if bytes.len() > MAX_ENCODED {
            return Err(resource("Encoded cover exceeds 4MiB"));
        }
        self.submit(session, Command::Create(bytes, policy), cancel)
    }
    pub fn png(&self, session: &Session, cancel: Cancel) -> Result<Ticket> {
        self.submit(session, Command::Png, cancel)
    }
    pub fn size(&self, session: &Session, cancel: Cancel) -> Result<Ticket> {
        self.submit(session, Command::Size, cancel)
    }
    pub fn resize_jpeg90(&self, session: &Session, cancel: Cancel) -> Result<Ticket> {
        self.submit(session, Command::ResizeJpeg90, cancel)
    }
    pub fn release(&self, session: &Session) -> Result<Ticket> {
        self.submit(session, Command::Release, Cancel::default())
    }
    /// Admission/cancellation happens synchronously, before this future is polled.
    pub fn retire<'a>(
        &'a self,
        session: &'a Session,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<()>> + 'a {
        let authority = session
            .inner
            .core
            .upgrade()
            .is_some_and(|c| Arc::ptr_eq(&c, &self.core));
        if authority {
            let mut queue = self.core.queue.lock().unwrap_or_else(|e| e.into_inner());
            session.inner.close(); // same lock as command registration
            cancel_queued(&mut queue);
        }
        async move {
            if !authority {
                return Err(Error::new("cover_authority", "Foreign service session"));
            }
            let wait = async {
                loop {
                    let notified = session.inner.changed.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable(); // register before testing actual ownership
                    if !session.inner.pending.load(Ordering::Acquire) {
                        session.inner.dispose();
                        return Ok(());
                    }
                    notified.await
                }
            };
            tokio::time::timeout(timeout, wait).await.map_err(|_| {
                Error::new(
                    "cover_drain_timeout",
                    "Actual worker retained; retry same session",
                )
            })?
        }
    }
    pub fn status(&self) -> Status {
        let q = self.core.queue.lock().unwrap_or_else(|e| e.into_inner());
        let sessions = q
            .sessions
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|s| !s.disposed.load(Ordering::Acquire))
            .collect::<Vec<_>>();
        Status {
            sessions: sessions.len(),
            reserved_bytes: sessions.len() * SESSION_RESERVATION,
            queued: q.jobs.len(),
            active: sessions
                .iter()
                .filter(|s| s.active.load(Ordering::Acquire))
                .count(),
            finished_workers: self.core.finished.load(Ordering::Acquire),
            closing: q.closing,
            failures: q.failures.clone(),
        }
    }
    /// Cancels queued work, waits genuine owner returns, disposes results/images, joins both threads.
    pub fn shutdown(
        &self,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<()>> + '_ {
        {
            let mut q = self.core.queue.lock().unwrap_or_else(|e| e.into_inner());
            q.closing = true;
            for s in q.sessions.iter().filter_map(Weak::upgrade) {
                s.close()
            }
            cancel_queued(&mut q);
            self.core.wake.notify_all();
        }
        async move {
            let wait = async {
                loop {
                    let notified = self.core.changed.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable(); // notify_waiters must not race this check
                    if self.core.finished.load(Ordering::Acquire) == WORKERS {
                        break;
                    }
                    notified.await
                }
            };
            tokio::time::timeout(timeout, wait).await.map_err(|_| {
                Error::new(
                    "cover_drain_timeout",
                    "Actual worker(s) retained; retry same service",
                )
            })?;
            let mut joins = self.core.joins.lock().unwrap_or_else(|e| e.into_inner());
            for join in joins.drain(..) {
                if join.join().is_err() {
                    let mut q = self.core.queue.lock().unwrap_or_else(|e| e.into_inner());
                    if q.failures.len() < WORKERS {
                        q.failures
                            .push(Error::new("cover_worker_failed", "Worker panic joined"))
                    }
                }
            }
            let q = self.core.queue.lock().unwrap_or_else(|e| e.into_inner());
            for s in q.sessions.iter().filter_map(Weak::upgrade) {
                // Both actual joins prove no command/file/scratch owner survives,
                // including an unexpected panic outside an operation boundary.
                s.active.store(false, Ordering::Release);
                s.pending.store(false, Ordering::Release);
                s.dispose()
            }
            if let Some(e) = q.failures.first() {
                Err(e.clone())
            } else {
                Ok(())
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn panic_outside(&self, s: &Session) -> Result<Ticket> {
        self.submit(s, Command::PanicOutside, Cancel::default())
    }
    #[cfg(test)]
    pub(crate) fn panic_command(&self, s: &Session) -> Result<Ticket> {
        self.submit(s, Command::Panic, Cancel::default())
    }
    #[cfg(test)]
    pub(crate) fn gate(&self, s: &Session, gate: Arc<crate::tests::Gate>) -> Result<Ticket> {
        self.submit(s, Command::Gate(gate), Cancel::default())
    }
}
impl Drop for CoverImageService {
    fn drop(&mut self) {
        let mut q = self.core.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.closing = true;
        for s in q.sessions.iter().filter_map(Weak::upgrade) {
            s.close()
        }
        cancel_queued(&mut q);
        self.core.wake.notify_all(); /* Actual threads keep Core until their return; explicit shutdown proves joins. */
    }
}
struct WorkerFinished(Arc<Core>);
impl Drop for WorkerFinished {
    fn drop(&mut self) {
        if thread::panicking() {
            let mut q = self.0.queue.lock().unwrap_or_else(|e| e.into_inner());
            q.closing = true;
            if q.failures.len() < WORKERS {
                q.failures.push(Error::new(
                    "cover_worker_failed",
                    "Unexpected owner panic; actual join required",
                ));
            }
            for s in q.sessions.iter().filter_map(Weak::upgrade) {
                s.closed.store(true, Ordering::Release);
            }
            cancel_queued(&mut q);
            self.0.wake.notify_all();
        }
        self.0.finished.fetch_add(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}
fn worker(core: Arc<Core>) {
    let _finished = WorkerFinished(core.clone());
    loop {
        let job = {
            let mut q = core.queue.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(job) = q.jobs.pop_front() {
                    job.cell.session.active.store(true, Ordering::Release);
                    break Some(job);
                }
                if q.closing {
                    break None;
                }
                q = core.wake.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let Some(job) = job else { break };
        #[cfg(test)]
        if matches!(&job.command, Command::PanicOutside) {
            panic!("controlled outer worker panic");
        }
        let s = job.cell.session.clone();
        let check = Check {
            session: s.clone(),
            cancel: job.cancel,
            deadline: job.deadline,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute(&s, job.command, &check)
        }))
        .unwrap_or_else(|_| {
            let e = Error::new("cover_worker_failed", "Bounded operation panicked");
            let mut q = core.queue.lock().unwrap_or_else(|e| e.into_inner());
            if q.failures.len() < WORKERS {
                q.failures.push(e.clone())
            }
            q.closing = true;
            for session in q.sessions.iter().filter_map(Weak::upgrade) {
                session.closed.store(true, Ordering::Release)
            }
            core.wake.notify_all();
            Err(e)
        });
        *job.cell.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        s.active.store(false, Ordering::Release);
        s.pending.store(false, Ordering::Release);
        if s.closed.load(Ordering::Acquire) {
            s.dispose()
        }
        s.changed.notify_waiters();
        drop(job.cell);
        let _ = job.notify.send(());
    }
}
fn execute(s: &SessionInner, command: Command, check: &Check) -> Result<Reply> {
    check.check()?;
    let reply = match command {
        Command::Covers(path) => Reply::Covers(extract::read(path, check)?),
        Command::Create(bytes, policy) => {
            let raster = codec::create(bytes, policy, check)?;
            let reply = Reply::Image {
                width: raster.width,
                height: raster.height,
            };
            *s.image.lock().unwrap_or_else(|e| e.into_inner()) = Some(raster);
            reply
        }
        Command::Png => {
            let image = s.image.lock().unwrap_or_else(|e| e.into_inner());
            let image = image
                .as_ref()
                .ok_or_else(|| invalid("No live native image"))?;
            Reply::Bytes {
                data: codec::png(image, check)?,
                diagnostics: vec!["native_png_encoding_differs"],
            }
        }
        Command::Size => {
            let image = s.image.lock().unwrap_or_else(|e| e.into_inner());
            let image = image
                .as_ref()
                .ok_or_else(|| invalid("No live native image"))?;
            Reply::Image {
                width: image.width,
                height: image.height,
            }
        }
        Command::ResizeJpeg90 => {
            let image = s.image.lock().unwrap_or_else(|e| e.into_inner());
            let image = image
                .as_ref()
                .ok_or_else(|| invalid("No live native image"))?;
            let data = codec::resize_jpeg90(image, check)?;
            Reply::Bytes {
                data,
                diagnostics: vec!["native_lanczos3_jpeg90_differs"],
            }
        }
        Command::Release => {
            s.image.lock().unwrap_or_else(|e| e.into_inner()).take();
            Reply::Released
        }
        #[cfg(test)]
        Command::PanicOutside => unreachable!(),
        #[cfg(test)]
        Command::Panic => {
            let _image = s.image.lock().unwrap_or_else(|e| e.into_inner());
            panic!("controlled native codec owner panic")
        }
        #[cfg(test)]
        Command::Gate(gate) => {
            gate.block();
            Reply::Released
        }
    };
    check.check()?;
    Ok(reply)
}

fn cancel_queued(q: &mut Queue) {
    let mut kept = VecDeque::new();
    while let Some(job) = q.jobs.pop_front() {
        if job.cell.session.closed.load(Ordering::Acquire) {
            *job.cell.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(Error::new(
                "cover_cancelled",
                "Queued native job canceled before IO",
            )));
            job.cell.session.pending.store(false, Ordering::Release);
            job.cell.session.dispose();
            job.cell.session.changed.notify_waiters();
            let _ = job.notify.send(());
        } else {
            kept.push_back(job)
        }
    }
    q.jobs = kept;
}
