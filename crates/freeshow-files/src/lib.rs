mod case_table;
mod locate;
pub mod paths;

pub use locate::{
    FolderOutcome, LocateOutcome, LocateRequest, Located, MediaRoot, Replacement,
    ThumbnailInvalidation, WorkStats,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Error {
    pub code: String,
    pub message: String,
    pub thumbnail_invalidations: Vec<ThumbnailInvalidation>,
    pub diagnostics: Vec<String>,
}
impl Error {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            thumbnail_invalidations: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone)]
pub struct Limits {
    pub workers: usize,
    pub queue_capacity: usize,
    pub max_folders: usize,
    pub max_path_bytes: usize,
    pub max_input_bytes: usize,
    pub max_entries: usize,
    pub max_directories: usize,
    pub max_operations: usize,
    pub max_work_bytes: usize,
    pub max_job_duration: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            workers: 2,
            queue_capacity: 8,
            max_folders: 128,
            max_path_bytes: 32 * 1024,
            max_input_bytes: 1024 * 1024,
            max_entries: 50_000,
            max_directories: 4096,
            max_operations: 250_000,
            max_work_bytes: 16 * 1024 * 1024,
            max_job_duration: Duration::from_secs(10),
        }
    }
}
impl Limits {
    fn validate(&self) -> Result<(), Error> {
        if !(1..=4).contains(&self.workers)
            || !(1..=64).contains(&self.queue_capacity)
            || self.max_folders > 4096
            || self.max_path_bytes > 1024 * 1024
            || self.max_input_bytes > 16 * 1024 * 1024
            || self.max_entries > 1_000_000
            || self.max_directories > 65_536
            || self.max_operations > 2_000_000
            || self.max_work_bytes > 64 * 1024 * 1024
            || self.max_job_duration > Duration::from_secs(60)
        {
            return Err(Error::new(
                "files_invalid",
                "Filesystem service limits exceed the hard resource bounds",
            ));
        }
        Ok(())
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
            deadline: Instant::now() + Duration::from_secs(10),
            cancel: CancelToken::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub closing: bool,
    pub workers: usize,
    pub active_jobs: usize,
    pub queued_jobs: usize,
    pub queue_capacity: usize,
}

enum Operation {
    Locate(LocateRequest, MediaRoot),
    Folder(MediaRoot),
    #[cfg(test)]
    Pause {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    },
}
enum Answer {
    Locate(LocateOutcome),
    Folder(FolderOutcome),
}
struct Job {
    id: u64,
    operation: Operation,
    control: JobControl,
    reply: oneshot::Sender<Result<Answer, Error>>,
}
struct State {
    closing: Arc<AtomicBool>,
    tokens: Mutex<HashMap<u64, Weak<AtomicBool>>>,
    next_id: AtomicU64,
    workers: AtomicUsize,
    active: AtomicUsize,
    queued: AtomicUsize,
    completed: watch::Sender<usize>,
}
struct Inner {
    sender: Mutex<Option<mpsc::Sender<Job>>>,
    limits: Arc<Limits>,
    state: Arc<State>,
    completed: watch::Receiver<usize>,
}

impl Inner {
    fn close(&self) {
        self.state.closing.store(true, Ordering::Release);
        if let Ok(tokens) = self.state.tokens.lock() {
            for token in tokens.values().filter_map(Weak::upgrade) {
                token.store(true, Ordering::Release);
            }
        }
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

/// A fixed pool of dedicated filesystem threads, with bounded immediate
/// admission. No filesystem call runs on GTK, the Tokio IO reactor or its
/// general blocking pool. Drop of a request future cancels its admitted work.
#[derive(Clone)]
pub struct FilesService(Arc<Inner>);

struct CancelOnDrop(Option<CancelToken>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(token) = self.0.take() {
            token.cancel();
        }
    }
}

impl FilesService {
    pub fn start(limits: Limits) -> Result<Self, Error> {
        limits.validate()?;
        let limits = Arc::new(limits);
        let (sender, receiver) = mpsc::channel::<Job>(limits.queue_capacity);
        let receiver = Arc::new(Mutex::new(receiver));
        let (completed, completion) = watch::channel(limits.workers);
        let state = Arc::new(State {
            closing: Arc::new(AtomicBool::new(false)),
            tokens: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            workers: AtomicUsize::new(limits.workers),
            active: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            completed,
        });
        for index in 0..limits.workers {
            let receiver = receiver.clone();
            let state = state.clone();
            let limits = limits.clone();
            std::thread::Builder::new()
                .name(format!("freeshow-files-{index}"))
                .spawn(move || {
                    loop {
                        let next = receiver
                            .lock()
                            .ok()
                            .and_then(|mut receiver| receiver.blocking_recv());
                        let Some(job) = next else {
                            break;
                        };
                        state.queued.fetch_sub(1, Ordering::AcqRel);
                        state.active.fetch_add(1, Ordering::AcqRel);
                        let budget =
                            locate::Budget::new(limits.clone(), job.control, state.closing.clone());
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            match job.operation {
                                Operation::Locate(request, root) => {
                                    budget.locate(request, root).map(Answer::Locate)
                                }
                                Operation::Folder(root) => budget.folder(root).map(Answer::Folder),
                                #[cfg(test)]
                                Operation::Pause { entered, release } => {
                                    let _ = entered.send(());
                                    let _ = release.recv();
                                    Ok(Answer::Folder(FolderOutcome {
                                        reply: "paused".into(),
                                        diagnostics: Vec::new(),
                                    }))
                                }
                            }
                        }))
                        .unwrap_or_else(|_| {
                            Err(Error::new("files_worker", "Filesystem worker panicked"))
                        });
                        let _ = job.reply.send(result);
                        if let Ok(mut tokens) = state.tokens.lock() {
                            tokens.remove(&job.id);
                        }
                        state.active.fetch_sub(1, Ordering::AcqRel);
                    }
                    state.workers.fetch_sub(1, Ordering::AcqRel);
                    state
                        .completed
                        .send_modify(|remaining| *remaining = remaining.saturating_sub(1));
                })
                .map_err(|error| Error::new("files_worker", error.to_string()))?;
        }
        Ok(Self(Arc::new(Inner {
            sender: Mutex::new(Some(sender)),
            limits,
            state,
            completed: completion,
        })))
    }

    fn admit(
        &self,
        operation: Operation,
        mut control: JobControl,
    ) -> Result<oneshot::Receiver<Result<Answer, Error>>, Error> {
        // Bound retained queued payloads before admission, without touching
        // the filesystem. The worker repeats validation after normalization.
        if matches!(&operation, Operation::Locate(request, _) if request.folders.len() > self.0.limits.max_folders)
        {
            return Err(Error::new("files_limit", "Search root count exceeded"));
        }
        let (paths, folders): (Vec<&str>, usize) = match &operation {
            Operation::Locate(request, root) => (
                std::iter::once(request.file_path.as_str())
                    .chain(std::iter::once(root.path.as_str()))
                    .chain(request.folders.iter().map(String::as_str))
                    .collect(),
                request.folders.len(),
            ),
            Operation::Folder(root) => (vec![root.path.as_str()], 0),
            #[cfg(test)]
            Operation::Pause { .. } => (Vec::new(), 0),
        };
        if folders > self.0.limits.max_folders
            || paths
                .iter()
                .any(|path| path.len() > self.0.limits.max_path_bytes)
            || paths.iter().map(|path| path.len()).sum::<usize>() > self.0.limits.max_input_bytes
        {
            return Err(Error::new(
                "files_limit",
                "Filesystem request exceeds admission payload bounds",
            ));
        }
        if paths.iter().any(|path| path.contains('\0')) {
            return Err(Error::new("files_invalid", "File paths cannot contain NUL"));
        }
        if control.cancel.is_cancelled() {
            return Err(Error::new(
                "files_cancelled",
                "Filesystem request was cancelled",
            ));
        }
        if control.deadline <= Instant::now() {
            return Err(Error::new(
                "files_deadline",
                "Filesystem request deadline elapsed",
            ));
        }
        control.deadline = control
            .deadline
            .min(Instant::now() + self.0.limits.max_job_duration);
        let (reply, result) = oneshot::channel();
        let sender = self
            .0
            .sender
            .lock()
            .map_err(|_| Error::new("files_worker", "Filesystem admission lock poisoned"))?;
        if self.0.state.closing.load(Ordering::Acquire) {
            return Err(Error::new(
                "files_shutdown",
                "Filesystem service is shut down",
            ));
        }
        let sender = sender
            .as_ref()
            .ok_or_else(|| Error::new("files_shutdown", "Filesystem service is shut down"))?;
        let id = self.0.state.next_id.fetch_add(1, Ordering::Relaxed);
        self.0
            .state
            .tokens
            .lock()
            .map_err(|_| Error::new("files_worker", "Filesystem cancellation registry poisoned"))?
            .insert(id, Arc::downgrade(&control.cancel.0));
        self.0.state.queued.fetch_add(1, Ordering::AcqRel);
        match sender.try_send(Job {
            id,
            operation,
            control,
            reply,
        }) {
            Ok(()) => Ok(result),
            Err(error) => {
                self.0.state.queued.fetch_sub(1, Ordering::AcqRel);
                if let Ok(mut tokens) = self.0.state.tokens.lock() {
                    tokens.remove(&id);
                }
                Err(Error::new(
                    if matches!(error, mpsc::error::TrySendError::Full(_)) {
                        "files_busy"
                    } else {
                        "files_shutdown"
                    },
                    "Filesystem service cannot admit this request",
                ))
            }
        }
    }

    async fn request(&self, operation: Operation, control: JobControl) -> Result<Answer, Error> {
        let token = control.cancel.clone();
        let receiver = self.admit(operation, control)?;
        let mut guard = CancelOnDrop(Some(token));
        let answer = receiver.await;
        guard.0.take();
        answer
            .map_err(|_| Error::new("files_worker", "Filesystem worker stopped before replying"))?
    }

    pub async fn locate(
        &self,
        request: LocateRequest,
        root: MediaRoot,
        control: JobControl,
    ) -> Result<LocateOutcome, Error> {
        match self
            .request(Operation::Locate(request, root), control)
            .await?
        {
            Answer::Locate(answer) => Ok(answer),
            _ => unreachable!(),
        }
    }

    pub async fn media_folder_path(
        &self,
        root: MediaRoot,
        control: JobControl,
    ) -> Result<FolderOutcome, Error> {
        match self.request(Operation::Folder(root), control).await? {
            Answer::Folder(answer) => Ok(answer),
            _ => unreachable!(),
        }
    }

    pub fn status(&self) -> ServiceStatus {
        ServiceStatus {
            closing: self.0.state.closing.load(Ordering::Acquire),
            workers: self.0.state.workers.load(Ordering::Acquire),
            active_jobs: self.0.state.active.load(Ordering::Acquire),
            queued_jobs: self.0.state.queued.load(Ordering::Acquire),
            queue_capacity: self.0.limits.queue_capacity,
        }
    }

    /// Terminal admission close plus cooperative cancellation/drain. Individual
    /// OS filesystem calls are not interruptible; hosts can bound how long they
    /// await this future without creating replacement worker threads.
    pub async fn shutdown(&self) -> Result<(), Error> {
        self.0.close();
        let mut completion = self.0.completed.clone();
        while *completion.borrow_and_update() != 0 {
            completion
                .changed()
                .await
                .map_err(|_| Error::new("files_worker", "Filesystem completion channel stopped"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
