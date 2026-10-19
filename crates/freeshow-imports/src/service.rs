pub(crate) use crate::cleanup::CleanupSender;
use crate::*;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use tokio::sync::{oneshot, Semaphore};

pub struct ImportService {
    pub(crate) inner: Arc<Inner>,
}
pub(crate) struct Inner {
    pub(crate) config: ImportConfig,
    pub(crate) cancel: CancellationToken,
    pub(crate) jobs: Arc<Semaphore>,
    leases: Arc<Semaphore>,
    pub(crate) sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    pub(crate) cleanup: CleanupSender,
    shutdown: tokio::sync::Mutex<()>,
    observers: Arc<Semaphore>,
    worker_failed: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    pub(crate) before_admission: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    pub(crate) after_admission: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
pub(crate) struct Work {
    input: prepare::Input,
    _job: OwnedSemaphorePermit,
    reply: oneshot::Sender<Result<PreparedImport>>,
}
struct CancelOnDrop(Option<CancellationToken>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel.cancel();
        }
    }
}
impl ImportService {
    #[cfg(test)]
    pub(crate) fn retained_available(&self) -> usize {
        self.inner.leases.available_permits()
    }
    #[cfg(test)]
    pub(crate) fn observers_available(&self) -> usize {
        self.inner.observers.available_permits()
    }
    pub fn new(config: ImportConfig) -> Result<Self> {
        if !(1..=8).contains(&config.workers)
            || !(1..=16).contains(&config.max_prepared)
            || config.max_files == 0
            || config.max_files > 256
            || config.max_entries == 0
            || config.max_entries > 8192
            || config.max_depth == 0
            || config.max_depth > 32
            || config.max_archives == 0
            || config.max_archives > 64
            || config.operations == 0
            || config.operations > 1_000_000
            || config.timeout.is_zero()
            || config.timeout > Duration::from_secs(120)
            || config.text_bytes > 32 * 1024 * 1024
            || config.entry_bytes > 128 * 1024 * 1024
            || config.input_bytes > 512 * 1024 * 1024
            || config.archive_bytes > 128 * 1024 * 1024
            || config.expanded_bytes > 512 * 1024 * 1024
            || config.output_bytes > 64 * 1024 * 1024
            || config.expansion_ratio > 1000
        {
            return Err(ImportError::new(
                "invalid_configuration",
                "Invalid import resource limits",
            ));
        }
        let cleanup = CleanupSender::new(config.max_prepared)?;
        let (sender, receiver) = mpsc::sync_channel::<Work>(config.workers);
        let receiver = Arc::new(Mutex::new(receiver));
        let cancel = CancellationToken::new();
        let mut threads = vec![];
        for index in 0..config.workers {
            let receiver = receiver.clone();
            let config = config.clone();
            let cleanup = cleanup.clone();
            let thread = std::thread::Builder::new()
                .name(format!("freeshow-import-{index}"))
                .spawn(move || loop {
                    let work = receiver.lock().expect("import receiver poisoned").recv();
                    let Ok(work) = work else { break };
                    let Work { input, _job, reply } = work;
                    let result = prepare::prepare(input, config.clone(), cleanup.clone());
                    drop(_job);
                    let _ = reply.send(result);
                });
            match thread {
                Ok(thread) => threads.push(thread),
                Err(error) => {
                    drop(sender);
                    for thread in threads {
                        let _ = thread.join();
                    }
                    return Err(ImportError::io(error));
                }
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                jobs: Arc::new(Semaphore::new(config.workers)),
                leases: Arc::new(Semaphore::new(config.max_prepared)),
                config,
                cancel,
                sender: Mutex::new(Some(sender)),
                threads: Mutex::new(threads),
                cleanup,
                shutdown: tokio::sync::Mutex::new(()),
                observers: Arc::new(Semaphore::new(4)),
                worker_failed: std::sync::atomic::AtomicBool::new(false),
                #[cfg(test)]
                before_admission: Mutex::new(None),
                #[cfg(test)]
                after_admission: Mutex::new(None),
            }),
        })
    }
    /// True after failed cleanup; admission fails closed to bound retained disk.
    pub fn cleanup_failed(&self) -> bool {
        self.inner.cleanup.failed()
    }
    /// Wait for already released private staging. Live plans retain their leases
    /// and do not delay this barrier; ordinary admission limits still apply.
    pub async fn finish_cleanup(&self) -> Result<()> {
        let _observer = self.observer()?;
        let _serial = self.inner.shutdown.lock().await;
        self.inner.cleanup.fence().await
    }
    pub async fn prepare(
        &self,
        command: &str,
        payload: &Value,
        roots: ImportRoots,
    ) -> Result<PreparedImport> {
        self.prepare_with_cancel(command, payload, roots, CancellationToken::new())
            .await
    }
    pub async fn prepare_with_cancel(
        &self,
        command: &str,
        payload: &Value,
        roots: ImportRoots,
        request_cancel: CancellationToken,
    ) -> Result<PreparedImport> {
        if command != "IMPORT_FILES" {
            return Err(ImportError::new(
                "unsupported_command",
                format!("Unsupported import command: {command}"),
            ));
        }
        if request_cancel.is_cancelled() {
            return Err(ImportError::cancelled());
        }
        if self.inner.cancel.is_cancelled() {
            return Err(ImportError::new(
                "shutdown",
                "Import service is shutting down",
            ));
        }
        if self.cleanup_failed() {
            return Err(ImportError::new(
                "cleanup_failure",
                "Import cleanup failed; service restart required",
            ));
        }
        let object = payload.as_object().ok_or_else(|| {
            ImportError::new("invalid_payload", "Import payload must be an object")
        })?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| ImportError::new("invalid_payload", "Import id must be a string"))?;
        if !FORMATS.contains(&id) {
            return Err(ImportError::new(
                "unsupported_format",
                format!("Unsupported import format: {id}"),
            ));
        }
        let paths = match object.get("paths") {
            Some(Value::Null) | None => &[][..],
            Some(Value::Array(paths)) => paths.as_slice(),
            _ => {
                return Err(ImportError::new(
                    "invalid_payload",
                    "Import paths must be an array",
                ))
            }
        };
        if paths.len() > self.inner.config.max_files {
            return Err(ImportError::resource("Too many selected import files"));
        }
        for path in paths {
            let path = path.as_str().ok_or_else(|| {
                ImportError::new("invalid_payload", "Import paths must be strings")
            })?;
            if path.len() > 32768 || path.contains('\0') {
                return Err(ImportError::resource("Import path length/NUL limit"));
            }
        }
        if !roots.imports_root.is_absolute()
            || roots.imports_root.as_os_str().len() > 32768
            || roots
                .imports_root
                .to_str()
                .is_none_or(|s| s.contains(['\0', '\n', '\r']))
        {
            return Err(ImportError::new(
                "import_invalid_path",
                "Import root must be an absolute Unicode path",
            ));
        }
        #[cfg(unix)]
        if roots.imports_root.to_str().is_some_and(|root| {
            root.contains('\\')
                || root.contains("//")
                || root.split('/').any(|part| matches!(part, "." | ".."))
        }) {
            return Err(ImportError::new(
                "import_invalid_path",
                "Import root must use normalized native separators",
            ));
        }
        let busy = || {
            if self.inner.cancel.is_cancelled() {
                ImportError::new("shutdown", "Import service is shutting down")
            } else {
                ImportError::new("busy", "Import jobs or retained plans are at capacity")
            }
        };
        #[cfg(test)]
        if let Some(hook) = self.inner.before_admission.lock().unwrap().take() {
            hook();
        }
        let lease = self
            .inner
            .leases
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy())?;
        let job = self
            .inner
            .jobs
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy())?;
        #[cfg(test)]
        if let Some(hook) = self.inner.after_admission.lock().unwrap().take() {
            hook();
        }
        let cancel = self.inner.cancel.child_token();
        if self.inner.cancel.is_cancelled() {
            return Err(busy());
        }
        let work_paths = paths
            .iter()
            .map(|p| p.as_str().unwrap().to_owned())
            .collect();
        let (reply, receive) = oneshot::channel();
        {
            let sender = self.inner.sender.lock().expect("import sender poisoned");
            if self.inner.cancel.is_cancelled() {
                return Err(busy());
            }
            sender
                .as_ref()
                .ok_or_else(busy)?
                .try_send(Work {
                    input: prepare::Input {
                        id: id.into(),
                        paths: work_paths,
                        roots,
                        cancel: cancel.clone(),
                        request_cancel: request_cancel.clone(),
                        started: Instant::now(),
                        lease,
                    },
                    _job: job,
                    reply,
                })
                .map_err(|error| match error {
                    mpsc::TrySendError::Full(_) => busy(),
                    mpsc::TrySendError::Disconnected(_) => {
                        ImportError::new("worker_failure", "Import workers are unavailable")
                    }
                })?;
        }
        let mut guard = CancelOnDrop(Some(cancel.clone()));
        let result = tokio::select! {result=receive=>result.map_err(|_|ImportError::new("worker_failure","Import worker exited"))?,_ = request_cancel.cancelled()=>return Err(ImportError::cancelled())};
        // A successful prepared plan must outlive this request's cancellation guard.
        if result.is_ok() {
            guard.0.take();
        }
        result
    }
    fn observer(&self) -> Result<OwnedSemaphorePermit> {
        self.inner
            .observers
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                ImportError::new(
                    "lifecycle_busy",
                    "Import lifecycle observers are at capacity",
                )
            })
    }
    /// Stop preparation, join actual parser workers and fence already released
    /// stages. Live plans remain owned and may enqueue cleanup when released.
    pub async fn close_preparation(&self) -> Result<()> {
        let _observer = self.observer()?;
        let _serial = self.inner.shutdown.lock().await;
        self.join_preparation().await?;
        self.inner.cleanup.fence().await
    }
    /// Terminal proof: all parser workers and the cleanup worker actually joined.
    /// Live staged plans must first be released by their native host. Dropping or
    /// timing out this observer retains the same handles and cleanup ownership.
    pub async fn shutdown(&self) -> Result<()> {
        let _observer = self.observer()?;
        let _serial = self.inner.shutdown.lock().await;
        let preparation = self.join_preparation().await;
        // A pending parser might still create a stage: never close its sender.
        if !self
            .inner
            .threads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            return preparation;
        }
        let cleanup = self.inner.cleanup.join().await;
        preparation.and(cleanup)
    }
    async fn join_preparation(&self) -> Result<()> {
        self.inner.cancel.cancel();
        self.inner
            .sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let permits = self.inner.config.workers as u32;
        let _all = match self.inner.jobs.clone().try_acquire_many_owned(permits) {
            Ok(all) => all,
            Err(_) => {
                crate::cleanup::require_runtime()?;
                self.inner
                    .jobs
                    .clone()
                    .acquire_many_owned(permits)
                    .await
                    .map_err(|_| ImportError::new("shutdown", "Import job semaphore closed"))?
            }
        };
        loop {
            if self
                .inner
                .threads
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .all(JoinHandle::is_finished)
            {
                break;
            }
            crate::cleanup::pause().await?;
        }
        let mut threads = self.inner.threads.lock().unwrap_or_else(|e| e.into_inner());
        for thread in threads.drain(..) {
            if thread.join().is_err() {
                self.inner
                    .worker_failed
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
        if self
            .inner
            .worker_failed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            Err(ImportError::new(
                "worker_failure",
                "Import preparation worker failed",
            ))
        } else {
            Ok(())
        }
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.sender
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        // Emergency disposal retains actual native joins. Hosts should use the
        // async lifecycle while ownership is retained; Drop can wait for OS IO.
        for thread in self
            .threads
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            let _ = thread.join();
        }
    }
}
