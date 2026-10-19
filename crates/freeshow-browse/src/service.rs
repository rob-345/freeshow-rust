use crate::*;
use std::{
    sync::{mpsc, Arc, Mutex},
    thread::JoinHandle,
};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
const MAX_WAITING: usize = 8;
#[derive(Clone)]
pub struct BrowseService {
    pub(crate) inner: Arc<Inner>,
}
pub(crate) struct Inner {
    config: BrowseConfig,
    pub(crate) cancel: CancellationToken,
    pub(crate) permits: Arc<Semaphore>,
    waiting: Arc<Semaphore>,
    pub(crate) sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    shutdown: tokio::sync::Mutex<()>,
    #[cfg(test)]
    pub before: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    pub after: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(any(test, feature = "test-harness"))]
    pub worker_hooks: Arc<TestWorkerHooks>,
}
#[cfg(any(test, feature = "test-harness"))]
type EntryHook = Arc<dyn Fn(usize, &Request, Instant) + Send + Sync>;
#[cfg(test)]
type ReturnHook = Arc<dyn Fn() + Send + Sync>;
#[cfg(any(test, feature = "test-harness"))]
#[derive(Default)]
pub(crate) struct TestWorkerHooks {
    pub entered: Mutex<Option<EntryHook>>,
    #[cfg(test)]
    pub returned: Mutex<Option<ReturnHook>>,
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
        #[cfg(any(test, feature = "test-harness"))]
        let worker_hooks = Arc::new(TestWorkerHooks::default());
        for index in 0..config.workers {
            let receiver = receiver.clone();
            let config = config.clone();
            #[cfg(any(test, feature = "test-harness"))]
            let hooks = worker_hooks.clone();
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
                    #[cfg(any(test, feature = "test-harness"))]
                    {
                        let hook = hooks.entered.lock().unwrap().clone();
                        if let Some(hook) = hook {
                            hook(index, &request, started);
                        }
                    }
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
                    #[cfg(test)]
                    {
                        let hook = hooks.returned.lock().unwrap().clone();
                        if let Some(hook) = hook {
                            hook();
                        }
                    }
                    // A canceled receiver can make send dispose the actual
                    // result here. Retain execution capacity through disposal.
                    let _ = response.send(result);
                    drop(permit);
                })
                .map_err(|error| BrowseError::new("browse_worker_error", error.to_string()))?;
            workers.push(worker);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                permits: Arc::new(Semaphore::new(config.workers)),
                waiting: Arc::new(Semaphore::new(MAX_WAITING)),
                config,
                cancel: CancellationToken::new(),
                sender: Mutex::new(Some(sender)),
                workers: Mutex::new(workers),
                shutdown: tokio::sync::Mutex::new(()),
                #[cfg(test)]
                before: Mutex::new(None),
                #[cfg(test)]
                after: Mutex::new(None),
                #[cfg(any(test, feature = "test-harness"))]
                worker_hooks,
            }),
        })
    }
    pub async fn dispatch(&self, command: &str, payload: &Value) -> Result<BrowseReply> {
        self.dispatch_with_cancel(command, payload, CancellationToken::new())
            .await
    }
    #[cfg(test)]
    pub(crate) fn waiting_count(&self) -> usize {
        self.queued_waiters_for_test()
    }
    /// Observe only retained waiting slots; this performs no disposal or join.
    #[cfg(any(test, feature = "test-harness"))]
    pub fn queued_waiters_for_test(&self) -> usize {
        MAX_WAITING - self.inner.waiting.available_permits()
    }
    #[cfg(test)]
    pub(crate) fn retained_worker_count(&self) -> usize {
        self.inner.workers.lock().unwrap().len()
    }
    /// Arm one bounded entry gate per configured real filesystem owner. Call
    /// before dispatch, await all unique worker indices, then send/drop each
    /// matching release sender. A service can be armed only once. This hook is
    /// absent from ordinary builds and never creates a task/thread or fake work.
    #[cfg(any(test, feature = "test-harness"))]
    pub fn hold_workers_for_test(
        &self,
    ) -> Result<(
        tokio::sync::mpsc::Receiver<usize>,
        Vec<mpsc::SyncSender<()>>,
    )> {
        let mut hook = self.inner.worker_hooks.entered.lock().unwrap();
        if hook.is_some() {
            return Err(BrowseError::new(
                "browse_busy",
                "Browse worker entry gate is already installed",
            ));
        }
        let (entered, receive) = tokio::sync::mpsc::channel(self.inner.config.workers);
        let mut releases = Vec::with_capacity(self.inner.config.workers);
        let mut gates = Vec::with_capacity(self.inner.config.workers);
        for _ in 0..self.inner.config.workers {
            let (release, gate) = mpsc::sync_channel(1);
            releases.push(release);
            gates.push(Some(gate));
        }
        let gates = Mutex::new(gates);
        *hook = Some(Arc::new(move |index, _, _| {
            let gate = gates.lock().unwrap()[index].take();
            if let Some(gate) = gate {
                let _ = entered.try_send(index);
                let _ = gate.recv();
            }
        }));
        Ok((receive, releases))
    }
    pub async fn dispatch_with_cancel(
        &self,
        command: &str,
        payload: &Value,
        request_cancel: CancellationToken,
    ) -> Result<BrowseReply> {
        let reply = self.run(command, payload, request_cancel).await?;
        if !reply.thumbnail_intents.is_empty() {
            return Err(BrowseError {
                code: "thumbnail_unavailable",
                message: "Folder listing requires native thumbnail generation; no thumbnails were generated".into(),
                thumbnail_intents: reply.thumbnail_intents,
            });
        }
        Ok(reply)
    }
    /// Wait in a separate bounded FIFO admission queue instead of failing when
    /// the filesystem owners are occupied. No task is spawned by this method.
    pub async fn dispatch_queued(&self, command: &str, payload: &Value) -> Result<BrowseReply> {
        self.dispatch_queued_with_cancel(command, payload, CancellationToken::new())
            .await
    }
    pub async fn dispatch_queued_with_cancel(
        &self,
        command: &str,
        payload: &Value,
        request_cancel: CancellationToken,
    ) -> Result<BrowseReply> {
        let reply = self.run_queued(command, payload, request_cancel).await?;
        if !reply.thumbnail_intents.is_empty() {
            return Err(BrowseError {
                code: "thumbnail_unavailable",
                message: "Folder listing requires native thumbnail generation; no thumbnails were generated".into(),
                thumbnail_intents: reply.thumbnail_intents,
            });
        }
        Ok(reply)
    }
    /// Retain the original traversal and unresolved thumbnail slots for a host
    /// which owns bounded capture/effects and validates the frozen context.
    pub async fn prepare_listing_with_cancel(
        &self,
        payload: &Value,
        request_cancel: CancellationToken,
    ) -> Result<PreparedListing> {
        let reply = self.run("READ_FOLDER", payload, request_cancel).await?;
        Ok(PreparedListing {
            data: reply.data,
            thumbnail_intents: reply.thumbnail_intents,
            diagnostics: reply.diagnostics,
        })
    }
    /// The queued equivalent of `prepare_listing_with_cancel`; the same total
    /// configured deadline includes parsing, admission wait and filesystem work.
    pub async fn prepare_listing_queued_with_cancel(
        &self,
        payload: &Value,
        request_cancel: CancellationToken,
    ) -> Result<PreparedListing> {
        let reply = self
            .run_queued("READ_FOLDER", payload, request_cancel)
            .await?;
        Ok(PreparedListing {
            data: reply.data,
            thumbnail_intents: reply.thumbnail_intents,
            diagnostics: reply.diagnostics,
        })
    }
    async fn run_queued(
        &self,
        command: &str,
        payload: &Value,
        request_cancel: CancellationToken,
    ) -> Result<BrowseReply> {
        let started = Instant::now();
        let deadline = started + self.inner.config.timeout;
        let check = || {
            if self.inner.cancel.is_cancelled() {
                Err(BrowseError::new(
                    "browse_shutdown",
                    "Browse service is shutting down",
                ))
            } else if request_cancel.is_cancelled() {
                Err(BrowseError::cancelled())
            } else if Instant::now() >= deadline {
                Err(BrowseError::resource("Browse operation/deadline limit"))
            } else {
                Ok(())
            }
        };
        check()?;
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(BrowseError::new(
                "runtime_required",
                "Queued browse requests require a Tokio runtime with timers",
            ));
        }
        // Acquire before parsing retains any owned paths/options. The caller's
        // future owns this slot; cancellation/Drop removes its semaphore waiter.
        let waiting = self
            .inner
            .waiting
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                BrowseError::new("browse_busy", "Browse waiting admissions are at capacity")
            })?;
        let request = Request::parse(command, payload, &self.inner.config)?;
        check()?;
        let permit = tokio::select! {
            biased;
            _ = self.inner.cancel.cancelled() => return Err(BrowseError::new("browse_shutdown", "Browse service is shutting down")),
            _ = request_cancel.cancelled() => return Err(BrowseError::cancelled()),
            _ = tokio::time::sleep_until(deadline.into()) => return Err(BrowseError::resource("Browse operation/deadline limit")),
            permit = self.inner.permits.clone().acquire_owned() => permit.map_err(|_| BrowseError::new("browse_shutdown", "Browse semaphore closed"))?,
        };
        check()?;
        drop(waiting);
        let cancel = self.inner.cancel.child_token();
        let _guard = CancelOnDrop(cancel.clone());
        let (response, receive) = oneshot::channel();
        {
            let sender = self.inner.sender.lock().expect("browse sender poisoned");
            check()?;
            sender
                .as_ref()
                .ok_or_else(|| {
                    BrowseError::new("browse_shutdown", "Browse service is shutting down")
                })?
                .try_send(Work {
                    request,
                    cancel,
                    request_cancel: request_cancel.clone(),
                    started,
                    permit,
                    response,
                })
                .map_err(|error| match error {
                    mpsc::TrySendError::Full(_) => {
                        BrowseError::new("browse_busy", "Browse filesystem queue is at capacity")
                    }
                    mpsc::TrySendError::Disconnected(_) => {
                        BrowseError::new("browse_worker_error", "Browse workers unavailable")
                    }
                })?;
        }
        tokio::select! {
            biased;
            _ = self.inner.cancel.cancelled() => Err(BrowseError::new("browse_shutdown", "Browse service is shutting down")),
            _ = request_cancel.cancelled() => Err(BrowseError::cancelled()),
            _ = tokio::time::sleep_until(deadline.into()) => Err(BrowseError::resource("Browse operation/deadline limit")),
            reply = receive => {
                let reply = reply.map_err(|_| BrowseError::new("browse_worker_error", "Browse worker exited"))?;
                check()?;
                reply
            }
        }
    }
    async fn run(
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
