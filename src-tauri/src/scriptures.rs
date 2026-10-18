//! Owned, bounded local Bible loading with ordered source directory attempts.
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::LibraryService,
    Error, WireError,
};
use freeshow_scriptures::{
    CancellationToken, FolderIntent, Limits, ScriptureContext, ScriptureService,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{oneshot, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore};

pub const COMMANDS: &[&str] = &["BIBLE"];
const CAPACITY: usize = 2;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(15);
pub type Publish = Arc<dyn Fn(Event) -> Result<(), String> + Send + Sync>;
#[derive(Default)]
struct Registry {
    closed: bool,
    next: u64,
    active: BTreeMap<u64, (RuntimeContext, CancellationToken)>,
    failure: Option<(&'static str, String)>,
}
struct Lease {
    id: u64,
    cancel: CancellationToken,
    registry: Arc<Mutex<Registry>>,
    _permit: OwnedSemaphorePermit,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .remove(&self.id);
    }
}
struct Inner {
    library: LibraryService,
    service: ScriptureService,
    documents: PathBuf,
    registry: Arc<Mutex<Registry>>,
    slots: Arc<Semaphore>,
    shutdown: AsyncMutex<()>,
    publish: Publish,
}
#[derive(Clone)]
pub struct NativeScriptures {
    inner: Arc<Inner>,
}
fn source_error(error: freeshow_scriptures::Error) -> WireError {
    WireError::new(error.code, error.message)
}
fn closed() -> WireError {
    WireError::new(
        "scriptures_shutdown",
        "Native scripture admission is closed",
    )
}
impl NativeScriptures {
    pub fn new(
        library: LibraryService,
        documents: PathBuf,
        publish: Publish,
    ) -> Result<Self, WireError> {
        ScriptureContext::new(
            &library.context().map_err(WireError::from)?.data_path,
            &documents,
        )
        .map_err(source_error)?;
        Ok(Self {
            inner: Arc::new(Inner {
                library,
                documents,
                publish,
                service: ScriptureService::new(Limits::default()).map_err(source_error)?,
                registry: Arc::new(Mutex::new(Registry::default())),
                slots: Arc::new(Semaphore::new(CAPACITY)),
                shutdown: AsyncMutex::new(()),
            }),
        })
    }
    fn begin(&self, context: &RuntimeContext) -> Result<Arc<Lease>, WireError> {
        let mut registry = self
            .inner
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if registry.closed {
            return Err(closed());
        }
        let permit = self.inner.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new(
                "scriptures_busy",
                "Native scripture requests are at capacity",
            )
        })?;
        let id = registry.next;
        registry.next = id.checked_add(1).ok_or_else(|| {
            WireError::new(
                "scriptures_worker",
                "Native scripture request IDs exhausted",
            )
        })?;
        let cancel = CancellationToken::new();
        registry
            .active
            .insert(id, (context.clone(), cancel.clone()));
        Ok(Arc::new(Lease {
            id,
            cancel,
            registry: self.inner.registry.clone(),
            _permit: permit,
        }))
    }
    pub fn status(&self) -> Value {
        let registry = self
            .inner
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        json!({"accepting":!registry.closed,"requests":registry.active.len(),"capacity":CAPACITY,"service":self.inner.service.status(),
            "failure":registry.failure.as_ref().map(|(code,message)|json!({"code":code,"message":message}))})
    }
    pub fn context_changed(&self, context: &RuntimeContext) {
        let registry = self
            .inner
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (captured, cancel) in registry.active.values() {
            if captured != context {
                cancel.cancel();
            }
        }
    }
    fn validate(&self, request: &Request, lease: &Lease) -> Result<(), WireError> {
        self.inner
            .library
            .contextualize(request, &mut [])
            .map_err(WireError::from)?;
        if lease.cancel.is_cancelled() {
            return Err(WireError::new(
                "scripture_cancelled",
                "Native scripture request was cancelled",
            ));
        }
        Ok(())
    }
    pub async fn dispatch(&self, request: Request) -> Result<Vec<Event>, WireError> {
        if request.channel != "MAIN" || request.data["channel"] != "BIBLE" {
            return Err(WireError::new(
                "unsupported_command",
                "Unknown native scripture command",
            ));
        }
        if !within_limit(&request.data, 64 * 1024)
            || request
                .listener_id
                .as_ref()
                .is_some_and(|id| id.len() > 256)
        {
            return Err(WireError::new(
                "scripture_input_limit",
                "Native scripture input exceeds its byte limit",
            ));
        }
        self.inner
            .library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        let lease = self.begin(request.context.as_ref().unwrap())?;
        let native = self.clone();
        let (send, receive) = oneshot::channel();
        // Own the whole admitted continuation through any ordered directory
        // attempt. Caller cancellation cannot suppress its independent ALERT.
        tokio::spawn(async move {
            let result = native.run(request, lease).await;
            if let Err(Err(error)) = send.send(result) {
                eprintln!("Detached scripture request failed: {}", error.message);
            }
        });
        receive
            .await
            .map_err(|_| WireError::new("scriptures_worker", "Native scripture owner stopped"))?
    }
    async fn run(&self, request: Request, lease: Arc<Lease>) -> Result<Vec<Event>, WireError> {
        self.validate(&request, &lease)?;
        let current = request.context.as_ref().unwrap();
        let context = ScriptureContext::new(&current.data_path, &self.inner.documents)
            .map_err(source_error)?;
        let mut prepared = self
            .inner
            .service
            .load(&context, &request.data["data"], lease.cancel.clone())
            .await
            .map_err(source_error)?;
        self.validate(&request, &lease)?;
        if !prepared.is_complete() {
            if prepared.folder_intents.len() != 1
                || !matches!(&prepared.folder_intents[0],FolderIntent::EnsureBiblesDirectory{path} if path==context.bibles_root())
            {
                return Err(WireError::new(
                    "scripture_intent",
                    "Unexpected scripture directory preparation",
                ));
            }
            let path = context.bibles_root().to_path_buf();
            let publish = self.inner.publish.clone();
            let documents = self.inner.documents.clone();
            let owned = lease.clone();
            self.inner
                .library
                .commit(
                    Request {
                        channel: "MAIN".into(),
                        data: json!({"channel":"BIBLE"}),
                        listener_id: None,
                        context: Some(current.clone()),
                    },
                    "main",
                    move |runtime| {
                        let _lease = owned;
                        // The ordered owner validates both context and its own derived
                        // authority, rather than adopting a path from a preparation.
                        let authority = ScriptureContext::new(&runtime.stores.data, &documents)
                            .map_err(|error| Error::NativeOperation {
                                code: error.code,
                                message: error.message,
                            })?;
                        if authority.bibles_root() != path {
                            return Err(Error::Invalid(
                                "Scripture directory authority changed".into(),
                            ));
                        }
                        if let Err(error) = std::fs::create_dir_all(&path) {
                            eprintln!("Could not create scripture directory: {error}");
                            let mut alert = Event::reply(
                                "MAIN",
                                "ALERT",
                                json!(format!(
                                    "Error: Could not create folder at: {}!",
                                    path.display()
                                )),
                                None,
                            );
                            alert.context = Some(runtime.context());
                            publish(alert).map_err(|message| Error::NativeOperation {
                                code: "scripture_publication",
                                message,
                            })?;
                        }
                        Ok(vec![])
                    },
                )
                .await
                .map_err(WireError::from)?;
            self.validate(&request, &lease)?;
            // A failed source mkdir still proceeds to the genuine legacy read.
            // Preserve that acknowledgment. Each bounded parser preparation gets
            // its own budget; waiting for the ordered IO owner is not parser work.
            prepared = self
                .inner
                .service
                .load(
                    &context.after_directory_attempt(),
                    &request.data["data"],
                    lease.cancel.clone(),
                )
                .await
                .map_err(source_error)?;
        }
        if !prepared.is_complete() {
            return Err(WireError::new(
                "scripture_intent",
                "Scripture preparation did not complete",
            ));
        }
        for diagnostic in &prepared.diagnostics {
            eprintln!(
                "Scripture {} during {} (OS {:?})",
                diagnostic.code, diagnostic.operation, diagnostic.os_code
            );
        }
        self.validate(&request, &lease)?;
        let mut events = vec![Event::reply(
            "MAIN",
            "BIBLE",
            prepared.data.take().unwrap(),
            request.listener_id.clone(),
        )];
        self.inner
            .library
            .contextualize(&request, &mut events)
            .map_err(WireError::from)?;
        Ok(events)
    }
    pub async fn shutdown(&self) -> Result<(), WireError> {
        {
            let mut registry = self
                .inner
                .registry
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            registry.closed = true;
            for (_, cancel) in registry.active.values() {
                cancel.cancel();
            }
        }
        let _serial = tokio::time::timeout(DRAIN_TIMEOUT, self.inner.shutdown.lock())
            .await
            .map_err(|_| {
                WireError::new(
                    "scripture_host_shutdown_timeout",
                    "Scripture shutdown owner did not complete",
                )
            })?;
        let service = self.inner.service.shutdown().await;
        if let Err(error) = service {
            if error.code == "scripture_shutdown_timeout" {
                return Err(source_error(error));
            }
            let mut registry = self
                .inner
                .registry
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if registry.failure.is_none() {
                registry.failure = Some((error.code, error.message));
            }
        }
        let _drained = tokio::time::timeout(
            DRAIN_TIMEOUT,
            self.inner.slots.clone().acquire_many_owned(CAPACITY as u32),
        )
        .await
        .map_err(|_| {
            WireError::new(
                "scripture_host_shutdown_timeout",
                "Scripture host requests did not drain",
            )
        })?
        .map_err(|_| WireError::new("scriptures_worker", "Scripture host drain stopped"))?;
        let registry = self
            .inner
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match &registry.failure {
            Some((code, message)) => Err(WireError::new(code, message.clone())),
            None => Ok(()),
        }
    }
}
fn within_limit(value: &Value, limit: usize) -> bool {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("Scripture IPC byte limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(limit), value).is_ok()
}
