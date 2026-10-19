//! Native, bounded audio tag parsing and compatibility projection for READ_AUDIO_METADATA.
mod common;
mod id3_text;
mod js;
mod opened;
mod parser;
mod preflight;

pub use js::Js as CommonValue;
pub use opened::{
    AudioFormat, CoverKnowledge, FileIdentity, FileObservations, FileVersion, FormatHint,
    NativeCommon, NativeFileTime, OpenedAdmission, OpenedRead, OpenedTicket,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::{oneshot, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

pub const COMMANDS: &[&str] = &["READ_AUDIO_METADATA"];
#[derive(Clone, Debug)]
pub struct AudioConfig {
    pub max_jobs: usize,
    pub read_limit: usize,
    pub operation_limit: usize,
    pub allocation_limit: usize,
    pub tag_limit: usize,
    pub output_limit: usize,
    pub parse_timeout: Duration,
}
impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            max_jobs: 2,
            read_limit: 16 * 1024 * 1024,
            operation_limit: 50_000,
            allocation_limit: 4 * 1024 * 1024,
            tag_limit: 4096,
            output_limit: 4 * 1024 * 1024,
            parse_timeout: Duration::from_secs(10),
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("unsupported audio command: {0}")]
    Unsupported(String),
    #[error("invalid audio configuration/payload")]
    Invalid,
    #[error("audio parser capacity exhausted")]
    Busy,
    #[error("audio request cancelled")]
    Cancelled,
    #[error("audio service shut down")]
    Shutdown,
    #[error("audio input missing, malformed or unreadable")]
    Input,
    #[error("audio input changed during parsing")]
    Changed,
    #[error("audio resource limit: {0}")]
    Resource(String),
    #[error("unsupported audio variant: {0}")]
    UnsupportedVariant(String),
    #[error("audio worker failed: {0}")]
    Worker(String),
}
impl AudioError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsupported(_) => "unsupported_command",
            Self::Invalid => "invalid_request",
            Self::Busy => "audio_busy",
            Self::Cancelled => "cancelled",
            Self::Shutdown => "audio_shutdown",
            Self::Input => "audio_input_rejected",
            Self::Changed => "audio_input_changed",
            Self::Resource(_) => "audio_resource_limit",
            Self::UnsupportedVariant(_) => "audio_unsupported_variant",
            Self::Worker(_) => "audio_worker_error",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioDiagnostic {
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioReply {
    pub data: Value,
    pub diagnostics: Vec<AudioDiagnostic>,
}
impl AudioReply {
    fn plain(data: Value) -> Self {
        Self {
            data,
            diagnostics: vec![],
        }
    }
    fn failure(error: AudioError) -> Result<Self, AudioError> {
        match error {
            AudioError::Input => Ok(Self::plain(Value::Null)),
            AudioError::Cancelled => Err(AudioError::Cancelled),
            error => Ok(Self {
                data: Value::Null,
                diagnostics: vec![AudioDiagnostic {
                    code: error.code().into(),
                    message: error.to_string(),
                }],
            }),
        }
    }
}
/// Pure original-handler projection, including unknown common fields and JS key order.
/// Input/output are JSON; internally generated NaN/Infinity serialize to null like JSON.stringify.
pub fn project_metadata(metadata: Value) -> Value {
    js::project(&js::Js::from(metadata)).json()
}

enum Work {
    Public(PublicWork),
    Opened(opened::Work),
}
struct PublicWork {
    path: String,
    cancel: CancellationToken,
    response: oneshot::Sender<Result<AudioReply, AudioError>>,
    _permit: OwnedSemaphorePermit,
}
struct Inner {
    config: AudioConfig,
    cancel: CancellationToken,
    permits: Arc<Semaphore>,
    sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    completion: Arc<Completion>,
    shutdown_gate: tokio::sync::Mutex<()>,
}
#[derive(Clone)]
pub struct AudioService {
    inner: Arc<Inner>,
}
impl AudioService {
    pub fn new(config: AudioConfig) -> Result<Self, AudioError> {
        if !(1..=8).contains(&config.max_jobs)
            || !(1..=64 * 1024 * 1024).contains(&config.read_limit)
            || !(1..=1_000_000).contains(&config.operation_limit)
            || !(1..=16 * 1024 * 1024).contains(&config.allocation_limit)
            || !(1..=16384).contains(&config.tag_limit)
            || !(1..=16 * 1024 * 1024).contains(&config.output_limit)
            || config.parse_timeout.is_zero()
            || config.parse_timeout > Duration::from_secs(120)
        {
            return Err(AudioError::Invalid);
        }
        let (sender, receiver) = mpsc::sync_channel::<Work>(config.max_jobs);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = vec![];
        let completion = Arc::new(Completion {
            remaining: AtomicUsize::new(config.max_jobs),
            done: Notify::new(),
        });
        for index in 0..config.max_jobs {
            let receiver = receiver.clone();
            let config = config.clone();
            let completion = completion.clone();
            let worker = std::thread::Builder::new()
                .name(format!("freeshow-audio-{index}"))
                .spawn(move || {
                    let _exit = WorkerExit(completion);
                    loop {
                        let work = receiver.lock().expect("audio queue poisoned").recv();
                        let Ok(work) = work else {
                            break;
                        };
                        let work = match work {
                            Work::Opened(work) => {
                                opened::execute(work, &config);
                                continue;
                            }
                            Work::Public(work) => work,
                        };
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            parser::parse(&work.path, &config, work.cancel.clone()).map(|common| {
                                let mut metadata = js::Js::object();
                                metadata.set("common", common);
                                js::project(&metadata).json()
                            })
                        }))
                        .unwrap_or_else(|_| {
                            Err(AudioError::Worker("native parser panicked".into()))
                        });
                        let reply = match result {
                            Ok(data) => {
                                if !within_limit(&data, config.output_limit) {
                                    AudioReply::failure(AudioError::Resource(
                                        "serialized reply".into(),
                                    ))
                                } else {
                                    Ok(AudioReply::plain(data))
                                }
                            }
                            Err(error) => AudioReply::failure(error),
                        };
                        // Native parsing and output validation are complete. Release admission
                        // before waking the caller so a sequential request cannot see Busy.
                        drop(work._permit);
                        let _ = work.response.send(reply);
                    }
                });
            match worker {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    drop(sender);
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return Err(AudioError::Worker(error.to_string()));
                }
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                permits: Arc::new(Semaphore::new(config.max_jobs)),
                config,
                cancel: CancellationToken::new(),
                sender: Mutex::new(Some(sender)),
                workers: Mutex::new(workers),
                completion,
                shutdown_gate: tokio::sync::Mutex::new(()),
            }),
        })
    }
    pub async fn dispatch(&self, command: &str, payload: Value) -> Result<AudioReply, AudioError> {
        if !COMMANDS.contains(&command) {
            return Err(AudioError::Unsupported(command.into()));
        }
        if !payload.is_object() {
            return Err(AudioError::Invalid);
        }
        if self.inner.cancel.is_cancelled() {
            return Err(AudioError::Shutdown);
        }
        let permit = self.inner.acquire_job()?;
        if let Some(path) = payload.get("filePath").and_then(Value::as_str) {
            if path.len() > 32 * 1024 || path.contains('\0') {
                return Err(AudioError::Invalid);
            }
        }
        let Some(path) = payload
            .get("filePath")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
        else {
            return Ok(AudioReply::plain(Value::Null));
        };
        let cancel = self.inner.cancel.child_token();
        let _guard = CancelOnDrop(cancel.clone());
        let (tx, rx) = oneshot::channel();
        let work = Work::Public(PublicWork {
            path: path.into(),
            cancel,
            response: tx,
            _permit: permit,
        });
        {
            let sender = self
                .inner
                .sender
                .lock()
                .map_err(|_| AudioError::Worker("queue lock poisoned".into()))?;
            let Some(sender) = sender.as_ref() else {
                return Err(AudioError::Shutdown);
            };
            sender.try_send(work).map_err(|_| AudioError::Busy)?;
        }
        rx.await
            .map_err(|_| AudioError::Worker("worker disconnected".into()))?
    }
    /// Cooperative cancellation; native parser code and OS filesystem calls cannot be preempted.
    pub async fn shutdown(&self) {
        self.inner.cancel.cancel();
        self.inner
            .sender
            .lock()
            .expect("audio queue lock poisoned")
            .take();
        // Concurrent shutdown callers all observe completion of the real joins.
        let _join_owner = self.inner.shutdown_gate.lock().await;
        let _ = self
            .inner
            .permits
            .acquire_many(self.inner.config.max_jobs as u32)
            .await;
        loop {
            let finished = self.inner.completion.done.notified();
            tokio::pin!(finished);
            finished.as_mut().enable();
            if self.inner.completion.remaining.load(Ordering::Acquire) == 0 {
                break;
            }
            finished.await;
        }
        let workers = std::mem::take(
            &mut *self
                .inner
                .workers
                .lock()
                .expect("audio worker lock poisoned"),
        );
        // Parser work and receive loops have finished before this join. No runtime
        // or additional blocking task is needed. WorkerExit signals just before
        // actual thread return, so join may briefly wait for final TLS/destructors.
        for worker in workers {
            let _ = worker.join();
        }
    }
    /// Reserve shared parser/result capacity before constructing native input buffers.
    /// This does not authorize a path or perform filesystem IO.
    pub fn reserve_opened(&self) -> Result<OpenedAdmission, AudioError> {
        Ok(OpenedAdmission::new(
            self.inner.clone(),
            self.inner.acquire_job()?,
        ))
    }
}
impl Inner {
    fn acquire_job(&self) -> Result<OwnedSemaphorePermit, AudioError> {
        let permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            if self.cancel.is_cancelled() {
                AudioError::Shutdown
            } else {
                AudioError::Busy
            }
        })?;
        if self.cancel.is_cancelled() {
            return Err(AudioError::Shutdown);
        }
        Ok(permit)
    }
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Ok(sender) = self.sender.get_mut() {
            sender.take();
        }
    }
}

fn within_limit(data: &Value, limit: usize) -> bool {
    struct Counter {
        used: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.used) {
                return Err(std::io::Error::other("audio reply limit"));
            }
            self.used += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter { used: 0, limit }, data).is_ok()
}

struct Completion {
    remaining: AtomicUsize,
    done: Notify,
}
struct WorkerExit(Arc<Completion>);
impl Drop for WorkerExit {
    fn drop(&mut self) {
        self.0.remaining.fetch_sub(1, Ordering::AcqRel);
        self.0.done.notify_waiters();
    }
}

#[cfg(test)]
mod tests;
