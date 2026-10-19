pub mod device_id;
mod native;
pub use native::meminfo_field;
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch, Semaphore};

const SHUTDOWN_OBSERVERS: usize = 4;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Error {
    pub code: String,
    pub message: String,
}
impl Error {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    fn os(api: &str, error: std::io::Error) -> Self {
        Self::new("system_os", format!("{api}: {error}"))
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OsInfo {
    pub platform: String,
    pub name: String,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RamUsage {
    pub total: f64,
    pub free: f64,
    pub performance_mode: bool,
}

/// Internal evidence accompanying a sample; only `usage()` is the renderer reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MemorySample {
    pub total: u64,
    pub free: u64,
    pub total_source: String,
    pub free_source: String,
}
impl MemorySample {
    pub fn usage(&self) -> RamUsage {
        let total = self.total as f64;
        let free = self.free as f64;
        RamUsage {
            total,
            free,
            performance_mode: performance_mode(total, free),
        }
    }
}

pub fn performance_mode(total: f64, free: f64) -> bool {
    // Retain JS Number arithmetic/order, including strict usage comparison
    // and its NaN behavior. Actual snapshots reject an invalid zero total.
    total / 1024.0 / 1024.0 / 1024.0 <= 8.0 || (total - free) / total > 0.98
}
pub fn device_name(hostname: &str) -> String {
    hostname.replacen(".local", "", 1)
}
pub fn platform_alias(os: &str) -> Option<&'static str> {
    match os {
        "linux" => Some("linux"),
        "macos" => Some("darwin"),
        "windows" => Some("win32"),
        _ => None,
    }
}
pub fn architecture_alias(architecture: &str, little_endian: bool) -> Option<&'static str> {
    match architecture {
        "x86_64" => Some("x64"),
        "x86" => Some("ia32"),
        "aarch64" => Some("arm64"),
        "arm" => Some("arm"),
        "powerpc" => Some("ppc"),
        "powerpc64" => Some("ppc64"),
        "s390x" => Some("s390x"),
        "riscv64" => Some("riscv64"),
        "loongarch64" => Some("loong64"),
        "mips" if little_endian => Some("mipsel"),
        "mips" => Some("mips"),
        "mips64" if little_endian => Some("mips64el"),
        _ => None,
    }
}
fn get_os() -> Result<OsInfo, Error> {
    let platform = platform_alias(std::env::consts::OS)
        .ok_or_else(|| Error::new("system_unsupported", "Unsupported target OS"))?;
    let arch = architecture_alias(std::env::consts::ARCH, cfg!(target_endian = "little"))
        .ok_or_else(|| Error::new("system_unsupported", "Unsupported target architecture"))?;
    Ok(OsInfo {
        platform: platform.into(),
        name: native::hostname()?,
        arch: arch.into(),
    })
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub queue_capacity: usize,
    pub max_job_duration: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            queue_capacity: 8,
            max_job_duration: Duration::from_secs(2),
        }
    }
}
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);
impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
#[derive(Clone)]
pub struct JobControl {
    pub deadline: Instant,
    pub cancel: CancelToken,
}
impl Default for JobControl {
    fn default() -> Self {
        Self {
            deadline: Instant::now() + Duration::from_secs(2),
            cancel: CancelToken::default(),
        }
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub closing: bool,
    pub worker_running: bool,
    pub active_jobs: usize,
    pub queued_jobs: usize,
    pub queue_capacity: usize,
    pub worker_joined: bool,
    pub shutdown_observers: usize,
}

#[cfg(test)]
#[derive(Default)]
struct WorkerHooks {
    exit: Option<(
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
    )>,
    panic_at_exit: bool,
}

enum Operation {
    Os,
    #[cfg(test)]
    Panic,
    DeviceName,
    Memory,
    #[cfg(test)]
    Pause {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    },
}
#[derive(Debug)]
enum Answer {
    Os(OsInfo),
    DeviceName(String),
    Memory(MemorySample),
}
struct Job {
    operation: Operation,
    control: JobControl,
    reply: oneshot::Sender<Result<Answer, Error>>,
}
struct State {
    closing: AtomicBool,
    active: AtomicUsize,
    queued: AtomicUsize,
    done: watch::Sender<bool>,
    failure: Mutex<Option<Error>>,
}
struct Inner {
    sender: Mutex<Option<mpsc::Sender<Job>>>,
    state: Arc<State>,
    limits: Limits,
    completion: watch::Receiver<bool>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    shutdown_observers: Arc<Semaphore>,
}
impl Inner {
    // Keep this guard through the actual join: another shutdown/status caller
    // must not mistake a taken-but-unjoined handle for completed teardown.
    fn try_join_finished(&self) -> bool {
        let mut owner = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if owner.as_ref().is_some_and(|thread| !thread.is_finished()) {
            return false;
        }
        if let Some(thread) = owner.take() {
            // Rust's documented nonblocking join pattern. A brief final native
            // teardown interval after is_finished is not OS-preemptible.
            if thread.join().is_err() {
                self.state.retain_failure(Error::new(
                    "system_worker",
                    "System worker panicked during native teardown",
                ));
            }
        }
        true
    }
    fn close(&self) {
        self.state.closing.store(true, Ordering::Release);
        self.sender
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }
}
impl State {
    fn retain_failure(&self, error: Error) {
        self.closing.store(true, Ordering::Release);
        self.failure
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_or_insert(error);
    }
    fn failure(&self) -> Option<Error> {
        self.failure
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.close();
    }
}

/// One dedicated OS/proc worker. Requests have no renderer-supplied payload,
/// immediate bounded admission, cooperative deadlines and terminal shutdown.
#[derive(Clone)]
pub struct SystemService(Arc<Inner>);
struct CancelOnDrop(Option<CancelToken>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(token) = self.0.take() {
            token.cancel();
        }
    }
}

