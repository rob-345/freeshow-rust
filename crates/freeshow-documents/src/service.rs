use crate::*;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
#[derive(Clone)]
pub struct DocumentService {
    inner: Arc<Inner>,
}
struct Inner {
    config: DocumentConfig,
    cancel: CancellationToken,
    permits: Arc<Semaphore>,
    sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    shutdown: tokio::sync::Mutex<()>,
    #[cfg(test)]
    hooks: Arc<Hooks>,
}
struct Work {
    request: Request,
    budget: Budget,
    permit: OwnedSemaphorePermit,
    response: oneshot::Sender<Result<PreparedDocumentReply>>,
}
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Hooks {
    pub before_permit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    pub after_permit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    pub process: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(target_os = "linux")]
    pub chunk: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(target_os = "linux")]
    pub entry: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
impl DocumentService {
    /// Starts exactly two independent filesystem workers. No runtime is required
    /// to construct/drop this service. Call shutdown inside Tokio before exit.
    pub fn new(config: DocumentConfig) -> Result<Self> {
        config.validate()?;
        // Include reserved worker slots: a not-yet-scheduled worker must not
        // turn a successfully acquired job permit into a spurious Busy.
        let (sender, receiver) = mpsc::sync_channel::<Work>(WORKERS + config.queued_jobs);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::new();
        for index in 0..WORKERS {
            let receive = receiver.clone();
            let worker = std::thread::Builder::new()
                .name(format!("freeshow-documents-{index}"))
                .spawn(move || loop {
                    let work = receive.lock().expect("document queue poisoned").recv();
                    let Ok(Work {
                        request,
                        mut budget,
                        permit,
                        response,
                    }) = work
                    else {
                        break;
                    };
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        #[cfg(test)]
                        {
                            let hook = budget.hooks.process.lock().unwrap().clone();
                            if let Some(hook) = hook {
                                hook();
                            }
                        }
                        engine::process(request, &mut budget)
                    }))
                    .unwrap_or_else(|_| {
                        Err(DocumentError::new(
                            "document_worker_error",
                            "Document worker panicked",
                        ))
                    });
                    // Permit remains held until all synchronous filesystem work ends,
                    // including when a canceled caller no longer receives the reply.
                    drop(permit);
                    let _ = response.send(result);
                });
            match worker {
                Ok(worker) => workers.push(worker),
                Err(_) => {
                    drop(sender);
                    drop(receiver);
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return Err(DocumentError::new(
                        "document_worker_error",
                        "Could not start document workers",
                    ));
                }
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                permits: Arc::new(Semaphore::new(WORKERS + config.queued_jobs)),
                config,
                cancel: CancellationToken::new(),
                sender: Mutex::new(Some(sender)),
                workers: Mutex::new(workers),
                shutdown: tokio::sync::Mutex::new(()),
                #[cfg(test)]
                hooks: Arc::new(Hooks::default()),
            }),
        })
    }
    pub async fn dispatch(
        &self,
        command: &str,
        payload: &Value,
        context: &DocumentContext,
    ) -> Result<PreparedDocumentReply> {
        self.dispatch_with_cancel(command, payload, context, CancellationToken::new())
            .await
    }
    pub async fn dispatch_with_cancel(
        &self,
        command: &str,
        payload: &Value,
        context: &DocumentContext,
        request_cancel: CancellationToken,
    ) -> Result<PreparedDocumentReply> {
        let started = Instant::now();
        if !COMMANDS.contains(&command) {
            return Err(DocumentError::new(
                "unsupported_command",
                "Unsupported documents command",
            ));
        }
        if self.inner.cancel.is_cancelled() {
            return Err(DocumentError::shutdown());
        }
        if request_cancel.is_cancelled() {
            return Err(DocumentError::cancelled());
        }
        let request = match command {
            "READ_FILE" => {
                let path = payload.get("path").and_then(Value::as_str).ok_or_else(|| {
                    DocumentError::new("invalid_request", "READ_FILE requires a string path")
                })?;
                if path.len() > self.inner.config.path_bytes {
                    return Err(DocumentError::resource("Document path length limit"));
                }
                Request::File(path.into())
            }
            "READ_BIBLES_FOLDER" => {
                if context.data_root.as_os_str().len() > self.inner.config.path_bytes
                    || context.bibles_root.as_os_str().len() > self.inner.config.path_bytes
                {
                    return Err(DocumentError::resource(
                        "Trusted document root length limit",
                    ));
                }
                Request::Bibles(context.clone())
            }
            _ => unreachable!(),
        };
        let busy = || {
            if self.inner.cancel.is_cancelled() {
                DocumentError::shutdown()
            } else {
                DocumentError::new(
                    "documents_busy",
                    "Document filesystem workers and queue are at capacity",
                )
            }
        };
        #[cfg(test)]
        if let Some(hook) = self.inner.hooks.before_permit.lock().unwrap().take() {
            hook();
        }
        let permit = self
            .inner
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy())?;
        #[cfg(test)]
        if let Some(hook) = self.inner.hooks.after_permit.lock().unwrap().take() {
            hook();
        }
        if self.inner.cancel.is_cancelled() {
            return Err(DocumentError::shutdown());
        }
        if request_cancel.is_cancelled() {
            return Err(DocumentError::cancelled());
        }
        let cancel = self.inner.cancel.child_token();
        let _guard = CancelOnDrop(cancel.clone());
        let (send, receive) = oneshot::channel();
        {
            let sender = self.inner.sender.lock().expect("document sender poisoned");
            if self.inner.cancel.is_cancelled() {
                return Err(DocumentError::shutdown());
            }
            sender
                .as_ref()
                .ok_or_else(DocumentError::shutdown)?
                .try_send(Work {
                    request,
                    budget: Budget {
                        config: self.inner.config.clone(),
                        service_cancel: self.inner.cancel.clone(),
                        cancel,
                        request_cancel: request_cancel.clone(),
                        started,
                        operations: 0,
                        #[cfg(test)]
                        hooks: self.inner.hooks.clone(),
                    },
                    permit,
                    response: send,
                })
                .map_err(|error| match error {
                    mpsc::TrySendError::Full(_) => busy(),
                    mpsc::TrySendError::Disconnected(_) => {
                        DocumentError::new("document_worker_error", "Document workers unavailable")
                    }
                })?;
        }
        tokio::select! {
            reply = receive => {
                let reply = reply.map_err(|_| DocumentError::new("document_worker_error", "Document worker exited"))?;
                if self.inner.cancel.is_cancelled() { return Err(DocumentError::shutdown()); }
                if request_cancel.is_cancelled() { return Err(DocumentError::cancelled()); }
                reply
            },
            _ = request_cancel.cancelled() => Err(DocumentError::cancelled()),
        }
    }
    /// Terminal: closes admission, cancels/drains all accepted work, then joins
    /// finished worker threads. Never joins a live filesystem worker on Tokio.
    pub async fn shutdown(&self) -> Result<()> {
        let _serial = self.inner.shutdown.lock().await;
        self.inner.cancel.cancel();
        self.inner
            .sender
            .lock()
            .expect("document sender poisoned")
            .take();
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(DocumentError::new(
                "runtime_required",
                "Document shutdown drain requires a Tokio runtime",
            ));
        }
        let _all = self
            .inner
            .permits
            .clone()
            .acquire_many_owned((WORKERS + self.inner.config.queued_jobs) as u32)
            .await
            .map_err(|_| DocumentError::shutdown())?;
        while !self
            .inner
            .workers
            .lock()
            .unwrap()
            .iter()
            .all(JoinHandle::is_finished)
        {
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
            .expect("document sender poisoned")
            .take();
    }
}
#[cfg(test)]
mod tests;
