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
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{mpsc, oneshot, watch};

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
}

struct Snapshot {
    context: RuntimeContext,
    active_job: Option<ActiveJob>,
    worker_running: bool,
    reader: ShowReader,
    active_reads: usize,
}

type Reply = oneshot::Receiver<Result<Vec<Event>>>;

enum Job {
    Dispatch {
        sequence: Option<u64>,
        request: Request,
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
    admission: Arc<Mutex<u64>>,
    progress: watch::Receiver<u64>,
    snapshot: Arc<RwLock<Snapshot>>,
}

impl LibraryService {
    pub fn start(runtime: Runtime) -> Result<Self> {
        Self::with_capacity(runtime, QUEUE_CAPACITY)
    }

    fn with_capacity(mut runtime: Runtime, capacity: usize) -> Result<Self> {
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
                        Job::Dispatch {
                            sequence,
                            request,
                            window,
                            reply,
                        } => {
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
                            let prepared = runtime.prepare_dispatch(request, &window);
                            let context = runtime.context();
                            if let Ok(mut state) = shared.write() {
                                state.context = context.clone();
                                state.reader = runtime.show_reader();
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
            admission: Arc::new(Mutex::new(0)),
            progress,
            snapshot,
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
        let (reply, received) = oneshot::channel();
        let permit = self
            .jobs
            .reserve()
            .await
            .map_err(|_| unavailable("request queue closed"))?;
        // Ticket assignment and send are synchronous under one short lock.
        // A cancelled admission never leaves a missing sequence number.
        let mut admitted = self
            .admission
            .lock()
            .map_err(|_| unavailable("admission lock poisoned"))?;
        // A read-only or unsupported command waiting behind compression must
        // not make later show reads wait for the ZIP. Only file mutations and
        // index scans (which repair IDs/write caches) establish a dependency.
        let mutation = request.channel == "MAIN"
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
            );
        let sequence = if mutation {
            Some(
                admitted
                    .checked_add(1)
                    .ok_or_else(|| unavailable("request sequence exhausted"))?,
            )
        } else {
            None
        };
        permit.send(Job::Dispatch {
            sequence,
            request,
            window: window.into(),
            reply,
        });
        if let Some(sequence) = sequence {
            *admitted = sequence;
        }
        Ok(received)
    }

    async fn enqueue_read(&self, request: Request) -> Result<Reply> {
        // Keep read-after-write admission order, but a backup's completed copy
        // phase is enough: readers do not wait for its ZIP compression.
        let fence = *self
            .admission
            .lock()
            .map_err(|_| unavailable("admission lock poisoned"))?;
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

    pub async fn dispatch(&self, request: Request, window: &str) -> Result<Vec<Event>> {
        let read = request.channel == "MAIN" && request.data["channel"] == "SHOW";
        let received = if read {
            self.enqueue_read(request).await?
        } else {
            self.enqueue(request, window).await?
        };
        self.receive_reply(received, read).await
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
        let events = timeout(DEADLINE, changed).await.unwrap().unwrap().unwrap();
        let new = service.context().unwrap();
        assert_eq!(new.data_path, fs::canonicalize(destination).unwrap());
        assert_eq!(new.library_generation, context.library_generation + 1);
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
            request: request("PROJECTS", Value::Null, &context),
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