fn check(state: &State, control: &JobControl) -> Result<(), Error> {
    if state.closing.load(Ordering::Acquire) {
        return Err(Error::new("system_shutdown", "System service is shut down"));
    }
    if control.cancel.is_cancelled() {
        return Err(Error::new("system_cancelled", "System request cancelled"));
    }
    if Instant::now() >= control.deadline {
        return Err(Error::new(
            "system_deadline",
            "System request deadline elapsed",
        ));
    }
    Ok(())
}
impl SystemService {
    pub fn start(limits: Limits) -> Result<Self, Error> {
        Self::start_inner(
            limits,
            #[cfg(test)]
            WorkerHooks::default(),
        )
    }
    fn start_inner(limits: Limits, #[cfg(test)] hooks: WorkerHooks) -> Result<Self, Error> {
        if !(1..=32).contains(&limits.queue_capacity)
            || limits.max_job_duration.is_zero()
            || limits.max_job_duration > Duration::from_secs(10)
        {
            return Err(Error::new(
                "system_invalid",
                "System service limits exceed hard bounds",
            ));
        }
        let (sender, mut receiver) = mpsc::channel::<Job>(limits.queue_capacity);
        let (done, completion) = watch::channel(false);
        let state = Arc::new(State {
            closing: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            done,
            failure: Mutex::new(None),
        });
        let worker = state.clone();
        let thread = std::thread::Builder::new()
            .name("freeshow-system".into())
            .spawn(move || {
                while let Some(job) = receiver.blocking_recv() {
                    worker.queued.fetch_sub(1, Ordering::AcqRel);
                    worker.active.store(1, Ordering::Release);
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        check(&worker, &job.control)?;
                        let answer = match job.operation {
                            Operation::Os => Answer::Os(get_os()?),
                            #[cfg(test)]
                            Operation::Panic => panic!("Controlled system worker fault"),
                            Operation::DeviceName => {
                                Answer::DeviceName(device_name(&native::hostname()?))
                            }
                            Operation::Memory => Answer::Memory(native::memory()?),
                            #[cfg(test)]
                            Operation::Pause { entered, release } => {
                                let _ = entered.send(());
                                let _ = release.recv();
                                Answer::DeviceName("test gate".into())
                            }
                        };
                        check(&worker, &job.control)?;
                        Ok(answer)
                    }));
                    let result = match result {
                        Ok(result) => result,
                        Err(_) => {
                            let error = Error::new("system_worker", "System worker panicked");
                            // Publish terminal failure before the reply/consumer
                            // can observe completion and begin shutdown.
                            worker.retain_failure(error.clone());
                            Err(error)
                        }
                    };
                    worker.active.store(0, Ordering::Release);
                    let _ = job.reply.send(result);
                }
                worker.done.send_replace(true);
                #[cfg(test)]
                {
                    if let Some((entered, release)) = hooks.exit {
                        let _ = entered.send(());
                        let _ = release.recv();
                    }
                    assert!(
                        !hooks.panic_at_exit,
                        "Controlled native thread teardown fault"
                    );
                }
            })
            .map_err(|error| Error::os("start system worker", error))?;
        Ok(Self(Arc::new(Inner {
            sender: Mutex::new(Some(sender)),
            state,
            limits,
            completion,
            worker: Mutex::new(Some(thread)),
            shutdown_observers: Arc::new(Semaphore::new(SHUTDOWN_OBSERVERS)),
        })))
    }
    fn admit(
        &self,
        operation: Operation,
        mut control: JobControl,
    ) -> Result<oneshot::Receiver<Result<Answer, Error>>, Error> {
        check(&self.0.state, &control)?;
        control.deadline = control
            .deadline
            .min(Instant::now() + self.0.limits.max_job_duration);
        let (reply, response) = oneshot::channel();
        let sender = self
            .0
            .sender
            .lock()
            .map_err(|_| Error::new("system_worker", "System admission lock poisoned"))?;
        check(&self.0.state, &control)?;
        let sender = sender
            .as_ref()
            .ok_or_else(|| Error::new("system_shutdown", "System service is shut down"))?;
        self.0.state.queued.fetch_add(1, Ordering::AcqRel);
        if let Err(error) = sender.try_send(Job {
            operation,
            control,
            reply,
        }) {
            self.0.state.queued.fetch_sub(1, Ordering::AcqRel);
            return Err(Error::new(
                if matches!(error, mpsc::error::TrySendError::Full(_)) {
                    "system_busy"
                } else {
                    "system_shutdown"
                },
                "System request not admitted",
            ));
        }
        Ok(response)
    }
    async fn request(&self, operation: Operation, control: JobControl) -> Result<Answer, Error> {
        let token = control.cancel.clone();
        let response = self.admit(operation, control)?;
        let mut guard = CancelOnDrop(Some(token));
        let answer = response.await;
        guard.0.take();
        answer.map_err(|_| Error::new("system_worker", "System worker stopped before replying"))?
    }
    pub async fn get_os(&self, control: JobControl) -> Result<OsInfo, Error> {
        match self.request(Operation::Os, control).await? {
            Answer::Os(answer) => Ok(answer),
            _ => unreachable!(),
        }
    }
    pub async fn get_device_name(&self, control: JobControl) -> Result<String, Error> {
        match self.request(Operation::DeviceName, control).await? {
            Answer::DeviceName(answer) => Ok(answer),
            _ => unreachable!(),
        }
    }
    pub async fn memory_sample(&self, control: JobControl) -> Result<MemorySample, Error> {
        match self.request(Operation::Memory, control).await? {
            Answer::Memory(answer) => Ok(answer),
            _ => unreachable!(),
        }
    }
    pub async fn ram_usage(&self, control: JobControl) -> Result<RamUsage, Error> {
        Ok(self.memory_sample(control).await?.usage())
    }
    pub fn status(&self) -> Status {
        let owner = self
            .0
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Status {
            closing: self.0.state.closing.load(Ordering::Acquire),
            worker_running: owner.as_ref().is_some_and(|thread| !thread.is_finished()),
            active_jobs: self.0.state.active.load(Ordering::Acquire),
            queued_jobs: self.0.state.queued.load(Ordering::Acquire),
            queue_capacity: self.0.limits.queue_capacity,
            worker_joined: owner.is_none(),
            shutdown_observers: SHUTDOWN_OBSERVERS - self.0.shutdown_observers.available_permits(),
        }
    }
    /// Closes admission and cooperatively drains and actually joins the worker.
    /// Four observers are admitted without waiting. Timeout/drop preserves the
    /// real handle for a later retry; a native call cannot be interrupted.
    pub async fn shutdown(&self) -> Result<(), Error> {
        self.shutdown_until(Instant::now() + SHUTDOWN_TIMEOUT).await
    }
    async fn shutdown_until(&self, deadline: Instant) -> Result<(), Error> {
        self.0.close();
        let _observer = self
            .0
            .shutdown_observers
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                Error::new("system_busy", "System shutdown observer capacity exhausted")
            })?;
        loop {
            if self.0.try_join_finished() {
                if let Some(error) = self.0.state.failure() {
                    return Err(error);
                }
                // The watch is only loop-completion evidence AFTER an actual
                // native join, never a substitute for it.
                return if *self.0.completion.borrow() {
                    Ok(())
                } else {
                    Err(Error::new(
                        "system_worker",
                        "System worker exited without completion",
                    ))
                };
            }
            if Instant::now() >= deadline {
                return Err(Error::new(
                    "system_shutdown_timeout",
                    "Native system worker still owns teardown; shutdown can be retried",
                ));
            }
            if tokio::runtime::Handle::try_current().is_err() {
                return Err(Error::new(
                    "system_runtime",
                    "Pending system teardown requires a Tokio runtime; native handle retained",
                ));
            }
            tokio::time::sleep(
                Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now())),
            )
            .await;
        }
    }
}

#[cfg(test)]
mod tests;
