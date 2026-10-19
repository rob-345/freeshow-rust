//! Bounded document/EXIF reads; only Bible-root creation uses the library owner.
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::LibraryService,
    Error, WireError,
};
use freeshow_documents::{DocumentConfig, DocumentContext, DocumentService, FolderIntent};
use freeshow_exif::{Control, ExifService, Limits};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, Mutex as AsyncMutex, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

pub const COMMANDS: &[&str] = &["READ_FILE", "READ_BIBLES_FOLDER", "READ_EXIF"];
const MAX_REQUESTS: usize = 4;
pub type Publish = Arc<dyn Fn(Event) -> Result<(), String> + Send + Sync>;
#[derive(Default)]
struct Registry {
    closed: bool,
    next: u64,
    active: HashMap<u64, (RuntimeContext, CancellationToken)>,
}
#[derive(Clone)]
pub struct NativeFileReads {
    documents: DocumentService,
    exif: ExifService,
    registry: Arc<Mutex<Registry>>,
    slots: Arc<Semaphore>,
    changed: Arc<Notify>,
    shutdown: Arc<AsyncMutex<()>>,
    publish: Publish,
}
struct Lease {
    id: u64,
    cancel: CancellationToken,
    registry: Arc<Mutex<Registry>>,
    changed: Arc<Notify>,
    _permit: OwnedSemaphorePermit,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active
            .remove(&self.id);
        self.changed.notify_waiters();
    }
}
fn document_error(error: freeshow_documents::DocumentError) -> WireError {
    WireError::new(error.code, error.message)
}
fn exif_error(error: freeshow_exif::Error) -> WireError {
    let code = match error.code.as_str() {
        "exif_invalid" => "exif_invalid",
        "exif_limit" => "exif_limit",
        "exif_busy" => "exif_busy",
        "exif_cancelled" => "exif_cancelled",
        "exif_deadline" => "exif_deadline",
        "exif_shutdown" => "exif_shutdown",
        "exif_shutdown_timeout" => "exif_shutdown_timeout",
        _ => "exif_worker",
    };
    WireError::new(code, error.message)
}
impl NativeFileReads {
    pub fn new(publish: Publish) -> Result<Self, WireError> {
        Ok(Self {
            documents: DocumentService::new(DocumentConfig::default()).map_err(document_error)?,
            exif: ExifService::start(Limits::default()).map_err(exif_error)?,
            registry: Arc::new(Mutex::new(Registry::default())),
            slots: Arc::new(Semaphore::new(MAX_REQUESTS)),
            changed: Arc::new(Notify::new()),
            shutdown: Arc::new(AsyncMutex::new(())),
            publish,
        })
    }
    fn begin(&self, context: &RuntimeContext) -> Result<Lease, WireError> {
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new(
                if self.slots.is_closed() {
                    "file_reads_shutdown"
                } else {
                    "file_reads_busy"
                },
                "Native file reads are closed or at capacity",
            )
        })?;
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| WireError::new("file_reads_worker", "File read registry unavailable"))?;
        if registry.closed {
            return Err(WireError::new(
                "file_reads_shutdown",
                "Native file reads are closed",
            ));
        }
        let id = registry.next;
        registry.next = id.checked_add(1).ok_or_else(|| {
            WireError::new("file_reads_worker", "File read request IDs exhausted")
        })?;
        let cancel = CancellationToken::new();
        registry
            .active
            .insert(id, (context.clone(), cancel.clone()));
        Ok(Lease {
            id,
            cancel,
            registry: self.registry.clone(),
            changed: self.changed.clone(),
            _permit: permit,
        })
    }
    pub fn context_changed(&self, context: &RuntimeContext) {
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (captured, cancel) in registry.active.values() {
            if captured != context {
                cancel.cancel();
            }
        }
    }
    pub fn status(&self) -> Value {
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        json!({"accepting":!registry.closed,"requests":registry.active.len(),"capacity":MAX_REQUESTS})
    }
    pub async fn dispatch(
        &self,
        library: &LibraryService,
        request: Request,
    ) -> Result<Vec<Event>, WireError> {
        library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        let current = request.context.as_ref().unwrap();
        let mut lease = self.begin(current)?;
        let command = request.data["channel"].as_str().unwrap_or("");
        let mut event = if command == "READ_EXIF" {
            let id = request.data["data"]["id"]
                .as_str()
                .ok_or_else(|| WireError::new("exif_invalid", "EXIF id must be a path string"))?;
            let context = freeshow_exif::Context {
                runtime_id: current.runtime_id.clone(),
                library_generation: current.library_generation,
                data_path: current
                    .data_path
                    .to_str()
                    .ok_or_else(|| {
                        WireError::new("exif_invalid", "EXIF library path is not Unicode")
                    })?
                    .to_owned(),
            };
            let outcome = self
                .exif
                .read(
                    id.into(),
                    context,
                    Control {
                        deadline: Instant::now() + Duration::from_secs(10),
                        cancel: lease.cancel.clone(),
                    },
                )
                .await
                .map_err(exif_error)?;
            for diagnostic in &outcome.diagnostics {
                eprintln!("EXIF: {}: {}", diagnostic.code, diagnostic.message);
            }
            let mut event =
                Event::reply("MAIN", command, outcome.reply, request.listener_id.clone());
            event.native_exif = Some(
                serde_json::to_value(outcome.hydration)
                    .map_err(|error| WireError::new("exif_worker", error.to_string()))?,
            );
            event
        } else if freeshow_documents::COMMANDS.contains(&command) {
            let context = DocumentContext::new(&current.data_path).map_err(document_error)?;
            let mut prepared = self
                .documents
                .dispatch_with_cancel(
                    command,
                    &request.data["data"],
                    &context,
                    lease.cancel.clone(),
                )
                .await
                .map_err(document_error)?;
            if !prepared.is_complete() {
                if prepared.folder_intents.len() != 1
                    || !matches!(&prepared.folder_intents[0],FolderIntent::EnsureBiblesDirectory{path} if path==context.bibles_root())
                {
                    return Err(WireError::new(
                        "document_intent",
                        "Unexpected Bible folder preparation",
                    ));
                }
                let path = context.bibles_root().to_path_buf();
                let publish = self.publish.clone();
                let (send, receive) = oneshot::channel();
                // Accepted directory creation owns admission even if IPC drops.
                library
                    .commit(
                        Request {
                            channel: "MAIN".into(),
                            data: json!({"channel":command}),
                            listener_id: None,
                            context: Some(current.clone()),
                        },
                        "main",
                        move |runtime| {
                            let result = std::fs::create_dir_all(&path);
                            let ok = result.is_ok();
                            if let Err(error) = result {
                                eprintln!("Could not create Bible directory: {error}");
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
                                // Source makeDir publishes independently of its
                                // caller's IPC reply, including after caller drop.
                                publish(alert).map_err(|message| Error::NativeOperation {
                                    code: "document_publication",
                                    message,
                                })?;
                            }
                            let _ = send.send((lease, ok));
                            Ok(vec![])
                        },
                    )
                    .await
                    .map_err(WireError::from)?;
                let (retained, created) = receive.await.map_err(|_| {
                    WireError::new("file_reads_worker", "Lost Bible folder preparation")
                })?;
                lease = retained;
                if created {
                    prepared = self
                        .documents
                        .dispatch_with_cancel(
                            command,
                            &request.data["data"],
                            &context,
                            lease.cancel.clone(),
                        )
                        .await
                        .map_err(document_error)?;
                    if !prepared.is_complete() {
                        return Err(WireError::new(
                            "document_root_changed",
                            "Bible root changed after directory creation",
                        ));
                    }
                }
                // Failed source makeDir emits ALERT then falls through to [].
            }
            for diagnostic in &prepared.diagnostics {
                eprintln!("documents: {}: {}", diagnostic.code, diagnostic.operation);
            }
            Event::reply("MAIN", command, prepared.data, request.listener_id.clone())
        } else {
            return Err(WireError::new(
                "unsupported_command",
                "Unknown native file read",
            ));
        };
        if lease.cancel.is_cancelled() {
            return Err(WireError::new("cancelled", "Native file read cancelled"));
        }
        library
            .contextualize(&request, std::slice::from_mut(&mut event))
            .map_err(WireError::from)?;
        let mut events = vec![event];
        library
            .contextualize(&request, &mut events)
            .map_err(WireError::from)?;
        Ok(events)
    }
    pub async fn shutdown(&self) -> Result<(), WireError> {
        let _serial = self.shutdown.lock().await;
        {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            registry.closed = true;
            self.slots.close();
            for (_, cancel) in registry.active.values() {
                cancel.cancel();
            }
        }
        let (documents, exif) = tokio::join!(self.documents.shutdown(), self.exif.shutdown());
        documents.map_err(document_error)?;
        exif.map_err(exif_error)?;
        loop {
            let changed = self.changed.notified();
            if self
                .registry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .active
                .is_empty()
            {
                break;
            }
            changed.await;
        }
        Ok(())
    }
}
