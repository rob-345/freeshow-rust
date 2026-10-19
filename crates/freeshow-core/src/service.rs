//! Ordered library IO on a dedicated thread, with bounded asynchronous admission.
//!
//! Mutations have one owner. A bounded show-reader pool shares a filesystem
//! barrier with the ordered phase, and can run during backup compression.
//! Context/status locks remain short and are never held across filesystem IO.
use crate::{
    protocol::{Event, Request, RuntimeContext},
    runtime::{Runtime, ShowReader},
    Error, Result,
};
use serde::Serialize;
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, RwLock,
};
use tokio::sync::{mpsc, oneshot, watch, Mutex as AsyncMutex};

mod phase;
pub use phase::{
    LibraryPhase, PhaseTicket, PHASE_CAPACITY, PHASE_FINISH_CAPACITY, PHASE_JOB_CAPACITY,
    PHASE_JOB_LIMIT, PHASE_REPLY_BYTES, PHASE_REQUEST_BYTES,
};

const QUEUE_CAPACITY: usize = 8;
const READ_WORKERS: usize = 2;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveJob {
    pub channel: String,
    pub command: String,
    pub phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listener_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryStatus {
    pub context: RuntimeContext,
    pub active_job: Option<ActiveJob>,
    pub worker_running: bool,
    /// An observation, not a reservation; concurrent admission can change it.
    pub queued_jobs: usize,
    pub queue_capacity: usize,
    pub queued_reads: usize,
    pub read_queue_capacity: usize,
    pub read_workers: usize,
    pub active_reads: usize,
    pub accepting_requests: bool,
    /// Whole pending/active/retained phase capabilities, including retained replies.
    pub retained_phases: usize,
    pub phase_capacity: usize,
    pub retained_phase_jobs: usize,
    pub phase_job_capacity: usize,
    pub phase_job_limit: usize,
    pub phase_admission_closed: bool,
    pub retained_phase_observers: usize,
    pub phase_finish_capacity: usize,
}

/// Committed media configuration and its library provenance, captured together.
/// Reading this descriptor performs no filesystem work and never joins the IO
/// queue. Consumers must validate provenance again before publishing effects.
#[derive(Debug, Clone)]
pub struct MediaSettingsSnapshot {
    pub context: RuntimeContext,
    pub configured_media_folder: Value,
}

struct Snapshot {
    context: RuntimeContext,
    active_job: Option<ActiveJob>,
    worker_running: bool,
    reader: ShowReader,
    active_reads: usize,
    media_settings: std::result::Result<Value, String>,
}

type Reply = oneshot::Receiver<Result<Vec<Event>>>;
type LibraryCommit = Box<dyn FnOnce(&mut Runtime) -> Result<Vec<Event>> + Send>;
struct OrderedCommit {
    operation: LibraryCommit,
    refresh_media_settings: bool,
}

enum Job {
    Phase(Box<PhaseJob>),
    Drain {
        reply: oneshot::Sender<Result<Vec<Event>>>,
    },
    Dispatch {
        sequence: Option<u64>,
        request: Box<Request>,
        commit: Option<OrderedCommit>,
        window: String,
        reply: oneshot::Sender<Result<Vec<Event>>>,
    },
    #[cfg(test)]
    Pause {
        entered: oneshot::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    },
    #[cfg(test)]
    PauseBackup {
        entered: oneshot::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    },
    #[cfg(test)]
    Stop,
}

struct PhaseJob {
    request: Request,
    owner: phase::Owner,
    reply: oneshot::Sender<Result<()>>,
}

enum ReadJob {
    Dispatch {
        request: Request,
        reply: oneshot::Sender<Result<Vec<Event>>>,
    },
    #[cfg(test)]
    Pause {
        entered: oneshot::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    },
}

#[derive(Clone)]
pub struct LibraryService {
    jobs: mpsc::Sender<Job>,
    reads: mpsc::Sender<ReadJob>,
    admission: Arc<AsyncMutex<()>>,
    admitted_sequence: Arc<AtomicU64>,
    accepting: Arc<AtomicBool>,
    progress: watch::Receiver<u64>,
    snapshot: Arc<RwLock<Snapshot>>,
    phases: Arc<phase::Registry>,
}

impl LibraryService {
    pub fn start(runtime: Runtime) -> Result<Self> {
        Self::with_capacity(runtime, QUEUE_CAPACITY)
    }

    fn with_capacity(mut runtime: Runtime, capacity: usize) -> Result<Self> {
        let media_settings = runtime.stores.get_key("config", "mediaFolderPath")?;
        let (jobs, mut receiver) = mpsc::channel(capacity);
        let (completed, progress) = watch::channel(0);
        let (reads, reader_queue) = mpsc::channel(capacity);
        let reader_queue = Arc::new(Mutex::new(reader_queue));
        // This is an IO barrier, separate from the short diagnostic lock.
        // Readers hold a shared lease while opening/reading a show. Ordered
        // writes hold it exclusively, preventing partial multi-file mutations.
        let files = Arc::new(RwLock::new(()));
        let snapshot = Arc::new(RwLock::new(Snapshot {
            context: runtime.context(),
            active_job: None,
            worker_running: true,
            reader: runtime.show_reader(),
            active_reads: 0,
            media_settings: Ok(media_settings),
        }));
        for index in 0..READ_WORKERS {
            let queue = reader_queue.clone();
            let state = snapshot.clone();
            let barrier = files.clone();
            std::thread::Builder::new()
                .name(format!("freeshow-show-reader-{index}"))
                .spawn(move || show_reader_loop(queue, state, barrier))
                .map_err(|error| unavailable(&error.to_string()))?;
        }
        let shared = snapshot.clone();
        // The worker owns Runtime exclusively. It holds no sender, so dropping
        // all service handles drains admitted work and then stops this thread.
        std::thread::Builder::new()
            .name("freeshow-library".into())
            .spawn(move || {
                let _lifecycle = WorkerLifecycle(shared.clone());
                #[cfg(test)]
                let mut compression_pause: Option<(
                    oneshot::Sender<()>,
                    std::sync::mpsc::Receiver<()>,
                )> = None;
                while let Some(job) = receiver.blocking_recv() {
                    match job {
                        Job::Phase(job) => {
                            if !run_phase(*job, &mut runtime, &shared, &files, &completed) {
                                break;
                            }
                        }
                        Job::Drain { reply } => {
                            let _ = reply.send(Ok(Vec::new()));
                        }
                        Job::Dispatch {
                            sequence,
                            request,
                            commit,
                            window,
                            reply,
                        } => {
                            let refresh_media_settings = commit
                                .as_ref()
                                .is_some_and(|commit| commit.refresh_media_settings)
                                || (request.channel == "MAIN"
                                    && (matches!(
                                        request.data["channel"].as_str(),
                                        Some("SAVE" | "UPDATE_DATA_PATH" | "RESTORE")
                                    ) || (request.data["channel"] == "SET_STORE_VALUE"
                                        && request.data["data"]["file"] == "config")));
                            if let Ok(mut state) = shared.write() {
                                state.active_job = Some(ActiveJob {
                                    channel: request.channel.clone(),
                                    listener_id: request.listener_id.clone(),
                                    command: request.data["channel"].as_str().unwrap_or("").into(),
                                    phase: "ordered",
                                });
                            } else {
                                break;
                            }
                            let Ok(exclusive) = files.write() else { break };
                            let prepared = if let Some(commit) = commit {
                                runtime
                                    .validate_context(&request)
                                    .and_then(|()| (commit.operation)(&mut runtime))
                                    .map(Into::into)
                            } else {
                                runtime.prepare_dispatch(*request, &window)
                            };
                            let context = runtime.context();
                            // Do this on the owner, outside the short snapshot
                            // lock, even after an error: a multi-file operation
                            // may have applied an earlier part before failing.
                            let media_settings = refresh_media_settings.then(|| {
                                runtime
                                    .stores
                                    .get_key("config", "mediaFolderPath")
                                    .map_err(|error| error.to_string())
                            });
                            if let Ok(mut state) = shared.write() {
                                state.context = context.clone();
                                state.reader = runtime.show_reader();
                                if let Some(settings) = media_settings {
                                    state.media_settings = settings;
                                }
                                if prepared.as_ref().is_ok_and(|job| job.has_backup()) {
                                    state.active_job.as_mut().unwrap().phase = "backupCompression";
                                }
                            } else {
                                break;
                            }
                            // A prepared backup owns private copies. No library
                            // mutation can start until it finishes, but readers
                            // may now load uncached shows independently.
                            // Publish the ordered phase, including failed
                            // operations, before permitting dependent reads.
                            if let Some(sequence) = sequence {
                                completed.send_replace(sequence);
                            }
                            drop(exclusive);
                            #[cfg(test)]
                            if prepared.as_ref().is_ok_and(|job| job.has_backup()) {
                                if let Some((entered, release)) = compression_pause.take() {
                                    let _ = entered.send(());
                                    let _ = release.recv();
                                }
                            }
                            let mut result = prepared.map(|job| job.finish());
                            if let Ok(mut state) = shared.write() {
                                state.active_job = None;
                            } else {
                                break;
                            }
                            if let Ok(events) = &mut result {
                                for event in events {
                                    event.context = Some(context.clone());
                                }
                            }
                            // Once admitted, a mutation completes even if its
                            // caller closes or drops the response future.
                            let _ = reply.send(result);
                        }
                        #[cfg(test)]
                        Job::Pause { entered, release } => {
                            let Ok(_exclusive) = files.write() else { break };
                            let _ = entered.send(());
                            let _ = release.recv();
                        }
                        #[cfg(test)]
                        Job::PauseBackup { entered, release } => {
                            compression_pause = Some((entered, release));
                        }
                        #[cfg(test)]
                        Job::Stop => break,
                    }
                }
            })
            .map_err(|error| Error::WorkerUnavailable(error.to_string()))?;
        Ok(Self {
            jobs,
            reads,
            admission: Arc::new(AsyncMutex::new(())),
            admitted_sequence: Arc::new(AtomicU64::new(0)),
            accepting: Arc::new(AtomicBool::new(true)),
            progress,
            snapshot,
            phases: Arc::new(phase::Registry::new()),
        })
    }

    pub fn context(&self) -> Result<RuntimeContext> {
        let state = self
            .snapshot
            .read()
            .map_err(|_| unavailable("snapshot lock poisoned"))?;
        if !state.worker_running {
            return Err(unavailable("worker stopped"));
        }
        Ok(state.context.clone())
    }

    pub fn status(&self) -> Result<LibraryStatus> {
        let (
            retained_phases,
            retained_phase_jobs,
            retained_phase_observers,
            phase_admission_closed,
        ) = self.phases.status();
        let state = self
            .snapshot
            .read()
            .map_err(|_| unavailable("snapshot lock poisoned"))?;
        Ok(LibraryStatus {
            context: state.context.clone(),
            active_job: state.active_job.clone(),
            worker_running: state.worker_running,
            queued_jobs: self.jobs.max_capacity() - self.jobs.capacity(),
            queue_capacity: self.jobs.max_capacity(),
            queued_reads: self.reads.max_capacity() - self.reads.capacity(),
            read_queue_capacity: self.reads.max_capacity(),
            read_workers: READ_WORKERS,
            active_reads: state.active_reads,
            accepting_requests: self.accepting.load(Ordering::Acquire),
            retained_phases,
            phase_capacity: PHASE_CAPACITY,
            retained_phase_jobs,
            phase_job_capacity: PHASE_JOB_CAPACITY,
            phase_job_limit: PHASE_JOB_LIMIT,
            phase_admission_closed,
            retained_phase_observers,
            phase_finish_capacity: PHASE_FINISH_CAPACITY,
        })
    }

    pub fn media_settings(&self, request: &Request) -> Result<MediaSettingsSnapshot> {
        let state = self
            .snapshot
            .read()
            .map_err(|_| unavailable("snapshot lock poisoned"))?;
        if !state.worker_running || !self.accepting.load(Ordering::Acquire) {
            return Err(unavailable("library is stopped or closing"));
        }
        if request.context.as_ref() != Some(&state.context) {
            return Err(Error::StaleLibrary {
                recovery_path: None,
                recovery_error: None,
            });
        }
        let configured_media_folder = state
            .media_settings
            .as_ref()
            .map_err(|error| Error::Invalid(format!("media settings unavailable: {error}")))?
            .clone();
        Ok(MediaSettingsSnapshot {
            context: state.context.clone(),
            configured_media_folder,
        })
    }

    /// Native picker callbacks also belong to the library where they started.
    /// Capture one snapshot for both validation and event tagging.
    pub fn contextualize(&self, request: &Request, events: &mut [Event]) -> Result<()> {
        let context = self.context()?;
        if request.context.as_ref() != Some(&context) {
            return Err(Error::StaleLibrary {
                recovery_path: None,
                recovery_error: None,
            });
        }
        for event in events {
            event.context = Some(context.clone());
        }
        Ok(())
    }

    async fn enqueue(&self, request: Request, window: &str) -> Result<Reply> {
        self.enqueue_operation(request, window, None).await
    }

    async fn enqueue_operation(
        &self,
        request: Request,
        window: &str,
        commit: Option<OrderedCommit>,
    ) -> Result<Reply> {
        let (reply, received) = oneshot::channel();
        // FIFO asynchronous admission makes shutdown wait behind callers that
        // already began admission, including those waiting for queue space.
        // Context/status snapshots and show readers never take this lock.
        let _admission = self.admission.lock().await;
        if !self.accepting.load(Ordering::Acquire) {
            return Err(unavailable("library is closing"));
        }
        let permit = self
            .jobs
            .reserve()
            .await
            .map_err(|_| unavailable("request queue closed"))?;
        // A read-only or unsupported command waiting behind compression must
        // not make later show reads wait for the ZIP. Only file mutations and
        // index scans (which repair IDs/write caches) establish a dependency.
        let mutation = commit.is_some()
            || (request.channel == "MAIN"
                && matches!(
                    request.data["channel"].as_str(),
                    Some(
                        "SAVE"
                            | "UPDATE_DATA_PATH"
                            | "SET_STORE_VALUE"
                            | "RESTORE"
                            | "DELETE_BACKUP"
                            | "SHOWS"
                            | "REFRESH_SHOWS"
                    )
                ));
        let sequence = if mutation {
            Some(
                self.admitted_sequence
                    .load(Ordering::Acquire)
                    .checked_add(1)
                    .ok_or_else(|| unavailable("request sequence exhausted"))?,
            )
        } else {
            None
        };
        // Publish before send so an overlapping read cannot miss this ticket.
        // There is no suspension point between publishing and enqueueing it.
        if let Some(sequence) = sequence {
            self.admitted_sequence.store(sequence, Ordering::Release);
        }
        permit.send(Job::Dispatch {
            sequence,
            request: Box::new(request),
            commit,
            window: window.into(),
            reply,
        });
        Ok(received)
    }

    async fn enqueue_read(&self, request: Request) -> Result<Reply> {
        // Keep read-after-write admission order, but a backup's completed copy
        // phase is enough: readers do not wait for its ZIP compression.
        if !self.accepting.load(Ordering::Acquire) {
            return Err(unavailable("library is closing"));
        }
        let fence = self.admitted_sequence.load(Ordering::Acquire);
        let mut progress = self.progress.clone();
        progress
            .wait_for(|completed| *completed >= fence)
            .await
            .map_err(|_| unavailable("worker stopped before read fence"))?;
        let (reply, received) = oneshot::channel();
        self.reads
            .send(ReadJob::Dispatch { request, reply })
            .await
            .map_err(|_| unavailable("show reader queue closed"))?;
        Ok(received)
    }

    /// Terminal admission barrier. Earlier admission attempts finish first;
    /// once this barrier owns admission, new requests are rejected. Accepted
    /// file jobs are attempted in order even if this caller drops its wait.
    pub async fn finish_writes(&self) -> Result<()> {
        // A live phase owns admission. Independent terminal control must seal
        // it before waiting for admission, even if every result slot is full.
        // Accepted phase IO still drains before this terminal queue fence.
        self.phases.close();
        let admission = self.admission.lock().await;
        self.accepting.store(false, Ordering::Release);
        let (reply, received) = oneshot::channel();
        self.jobs
            .send(Job::Drain { reply })
            .await
            .map_err(|_| unavailable("request queue closed"))?;
        drop(admission);
        self.receive_reply(received, false).await.map(|_| ())
    }

    pub async fn dispatch(&self, request: Request, window: &str) -> Result<Vec<Event>> {
        let read = request.channel == "MAIN" && request.data["channel"] == "SHOW";
        let received = if read {
            self.enqueue_read(request).await?
        } else {
            self.enqueue(request, window).await?
        };
        self.receive_reply(received, read).await
    }

    /// Apply trusted native preparation results on the existing ordered owner.
    /// The closure runs only after its original request's context is validated
    /// under the filesystem barrier. This is not a renderer command registry.
    /// Once admitted, effects finish even if the caller drops its response wait.
    pub async fn commit(
        &self,
        request: Request,
        window: &str,
        operation: impl FnOnce(&mut Runtime) -> Result<Vec<Event>> + Send + 'static,
    ) -> Result<Vec<Event>> {
        let received = self
            .enqueue_operation(
                request,
                window,
                Some(OrderedCommit {
                    operation: Box::new(operation),
                    refresh_media_settings: true,
                }),
            )
            .await?;
        self.receive_reply(received, false).await
    }

    /// Apply a trusted native operation which preserves mediaFolderPath. Unlike
    /// ordinary `commit`, this does not perform an implicit unbounded CONFIG
    /// reread after the closure. The caller must retain the existing media
    /// setting; DEVICE_ID changes only machineId, and ACCESS queries change none.
    /// Context validation, ordered admission, accepted Drop ownership, mutation
    /// fences and terminal drain are identical to `commit`.
    pub async fn commit_without_media_refresh(
        &self,
        request: Request,
        window: &str,
        operation: impl FnOnce(&mut Runtime) -> Result<Vec<Event>> + Send + 'static,
    ) -> Result<Vec<Event>> {
        let received = self
            .enqueue_operation(
                request,
                window,
                Some(OrderedCommit {
                    operation: Box::new(operation),
                    refresh_media_settings: false,
                }),
            )
            .await?;
        self.receive_reply(received, false).await
    }

    /// Fence earlier ordered work, then retain normal admission for a bounded
    /// native continuation. Subjobs use `LibraryPhase::submit`, never ordinary
    /// `commit` (which intentionally waits behind the phase). The IO barrier is
    /// acquired only while executing each actual subjob, not while awaiting a
    /// different native capability. Status/context and idle-phase SHOW remain
    /// independent. Dropping a pending begin cannot retain admission.
    pub async fn begin_phase(&self, request: Request, window: &str) -> Result<LibraryPhase> {
        if self.jobs.is_closed() {
            return Err(unavailable("library worker stopped"));
        }
        let (phase, owner) =
            self.phases
                .create(&request, window, self.admitted_sequence.clone())?;
        let admission = self.admission.clone().lock_owned().await;
        if !self.accepting.load(Ordering::Acquire) || phase.is_closed() {
            return Err(phase::closed());
        }
        let permit = self
            .jobs
            .reserve()
            .await
            .map_err(|_| unavailable("request queue closed"))?;
        if phase.is_closed() {
            return Err(phase::closed());
        }
        phase.attach_admission(admission)?;
        let (reply, received) = oneshot::channel();
        permit.send(Job::Phase(Box::new(PhaseJob {
            request,
            owner,
            reply,
        })));
        self.receive_phase_reply(received).await?;
        Ok(phase)
    }

    async fn receive_phase_reply(&self, received: oneshot::Receiver<Result<()>>) -> Result<()> {
        // Like ordinary replies, observe receiver closure even if a reserved
        // permit published after the IO receiver's actual exit.
        tokio::select! {
            biased;
            result = received => result.map_err(|_| unavailable("worker stopped before phase admission"))??,
            _ = self.jobs.closed() => return Err(unavailable("library worker stopped")),
        }
        Ok(())
    }

    async fn receive_reply(&self, received: Reply, read: bool) -> Result<Vec<Event>> {
        // A reserved mpsc permit may publish after its receiver has dropped.
        // In that race the queued job can retain its oneshot sender while this
        // service is alive. Observe receiver closure independently of the reply.
        tokio::select! {
            biased;
            result = received => result.map_err(|_| unavailable("worker stopped before replying"))?,
            _ = self.jobs.closed() => Err(unavailable("library worker stopped")),
            _ = self.reads.closed(), if read => Err(unavailable("show readers stopped")),
        }
    }
}

fn run_phase(
    job: PhaseJob,
    runtime: &mut Runtime,
    snapshot: &Arc<RwLock<Snapshot>>,
    files: &RwLock<()>,
    completed: &watch::Sender<u64>,
) -> bool {
    let PhaseJob {
        request,
        mut owner,
        reply,
    } = job;
    if !owner.enter() {
        let _ = reply.send(Err(phase::closed()));
        owner.complete();
        return true;
    }
    let Ok(mut state) = snapshot.write() else {
        return false;
    };
    let idle = ActiveJob {
        channel: request.channel.clone(),
        command: request.data["channel"].as_str().unwrap_or("").into(),
        listener_id: request.listener_id.clone(),
        phase: "nativePhaseIdle",
    };
    state.active_job = Some(idle.clone());
    drop(state);
    let Ok(exclusive) = files.write() else {
        return false;
    };
    let validation = runtime.validate_context(&request);
    drop(exclusive);
    let valid = validation.is_ok();
    let _ = reply.send(validation);
    if valid {
        while let Some(mut work) = owner.receiver.blocking_recv() {
            let Ok(mut state) = snapshot.write() else {
                return false;
            };
            state.active_job = Some(ActiveJob {
                channel: work.request.channel.clone(),
                command: work.request.data["channel"].as_str().unwrap_or("").into(),
                listener_id: work.request.listener_id.clone(),
                phase: "nativePhase",
            });
            drop(state);
            let Ok(exclusive) = files.write() else {
                return false;
            };
            let operation = work
                .operation
                .take()
                .expect("phase operation consumed once");
            let mut result = runtime
                .validate_context(&work.request)
                .and_then(|()| operation(runtime));
            let context = runtime.context();
            let settings = runtime
                .stores
                .get_key("config", "mediaFolderPath")
                .map_err(|error| error.to_string());
            let Ok(mut state) = snapshot.write() else {
                return false;
            };
            state.context = context.clone();
            state.reader = runtime.show_reader();
            state.media_settings = settings;
            state.active_job = Some(idle.clone());
            drop(state);
            // Exactly like an ordinary mutation: publish the dependency fence
            // after actual IO/snapshot refresh, before dropping the FS barrier.
            completed.send_replace(work.sequence);
            drop(exclusive);
            if let Ok(events) = &mut result {
                for event in events.iter_mut() {
                    event.context = Some(context.clone());
                }
                if let Err(error) = phase::validate_events(events) {
                    result = Err(error);
                }
            }
            work.finish(result);
        }
    }
    let Ok(mut state) = snapshot.write() else {
        return false;
    };
    state.active_job = None;
    drop(state);
    // A completed-but-uncollected reply retains its quota, never admission.
    // The completion signal certifies accepted IO and this guard's release.
    owner.complete();
    true
}

fn show_reader_loop(
    queue: Arc<Mutex<mpsc::Receiver<ReadJob>>>,
    snapshot: Arc<RwLock<Snapshot>>,
    files: Arc<RwLock<()>>,
) {
    loop {
        // Serialize dequeue only; each worker releases the queue before IO.
        let job = match queue.lock() {
            Ok(mut queue) => queue.blocking_recv(),
            Err(_) => break,
        };
        let Some(job) = job else { break };
        match job {
            ReadJob::Dispatch { request, reply } => {
                if reply.is_closed() {
                    continue;
                }
                let result = (|| {
                    let _lease = files
                        .read()
                        .map_err(|_| unavailable("file barrier poisoned"))?;
                    let reader = {
                        let mut state = snapshot
                            .write()
                            .map_err(|_| unavailable("snapshot lock poisoned"))?;
                        if !state.worker_running {
                            return Err(unavailable("worker stopped"));
                        }
                        state.active_reads += 1;
                        state.reader.clone()
                    };
                    let _active = ActiveRead(snapshot.clone());
                    reader.dispatch(request)
                })();
                let _ = reply.send(result);
            }
            #[cfg(test)]
            ReadJob::Pause { entered, release } => {
                let _ = entered.send(());
                let _ = release.recv();
            }
        }
    }
}

struct ActiveRead(Arc<RwLock<Snapshot>>);

impl Drop for ActiveRead {
    fn drop(&mut self) {
        let mut state = self.0.write().unwrap_or_else(|error| error.into_inner());
        state.active_reads -= 1;
    }
}

fn unavailable(message: &str) -> Error {
    Error::WorkerUnavailable(message.into())
}

struct WorkerLifecycle(Arc<RwLock<Snapshot>>);

impl Drop for WorkerLifecycle {
    fn drop(&mut self) {
        // Runs on normal shutdown and unwind. Poisoned readers already fail
        // closed; this recovery is only for clearing the diagnostic state.
        let mut state = self.0.write().unwrap_or_else(|error| error.into_inner());
        state.worker_running = false;
        state.active_job = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::{fs, time::Duration};
    use tokio::time::timeout;

    const DEADLINE: Duration = Duration::from_secs(3);

    fn request(command: &str, data: Value, context: &RuntimeContext) -> Request {
        Request {
            channel: "MAIN".into(),
            data: json!({"channel":command,"data":data}),
            listener_id: Some("test".into()),
            context: Some(context.clone()),
        }
    }

    fn service() -> (tempfile::TempDir, LibraryService) {
        service_with_capacity(1)
    }

    fn service_with_capacity(capacity: usize) -> (tempfile::TempDir, LibraryService) {
        let temp = tempfile::tempdir().unwrap();
        let runtime = Runtime::new(temp.path().join("app"), temp.path().join("A")).unwrap();
        let service = LibraryService::with_capacity(runtime, capacity).unwrap();
        (temp, service)
    }

    async fn pause(service: &LibraryService) -> std::sync::mpsc::Sender<()> {
        let (entered, received) = oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        service
            .jobs
            .send(Job::Pause {
                entered,
                release: wait,
            })
            .await
            .unwrap();
        timeout(DEADLINE, received).await.unwrap().unwrap();
        release
    }

    #[tokio::test]
    async fn media_settings_remain_available_during_io_and_publish_only_committed_changes() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime = Runtime::new(temp.path().join("app"), temp.path().join("A")).unwrap();
        runtime
            .stores
            .set_key("config", "mediaFolderPath", json!("original 敬拜"))
            .unwrap();
        let service = LibraryService::with_capacity(runtime, 1).unwrap();
        let context = service.context().unwrap();
        let release = pause(&service).await;
        let changed = service
            .enqueue(
                request(
                    "SET_STORE_VALUE",
                    json!({"file":"config","key":"mediaFolderPath","value":"new café"}),
                    &context,
                ),
                "main",
            )
            .await
            .unwrap();
        let probe = service.clone();
        let current = context.clone();
        let (sent, received) = oneshot::channel();
        std::thread::spawn(move || {
            let _ = sent.send(probe.media_settings(&request(
                "LOCATE_MEDIA_FILE",
                Value::Null,
                &current,
            )));
        });
        let snapshot = timeout(DEADLINE, received).await.unwrap().unwrap().unwrap();
        assert_eq!(snapshot.context, context);
        assert_eq!(snapshot.configured_media_folder, "original 敬拜");
        release.send(()).unwrap();
        timeout(DEADLINE, changed).await.unwrap().unwrap().unwrap();
        assert_eq!(
            service
                .media_settings(&request("GET_MEDIA_FOLDER_PATH", Value::Null, &context))
                .unwrap()
                .configured_media_folder,
            "new café"
        );
        // A snapshot query must not read or create even the fallback directory.
        assert!(!temp.path().join("A/Media").exists());
        service.finish_writes().await.unwrap();
        assert!(matches!(
            service.media_settings(&request("LOCATE_MEDIA_FILE", Value::Null, &context)),
            Err(Error::WorkerUnavailable(_))
        ));
    }

    #[tokio::test]
    async fn media_settings_fail_closed_after_a_read_error_and_recover_after_valid_config() {
        let (temp, service) = service();
        let context = service.context().unwrap();
        let config = temp.path().join("app/config.json");
        fs::write(&config, b"invalid JSON").unwrap();
        assert!(service
            .dispatch(
                request(
                    "SET_STORE_VALUE",
                    json!({"file":"config","key":"mediaFolderPath","value":"uncommitted"}),
                    &context,
                ),
                "main",
            )
            .await
            .is_err());
        assert!(matches!(
            service.media_settings(&request("LOCATE_MEDIA_FILE", Value::Null, &context)),
            Err(Error::Invalid(_))
        ));
        assert_eq!(service.context().unwrap(), context);
        service
            .dispatch(request("PROJECTS", Value::Null, &context), "main")
            .await
            .unwrap();
        fs::write(&config, b"{}").unwrap();
        service
            .dispatch(
                request(
                    "SET_STORE_VALUE",
                    json!({"file":"config","key":"mediaFolderPath","value":false}),
                    &context,
                ),
                "main",
            )
            .await
            .unwrap();
        assert_eq!(
            service
                .media_settings(&request("LOCATE_MEDIA_FILE", Value::Null, &context))
                .unwrap()
                .configured_media_folder,
            false
        );
    }

    #[tokio::test]
    async fn native_commits_fence_reads_survive_dropped_callers_and_join_terminal_drain() {
        let (temp, service) = service();
        let context = service.context().unwrap();
        let release = pause(&service).await;
        let called = Arc::new(AtomicBool::new(false));
        let applied = called.clone();
        let mut commit = Box::pin(service.commit(
            request("LOCATE_MEDIA_FILE", Value::Null, &context),
            "main",
            move |runtime| {
                applied.store(true, Ordering::Release);
                runtime
                    .stores
                    .set_key("CACHE_SYNC", "sentinel", json!(true))?;
                runtime
                    .stores
                    .set_key("config", "mediaFolderPath", json!("committed"))?;
                runtime.save(
                    &json!({"showsCache":{"song":{"name":"Live","text":"after native commit"}}}),
                )?;
                Ok(vec![Event::reply(
                    "MAIN",
                    "LOCATE_MEDIA_FILE",
                    Value::Null,
                    None,
                )])
            },
        ));
        assert!(timeout(Duration::from_millis(50), &mut commit)
            .await
            .is_err());
        assert_eq!(service.status().unwrap().queued_jobs, 1);
        let mut read = Box::pin(service.dispatch(
            request("SHOW", json!({"id":"song","name":"Live"}), &context),
            "main",
        ));
        assert!(timeout(Duration::from_millis(50), &mut read).await.is_err());
        assert!(!called.load(Ordering::Acquire));
        drop(commit);
        release.send(()).unwrap();
        let events = timeout(DEADLINE, read).await.unwrap().unwrap();
        assert_eq!(events[0].context.as_ref(), Some(&context));
        assert_eq!(
            events[0].message.data.as_ref().unwrap()["content"][1]["text"],
            "after native commit"
        );
        assert!(called.load(Ordering::Acquire));
        assert_eq!(
            service
                .media_settings(&request("LOCATE_MEDIA_FILE", Value::Null, &context))
                .unwrap()
                .configured_media_folder,
            "committed"
        );
        let events = service
            .commit(
                request("IMPORT_FILES", Value::Null, &context),
                "main",
                |_| {
                    Ok(vec![Event::reply(
                        "MAIN",
                        "IMPORT2",
                        json!([]),
                        Some("native".into()),
                    )])
                },
            )
            .await
            .unwrap();
        assert_eq!(events[0].context.as_ref(), Some(&context));
        assert_eq!(events[0].listener_id.as_deref(), Some("native"));
        service.finish_writes().await.unwrap();
        let saved: Value =
            serde_json::from_slice(&fs::read(temp.path().join("app/cache_sync.json")).unwrap())
                .unwrap();
        assert_eq!(saved["sentinel"], true);
        assert!(matches!(
            service
                .commit(
                    request("IMPORT_FILES", Value::Null, &context),
                    "main",
                    |_| panic!("closed commit must not run")
                )
                .await,
            Err(Error::WorkerUnavailable(_))
        ));
    }

    #[tokio::test]
    async fn shutdown_includes_earlier_waiting_admissions_and_rejects_later_work() {
        let (temp, service) = service();
        let context = service.context().unwrap();
        let release = pause(&service).await;
        let first = service
            .enqueue(
                request(
                    "SET_STORE_VALUE",
                    json!({"file":"PROJECTS","key":"projects.first","value":true}),
                    &context,
                ),
                "main",
            )
            .await
            .unwrap();
        drop(first);
        let waiting = service.enqueue(
            request(
                "SET_STORE_VALUE",
                json!({"file":"PROJECTS","key":"projects.second","value":true}),
                &context,
            ),
            "main",
        );
        tokio::pin!(waiting);
        assert!(timeout(Duration::from_millis(50), &mut waiting)
            .await
            .is_err());
        let closing = service.finish_writes();
        tokio::pin!(closing);
        assert!(timeout(Duration::from_millis(50), &mut closing)
            .await
            .is_err());
        let later = service.enqueue(
            request(
                "SET_STORE_VALUE",
                json!({"file":"PROJECTS","key":"projects.rejected","value":true}),
                &context,
            ),
            "main",
        );
        tokio::pin!(later);
        assert!(timeout(Duration::from_millis(50), &mut later)
            .await
            .is_err());
        // Waiting admission must not hold the context/status lock.
        assert_eq!(service.context().unwrap(), context);
        assert_eq!(service.status().unwrap().queue_capacity, 1);
        release.send(()).unwrap();
        let (second, drained, rejected) =
            timeout(DEADLINE, async { tokio::join!(waiting, closing, later) })
                .await
                .unwrap();
        drained.unwrap();
        timeout(DEADLINE, second.unwrap())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(rejected, Err(Error::WorkerUnavailable(_))));
        let saved: Value =
            serde_json::from_slice(&fs::read(temp.path().join("A/Config/projects.json")).unwrap())
                .unwrap();
        assert_eq!(saved["projects"]["first"], true);
        assert_eq!(saved["projects"]["second"], true);
        assert!(saved["projects"].get("rejected").is_none());
        assert!(!service.status().unwrap().accepting_requests);
        assert!(matches!(
            service
                .dispatch(request("SHOW", json!({"id":"missing"}), &context), "main")
                .await,
            Err(Error::WorkerUnavailable(_))
        ));
        timeout(DEADLINE, service.finish_writes())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cancelling_shutdown_wait_does_not_cancel_an_accepted_write_or_reopen_admission() {
        let (temp, service) = service();
        let context = service.context().unwrap();
        let release = pause(&service).await;
        let accepted = service
            .enqueue(
                request(
                    "SET_STORE_VALUE",
                    json!({"file":"PROJECTS","key":"projects.kept","value":true}),
                    &context,
                ),
                "main",
            )
            .await
            .unwrap();
        assert!(timeout(Duration::from_millis(50), service.finish_writes())
            .await
            .is_err());
        assert!(!service.status().unwrap().accepting_requests);
        release.send(()).unwrap();
        timeout(DEADLINE, accepted).await.unwrap().unwrap().unwrap();
        timeout(DEADLINE, service.finish_writes())
            .await
            .unwrap()
            .unwrap();
        let saved: Value =
            serde_json::from_slice(&fs::read(temp.path().join("A/Config/projects.json")).unwrap())
                .unwrap();
        assert_eq!(saved["projects"]["kept"], true);
    }

    #[tokio::test]
    async fn shutdown_reports_a_stopped_worker_instead_of_claiming_a_drain() {
        let (_temp, service) = service();
        service.jobs.send(Job::Stop).await.unwrap();
        timeout(DEADLINE, service.jobs.closed()).await.unwrap();
        assert!(matches!(
            timeout(DEADLINE, service.finish_writes()).await.unwrap(),
            Err(Error::WorkerUnavailable(_))
        ));
    }

    #[tokio::test]
    async fn backup_compression_allows_show_reads_but_preserves_dependent_write_order() {
        let (_temp, service) = service_with_capacity(2);
        let context = service.context().unwrap();
        let (entered, received) = oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        service
            .jobs
            .send(Job::PauseBackup {
                entered,
                release: wait,
            })
            .await
            .unwrap();
        let mut backup = service.enqueue(request("SAVE", json!({"showsCache":{"song":{"name":"Live","text":"snapshot"}},"customTriggers":{"backup":true}}), &context), "main").await.unwrap();
        timeout(DEADLINE, received).await.unwrap().unwrap();
        assert_eq!(
            service.status().unwrap().active_job.unwrap().phase,
            "backupCompression"
        );
        let media_settings = service
            .media_settings(&request("LOCATE_MEDIA_FILE", Value::Null, &context))
            .unwrap();
        assert_eq!(media_settings.context, context);
        assert_eq!(media_settings.configured_media_folder, Value::Null);
        let read = timeout(
            DEADLINE,
            service.dispatch(
                request("SHOW", json!({"id":"song","name":"Live"}), &context),
                "main",
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(read[0].context.as_ref(), Some(&context));
        assert_eq!(
            read[0].message.data.as_ref().unwrap()["content"][1]["text"],
            "snapshot"
        );
        assert!(timeout(Duration::from_millis(50), &mut backup)
            .await
            .is_err());
        let unrelated = service
            .enqueue(request("VERSION", Value::Null, &context), "main")
            .await
            .unwrap();
        timeout(
            DEADLINE,
            service.dispatch(
                request("SHOW", json!({"id":"song","name":"Live"}), &context),
                "main",
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let changed = service
            .enqueue(
                request(
                    "SAVE",
                    json!({"showsCache":{"song":{"name":"Live","text":"later write"}}}),
                    &context,
                ),
                "main",
            )
            .await
            .unwrap();
        let dependent = service.dispatch(
            request("SHOW", json!({"id":"song","name":"Live"}), &context),
            "main",
        );
        tokio::pin!(dependent);
        assert!(timeout(Duration::from_millis(50), &mut dependent)
            .await
            .is_err());
        release.send(()).unwrap();
        let events = timeout(DEADLINE, backup).await.unwrap().unwrap().unwrap();
        timeout(DEADLINE, unrelated)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        timeout(DEADLINE, changed).await.unwrap().unwrap().unwrap();
        let read = timeout(DEADLINE, dependent).await.unwrap().unwrap();
        assert_eq!(
            read[0].message.data.as_ref().unwrap()["content"][1]["text"],
            "later write"
        );
        let path = events
            .iter()
            .find(|event| event.message.channel == "BACKUP")
            .unwrap()
            .message
            .data
            .as_ref()
            .unwrap()["path"]
            .as_str()
            .unwrap();
        let original = crate::archive::read_zip(std::path::Path::new(path))
            .unwrap()
            .into_iter()
            .find(|entry| entry.name == "SHOWS/Live.show")
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&original.content).unwrap()[1]["text"],
            "snapshot"
        );
    }

    #[tokio::test]
    async fn show_pool_has_bounded_admission_and_does_not_block_ordered_work() {
        let (_temp, service) = service();
        let context = service.context().unwrap();
        let mut releases = Vec::new();
        for _ in 0..READ_WORKERS {
            let (entered, received) = oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            service
                .reads
                .send(ReadJob::Pause {
                    entered,
                    release: wait,
                })
                .await
                .unwrap();
            timeout(DEADLINE, received).await.unwrap().unwrap();
            releases.push(release);
        }
        let queued = service
            .enqueue_read(request("SHOW", json!({"id":"missing"}), &context))
            .await
            .unwrap();
        assert!(timeout(
            Duration::from_millis(50),
            service.enqueue_read(request("SHOW", json!({"id":"cancelled"}), &context))
        )
        .await
        .is_err());
        let status = service.status().unwrap();
        assert_eq!(status.queued_reads, 1);
        assert_eq!(status.read_queue_capacity, 1);
        assert_eq!(status.read_workers, READ_WORKERS);
        timeout(
            DEADLINE,
            service.dispatch(request("PROJECTS", Value::Null, &context), "main"),
        )
        .await
        .unwrap()
        .unwrap();
        for release in releases {
            release.send(()).unwrap();
        }
        let read = timeout(DEADLINE, queued).await.unwrap().unwrap().unwrap();
        assert_eq!(read[0].message.data.as_ref().unwrap()["error"], "not_found");
        // An invalid read must not stop either the pool or subsequent writes.
        assert!(matches!(
            service
                .dispatch(
                    request("SHOW", json!({"name":"../escape"}), &context),
                    "main"
                )
                .await,
            Err(Error::Invalid(_))
        ));
        timeout(
            DEADLINE,
            service.dispatch(
                request("SHOW", json!({"id":"after-error"}), &context),
                "main",
            ),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    async fn show_reads_wait_for_ordered_library_changes_and_reject_old_contexts() {
        let (temp, service) = service();
        let context = service.context().unwrap();
        let destination = temp.path().join("B");
        fs::create_dir(&destination).unwrap();
        fs::create_dir(destination.join("Shows")).unwrap();
        fs::write(
            destination.join("Shows/Live.show"),
            b"[\"song\",{\"text\":\"new library\"}]",
        )
        .unwrap();
        let release = pause(&service).await;
        let changed = service
            .enqueue(
                request(
                    "UPDATE_DATA_PATH",
                    json!({"oldPath":context.data_path,"newPath":destination}),
                    &context,
                ),
                "main",
            )
            .await
            .unwrap();
        let old_read = service.dispatch(
            request("SHOW", json!({"id":"song","name":"Live"}), &context),
            "main",
        );
        tokio::pin!(old_read);
        assert!(timeout(Duration::from_millis(50), &mut old_read)
            .await
            .is_err());
        release.send(()).unwrap();
        timeout(DEADLINE, changed).await.unwrap().unwrap().unwrap();
        assert!(matches!(
            timeout(DEADLINE, old_read).await.unwrap(),
            Err(Error::StaleLibrary { .. })
        ));
        let new = service.context().unwrap();
        let read = timeout(
            DEADLINE,
            service.dispatch(
                request("SHOW", json!({"id":"song","name":"Live"}), &new),
                "main",
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(read[0].context.as_ref(), Some(&new));
        assert_eq!(
            read[0].message.data.as_ref().unwrap()["content"][1]["text"],
            "new library"
        );
    }

    #[tokio::test]
    async fn show_reads_do_not_observe_an_in_progress_ordered_phase() {
        let (temp, service) = service();
        let context = service.context().unwrap();
        let release = pause(&service).await;
        let read = service.dispatch(
            request("SHOW", json!({"id":"song","name":"Live"}), &context),
            "main",
        );
        tokio::pin!(read);
        assert!(timeout(Duration::from_millis(50), &mut read).await.is_err());
        fs::write(
            temp.path().join("A/Shows/Live.show"),
            b"[\"song\",{\"text\":\"complete\"}]",
        )
        .unwrap();
        release.send(()).unwrap();
        let read = timeout(DEADLINE, read).await.unwrap().unwrap();
        assert_eq!(
            read[0].message.data.as_ref().unwrap()["content"][1]["text"],
            "complete"
        );
    }

    #[tokio::test]
    async fn blocked_io_keeps_context_available_and_applies_backpressure() {
        let (_temp, service) = service();
        let context = service.context().unwrap();
        let release = pause(&service).await;
        let first = service
            .enqueue(request("PROJECTS", Value::Null, &context), "main")
            .await
            .unwrap();
        // Probe from another thread, so a lock regression fails with a timeout
        // instead of deadlocking this single-thread async test executor.
        let probe = service.clone();
        let (sent, received) = oneshot::channel();
        std::thread::spawn(move || {
            let _ = sent.send((probe.context(), probe.status()));
        });
        let (observed, status) = timeout(DEADLINE, received).await.unwrap().unwrap();
        assert_eq!(observed.unwrap(), context);
        let status = status.unwrap();
        assert_eq!(status.queued_jobs, 1);
        assert_eq!(status.queue_capacity, 1);
        assert!(timeout(
            Duration::from_millis(50),
            service.enqueue(
                request(
                    "SET_STORE_VALUE",
                    json!({"file":"PROJECTS","key":"projects.cancelled","value":true}),
                    &context
                ),
                "main"
            )
        )
        .await
        .is_err());
        release.send(()).unwrap();
        assert!(timeout(DEADLINE, first).await.unwrap().unwrap().is_ok());
        let events = timeout(
            DEADLINE,
            service.dispatch(request("PROJECTS", Value::Null, &context), "main"),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(events[0].message.data.as_ref().unwrap()["projects"]
            .get("cancelled")
            .is_none());
    }

    #[tokio::test]
    async fn admitted_writes_survive_dropped_callers_and_failures_do_not_stop_the_worker() {
        let (_temp, service) = service();
        let context = service.context().unwrap();
        let release = pause(&service).await;
        let ignored = service
            .enqueue(
                request(
                    "SET_STORE_VALUE",
                    json!({"file":"PROJECTS","key":"projects.kept","value":{"name":"Kept"}}),
                    &context,
                ),
                "main",
            )
            .await
            .unwrap();
        drop(ignored);
        release.send(()).unwrap();
        let failed = timeout(
            DEADLINE,
            service.dispatch(request("NOT_IMPLEMENTED", Value::Null, &context), "main"),
        )
        .await
        .unwrap();
        assert!(matches!(failed, Err(Error::Unsupported(_))));
        let events = timeout(
            DEADLINE,
            service.dispatch(request("PROJECTS", Value::Null, &context), "main"),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            events[0].message.data.as_ref().unwrap()["projects"]["kept"]["name"],
            "Kept"
        );
        assert_eq!(events[0].context.as_ref(), Some(&context));
    }

    #[tokio::test]
    async fn ordered_library_change_rejects_queued_old_saves_and_publishes_new_context() {
        let (temp, service) = service();
        let context = service.context().unwrap();
        let destination = temp.path().join("B");
        fs::create_dir(&destination).unwrap();
        let release = pause(&service).await;
        let changed = service
            .enqueue(
                request(
                    "UPDATE_DATA_PATH",
                    json!({"oldPath":context.data_path,"newPath":destination}),
                    &context,
                ),
                "main",
            )
            .await
            .unwrap();
        release.send(()).unwrap();
        let old_save = service
            .enqueue(
                request("SAVE", json!({"PROJECTS":{"wrong":true}}), &context),
                "main",
            )
            .await
            .unwrap();
        let stale_called = Arc::new(AtomicBool::new(false));
        let attempted = stale_called.clone();
        let stale_commit = service
            .enqueue_operation(
                request("LOCATE_MEDIA_FILE", Value::Null, &context),
                "main",
                Some(Box::new(move |_| {
                    attempted.store(true, Ordering::Release);
                    panic!("stale native effects must not run");
                })),
            )
            .await
            .unwrap();
        let events = timeout(DEADLINE, changed).await.unwrap().unwrap().unwrap();
        let new = service.context().unwrap();
        assert_eq!(new.data_path, fs::canonicalize(destination).unwrap());
        assert_eq!(new.library_generation, context.library_generation + 1);
        let settings = service
            .media_settings(&request("LOCATE_MEDIA_FILE", Value::Null, &new))
            .unwrap();
        assert_eq!(settings.context, new);
        assert_eq!(settings.configured_media_folder, Value::Null);
        assert!(matches!(
            service.media_settings(&request("LOCATE_MEDIA_FILE", Value::Null, &context)),
            Err(Error::StaleLibrary { .. })
        ));
        assert!(events
            .iter()
            .all(|event| event.context.as_ref() == Some(&new)));
        let error = timeout(DEADLINE, old_save)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(matches!(
            error,
            Error::StaleLibrary {
                recovery_path: Some(_),
                ..
            }
        ));
        assert!(matches!(
            timeout(DEADLINE, stale_commit).await.unwrap().unwrap(),
            Err(Error::StaleLibrary { .. })
        ));
        assert!(!stale_called.load(Ordering::Acquire));
        let events = service
            .dispatch(request("PROJECTS", Value::Null, &new), "main")
            .await
            .unwrap();
        assert!(events[0]
            .message
            .data
            .as_ref()
            .unwrap()
            .get("wrong")
            .is_none());
        assert!(service
            .contextualize(&request("OPEN_FOLDER", Value::Null, &context), &mut [])
            .is_err());
    }

    #[tokio::test]
    async fn stopped_worker_rejects_pending_and_new_requests() {
        let (_temp, service) = service();
        let context = service.context().unwrap();
        let release = pause(&service).await;
        service.jobs.send(Job::Stop).await.unwrap();
        let pending = service.dispatch(request("PROJECTS", Value::Null, &context), "main");
        release.send(()).unwrap();
        let result = timeout(DEADLINE, pending).await.unwrap();
        assert!(matches!(result, Err(Error::WorkerUnavailable(_))));
        assert!(service.context().is_err());
        assert!(!service.status().unwrap().worker_running);
        assert!(matches!(
            timeout(
                DEADLINE,
                service.dispatch(request("SHOW", json!({"id":"missing"}), &context), "main")
            )
            .await
            .unwrap(),
            Err(Error::WorkerUnavailable(_))
        ));
    }

    #[tokio::test]
    async fn a_permit_published_after_worker_shutdown_cannot_leave_a_reply_waiting_forever() {
        let (_temp, service) = service_with_capacity(2);
        let context = service.context().unwrap();
        let release = pause(&service).await;
        let permit = service.jobs.reserve().await.unwrap();
        service.jobs.send(Job::Stop).await.unwrap();
        release.send(()).unwrap();
        timeout(DEADLINE, service.jobs.closed()).await.unwrap();
        let (reply, received) = oneshot::channel();
        permit.send(Job::Dispatch {
            sequence: None,
            request: Box::new(request("PROJECTS", Value::Null, &context)),
            commit: None,
            window: "main".into(),
            reply,
        });
        assert!(matches!(
            timeout(DEADLINE, service.receive_reply(received, false))
                .await
                .unwrap(),
            Err(Error::WorkerUnavailable(_))
        ));
    }

    #[tokio::test]
    async fn reserved_phase_after_receiver_exit_cannot_retain_admission_or_hide_failure() {
        let (_temp, service) = service_with_capacity(2);
        let context = service.context().unwrap();
        let source = request("NOW_PLAYING", Value::Null, &context);
        let (phase, owner) = service
            .phases
            .create(&source, "main", service.admitted_sequence.clone())
            .unwrap();
        phase
            .attach_admission(service.admission.clone().lock_owned().await)
            .unwrap();
        let permit = service.jobs.reserve().await.unwrap();
        service.jobs.send(Job::Stop).await.unwrap();
        timeout(DEADLINE, service.jobs.closed()).await.unwrap();
        let (reply, received) = oneshot::channel();
        permit.send(Job::Phase(Box::new(PhaseJob {
            request: source,
            owner,
            reply,
        })));
        assert!(matches!(
            timeout(DEADLINE, service.receive_phase_reply(received))
                .await
                .unwrap(),
            Err(Error::WorkerUnavailable(_))
        ));
        // The retained closed-queue marker never entered the actual IO owner.
        // Canceling begin/closing its capability must release admission now.
        drop(phase);
        drop(timeout(DEADLINE, service.admission.lock()).await.unwrap());
        assert!(matches!(
            timeout(
                DEADLINE,
                service.commit(request("AFTER", Value::Null, &context), "main", |_| panic!(
                    "worker stopped"
                ))
            )
            .await
            .unwrap(),
            Err(Error::WorkerUnavailable(_))
        ));
        assert!(matches!(
            timeout(
                DEADLINE,
                service.begin_phase(request("NOW_PLAYING", Value::Null, &context), "main")
            )
            .await
            .unwrap(),
            Err(Error::WorkerUnavailable(_))
        ));
    }

    #[tokio::test]
    async fn closed_late_phase_marker_cannot_regress_or_strand_real_mutation_fences() {
        let (temp, service) = service_with_capacity(2);
        let context = service.context().unwrap();
        let mut old = context.clone();
        old.library_generation += 1;
        // A stale embedded SAVE would write a recovery artifact if a closed
        // late marker incorrectly ran validation or any source effect.
        let source = request("SAVE", json!({"runtimeContext":old}), &context);
        let (phase, owner) = service
            .phases
            .create(&source, "main", service.admitted_sequence.clone())
            .unwrap();
        phase
            .attach_admission(service.admission.clone().lock_owned().await)
            .unwrap();
        let reserved = service.jobs.reserve().await.unwrap();
        phase.close();
        let (entered, received) = oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let mut mutation = Box::pin(service.commit(
            request("REAL_MUTATION", Value::Null, &context),
            "main",
            move |runtime| {
                let _ = entered.send(());
                wait.recv().unwrap();
                runtime.save(&json!({"showsCache":{"song":{"name":"Live","text":"real"}}}))?;
                Ok(vec![])
            },
        ));
        tokio::select! {
            result = &mut mutation => panic!("mutation escaped gate: {result:?}"),
            result = received => result.unwrap(),
        }
        assert_eq!(service.admitted_sequence.load(Ordering::Acquire), 1);
        assert_eq!(*service.progress.borrow(), 0);
        // Publish the reserved closed marker after the real mutation's send.
        let (reply, received) = oneshot::channel();
        reserved.send(Job::Phase(Box::new(PhaseJob {
            request: source,
            owner,
            reply,
        })));
        let mut show = Box::pin(service.dispatch(
            request("SHOW", json!({"id":"song","name":"Live"}), &context),
            "main",
        ));
        assert!(timeout(Duration::from_millis(30), &mut show).await.is_err());
        release.send(()).unwrap();
        timeout(DEADLINE, mutation).await.unwrap().unwrap();
        assert!(matches!(
            timeout(DEADLINE, service.receive_phase_reply(received))
                .await
                .unwrap(),
            Err(Error::NativeOperation {
                code: "library_phase_closed",
                ..
            })
        ));
        let events = timeout(DEADLINE, show).await.unwrap().unwrap();
        assert_eq!(
            events[0].message.data.as_ref().unwrap()["content"][1]["text"],
            "real"
        );
        assert_eq!(*service.progress.borrow(), 1);
        assert!(!temp.path().join("app/Recovery").exists());
        phase.finish().await.unwrap();
        service
            .commit(
                request("SECOND_MUTATION", Value::Null, &context),
                "main",
                |runtime| {
                    runtime
                        .save(&json!({"showsCache":{"song":{"name":"Live","text":"second"}}}))?;
                    Ok(vec![])
                },
            )
            .await
            .unwrap();
        let events = timeout(
            DEADLINE,
            service.dispatch(
                request("SHOW", json!({"id":"song","name":"Live"}), &context),
                "main",
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            events[0].message.data.as_ref().unwrap()["content"][1]["text"],
            "second"
        );
        assert_eq!(*service.progress.borrow(), 2);
    }

    #[tokio::test]
    async fn dropping_service_handles_drains_writes_and_stops_worker() {
        let (temp, service) = service();
        let context = service.context().unwrap();
        let snapshot = service.snapshot.clone();
        let received = service
            .enqueue(
                request(
                    "SET_STORE_VALUE",
                    json!({"file":"PROJECTS","key":"projects.drained","value":true}),
                    &context,
                ),
                "main",
            )
            .await
            .unwrap();
        drop(service);
        timeout(DEADLINE, received).await.unwrap().unwrap().unwrap();
        timeout(DEADLINE, async {
            while snapshot.read().unwrap().worker_running {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let saved: Value =
            serde_json::from_slice(&fs::read(temp.path().join("A/Config/projects.json")).unwrap())
                .unwrap();
        assert_eq!(saved["projects"]["drained"], true);
    }
}
