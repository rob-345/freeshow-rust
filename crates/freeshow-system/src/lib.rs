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
use tokio::sync::{mpsc, oneshot, watch};

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
}

enum Operation {
    Os,
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
}
struct Inner {
    sender: Mutex<Option<mpsc::Sender<Job>>>,
    state: Arc<State>,
    limits: Limits,
    completion: watch::Receiver<bool>,
}
impl Inner {
    fn close(&self) {
        self.state.closing.store(true, Ordering::Release);
        if let Ok(mut sender) = self.sender.lock() {
            sender.take();
        }
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
        });
        let worker = state.clone();
        std::thread::Builder::new()
            .name("freeshow-system".into())
            .spawn(move || {
                while let Some(job) = receiver.blocking_recv() {
                    worker.queued.fetch_sub(1, Ordering::AcqRel);
                    worker.active.store(1, Ordering::Release);
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        check(&worker, &job.control)?;
                        let answer = match job.operation {
                            Operation::Os => Answer::Os(get_os()?),
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
                    }))
                    .unwrap_or_else(|_| Err(Error::new("system_worker", "System worker panicked")));
                    worker.active.store(0, Ordering::Release);
                    let _ = job.reply.send(result);
                }
                worker.done.send_replace(true);
            })
            .map_err(|error| Error::os("start system worker", error))?;
        Ok(Self(Arc::new(Inner {
            sender: Mutex::new(Some(sender)),
            state,
            limits,
            completion,
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
        Status {
            closing: self.0.state.closing.load(Ordering::Acquire),
            worker_running: !*self.0.completion.borrow(),
            active_jobs: self.0.state.active.load(Ordering::Acquire),
            queued_jobs: self.0.state.queued.load(Ordering::Acquire),
            queue_capacity: self.0.limits.queue_capacity,
        }
    }
    /// Closes admission, cancels queued/active work cooperatively and drains the
    /// one worker. An OS API already in progress cannot be interrupted.
    pub async fn shutdown(&self) -> Result<(), Error> {
        self.0.close();
        let mut done = self.0.completion.clone();
        while !*done.borrow_and_update() {
            done.changed()
                .await
                .map_err(|_| Error::new("system_worker", "System completion channel closed"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
