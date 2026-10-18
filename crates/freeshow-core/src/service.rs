//! Ordered library IO on a dedicated thread, with bounded asynchronous admission.
//!
//! No library filesystem operation holds the snapshot lock. Desktop controls and
//! future output/network services can inspect context without joining this queue.
use crate::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    Error, Result,
};
use serde::Serialize;
use std::sync::{Arc, RwLock};
use tokio::sync::{mpsc, oneshot};

const QUEUE_CAPACITY: usize = 8;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveJob {
    pub channel: String,
    pub command: String,
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
}

struct Snapshot {
    context: RuntimeContext,
    active_job: Option<ActiveJob>,
    worker_running: bool,
}

type Reply = oneshot::Receiver<Result<Vec<Event>>>;

enum Job {
    Dispatch {
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
    Stop,
}

#[derive(Clone)]
pub struct LibraryService {
    jobs: mpsc::Sender<Job>,
    snapshot: Arc<RwLock<Snapshot>>,
}

impl LibraryService {
    pub fn start(runtime: Runtime) -> Result<Self> {
        Self::with_capacity(runtime, QUEUE_CAPACITY)
    }

    fn with_capacity(mut runtime: Runtime, capacity: usize) -> Result<Self> {
        let (jobs, mut receiver) = mpsc::channel(capacity);
        let snapshot = Arc::new(RwLock::new(Snapshot {
            context: runtime.context(),
            active_job: None,
            worker_running: true,
        }));
        let shared = snapshot.clone();
        // The worker owns Runtime exclusively. It holds no sender, so dropping
        // all service handles drains admitted work and then stops this thread.
        std::thread::Builder::new()
            .name("freeshow-library".into())
            .spawn(move || {
                let _lifecycle = WorkerLifecycle(shared.clone());
                while let Some(job) = receiver.blocking_recv() {
                    match job {
                        Job::Dispatch {
                            request,
                            window,
                            reply,
                        } => {
                            if let Ok(mut state) = shared.write() {
                                state.active_job = Some(ActiveJob {
                                    channel: request.channel.clone(),
                                    listener_id: request.listener_id.clone(),
                                    command: request.data["channel"].as_str().unwrap_or("").into(),
                                });
                            } else {
                                break;
                            }
                            let mut result = runtime.dispatch(request, &window);
                            let context = runtime.context();
                            if let Ok(mut state) = shared.write() {
                                state.context = context.clone();
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
                            let _ = entered.send(());
                            let _ = release.recv();
                        }
                        #[cfg(test)]
                        Job::Stop => break,
                    }
                }
            })
            .map_err(|error| Error::WorkerUnavailable(error.to_string()))?;
        Ok(Self { jobs, snapshot })
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
        self.jobs
            .send(Job::Dispatch {
                request,
                window: window.into(),
                reply,
            })
            .await
            .map_err(|_| unavailable("request queue closed"))?;
        Ok(received)
    }

    pub async fn dispatch(&self, request: Request, window: &str) -> Result<Vec<Event>> {
        self.enqueue(request, window)
            .await?
            .await
            .map_err(|_| unavailable("worker stopped before replying"))?
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
        let temp = tempfile::tempdir().unwrap();
        let runtime = Runtime::new(temp.path().join("app"), temp.path().join("A")).unwrap();
        let service = LibraryService::with_capacity(runtime, 1).unwrap();
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
        assert_eq!(new.data_path, destination);
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
