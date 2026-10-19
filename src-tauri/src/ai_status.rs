//! Explicit Nemotron status on a lazy fixed filesystem owner. Only validated
//! stamp intents enter ordered library IO; the native root is process-global.
use freeshow_ai_status::{
    CancelToken, Checker, Control, Job, Limits, NemotronNoModel, Platform, Prepared, StatusRead,
    TrustedUserData,
};
use freeshow_core::{
    protocol::{Event, NativeLifetime, Request, RuntimeContext},
    service::LibraryService,
    WireError,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
static OWNER_ACTIVE: AtomicBool = AtomicBool::new(false);
type Failure = (&'static str, &'static str);
fn error(e: freeshow_ai_status::Error) -> WireError {
    WireError::new(e.code, e.message)
}
fn worker_error() -> WireError {
    WireError::new("ai_status_worker", "Native status filesystem owner failed")
}
fn check(control: &Control) -> Result<(), WireError> {
    if control.cancel.is_cancelled() {
        Err(WireError::new(
            "ai_status_cancelled",
            "Native status observer cancelled",
        ))
    } else if Instant::now() >= control.deadline {
        Err(WireError::new(
            "ai_status_deadline",
            "Native status observation deadline elapsed; accepted work remains owned",
        ))
    } else {
        Ok(())
    }
}
struct Shared {
    cancel: CancelToken,
    failure: Mutex<Option<Failure>>,
}
impl Shared {
    fn fail(&self) {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(("ai_status_worker", "Native status filesystem owner failed"));
        self.cancel.cancel();
    }
    fn failure(&self) -> Option<WireError> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|(code, message)| WireError::new(code, message))
    }
}
struct Envelope {
    result: freeshow_ai_status::Result<Prepared>,
    lease: Arc<OwnedSemaphorePermit>,
}
struct Work {
    job: Job,
    response: oneshot::Sender<Envelope>,
    lease: Arc<OwnedSemaphorePermit>,
}
struct OwnerInner {
    shared: Arc<Shared>,
    sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    observers: Arc<Semaphore>,
}
impl OwnerInner {
    fn close(&self) {
        self.shared.cancel.cancel();
        self.sender.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
    fn join_finished(&self) -> bool {
        let mut handle = self.thread.lock().unwrap_or_else(|e| e.into_inner());
        if handle.as_ref().is_some_and(|h| !h.is_finished()) {
            return false;
        }
        self.join_locked(&mut handle);
        true
    }
    fn join_locked(&self, handle: &mut Option<JoinHandle<()>>) {
        if let Some(handle) = handle.take() {
            if handle.join().is_err() {
                self.shared.fail();
            }
            OWNER_ACTIVE.store(false, Ordering::Release);
        }
    }
}
impl Drop for OwnerInner {
    fn drop(&mut self) {
        self.close();
        // Work/ready payloads never own OwnerInner, so this cannot run on the
        // owned worker. Emergency final Drop really joins, including entered IO.
        let mut handle = self.thread.lock().unwrap_or_else(|e| e.into_inner());
        self.join_locked(&mut handle);
    }
}
#[derive(Clone)]
struct FilesystemOwner(Arc<OwnerInner>);
impl FilesystemOwner {
    fn start(cancel: CancelToken, #[cfg(test)] hooks: Arc<Hooks>) -> Result<Self, WireError> {
        OWNER_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                WireError::new(
                    "ai_status_owner_busy",
                    "Prior native status owner still requires actual join",
                )
            })?;
        let shared = Arc::new(Shared {
            cancel,
            failure: Mutex::new(None),
        });
        let state = shared.clone();
        let (sender, receiver) = mpsc::sync_channel::<Work>(4);
        let worker = std::thread::Builder::new()
            .name("freeshow-ai-status".into())
            .spawn(move || {
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    while let Ok(work) = receiver.recv() {
                        let Work {
                            job,
                            response,
                            lease,
                        } = work;
                        // Retain the whole host lease outside catch_unwind and
                        // beyond send, including abandoned/error/ready payloads.
                        let frame_lease = lease.clone();
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            #[cfg(test)]
                            {
                                Hooks::gate(&hooks.processing);
                                assert!(
                                    !hooks.panic.swap(false, Ordering::AcqRel),
                                    "Controlled actual status worker panic"
                                );
                            }
                            job.prepare()
                        }))
                        .unwrap_or_else(|_| {
                            state.fail();
                            Err(freeshow_ai_status::Error {
                                code: "ai_status_worker",
                                message: "Native status filesystem owner failed",
                            })
                        });
                        let _ = response.send(Envelope { result, lease });
                        #[cfg(test)]
                        Hooks::gate(&hooks.after_result);
                        drop(frame_lease);
                    }
                    #[cfg(test)]
                    {
                        Hooks::gate(&hooks.exit);
                        assert!(
                            !hooks.panic_exit.swap(false, Ordering::AcqRel),
                            "Controlled actual status exit panic"
                        );
                    }
                }));
                if run.is_err() {
                    state.fail();
                }
            })
            .map_err(|_| {
                OWNER_ACTIVE.store(false, Ordering::Release);
                WireError::new(
                    "ai_status_worker",
                    "Unable to start native status filesystem owner",
                )
            })?;
        Ok(Self(Arc::new(OwnerInner {
            shared,
            sender: Mutex::new(Some(sender)),
            thread: Mutex::new(Some(worker)),
            observers: Arc::new(Semaphore::new(4)),
        })))
    }
    fn submit(&self, work: Work) -> Result<(), WireError> {
        let sender = self.0.sender.lock().unwrap_or_else(|e| e.into_inner());
        if self.0.shared.cancel.is_cancelled() {
            return Err(self.0.shared.failure().unwrap_or_else(|| {
                WireError::new("ai_status_shutdown", "Native status admission is closed")
            }));
        }
        sender
            .as_ref()
            .ok_or_else(worker_error)?
            .try_send(work)
            .map_err(|_| worker_error())
    }
    async fn shutdown(&self) -> Result<(), WireError> {
        self.0.close();
        let _observer = self.0.observers.clone().try_acquire_owned().map_err(|_| {
            WireError::new(
                "ai_status_observer_busy",
                "Native status shutdown observers exhausted",
            )
        })?;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !self.0.join_finished() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .map_err(|_| {
            WireError::new(
                "ai_status_shutdown_timeout",
                "Native status thread join remains owned",
            )
        })?;
        self.0.shared.failure().map_or(Ok(()), Err)
    }
}
#[derive(Default)]
struct Registry {
    closed: bool,
    next: u64,
    active: HashMap<u64, (RuntimeContext, CancelToken)>,
}
struct Host {
    checker: Result<Checker, freeshow_ai_status::Error>,
    registry: Arc<Mutex<Registry>>,
    slots: Arc<Semaphore>,
    cancel: CancelToken,
    owner: Mutex<Option<FilesystemOwner>>,
    #[cfg(test)]
    hooks: Arc<Hooks>,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(owner) = self
            .owner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            owner.0.close();
        }
    }
}
#[derive(Clone)]
pub struct NativeStatus(Arc<Host>);
struct Caller {
    id: u64,
    control: Control,
    registry: Arc<Mutex<Registry>>,
}
impl Drop for Caller {
    fn drop(&mut self) {
        self.control.cancel.cancel();
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .remove(&self.id);
    }
}
struct Delivery {
    _read: StatusRead,
    _lease: Arc<OwnedSemaphorePermit>,
}
fn event(
    request: &Request,
    read: StatusRead,
    lease: Arc<OwnedSemaphorePermit>,
) -> Result<Event, WireError> {
    let value = serde_json::to_value(read.reply()).map_err(|_| {
        WireError::new("ai_status_encoding", "Unable to encode native model status")
    })?;
    if let Some(diagnostic) = read.write_diagnostic() {
        eprintln!("ai_status_stamp: {diagnostic:?}");
    }
    let mut event = Event::reply("MAIN", "AI_GET_STATUS", value, request.listener_id.clone());
    event.native_lifetime = Some(NativeLifetime::new(Delivery {
        _read: read,
        _lease: lease,
    }));
    Ok(event)
}
fn core_error(error: WireError) -> freeshow_core::Error {
    freeshow_core::Error::NativeOperation {
        code: error.code,
        message: error.message,
    }
}
impl NativeStatus {
    pub fn new(root: PathBuf) -> Self {
        let checker =
            TrustedUserData::from_host(root).and_then(|root| Checker::new(root, Limits::default()));
        Self(Arc::new(Host {
            checker,
            registry: Arc::new(Mutex::new(Registry::default())),
            slots: Arc::new(Semaphore::new(4)),
            cancel: CancelToken::new(),
            owner: Mutex::new(None),
            #[cfg(test)]
            hooks: Arc::new(Hooks::default()),
        }))
    }
    fn begin(
        &self,
        context: RuntimeContext,
        query: NemotronNoModel,
    ) -> Result<(oneshot::Receiver<Envelope>, Caller), WireError> {
        let permit = self.0.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new(
                if self.0.slots.is_closed() {
                    "ai_status_shutdown"
                } else {
                    "ai_status_busy"
                },
                "Native status whole work/result capacity unavailable",
            )
        })?;
        let mut registry = self.0.registry.lock().unwrap_or_else(|e| e.into_inner());
        if registry.closed {
            return Err(WireError::new(
                "ai_status_shutdown",
                "Native status admission is closed",
            ));
        }
        let checker = self.0.checker.as_ref().map_err(|e| error(e.clone()))?;
        let control = Control {
            cancel: self.0.cancel.child_token(),
            ..Control::default()
        };
        let job = checker
            .try_begin(query, Platform::native(), control.clone())
            .map_err(error)?;
        let id = registry.next;
        let next = id.checked_add(1).ok_or_else(|| {
            WireError::new("ai_status_worker", "Native status sequence exhausted")
        })?;
        let mut owner = self.0.owner.lock().unwrap_or_else(|e| e.into_inner());
        if owner.is_none() {
            *owner = Some(FilesystemOwner::start(
                self.0.cancel.clone(),
                #[cfg(test)]
                self.0.hooks.clone(),
            )?);
        }
        let (response, receiver) = oneshot::channel();
        owner.as_ref().unwrap().submit(Work {
            job,
            response,
            lease: Arc::new(permit),
        })?;
        registry.next = next;
        registry
            .active
            .insert(id, (context, control.cancel.clone()));
        Ok((
            receiver,
            Caller {
                id,
                control,
                registry: self.0.registry.clone(),
            },
        ))
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn test_snapshot(&self) -> (usize, usize, bool, bool) {
        let retained = 4 - self.0.slots.available_permits();
        let registered = self.0.registry.lock().unwrap().active.len();
        let owner = self.0.owner.lock().unwrap();
        let (started, joined) = owner
            .as_ref()
            .map(|owner| (true, owner.0.thread.lock().unwrap().is_none()))
            .unwrap_or((false, false));
        (retained, registered, started, joined)
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn test_hold(&self, phase: &str) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let slot = match phase {
            "processing" => &self.0.hooks.processing,
            "result" => &self.0.hooks.after_result,
            "exit" => &self.0.hooks.exit,
            _ => panic!("Unknown native test phase"),
        };
        let (entered, observed) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let mut target = slot.lock().unwrap();
        assert!(target.is_none());
        *target = Some(Gate {
            entered,
            release: held,
        });
        (observed, release)
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn test_panic_next(&self) {
        self.0.hooks.panic.store(true, Ordering::Release);
    }
    pub fn seal(&self) {
        {
            let mut registry = self.0.registry.lock().unwrap_or_else(|e| e.into_inner());
            registry.closed = true;
            self.0.slots.close();
            for (_, cancel) in registry.active.values() {
                cancel.cancel();
            }
        }
        self.0.cancel.cancel();
        if let Some(owner) = self
            .0
            .owner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            owner.0.close();
        }
    }
    pub fn context_changed(&self, current: &RuntimeContext) {
        for (context, cancel) in self
            .0
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .values()
        {
            if context != current {
                cancel.cancel();
            }
        }
    }
    pub async fn shutdown(&self) -> Result<(), WireError> {
        self.seal();
        let owner = self
            .0
            .owner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match owner {
            Some(owner) => owner.shutdown().await,
            None => Ok(()),
        }
    }
    pub async fn dispatch(
        &self,
        library: &LibraryService,
        mut request: Request,
        window: &str,
    ) -> Result<Vec<Event>, WireError> {
        if window != "main" {
            return Err(WireError::new(
                "ai_status_role",
                "Only the main window may request local model status",
            ));
        }
        if request.channel != "MAIN" || request.data["channel"] != "AI_GET_STATUS" {
            return Err(WireError::new(
                "unsupported_command",
                "Unknown model status command",
            ));
        }
        library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        if request
            .listener_id
            .as_ref()
            .is_some_and(|id| id.len() > 8192)
        {
            return Err(WireError::new(
                "ai_status_limit",
                "Native model status listener exceeds bounds",
            ));
        }
        let query = NemotronNoModel::from_payload(&request.data["data"]).map_err(error)?;
        request.data = serde_json::json!({"channel":"AI_GET_STATUS"});
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(WireError::new(
                "ai_status_runtime",
                "Native model status observation requires Tokio",
            ));
        }
        let (response, caller) = self.begin(request.context.clone().unwrap(), query)?;
        let control = caller.control.clone();
        let envelope=tokio::time::timeout_at(control.deadline.into(),async {
            tokio::select! {result=response=>result.map_err(|_|worker_error()),_=control.cancel.cancelled()=>Err(WireError::new("ai_status_cancelled","Native status observer cancelled"))}
        }).await.map_err(|_|WireError::new("ai_status_deadline","Native status observation deadline elapsed; work remains owned"))??;
        check(&control)?;
        let Envelope { result, lease } = envelope;
        let mut events = match result.map_err(error)? {
            Prepared::Ready(read) => vec![event(&request, read, lease)?],
            Prepared::Stamp(intent) => {
                let reply_request = Request {
                    channel: request.channel.clone(),
                    data: request.data.clone(),
                    listener_id: request.listener_id.clone(),
                    context: request.context.clone(),
                };
                let commit_control = control.clone();
                let event_request = Request {
                    channel: request.channel.clone(),
                    data: request.data.clone(),
                    listener_id: request.listener_id.clone(),
                    context: request.context.clone(),
                };
                library
                    .commit_without_media_refresh(reply_request, window, move |runtime| {
                        check(&commit_control).map_err(core_error)?;
                        // Opaque intent root must still equal the trusted native
                        // app-data authority; renderer/library paths never select it.
                        if intent.user_data() != runtime.stores.app_data {
                            return Err(freeshow_core::Error::NativeOperation {
                                code: "ai_status_owner",
                                message: "Native model stamp authority changed".into(),
                            });
                        }
                        let read = intent.apply().map_err(|e| core_error(error(e)))?;
                        check(&commit_control).map_err(core_error)?;
                        Ok(vec![event(&event_request, read, lease).map_err(core_error)?])
                    })
                    .await
                    .map_err(WireError::from)?
            }
        };
        check(&control)?;
        library
            .contextualize(&request, &mut events)
            .map_err(WireError::from)?;
        Ok(events)
    }
}
#[cfg(test)]
#[derive(Default)]
struct Hooks {
    processing: Mutex<Option<Gate>>,
    after_result: Mutex<Option<Gate>>,
    exit: Mutex<Option<Gate>>,
    panic: AtomicBool,
    panic_exit: AtomicBool,
}
#[cfg(test)]
struct Gate {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}
#[cfg(test)]
impl Hooks {
    fn gate(slot: &Mutex<Option<Gate>>) {
        let gate = slot.lock().unwrap().take();
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
            let _ = gate.release.recv();
        }
    }
}
