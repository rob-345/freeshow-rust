use crate::*;
use std::{
    sync::{mpsc, Arc, Mutex},
    thread::JoinHandle,
};
use tokio::sync::{oneshot, Semaphore};
#[derive(Clone)]
pub struct ThumbnailService {
    pub(crate) inner: Arc<Inner>,
}
pub(crate) struct Inner {
    config: ThumbnailConfig,
    cancel: CancellationToken,
    jobs: Arc<Semaphore>,
    leases: Arc<Semaphore>,
    sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    pub(crate) cleanup: Cleanup,
    shutdown: tokio::sync::Mutex<()>,
    #[cfg(test)]
    pub before: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    pub after: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
#[derive(Clone)]
pub(crate) struct Cleanup {
    sender: mpsc::SyncSender<Remove>,
    failed: Arc<std::sync::atomic::AtomicBool>,
    identity: Arc<()>,
}
enum Remove {
    Stage(TempDir, Option<OwnedSemaphorePermit>),
    Fence(oneshot::Sender<()>),
}
impl Cleanup {
    pub(crate) fn remove(&self, stage: TempDir, lease: Option<OwnedSemaphorePermit>) {
        if let Err(error) = self.sender.try_send(Remove::Stage(stage, lease)) {
            self.failed
                .store(true, std::sync::atomic::Ordering::Release);
            let (mpsc::TrySendError::Full(msg) | mpsc::TrySendError::Disconnected(msg)) = error;
            if let Remove::Stage(stage, _) = msg {
                let _ = stage.keep();
            }
        }
    }
    pub(crate) async fn fence(&self) -> Result<()> {
        let (send, receive) = oneshot::channel();
        self.sender
            .try_send(Remove::Fence(send))
            .map_err(|_| ThumbnailError::new("cleanup_failure", "Thumbnail cleanup unavailable"))?;
        receive
            .await
            .map_err(|_| ThumbnailError::new("cleanup_failure", "Thumbnail cleanup exited"))?;
        if self.failed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(ThumbnailError::new(
                "cleanup_failure",
                "Thumbnail cleanup failed",
            ));
        }
        Ok(())
    }
}
enum Task {
    Prepare(prepare::Input, PreparedThumbnail),
    Complete(PreparedThumbnail, prepare::Completion),
}
struct Work {
    task: Task,
    budget: Budget,
    permit: OwnedSemaphorePermit,
    response: oneshot::Sender<Result<PreparedThumbnail>>,
}
struct CancelOnDrop(Option<CancellationToken>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel.cancel();
        }
    }
}
impl ThumbnailService {
    pub fn new(config: ThumbnailConfig) -> Result<Self> {
        if !(1..=8).contains(&config.workers)
            || !(1..=16).contains(&config.max_prepared)
            || !(1..=32768).contains(&config.path_bytes)
            || !(1..=4096).contains(&config.max_size)
            || config.input_bytes == 0
            || config.input_bytes > 512 * 1024 * 1024
            || config.max_pixels == 0
            || config.max_pixels > 16_777_216
            || config.png_bytes == 0
            || config.png_bytes > 32 * 1024 * 1024
            || config.chunks == 0
            || config.chunks > 4096
            || config
                .staging_root
                .as_ref()
                .is_some_and(|root| !root.is_absolute() || root.as_os_str().len() > 32768)
            || config.timeout.is_zero()
            || config.timeout > Duration::from_secs(120)
        {
            return Err(ThumbnailError::new(
                "invalid_configuration",
                "Invalid thumbnail bounds",
            ));
        }
        let (send, recv) = mpsc::sync_channel(config.max_prepared + 1);
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reaper_failed = failed.clone();
        std::thread::Builder::new()
            .name("freeshow-thumbnail-cleanup".into())
            .spawn(move || {
                while let Ok(message) = recv.recv() {
                    match message {
                        Remove::Stage(stage, _lease) => {
                            if stage.close().is_err() {
                                reaper_failed.store(true, std::sync::atomic::Ordering::Release);
                            }
                        }
                        Remove::Fence(reply) => {
                            let _ = reply.send(());
                        }
                    }
                }
            })
            .map_err(ThumbnailError::io)?;
        let cleanup = Cleanup {
            sender: send,
            failed,
            identity: Arc::new(()),
        };
        let (sender, receiver) = mpsc::sync_channel::<Work>(config.workers);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = vec![];
        for index in 0..config.workers {
            let receiver = receiver.clone();
            workers.push(
                std::thread::Builder::new()
                    .name(format!("freeshow-thumbnail-{index}"))
                    .spawn(move || loop {
                        let work = receiver.lock().expect("thumbnail queue poisoned").recv();
                        let Ok(Work {
                            task,
                            budget,
                            permit,
                            response,
                        }) = work
                        else {
                            break;
                        };
                        let result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match task {
                                Task::Prepare(input, plan) => prepare::run(input, plan, &budget),
                                Task::Complete(plan, completion) => {
                                    prepare::complete(plan, completion, &budget)
                                }
                            }))
                            .unwrap_or_else(|_| {
                                Err(ThumbnailError::new(
                                    "thumbnail_worker_error",
                                    "Thumbnail worker panicked",
                                ))
                            });
                        drop(permit);
                        let _ = response.send(result);
                    })
                    .map_err(ThumbnailError::io)?,
            );
        }
        Ok(Self {
            inner: Arc::new(Inner {
                jobs: Arc::new(Semaphore::new(config.workers)),
                leases: Arc::new(Semaphore::new(config.max_prepared)),
                config,
                cancel: CancellationToken::new(),
                sender: Mutex::new(Some(sender)),
                workers: Mutex::new(workers),
                cleanup,
                shutdown: tokio::sync::Mutex::new(()),
                #[cfg(test)]
                before: Mutex::new(None),
                #[cfg(test)]
                after: Mutex::new(None),
            }),
        })
    }
    pub async fn prepare(
        &self,
        command: &str,
        payload: &Value,
        roots: &ThumbnailRoots,
        request_cancel: CancellationToken,
    ) -> Result<PreparedThumbnail> {
        if self.inner.cancel.is_cancelled() {
            return Err(self.busy());
        }
        if self
            .inner
            .cleanup
            .failed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ThumbnailError::new(
                "cleanup_failure",
                "Thumbnail cleanup unavailable",
            ));
        }
        let input = prepare::validate(command, payload, roots, &self.inner.config)?;
        let lease = self
            .inner
            .leases
            .clone()
            .try_acquire_owned()
            .map_err(|_| self.busy())?;
        let cancel = self.inner.cancel.child_token();
        let plan = PreparedThumbnail {
            reply: Value::Null,
            outcome: ThumbnailOutcome::Missing,
            capture: None,
            effects: vec![],
            diagnostics: vec![],
            cache_root: roots.cache_root.clone(),
            stage: None,
            lease: Some(lease),
            cleanup: self.inner.cleanup.clone(),
            cancel: cancel.clone(),
            request_cancel: request_cancel.clone(),
            created: Instant::now(),
            source: None,
            destination: None,
            guard: CacheGuard::Absent,
            failure_matches_request: true,
        };
        self.submit(Task::Prepare(input, plan), cancel, request_cancel)
            .await
    }
    pub async fn prepare_capture(
        &self,
        input: &str,
        size: u32,
        roots: &ThumbnailRoots,
        cancel: CancellationToken,
    ) -> Result<PreparedThumbnail> {
        self.prepare(
            "GET_THUMBNAIL",
            &serde_json::json!({"input":input,"size":size}),
            roots,
            cancel,
        )
        .await
    }
    /// Consume the actual owned live private plan; a bare public capture ID is
    /// insufficient authority. Host additionally validates frozen RuntimeContext.
    pub async fn complete(
        &self,
        plan: PreparedThumbnail,
        id: &str,
        completion: CaptureCompletion<'_>,
    ) -> Result<PreparedThumbnail> {
        plan.ensure_current()?;
        if !Arc::ptr_eq(&plan.cleanup.identity, &self.inner.cleanup.identity) {
            return Err(ThumbnailError::new(
                "invalid_capture_completion",
                "Capture belongs to a different service",
            ));
        }
        if id.len() > self.inner.config.path_bytes + 32 {
            return Err(ThumbnailError::resource("Capture ID limit"));
        }
        let completion = match completion {
            CaptureCompletion::Png(bytes) => {
                if bytes.len() > self.inner.config.png_bytes {
                    return Err(ThumbnailError::resource("Canvas completion byte limit"));
                }
                prepare::Completion::Png {
                    bytes: bytes.to_vec(),
                    id: id.into(),
                }
            }
            CaptureCompletion::Failed => prepare::Completion::Failed { id: id.into() },
            CaptureCompletion::TimedOut => prepare::Completion::TimedOut { id: id.into() },
        };
        let cancel = plan.cancel.clone();
        let request_cancel = plan.request_cancel.clone();
        self.submit(Task::Complete(plan, completion), cancel, request_cancel)
            .await
    }
    fn busy(&self) -> ThumbnailError {
        if self.inner.cancel.is_cancelled() {
            ThumbnailError::new("thumbnail_shutdown", "Thumbnail service shut down")
        } else {
            ThumbnailError::new(
                "thumbnail_busy",
                "Thumbnail jobs/prepared plans at capacity",
            )
        }
    }
    async fn submit(
        &self,
        task: Task,
        cancel: CancellationToken,
        request_cancel: CancellationToken,
    ) -> Result<PreparedThumbnail> {
        if self.inner.cancel.is_cancelled() {
            return Err(self.busy());
        }
        if request_cancel.is_cancelled() {
            return Err(ThumbnailError::new(
                "cancelled",
                "Thumbnail request cancelled",
            ));
        }
        #[cfg(test)]
        if let Some(hook) = self.inner.before.lock().unwrap().take() {
            hook();
        }
        let permit = self
            .inner
            .jobs
            .clone()
            .try_acquire_owned()
            .map_err(|_| self.busy())?;
        #[cfg(test)]
        if let Some(hook) = self.inner.after.lock().unwrap().take() {
            hook();
        }
        if self.inner.cancel.is_cancelled() {
            return Err(self.busy());
        }
        let mut guard = CancelOnDrop(Some(cancel.clone()));
        let (response, receive) = oneshot::channel();
        {
            let sender = self.inner.sender.lock().expect("thumbnail sender poisoned");
            if self.inner.cancel.is_cancelled() {
                return Err(self.busy());
            }
            sender
                .as_ref()
                .ok_or_else(|| self.busy())?
                .try_send(Work {
                    task,
                    budget: Budget {
                        config: self.inner.config.clone(),
                        cancel,
                        request_cancel: request_cancel.clone(),
                        started: Instant::now(),
                    },
                    permit,
                    response,
                })
                .map_err(|_| self.busy())?;
        }
        let result = tokio::select! {reply=receive=>reply.map_err(|_|ThumbnailError::new("thumbnail_worker_error","Thumbnail worker exited"))?,_=request_cancel.cancelled()=>Err(ThumbnailError::new("cancelled","Thumbnail request cancelled"))};
        if result.is_ok() {
            guard.0 = None;
        }
        result
    }
    pub async fn shutdown(&self) -> Result<()> {
        let _serial = self.inner.shutdown.lock().await;
        self.inner.cancel.cancel();
        self.inner.sender.lock().unwrap().take();
        let _all = self
            .inner
            .jobs
            .clone()
            .acquire_many_owned(self.inner.config.workers as u32)
            .await
            .map_err(|_| self.busy())?;
        self.inner.cleanup.fence().await?;
        loop {
            if self
                .inner
                .workers
                .lock()
                .unwrap()
                .iter()
                .all(JoinHandle::is_finished)
            {
                break;
            }
            if tokio::runtime::Handle::try_current().is_err() {
                return Err(ThumbnailError::new(
                    "runtime_required",
                    "Thumbnail drain needs Tokio runtime",
                ));
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        for worker in self.inner.workers.lock().unwrap().drain(..) {
            let _ = worker.join();
        }
        Ok(())
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.sender.get_mut().unwrap().take();
    }
}
