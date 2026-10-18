//! Fixed-worker media preparation followed by current-context ordered effects.
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
    show::truthy,
    Error, WireError,
};
use freeshow_media_copy::{
    CancellationToken, CopyConfig, CopyContext, CopyError, MediaCopyService, MediaSyncRoot,
    StoreSnapshot,
};
use freeshow_media_copy_effects::{OpenFolderIntent, TrustedCopyRoots};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{oneshot, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore};

pub const COMMANDS: &[&str] = &["BUNDLE_MEDIA_FILES", "MEDIA_FOLDER_COPY"];
const CAPACITY: usize = 2;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(20);
pub type Publish = Arc<dyn Fn(Event) -> Result<(), String> + Send + Sync>;
/// Trusted platform folder launch, called on the ordered filesystem owner.
/// No renderer-supplied callback or command line is accepted.
pub type OpenFolder = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;
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
    service: MediaCopyService,
    slots: Arc<Semaphore>,
    registry: Arc<Mutex<Registry>>,
    shutdown: AsyncMutex<()>,
    // A batch's absent-directory guards cover the whole destination tree.
    // Later batches must prepare after prior effects finish so our own mkdir
    // cannot invalidate their snapshots. At most two admitted callers wait;
    // unrelated library reads and other services remain independent.
    batches: AsyncMutex<()>,
    publish: Publish,
    open: OpenFolder,
}
#[derive(Clone)]
pub struct NativeMediaCopy {
    inner: Arc<Inner>,
}
struct Capture {
    context: CopyContext,
    root: PathBuf,
    configured: Value,
    uses_sync: bool,
    stores: Vec<(&'static str, Vec<u8>)>,
}
fn native_error(error: CopyError) -> WireError {
    for diagnostic in &error.diagnostics {
        eprintln!(
            "Media-copy {} (OS {:?})",
            diagnostic.code, diagnostic.os_code
        );
    }
    WireError::new(error.code, error.message)
}
fn operation(code: &'static str, message: impl Into<String>) -> Error {
    Error::NativeOperation {
        code,
        message: message.into(),
    }
}
fn commit_request(request: &Request) -> Request {
    Request {
        channel: request.channel.clone(),
        data: json!({"channel":request.data["channel"]}),
        listener_id: request.listener_id.clone(),
        context: request.context.clone(),
    }
}
impl NativeMediaCopy {
    pub fn new(
        library: LibraryService,
        publish: Publish,
        open: OpenFolder,
    ) -> Result<Self, WireError> {
        Ok(Self {
            inner: Arc::new(Inner {
                library,
                publish,
                open,
                service: MediaCopyService::new(CopyConfig::default()).map_err(native_error)?,
                slots: Arc::new(Semaphore::new(CAPACITY)),
                registry: Arc::new(Mutex::new(Registry::default())),
                shutdown: AsyncMutex::new(()),
                batches: AsyncMutex::new(()),
            }),
        })
    }
    fn failure(&self, code: &'static str, message: String) {
        let mut registry = self
            .inner
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        registry.closed = true;
        registry.failure.get_or_insert((code, message));
    }
    fn begin(&self, context: &RuntimeContext) -> Result<Arc<Lease>, WireError> {
        let mut registry = self
            .inner
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if registry.closed {
            return Err(WireError::new(
                "media_copy_shutdown",
                "Media-copy admission is closed",
            ));
        }
        if self.inner.service.cleanup_failed() {
            registry.closed = true;
            registry.failure.get_or_insert((
                "cleanup_failure",
                "Media-copy staging cleanup failed".into(),
            ));
            return Err(WireError::new(
                "cleanup_failure",
                "Media-copy staging cleanup failed",
            ));
        }
        let permit = self.inner.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new("media_copy_busy", "Media-copy requests are at capacity")
        })?;
        let id = registry.next;
        registry.next = id.checked_add(1).ok_or_else(|| {
            WireError::new("copy_worker_error", "Media-copy request IDs exhausted")
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
        json!({"accepting":!registry.closed,"requests":registry.active.len(),"capacity":CAPACITY,"cleanupFailed":self.inner.service.cleanup_failed(),"failure":registry.failure.as_ref().map(|(code,message)|json!({"code":code,"message":message}))})
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
                "copy_cancelled",
                "Media-copy request was cancelled",
            ));
        }
        Ok(())
    }
    pub async fn dispatch(&self, request: Request) -> Result<Vec<Event>, WireError> {
        if request.channel != "MAIN"
            || !COMMANDS.contains(&request.data["channel"].as_str().unwrap_or(""))
        {
            return Err(WireError::new(
                "unsupported_command",
                "Unknown native media-copy command",
            ));
        }
        if !within_limit(&request.data, 1024 * 1024)
            || request
                .listener_id
                .as_ref()
                .is_some_and(|id| id.len() > 256)
        {
            return Err(WireError::new(
                "copy_resource_limit",
                "Media-copy IPC exceeds its byte limit",
            ));
        }
        self.inner
            .library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        let lease = self.begin(request.context.as_ref().unwrap())?;
        let native = self.clone();
        let (send, receive) = oneshot::channel();
        // The admitted operation owns accepted writes/ALERTs after caller Drop.
        tokio::spawn(async move {
            let retained = lease.clone();
            let result = native.run(request, lease).await;
            if let Err(error) = &result {
                if matches!(error.code, "cleanup_failure" | "copy_worker_error") {
                    native.failure(error.code, error.message.clone());
                }
            }
            if let Err((Err(error), _lease)) = send.send((result, retained)) {
                eprintln!("Detached media-copy request failed: {}", error.message);
            }
        });
        let (result, _lease) = receive
            .await
            .map_err(|_| WireError::new("copy_worker_error", "Media-copy host owner stopped"))?;
        result
    }
    async fn capture(&self, request: &Request, lease: Arc<Lease>) -> Result<Capture, WireError> {
        if request.data["channel"] == "MEDIA_FOLDER_COPY" {
            // This committed native snapshot performs no filesystem work and
            // stays available while a private backup compresses on the owner.
            // The final closure revalidates both context and configuration.
            let snapshot = self
                .inner
                .library
                .media_settings(request)
                .map_err(WireError::from)?;
            let configured = snapshot.configured_media_folder;
            if !within_limit(&configured, 1024 * 1024) {
                return Err(WireError::new(
                    "copy_resource_limit",
                    "Media folder setting exceeds its byte limit",
                ));
            }
            let sync = sync_root(&configured).map_err(WireError::from)?;
            let context = CopyContext::new(
                snapshot.context.data_path,
                sync,
                None,
                Arc::new(StoreSnapshot::new(json!({}), json!({}), json!({}))),
            )
            .map_err(native_error)?;
            return Ok(Capture {
                root: context.sync_root().to_path_buf(),
                context,
                configured,
                uses_sync: true,
                stores: vec![],
            });
        }
        let (send, receive) = oneshot::channel();
        let command = request.data["channel"].as_str().unwrap().to_owned();
        let custom = truthy(&request.data["data"]["outputPath"]);
        self.inner
            .library
            .commit(commit_request(request), "main", move |runtime| {
                let _lease = lease;
                let mut remaining = 1024 * 1024;
                let configured =
                    read_store(runtime, "config", &mut remaining)?["mediaFolderPath"].clone();
                let sync = sync_root(&configured)?;
                let uses_sync = command == "MEDIA_FOLDER_COPY" || custom;
                let mut guards = vec![];
                let mut remaining = 4 * 1024 * 1024;
                let mut read = |id: &'static str| -> freeshow_core::Result<Value> {
                    let bytes = read_store_bytes(runtime, id, &mut remaining)?;
                    let value = parse_store(&bytes)?;
                    guards.push((id, bytes));
                    Ok(value)
                };
                let stores = if command == "BUNDLE_MEDIA_FILES" {
                    StoreSnapshot::new(read("PROJECTS")?, read("OVERLAYS")?, read("TEMPLATES")?)
                } else {
                    StoreSnapshot::new(json!({}), json!({}), json!({}))
                };
                let stores = Arc::new(stores);
                let mut context =
                    CopyContext::new(&runtime.stores.data, sync.clone(), None, stores.clone())
                        .map_err(|e| operation(e.code, e.message))?;
                let root = if uses_sync {
                    context.sync_root()
                } else {
                    context.default_media_root()
                }
                .to_path_buf();
                // The retained custom bundle destination is the already committed
                // native media setting. The payload itself cannot authorize a root.
                if command == "BUNDLE_MEDIA_FILES" && custom {
                    context =
                        CopyContext::new(&runtime.stores.data, sync, Some(root.clone()), stores)
                            .map_err(|e| operation(e.code, e.message))?;
                }
                send.send(Capture {
                    context,
                    root,
                    configured,
                    uses_sync,
                    stores: guards,
                })
                .map_err(|_| {
                    operation("copy_worker_error", "Media-copy snapshot receiver stopped")
                })?;
                Ok(vec![])
            })
            .await
            .map_err(WireError::from)?;
        receive
            .await
            .map_err(|_| WireError::new("copy_worker_error", "Media-copy snapshot owner stopped"))
    }
    async fn run(&self, request: Request, lease: Arc<Lease>) -> Result<Vec<Event>, WireError> {
        let result = self.run_owned(request, lease.clone()).await;
        // Every exit, including validation/preparation failure, fences prior
        // cleanup messages before releasing whole-request admission.
        let cleanup = self
            .inner
            .service
            .flush_released()
            .await
            .map_err(native_error);
        drop(lease);
        cleanup?;
        result
    }
    async fn run_owned(
        &self,
        request: Request,
        lease: Arc<Lease>,
    ) -> Result<Vec<Event>, WireError> {
        self.validate(&request, &lease)?;
        let captured = self.capture(&request, lease.clone()).await?;
        let _batch = self.inner.batches.lock().await;
        self.validate(&request, &lease)?;
        let command = request.data["channel"].as_str().unwrap();
        let prepared = self
            .inner
            .service
            .prepare_with_cancel(
                command,
                &request.data["data"],
                &captured.context,
                lease.cancel.clone(),
            )
            .await
            .map_err(native_error)?;
        self.validate(&request, &lease)?;
        // Pin BEFORE queued commit admission. Caller cancellation/TTL cannot
        // release staging while its accepted ordered closure still owns it.
        let pin = prepared.pin_for_commit().map_err(native_error)?;
        let publish = self.inner.publish.clone();
        let open = self.inner.open.clone();
        let owned = lease.clone();
        let listener = request.listener_id.clone();
        let result = self
            .inner
            .library
            .commit(commit_request(&request), "main", move |runtime| {
                // Field/local lifetime order keeps the plan/pin owned until apply
                // returns; the whole lease remains alive through this closure.
                let _lease = owned;
                let _pin = pin;
                let prepared = prepared;
                if runtime.stores.data.as_os_str() != prepared.captured_data_root().as_os_str() {
                    return Err(operation(
                        "copy_context_changed",
                        "Media-copy library root changed",
                    ));
                }
                if captured.uses_sync {
                    let mut remaining = 1024 * 1024;
                    let configured =
                        read_store(runtime, "config", &mut remaining)?["mediaFolderPath"].clone();
                    if configured != captured.configured {
                        return Err(operation(
                            "copy_context_changed",
                            "Media-copy setting changed before installation",
                        ));
                    }
                }
                let mut remaining = 4 * 1024 * 1024;
                for (id, bytes) in &captured.stores {
                    if &read_store_bytes(runtime, id, &mut remaining)? != bytes {
                        return Err(operation(
                            "copy_input_changed",
                            "Media-copy store snapshot changed before installation",
                        ));
                    }
                }
                let result = freeshow_media_copy_effects::apply(
                    TrustedCopyRoots {
                        data_root: &runtime.stores.data,
                        media_root: &captured.root,
                    },
                    &prepared,
                    &_pin,
                    &CancellationToken::new(),
                );
                let (alerts, open_folder) = match &result {
                    Ok(receipt) => (receipt.alerts(), receipt.open_folder()),
                    Err(error) => (
                        error.observed.alerts.as_slice(),
                        error.observed.open_folder.as_ref(),
                    ),
                };
                let context = runtime.context();
                let mut publication_error = None;
                let mut alert = |text: &str| {
                    let mut event = Event::reply("MAIN", "ALERT", json!(text), None);
                    event.context = Some(context.clone());
                    if let Err(error) = publish(event) {
                        publication_error.get_or_insert(error);
                    }
                };
                for text in alerts {
                    alert(text);
                }
                match open_folder {
                    // The receipt already includes the source's missing-path
                    // ALERT; the intent must not publish it a second time.
                    Some(OpenFolderIntent::Missing { .. }) => {}
                    Some(OpenFolderIntent::OpenDirectory { path }) => {
                        if let Err(error) = open(path) {
                            eprintln!("Could not open bundled-media folder: {error}");
                        }
                    }
                    None => {}
                }
                if let Some(error) = publication_error {
                    return Err(operation("copy_publication", error));
                }
                let receipt = result.map_err(|error| {
                    for message in &error.observed.diagnostics {
                        eprintln!("Media-copy cleanup: {message}");
                    }
                    operation(error.code, error.to_string())
                })?;
                for diagnostic in receipt.preparation_diagnostics() {
                    eprintln!(
                        "Media-copy {} (OS {:?})",
                        diagnostic.code, diagnostic.os_code
                    );
                }
                for diagnostic in receipt.diagnostics() {
                    eprintln!("Media-copy: {diagnostic}");
                }
                // BUNDLE has no successful callback in the original source.
                Ok(receipt
                    .reply()
                    .map(|value| {
                        vec![Event::reply(
                            "MAIN",
                            "MEDIA_FOLDER_COPY",
                            json!(value),
                            listener,
                        )]
                    })
                    .unwrap_or_default())
            })
            .await
            .map_err(WireError::from);
        let mut events = result?;
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
                    "copy_host_shutdown_timeout",
                    "Media-copy shutdown owner did not drain",
                )
            })?;
        // Accepted pins must leave queued/active IO before preparation shutdown
        // asks its cleanup owner to stop. A timeout retains live ownership.
        let _drained = tokio::time::timeout(
            DRAIN_TIMEOUT,
            self.inner.slots.clone().acquire_many_owned(CAPACITY as u32),
        )
        .await
        .map_err(|_| {
            WireError::new(
                "copy_host_shutdown_timeout",
                "Media-copy host requests did not drain",
            )
        })?
        .map_err(|_| WireError::new("copy_worker_error", "Media-copy host drain stopped"))?;
        match tokio::time::timeout(DRAIN_TIMEOUT, self.inner.service.shutdown()).await {
            Err(_) => {
                return Err(WireError::new(
                    "copy_host_shutdown_timeout",
                    "Media-copy filesystem/cleanup workers did not drain",
                ))
            }
            Ok(Err(error)) => self.failure(error.code, error.message),
            Ok(Ok(())) => {}
        }
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
fn sync_root(configured: &Value) -> freeshow_core::Result<MediaSyncRoot> {
    if !truthy(configured) {
        return Ok(MediaSyncRoot::Default);
    }
    let path = configured.as_str().ok_or_else(|| {
        operation(
            "copy_unsupported_value",
            "Configured media folder is not a string",
        )
    })?;
    Ok(MediaSyncRoot::Configured(path.into()))
}
// Short, bounded store capture runs at a write boundary. CPU/file preparation
// then uses its immutable Arc snapshot outside that boundary.
fn read_store(runtime: &Runtime, id: &str, remaining: &mut usize) -> freeshow_core::Result<Value> {
    parse_store(&read_store_bytes(runtime, id, remaining)?)
}
fn parse_store(bytes: &[u8]) -> freeshow_core::Result<Value> {
    let value: Value = serde_json::from_slice(bytes)?;
    if !value.is_object() {
        return Err(operation(
            "copy_unsupported_value",
            "Media-copy store must be an object",
        ));
    }
    Ok(value)
}
fn read_store_bytes(
    runtime: &Runtime,
    id: &str,
    remaining: &mut usize,
) -> freeshow_core::Result<Vec<u8>> {
    let path = runtime.stores.path(id)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(b"{}".to_vec()),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(operation(
            "copy_unsupported_value",
            "Media-copy store is not a regular file",
        ));
    }
    if metadata.len() > *remaining as u64 {
        return Err(operation(
            "copy_resource_limit",
            "Media-copy store snapshot exceeds its aggregate byte bound",
        ));
    }
    let mut bytes = Vec::new();
    file.take((*remaining as u64) + 1).read_to_end(&mut bytes)?;
    if bytes.len() > *remaining {
        return Err(operation(
            "copy_resource_limit",
            "Media-copy store grew beyond its snapshot bound",
        ));
    }
    *remaining -= bytes.len();
    Ok(bytes)
}
fn within_limit(value: &Value, limit: usize) -> bool {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("Media-copy IPC byte bound"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(limit), value).is_ok()
}
