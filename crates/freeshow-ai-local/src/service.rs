use crate::{
    engine::{self, Budget},
    *,
};
use std::{
    sync::{mpsc, Arc, Mutex},
    thread::JoinHandle,
};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};

const OBSERVERS: usize = 4;
#[derive(Clone)]
pub struct InventoryService {
    inner: Arc<Inner>,
}
struct Inner {
    root: PathBuf,
    limits: Limits,
    state: Arc<State>,
    slots: Arc<Semaphore>,
    observers: Arc<Semaphore>,
    sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    #[cfg(test)]
    pub(crate) hooks: Arc<Hooks>,
}
struct State {
    cancel: CancellationToken,
    failure: Mutex<Option<Error>>,
}
impl State {
    fn failure(&self) -> Option<Error> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn fail(&self) {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(Error::new(
                "ai_local_worker",
                "Local inventory worker failed",
            ));
        self.cancel.cancel();
    }
}
impl Inner {
    fn close(&self) {
        self.state.cancel.cancel();
        self.sender.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
    fn join_finished(&self) -> bool {
        let mut worker = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if worker.as_ref().is_some_and(|w| !w.is_finished()) {
            return false;
        }
        // Retain this named guard through join so a second observer cannot see
        // taken-but-unjoined ownership. No join while native work is running.
        if let Some(w) = worker.take() {
            if w.join().is_err() {
                self.state.fail();
            }
        }
        true
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.close();
    }
}
struct Lease {
    _owner: Arc<Inner>,
    _permit: OwnedSemaphorePermit,
}
struct Envelope {
    result: Result<Vec<BinFile>>,
    _lease: Arc<Lease>,
}
struct Work {
    root: PathBuf,
    budget: Budget,
    response: oneshot::Sender<Envelope>,
    lease: Arc<Lease>,
}
pub struct Ticket {
    response: Option<oneshot::Receiver<Envelope>>,
    control: Control,
    inner: Arc<Inner>,
    _lease: Arc<Lease>,
    armed: bool,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if self.armed {
            self.control.cancel.cancel();
        }
    }
}
/// Holds complete-request capacity while the caller borrows/publishes the rows.
/// Drop releases the result and its lease together; no public unguarded rows.
pub struct InventoryRead {
    envelope: Envelope,
}
impl InventoryRead {
    pub fn result(&self) -> std::result::Result<&[BinFile], &Error> {
        self.envelope.result.as_deref()
    }
    #[cfg(test)]
    pub(crate) fn into_result(self) -> Result<Vec<BinFile>> {
        self.envelope.result
    }
}
impl Ticket {
    fn check_observation(&self) -> Result<()> {
        if let Some(error) = self.inner.state.failure() {
            return Err(error);
        }
        if self.inner.state.cancel.is_cancelled() {
            return Err(Error::closed());
        }
        if self.control.cancel.is_cancelled() {
            return Err(Error::new(
                "ai_local_cancelled",
                "Local inventory request cancelled",
            ));
        }
        if Instant::now() >= self.control.deadline {
            return Err(Error::new(
                "ai_local_deadline",
                "Local inventory observer deadline elapsed; accepted work remains owned",
            ));
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn ready_for_test(&self) -> bool {
        self.response.as_ref().is_some_and(|r| !r.is_empty())
    }
    pub async fn finish(mut self) -> Result<InventoryRead> {
        self.check_observation()?;
        let response = self.response.take().expect("One inventory reply");
        let observed = async {
            tokio::select! {
                reply=response=>reply.map_err(|_|self.inner.state.failure().unwrap_or_else(||Error::new("ai_local_worker","Local inventory worker stopped"))),
                _=self.control.cancel.cancelled()=>Err(Error::new("ai_local_cancelled","Local inventory request cancelled")),
                _=self.inner.state.cancel.cancelled()=>Err(self.inner.state.failure().unwrap_or_else(Error::closed)),
            }
        };
        let envelope = if tokio::runtime::Handle::try_current().is_ok() {
            tokio::time::timeout_at(self.control.deadline.into(), observed)
                .await
                .map_err(|_| {
                    Error::new(
                        "ai_local_deadline",
                        "Local inventory observer deadline elapsed; accepted work remains owned",
                    )
                })??
        } else {
            return Err(Error::new(
                "ai_local_runtime",
                "Inventory reply wait requires Tokio runtime; accepted work remains owned",
            ));
        };
        self.check_observation()?;
        self.armed = false;
        Ok(InventoryRead { envelope })
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub closing: bool,
    pub retained: usize,
    pub capacity: usize,
    pub worker_running: bool,
    pub worker_joined: bool,
    pub shutdown_observers: usize,
    pub failure: Option<ErrorStatus>,
}
#[derive(Debug, Serialize)]
pub struct ErrorStatus {
    pub code: &'static str,
    pub message: &'static str,
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct Hooks {
    pub processing: Mutex<Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
    pub exit: Mutex<Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
    pub panic: std::sync::atomic::AtomicBool,
    pub panic_exit: std::sync::atomic::AtomicBool,
    pub logical_done: std::sync::atomic::AtomicBool,
    pub native_entries: std::sync::atomic::AtomicUsize,
}
impl InventoryService {
    pub fn start(root: TrustedUserData, limits: Limits) -> Result<Self> {
        limits.validate()?;
        let capacity = limits.capacity;
        let state = Arc::new(State {
            cancel: CancellationToken::new(),
            failure: Mutex::new(None),
        });
        let (sender, receiver) = mpsc::sync_channel::<Work>(limits.capacity);
        let worker_state = state.clone();
        #[cfg(test)]
        let hooks = Arc::new(Hooks::default());
        #[cfg(test)]
        let worker_hooks = hooks.clone();
        let worker = std::thread::Builder::new()
            .name("freeshow-ai-local".into())
            .spawn(move || {
                while let Ok(Work {
                    root,
                    mut budget,
                    response,
                    lease,
                }) = receiver.recv()
                {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        #[cfg(test)]
                        {
                            if let Some((entered, release)) =
                                worker_hooks.processing.lock().unwrap().take()
                            {
                                let _ = entered.send(());
                                let _ = release.recv();
                            }
                            assert!(
                                !worker_hooks
                                    .panic
                                    .swap(false, std::sync::atomic::Ordering::AcqRel),
                                "Controlled inventory worker panic"
                            );
                        }
                        budget.check()?;
                        #[cfg(test)]
                        worker_hooks
                            .native_entries
                            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                        engine::query(&root, &mut budget)
                    }))
                    .unwrap_or_else(|_| {
                        worker_state.fail();
                        Err(worker_state.failure().unwrap())
                    });
                    let envelope = Envelope {
                        result,
                        _lease: lease.clone(),
                    };
                    // A failed send disposes result payload BEFORE the last frame
                    // permit. Successful ready-uncollected replies own their lease.
                    drop(response.send(envelope));
                    drop(budget);
                    drop(root);
                    drop(lease);
                }
                #[cfg(test)]
                {
                    worker_hooks
                        .logical_done
                        .store(true, std::sync::atomic::Ordering::Release);
                    if let Some((entered, release)) = worker_hooks.exit.lock().unwrap().take() {
                        let _ = entered.send(());
                        let _ = release.recv();
                    }
                    assert!(
                        !worker_hooks
                            .panic_exit
                            .load(std::sync::atomic::Ordering::Acquire),
                        "Controlled native inventory teardown fault"
                    );
                }
            })
            .map_err(|_| {
                Error::new(
                    "ai_local_worker",
                    "Unable to start local inventory filesystem owner",
                )
            })?;
        Ok(Self {
            inner: Arc::new(Inner {
                root: root.0,
                limits,
                state,
                slots: Arc::new(Semaphore::new(capacity)),
                observers: Arc::new(Semaphore::new(OBSERVERS)),
                sender: Mutex::new(Some(sender)),
                worker: Mutex::new(Some(worker)),
                #[cfg(test)]
                hooks,
            }),
        })
    }
    #[cfg(test)]
    pub(crate) fn test_hooks(&self) -> &Hooks {
        &self.inner.hooks
    }
    pub fn try_submit(&self, mut control: Control) -> Result<Ticket> {
        if let Some(e) = self.inner.state.failure() {
            return Err(e);
        }
        if self.inner.state.cancel.is_cancelled() {
            return Err(Error::closed());
        }
        control.deadline = control
            .deadline
            .min(Instant::now() + self.inner.limits.job_timeout);
        let control = Control {
            cancel: control.cancel.child_token(),
            deadline: control.deadline,
        };
        let budget = Budget {
            limits: self.inner.limits.clone(),
            control: control.clone(),
            service_cancel: self.inner.state.cancel.clone(),
            directories: 0,
            entries: 0,
            steps: 0,
            path_bytes: 0,
        };
        budget.check()?;
        let permit = self.inner.slots.clone().try_acquire_owned().map_err(|_| {
            Error::new(
                "ai_local_busy",
                "Local inventory whole-request capacity exhausted",
            )
        })?;
        let lease = Arc::new(Lease {
            _owner: self.inner.clone(),
            _permit: permit,
        });
        let (response, receive) = oneshot::channel();
        let sender = self.inner.sender.lock().unwrap_or_else(|e| e.into_inner());
        budget.check()?;
        sender
            .as_ref()
            .ok_or_else(Error::closed)?
            .try_send(Work {
                root: self.inner.root.clone(),
                budget,
                response,
                lease: lease.clone(),
            })
            .map_err(|_| self.inner.state.failure().unwrap_or_else(Error::closed))?;
        Ok(Ticket {
            response: Some(receive),
            control,
            inner: self.inner.clone(),
            _lease: lease,
            armed: true,
        })
    }
    pub fn status(&self) -> Status {
        let worker = self.inner.worker.lock().unwrap_or_else(|e| e.into_inner());
        Status {
            closing: self.inner.state.cancel.is_cancelled(),
            retained: self.inner.limits.capacity - self.inner.slots.available_permits(),
            capacity: self.inner.limits.capacity,
            worker_running: worker.as_ref().is_some_and(|w| !w.is_finished()),
            worker_joined: worker.is_none(),
            shutdown_observers: OBSERVERS - self.inner.observers.available_permits(),
            failure: self.inner.state.failure().map(|e| ErrorStatus {
                code: e.code,
                message: e.message,
            }),
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.close();
        let _observer = self
            .inner
            .observers
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                Error::new(
                    "ai_local_busy",
                    "Local inventory shutdown observer capacity exhausted",
                )
            })?;
        let deadline = Instant::now() + self.inner.limits.shutdown_timeout;
        loop {
            let joined = self.inner.join_finished();
            if joined && self.inner.slots.available_permits() == self.inner.limits.capacity {
                return self.inner.state.failure().map_or(Ok(()), Err);
            }
            if Instant::now() >= deadline {
                return Err(Error::new("ai_local_shutdown_timeout","Local inventory work/results/native thread still owned; shutdown can be retried"));
            }
            if tokio::runtime::Handle::try_current().is_err() {
                return Err(Error::new(
                    "ai_local_runtime",
                    "Pending inventory teardown requires Tokio runtime; ownership retained",
                ));
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
}
