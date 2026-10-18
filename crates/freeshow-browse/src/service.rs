use crate::*;
use std::{
    sync::{mpsc, Arc, Mutex},
    thread::JoinHandle,
};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
#[derive(Clone)]
pub struct BrowseService {
    pub(crate) inner: Arc<Inner>,
}
pub(crate) struct Inner {
    config: BrowseConfig,
    pub(crate) cancel: CancellationToken,
    pub(crate) permits: Arc<Semaphore>,
    pub(crate) sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    shutdown: tokio::sync::Mutex<()>,
    #[cfg(test)]
    pub before: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    pub after: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
pub(crate) struct Work {
    request: Request,
    cancel: CancellationToken,
    request_cancel: CancellationToken,
    started: Instant,
    permit: OwnedSemaphorePermit,
    response: oneshot::Sender<Result<BrowseReply>>,
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
impl BrowseService {
    pub fn new(config: BrowseConfig) -> Result<Self> {
        if !(1..=8).contains(&config.workers)
            || !(1..=256).contains(&config.roots)
            || !(1..=50000).contains(&config.entries)
            || !(1..=32).contains(&config.depth)
            || !(1..=32768).contains(&config.path_bytes)
            || !(1..=8192).contains(&config.name_units)
            || !(1..=32 * 1024 * 1024).contains(&config.output_bytes)
            || !(1..=1_000_000).contains(&config.operations)
            || !(1..=100_000_000).contains(&config.levenshtein_cells)
            || !(1..=100_000_000).contains(&config.substring_units)
            || config.timeout.is_zero()
            || config.timeout > Duration::from_secs(120)
        {
            return Err(BrowseError::new(
                "invalid_configuration",
                "Invalid browse resource limits",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel::<Work>(config.workers);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = vec![];
        for index in 0..config.workers {
            let receiver = receiver.clone();
            let config = config.clone();
            let worker = std::thread::Builder::new()
                .name(format!("freeshow-browse-{index}"))
                .spawn(move || loop {
                    let work = receiver.lock().expect("browse queue poisoned").recv();
                    let Ok(work) = work else { break };
                    let Work {
                        request,
                        cancel,
                        request_cancel,
                        started,
                        permit,
                        response,
                    } = work;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        engine::process(
                            request,
                            &mut Budget {
                                config: config.clone(),
                                cancel,
                                request_cancel,
                                started,
                                operations: 0,
                                entries: 0,
                                strings: 0,
                                cells: 0,
                                substrings: 0,
                            },
                        )
                    }))
                    .unwrap_or_else(|_| {
                        Err(BrowseError::new(
                            "browse_worker_error",
                            "Browse worker panicked",
                        ))
                    });
                    drop(permit);
                    let _ = response.send(result);
                })
                .map_err(|error| BrowseError::new("browse_worker_error", error.to_string()))?;
            workers.push(worker);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                permits: Arc::new(Semaphore::new(config.workers)),
                config,
                cancel: CancellationToken::new(),
                sender: Mutex::new(Some(sender)),
                workers: Mutex::new(workers),
                shutdown: tokio::sync::Mutex::new(()),
                #[cfg(test)]
                before: Mutex::new(None),
                #[cfg(test)]
                after: Mutex::new(None),
            }),
        })
    }
    pub async fn dispatch(&self, command: &str, payload: &Value) -> Result<BrowseReply> {
        self.dispatch_with_cancel(command, payload, CancellationToken::new())
            .await
    }
    pub async fn dispatch_with_cancel(
        &self,
        command: &str,
        payload: &Value,
        request_cancel: CancellationToken,
    ) -> Result<BrowseReply> {
        if !COMMANDS.contains(&command) {
            return Err(BrowseError::new(
                "unsupported_command",
                "Unsupported browse command",
            ));
        }
        let busy = || {
            if self.inner.cancel.is_cancelled() {
                BrowseError::new("browse_shutdown", "Browse service is shutting down")
            } else {
                BrowseError::new("browse_busy", "Browse filesystem workers are at capacity")
            }
        };
        if self.inner.cancel.is_cancelled() {
            return Err(busy());
        }
        if request_cancel.is_cancelled() {
            return Err(BrowseError::cancelled());
        }
        let request = Request::parse(command, payload, &self.inner.config)?;
        #[cfg(test)]
        if let Some(hook) = self.inner.before.lock().unwrap().take() {
            hook();
        }
        let permit = self
            .inner
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy())?;
        #[cfg(test)]
        if let Some(hook) = self.inner.after.lock().unwrap().take() {
            hook();
        }
        if self.inner.cancel.is_cancelled() {
            return Err(busy());
        }
        let cancel = self.inner.cancel.child_token();
        let _guard = CancelOnDrop(cancel.clone());
        let (response, receive) = oneshot::channel();
        {
            let sender = self.inner.sender.lock().expect("browse sender poisoned");
            if self.inner.cancel.is_cancelled() {
                return Err(busy());
            }
            sender
                .as_ref()
                .ok_or_else(busy)?
                .try_send(Work {
                    request,
                    cancel,
                    request_cancel: request_cancel.clone(),
                    started: Instant::now(),
                    permit,
                    response,
                })
                .map_err(|error| match error {
                    mpsc::TrySendError::Full(_) => busy(),
                    mpsc::TrySendError::Disconnected(_) => {
                        BrowseError::new("browse_worker_error", "Browse workers unavailable")
                    }
                })?;
        }
        tokio::select! {reply=receive=>reply.map_err(|_|BrowseError::new("browse_worker_error","Browse worker exited"))?,_=request_cancel.cancelled()=>Err(BrowseError::cancelled())}
    }
    pub async fn shutdown(&self) -> Result<()> {
        let _serial = self.inner.shutdown.lock().await;
        self.inner.cancel.cancel();
        self.inner
            .sender
            .lock()
            .expect("browse sender poisoned")
            .take();
        let _all = self
            .inner
            .permits
            .clone()
            .acquire_many_owned(self.inner.config.workers as u32)
            .await
            .map_err(|_| BrowseError::new("browse_shutdown", "Browse semaphore closed"))?;
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
                return Err(BrowseError::new(
                    "runtime_required",
                    "Pending browse worker shutdown requires Tokio runtime",
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
        self.sender
            .get_mut()
            .expect("browse sender poisoned")
            .take();
    }
}
