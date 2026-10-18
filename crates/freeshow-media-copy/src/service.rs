use crate::*;
use std::{
    sync::{mpsc, Mutex},
    thread::JoinHandle,
};
use tempfile::TempDir;
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
#[derive(Clone)]
pub struct MediaCopyService {
    inner: Arc<Inner>,
}
struct Inner {
    config: CopyConfig,
    cancel: CancellationToken,
    jobs: Arc<Semaphore>,
    leases: Arc<Semaphore>,
    sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    worker_failed: Arc<AtomicBool>,
    cleanup: Cleanup,
    cleanup_thread: Mutex<Option<JoinHandle<()>>>,
    shutdown: tokio::sync::Mutex<()>,
    #[cfg(test)]
    hooks: Arc<Hooks>,
}
struct Work {
    command: String,
    payload: Value,
    context: CopyContext,
    budget: Budget,
    lease: OwnedSemaphorePermit,
    job: OwnedSemaphorePermit,
    response: oneshot::Sender<Result<PreparedMediaCopy>>,
}
#[derive(Clone)]
pub(crate) struct Cleanup {
    sender: mpsc::SyncSender<Clean>,
    failed: Arc<AtomicBool>,
}
struct Retained {
    stage: TempDir,
    _lease: OwnedSemaphorePermit,
    state: Arc<StageState>,
    created: Instant,
    ttl: Duration,
}
enum Clean {
    #[cfg(target_os = "linux")]
    Register(Retained),
    Remove(Arc<StageState>),
    Stop(Option<oneshot::Sender<Result<()>>>),
    Fence(oneshot::Sender<()>),
}
impl Cleanup {
    #[cfg(target_os = "linux")]
    pub(crate) fn register(
        &self,
        stage: TempDir,
        lease: OwnedSemaphorePermit,
        state: Arc<StageState>,
        created: Instant,
        ttl: Duration,
    ) -> Result<()> {
        // Called only on one of the two preparation workers; failed registration
        // may drop its never-published TempDir there, never on the Tokio caller.
        self.sender
            .try_send(Clean::Register(Retained {
                stage,
                _lease: lease,
                state,
                created,
                ttl,
            }))
            .map_err(|error| {
                self.failed.store(true,Ordering::Release);
                let (mpsc::TrySendError::Full(message)|mpsc::TrySendError::Disconnected(message))=error;
                if let Clean::Register(entry)=message { dispose(entry,&self.failed); }
                CopyError::new("cleanup_failure", "Private staging cleanup unavailable")
            })
    }
    pub(crate) fn release_pin(&self, state: &Arc<StageState>) {
        if state
            .status
            .compare_exchange(PINNED, RELEASED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && self.sender.try_send(Clean::Remove(state.clone())).is_err()
        {
            self.failed.store(true, Ordering::Release);
        }
    }
    pub(crate) fn remove(&self, state: &Arc<StageState>) {
        if state
            .status
            .compare_exchange(LIVE, RELEASED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        if self.sender.try_send(Clean::Remove(state.clone())).is_err() {
            self.failed.store(true, Ordering::Release);
        }
    }
}
fn dispose(entry: Retained, failed: &AtomicBool) {
    entry.state.status.store(DEAD, Ordering::Release);
    if entry.stage.close().is_err() {
        failed.store(true, Ordering::Release);
    }
    // Lease is released only after the cleanup attempt, including failures;
    // failed cleanup closes future admission, bounding any retained disk leak.
}
fn removable(entry: &Retained, force: bool) -> bool {
    loop {
        let status = entry.state.status.load(Ordering::Acquire);
        if status == PINNED {
            return false;
        }
        if status >= RELEASED {
            return true;
        }
        if !force && entry.created.elapsed() < entry.ttl {
            return false;
        }
        if entry
            .state
            .status
            .compare_exchange(LIVE, DEAD, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return true;
        }
    }
}
fn reaper(receiver: mpsc::Receiver<Clean>, failed: Arc<AtomicBool>) {
    let mut entries: Vec<Retained> = vec![];
    let mut stopping = false;
    loop {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            #[cfg(target_os = "linux")]
            Ok(Clean::Register(entry)) => entries.push(entry),
            Ok(Clean::Remove(state)) => {
                if let Some(index) = entries.iter().position(|e| Arc::ptr_eq(&e.state, &state)) {
                    if removable(&entries[index], stopping) {
                        dispose(entries.swap_remove(index), &failed);
                    }
                }
            }
            Ok(Clean::Fence(reply)) => {
                let _ = reply.send(());
            }
            Ok(Clean::Stop(reply)) => {
                stopping = true;
                let mut index = 0;
                while index < entries.len() {
                    if removable(&entries[index], true) {
                        dispose(entries.swap_remove(index), &failed);
                    } else {
                        index += 1;
                    }
                }
                if let Some(reply) = reply {
                    let result = if entries.is_empty() {
                        Ok(())
                    } else {
                        Err(CopyError::new(
                            "copy_commit_in_progress",
                            "Drain trusted copy commit leases before shutdown",
                        ))
                    };
                    let _ = reply.send(result);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                for entry in entries.drain(..) {
                    dispose(entry, &failed);
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let mut index = 0;
        while index < entries.len() {
            if removable(&entries[index], stopping) {
                dispose(entries.swap_remove(index), &failed);
            } else {
                index += 1;
            }
        }
        if stopping && entries.is_empty() {
            break;
        }
    }
}
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Hooks {
    pub worker: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    pub before: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    pub after: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    pub process: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(target_os = "linux")]
    pub chunk: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
struct CancelOnDrop(Option<CancellationToken>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(token) = self.0.take() {
            token.cancel();
        }
    }
}
impl MediaCopyService {
    pub fn new(config: CopyConfig) -> Result<Self> {
        config.validate()?;
        let (send_clean, receive_clean) = mpsc::sync_channel(3 * config.max_prepared + 2);
        let failed = Arc::new(AtomicBool::new(false));
        let thread_failed = failed.clone();
        let cleanup_thread = std::thread::Builder::new()
            .name("freeshow-copy-cleanup".into())
            .spawn(move || reaper(receive_clean, thread_failed))
            .map_err(|_| {
                CopyError::new("copy_worker_error", "Could not start copy cleanup worker")
            })?;
        let cleanup = Cleanup {
            sender: send_clean,
            failed,
        };
        let (sender, receiver) = mpsc::sync_channel::<Work>(WORKERS + config.queued_jobs);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = vec![];
        let worker_failed = Arc::new(AtomicBool::new(false));
        for index in 0..WORKERS {
            let receive = receiver.clone();
            let clean = cleanup.clone();
            let failed = worker_failed.clone();
            let worker = std::thread::Builder::new()
                .name(format!("freeshow-copy-{index}"))
                .spawn(move || {
                    // Report an unexpected real thread exit independently of
                    // whether an IPC caller still waits for its response.
                    struct Exit {
                        failed: Arc<AtomicBool>,
                        orderly: bool,
                    }
                    impl Drop for Exit {
                        fn drop(&mut self) {
                            if !self.orderly {
                                self.failed.store(true, Ordering::Release);
                            }
                        }
                    }
                    let mut exit = Exit {
                        failed: failed.clone(),
                        orderly: false,
                    };
                    loop {
                        let work = receive.lock().expect("copy queue poisoned").recv();
                        let Ok(Work {
                            command,
                            payload,
                            context,
                            mut budget,
                            lease,
                            job,
                            response,
                        }) = work
                        else {
                            break;
                        };
                        #[cfg(test)]
                        {
                            let hook = budget.hooks.worker.lock().unwrap().take();
                            if let Some(hook) = hook {
                                hook();
                            }
                        }
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            #[cfg(test)]
                            {
                                let hook = budget.hooks.process.lock().unwrap().clone();
                                if let Some(hook) = hook {
                                    hook();
                                }
                            }
                            engine::prepare(
                                &command,
                                payload,
                                context,
                                &mut budget,
                                lease,
                                clean.clone(),
                            )
                        }))
                        .unwrap_or_else(|_| {
                            failed.store(true, Ordering::Release);
                            Err(CopyError::new(
                                "copy_worker_error",
                                "Media copy worker panicked",
                            ))
                        });
                        drop(job);
                        let _ = response.send(result);
                    }
                    exit.orderly = true;
                });
            match worker {
                Ok(worker) => workers.push(worker),
                Err(_) => {
                    drop(sender);
                    drop(receiver);
                    for worker in workers {
                        let _ = worker.join();
                    }
                    let _ = cleanup.sender.try_send(Clean::Stop(None));
                    let _ = cleanup_thread.join();
                    return Err(CopyError::new(
                        "copy_worker_error",
                        "Could not start copy preparation worker",
                    ));
                }
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                jobs: Arc::new(Semaphore::new(WORKERS + config.queued_jobs)),
                leases: Arc::new(Semaphore::new(config.max_prepared)),
                config,
                cancel: CancellationToken::new(),
                sender: Mutex::new(Some(sender)),
                workers: Mutex::new(workers),
                worker_failed,
                cleanup,
                cleanup_thread: Mutex::new(Some(cleanup_thread)),
                shutdown: tokio::sync::Mutex::new(()),
                #[cfg(test)]
                hooks: Arc::new(Hooks::default()),
            }),
        })
    }
    pub fn cleanup_failed(&self) -> bool {
        self.inner.cleanup.failed.load(Ordering::Acquire)
    }
    pub async fn prepare(
        &self,
        command: &str,
        payload: &Value,
        context: &CopyContext,
    ) -> Result<PreparedMediaCopy> {
        self.prepare_with_cancel(command, payload, context, CancellationToken::new())
            .await
    }
    pub async fn prepare_with_cancel(
        &self,
        command: &str,
        payload: &Value,
        context: &CopyContext,
        request_cancel: CancellationToken,
    ) -> Result<PreparedMediaCopy> {
        if self.inner.worker_failed.load(Ordering::Acquire) {
            return Err(CopyError::new(
                "copy_worker_error",
                "Media copy worker failed; admission closed",
            ));
        }
        let started = Instant::now();
        if !COMMANDS.contains(&command) {
            return Err(CopyError::new(
                "unsupported_command",
                "Unsupported media copy command",
            ));
        }
        if self.inner.cancel.is_cancelled() {
            return Err(CopyError::shutdown());
        }
        if request_cancel.is_cancelled() {
            return Err(CopyError::cancelled());
        }
        if self.cleanup_failed() {
            return Err(CopyError::new(
                "cleanup_failure",
                "Staging cleanup failed; copy admission closed",
            ));
        }
        // Clone only the bounded command fields; store snapshots are immutable Arc.
        let payload = engine::bounded_payload(command, payload, &self.inner.config)?;
        for root in [
            &context.data_root,
            &context.shows_root,
            &context.default_media,
            &context.sync_root,
        ] {
            if root.len() > self.inner.config.path_bytes {
                return Err(CopyError::resource("Copy context root path limit"));
            }
        }
        if context
            .approved_bundle_root
            .as_ref()
            .is_some_and(|s| s.len() > self.inner.config.path_bytes)
        {
            return Err(CopyError::resource("Approved bundle root path limit"));
        }
        let busy = || {
            if self.inner.cancel.is_cancelled() {
                CopyError::shutdown()
            } else {
                CopyError::new(
                    "copy_busy",
                    "Copy workers/queue or retained preparations are at capacity",
                )
            }
        };
        #[cfg(test)]
        if let Some(hook) = self.inner.hooks.before.lock().unwrap().take() {
            hook();
        }
        let job = self
            .inner
            .jobs
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy())?;
        let lease = self
            .inner
            .leases
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy())?;
        #[cfg(test)]
        if let Some(hook) = self.inner.hooks.after.lock().unwrap().take() {
            hook();
        }
        if self.inner.cancel.is_cancelled() {
            return Err(CopyError::shutdown());
        }
        if request_cancel.is_cancelled() {
            return Err(CopyError::cancelled());
        }
        if self.cleanup_failed() {
            return Err(CopyError::new(
                "cleanup_failure",
                "Copy cleanup failed during admission",
            ));
        }
        let cancel = self.inner.cancel.child_token();
        let mut guard = CancelOnDrop(Some(cancel.clone()));
        let (send, receive) = oneshot::channel();
        {
            let sender = self.inner.sender.lock().expect("copy sender poisoned");
            if self.inner.cancel.is_cancelled() {
                return Err(CopyError::shutdown());
            }
            if self.cleanup_failed() {
                return Err(CopyError::new(
                    "cleanup_failure",
                    "Copy cleanup failed before queue admission",
                ));
            }
            sender
                .as_ref()
                .ok_or_else(CopyError::shutdown)?
                .try_send(Work {
                    command: command.into(),
                    payload,
                    context: context.clone(),
                    budget: Budget {
                        config: self.inner.config.clone(),
                        service_cancel: self.inner.cancel.clone(),
                        cancel,
                        request_cancel: request_cancel.clone(),
                        started,
                        operations: 0,
                        #[cfg(target_os = "linux")]
                        copied: 0,
                        #[cfg(target_os = "linux")]
                        show_guards: vec![],
                        #[cfg(target_os = "linux")]
                        text: 0,
                        #[cfg(target_os = "linux")]
                        diagnostics: vec![],
                        #[cfg(target_os = "linux")]
                        omitted: 0,
                        #[cfg(test)]
                        hooks: self.inner.hooks.clone(),
                    },
                    lease,
                    job,
                    response: send,
                })
                .map_err(|e| match e {
                    mpsc::TrySendError::Full(_) => busy(),
                    mpsc::TrySendError::Disconnected(_) => {
                        CopyError::new("copy_worker_error", "Copy workers unavailable")
                    }
                })?;
        }
        let result = tokio::select! {reply=receive=>reply.map_err(|_|CopyError::new("copy_worker_error","Copy worker exited"))?,_=request_cancel.cancelled()=>Err(CopyError::cancelled())};
        if self.inner.cancel.is_cancelled() {
            return Err(CopyError::shutdown());
        }
        if request_cancel.is_cancelled() {
            return Err(CopyError::cancelled());
        }
        if result.is_ok() {
            guard.0.take();
        }
        result
    }
    /// Await cleanup messages already queued by released plans; live retained
    /// plans remain live. Serialized with shutdown, and never does caller IO.
    pub async fn flush_released(&self) -> Result<()> {
        let _serial = self.inner.shutdown.lock().await;
        if !self
            .inner
            .cleanup_thread
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(JoinHandle::is_finished)
        {
            let (send, receive) = oneshot::channel();
            self.inner
                .cleanup
                .sender
                .try_send(Clean::Fence(send))
                .map_err(|_| CopyError::new("cleanup_failure", "Copy cleanup fence unavailable"))?;
            receive
                .await
                .map_err(|_| CopyError::new("cleanup_failure", "Copy cleanup fence interrupted"))?;
        }
        if self.cleanup_failed() {
            return Err(CopyError::new(
                "cleanup_failure",
                "Private staging cleanup failed",
            ));
        }
        Ok(())
    }
    /// Terminal shutdown also expires/cleans every retained plan. It never waits
    /// for a host to drop its plan and never joins a live FS worker on Tokio.
    pub async fn shutdown(&self) -> Result<()> {
        let _serial = self.inner.shutdown.lock().await;
        self.inner.cancel.cancel();
        self.inner.sender.lock().unwrap().take();
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(CopyError::new(
                "runtime_required",
                "Copy shutdown needs Tokio with time enabled",
            ));
        }
        let _all = self
            .inner
            .jobs
            .clone()
            .acquire_many_owned((WORKERS + self.inner.config.queued_jobs) as u32)
            .await
            .map_err(|_| CopyError::shutdown())?;
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
            if worker.join().is_err() {
                self.inner.worker_failed.store(true, Ordering::Release);
            }
        }
        if self.inner.cleanup_thread.lock().unwrap().is_some() {
            if !self
                .inner
                .cleanup_thread
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(JoinHandle::is_finished)
            {
                let (send, receive) = oneshot::channel();
                match self.inner.cleanup.sender.try_send(Clean::Stop(Some(send))) {
                    Ok(()) => {
                        if let Ok(result) = receive.await {
                            result?;
                        }
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {} // A prior pin-drain stop may already have exited.
                    Err(mpsc::TrySendError::Full(_)) => {
                        return Err(CopyError::new(
                            "cleanup_failure",
                            "Copy cleanup stop queue unexpectedly full",
                        ))
                    }
                }
            }
            while !self
                .inner
                .cleanup_thread
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(JoinHandle::is_finished)
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            if let Some(worker) = self.inner.cleanup_thread.lock().unwrap().take() {
                if worker.join().is_err() {
                    self.inner.cleanup.failed.store(true, Ordering::Release);
                }
            }
        }
        if self.inner.worker_failed.load(Ordering::Acquire) {
            return Err(CopyError::new(
                "copy_worker_error",
                "Media copy worker failed; teardown remains failed",
            ));
        }
        if self.cleanup_failed() {
            return Err(CopyError::new(
                "cleanup_failure",
                "Copy private staging cleanup failed",
            ));
        }
        Ok(())
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.sender.get_mut().unwrap().take();
        let _ = self.cleanup.sender.try_send(Clean::Stop(None));
    }
}
#[cfg(test)]
mod tests;
