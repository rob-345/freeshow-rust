//! Bounded retained-canvas requests; final cache effects stay on the IO owner.
use crate::assets::NativeAssets;
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::LibraryService,
    Error, WireError,
};
use freeshow_thumbnails::{
    CaptureCompletion, PreparedThumbnail, ThumbnailConfig, ThumbnailOutcome, ThumbnailRoots,
    ThumbnailService,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::{
    oneshot, Mutex as AsyncMutex, Notify, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore,
};
use tokio_util::sync::CancellationToken;

pub const COMMANDS: &[&str] = freeshow_thumbnails::COMMANDS;
pub type Publish<'a> = &'a (dyn Fn(Event) -> Result<(), String> + Send + Sync);
const MAX_REQUESTS: usize = 4;
const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;

struct PendingCapture {
    context: RuntimeContext,
    response: oneshot::Sender<Option<Vec<u8>>>,
}
#[derive(Default)]
struct Registry {
    closed: bool,
    active: HashMap<String, (RuntimeContext, CancellationToken)>,
    captures: HashMap<String, PendingCapture>,
    generations: HashMap<String, Weak<AsyncMutex<()>>>,
}
#[derive(Clone)]
pub struct NativeThumbnails {
    service: ThumbnailService,
    roots: ThumbnailRoots,
    registry: Arc<Mutex<Registry>>,
    slots: Arc<Semaphore>,
    changed: Arc<Notify>,
    shutdown: Arc<AsyncMutex<()>>,
}
struct Lease {
    id: String,
    context: RuntimeContext,
    cancel: CancellationToken,
    registry: Arc<Mutex<Registry>>,
    changed: Arc<Notify>,
    // Retain the source-cache lock through accepted IO-owner effects, including
    // when the dispatch future is dropped while an install is queued.
    _generation: Option<OwnedMutexGuard<()>>,
    _permit: OwnedSemaphorePermit,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.cancel.cancel();
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        registry.active.remove(&self.id);
        registry.captures.remove(&self.id);
        drop(registry);
        self.changed.notify_waiters();
    }
}
// Staging cleanup is enqueued before the whole-pipeline lease is released.
struct Job {
    prepared: PreparedThumbnail,
    lease: Lease,
}
fn failed(code: &'static str, message: impl Into<String>) -> WireError {
    WireError::new(code, message)
}
fn thumbnail_error(error: freeshow_thumbnails::ThumbnailError) -> WireError {
    failed(error.code, error.message)
}
fn context_request(context: &RuntimeContext) -> Request {
    Request {
        channel: "MAIN".into(),
        data: json!({"channel":"GET_THUMBNAIL"}),
        listener_id: None,
        context: Some(context.clone()),
    }
}
impl NativeThumbnails {
    pub fn new(cache_root: PathBuf) -> Result<Self, WireError> {
        Ok(Self {
            service: ThumbnailService::new(ThumbnailConfig::default()).map_err(thumbnail_error)?,
            roots: ThumbnailRoots { cache_root },
            registry: Arc::new(Mutex::new(Registry::default())),
            slots: Arc::new(Semaphore::new(MAX_REQUESTS)),
            changed: Arc::new(Notify::new()),
            shutdown: Arc::new(AsyncMutex::new(())),
        })
    }
    pub fn cache_root(&self) -> &std::path::Path {
        &self.roots.cache_root
    }
    fn begin(&self, context: &RuntimeContext) -> Result<Lease, WireError> {
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            failed(
                if self.slots.is_closed() {
                    "thumbnail_shutdown"
                } else {
                    "thumbnail_busy"
                },
                "Native thumbnail requests are closed or at capacity",
            )
        })?;
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| failed("thumbnail_worker", "Thumbnail registry unavailable"))?;
        if registry.closed {
            return Err(failed("thumbnail_shutdown", "Native thumbnails are closed"));
        }
        let id = format!("native-thumbnail-{}", uuid::Uuid::new_v4().simple());
        let cancel = CancellationToken::new();
        registry
            .active
            .insert(id.clone(), (context.clone(), cancel.clone()));
        Ok(Lease {
            id,
            context: context.clone(),
            cancel,
            registry: self.registry.clone(),
            changed: self.changed.clone(),
            _generation: None,
            _permit: permit,
        })
    }
    async fn coordinate(&self, request: &Request, lease: &mut Lease) -> Result<(), WireError> {
        let command = request.data["channel"].as_str().unwrap_or("");
        let key = match command {
            "GET_THUMBNAIL" => "input",
            "DOES_MEDIA_EXIST" => "path",
            _ => return Ok(()),
        };
        let Some(path) = request.data["data"][key].as_str() else {
            return Ok(()); // The preparation service reports malformed requests.
        };
        if path.len() > ThumbnailConfig::default().path_bytes || path.contains('\0') {
            return Err(failed(
                "thumbnail_resource_limit",
                "Thumbnail path length/NUL limit",
            ));
        }
        // The source hashes paths to cache names. Serialize all sizes and
        // invalidations for that hash, including distinct paths that collide.
        // Independent source hashes still run concurrently. Whole-request
        // admission bounds both these waiters and the weak-key registry to four.
        let key = freeshow_thumbnails::file_path_hash_code(path);
        let generation = {
            let mut registry = self
                .registry
                .lock()
                .map_err(|_| failed("thumbnail_worker", "Thumbnail registry unavailable"))?;
            registry
                .generations
                .retain(|_, value| value.strong_count() > 0);
            match registry.generations.get(&key).and_then(Weak::upgrade) {
                Some(value) => value,
                None => {
                    let value = Arc::new(AsyncMutex::new(()));
                    registry.generations.insert(key, Arc::downgrade(&value));
                    value
                }
            }
        };
        lease._generation = Some(tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(30), generation.lock_owned()) => {
                result.map_err(|_| failed("thumbnail_timeout", "Thumbnail generation wait exceeded 30 seconds"))?
            },
            _ = lease.cancel.cancelled() => return Err(failed("cancelled", "Thumbnail generation wait cancelled")),
        });
        if lease.cancel.is_cancelled() {
            return Err(failed("cancelled", "Thumbnail generation wait cancelled"));
        }
        Ok(())
    }
    pub fn context_changed(&self, current: &RuntimeContext) {
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (captured, cancel) in registry.active.values() {
            if captured != current {
                cancel.cancel();
            }
        }
        registry
            .captures
            .retain(|_, pending| &pending.context == current);
    }
    pub fn status(&self) -> Value {
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        json!({"accepting":!registry.closed,"requests":registry.active.len(),"captures":registry.captures.len(),"capacity":MAX_REQUESTS})
    }
    /// The private raw upload carries only a one-use native correlation ID.
    /// Neither a renderer path nor an ordinary SAVE_IMAGE is write authority.
    pub fn complete(
        &self,
        library: &LibraryService,
        window: &str,
        id: &str,
        bytes: &[u8],
        capture_failed: bool,
    ) -> Result<(), WireError> {
        if window != "main"
            || id.len() != 49
            || !id.starts_with("native-thumbnail-")
            || bytes.len() > MAX_PNG_BYTES
            || (capture_failed && !bytes.is_empty())
        {
            return Err(failed(
                "invalid_capture_completion",
                "Invalid native canvas upload",
            ));
        }
        let current = library.context().map_err(WireError::from)?;
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| failed("thumbnail_worker", "Thumbnail registry unavailable"))?;
        if registry.closed {
            return Err(failed("thumbnail_shutdown", "Native thumbnails are closed"));
        }
        let pending = registry.captures.get(id).ok_or_else(|| {
            failed(
                "invalid_capture_completion",
                "Canvas capture is absent or already completed",
            )
        })?;
        if pending.context != current {
            return Err(failed(
                "stale_library",
                "Canvas capture belongs to an obsolete library",
            ));
        }
        let pending = registry.captures.remove(id).unwrap();
        drop(registry);
        pending
            .response
            .send(if capture_failed {
                None
            } else {
                Some(bytes.to_vec())
            })
            .map_err(|_| failed("invalid_capture_completion", "Canvas request was cancelled"))
    }
    async fn effects(&self, library: &LibraryService, mut job: Job) -> Result<Job, WireError> {
        if job.prepared.effects.is_empty() {
            return Ok(job);
        }
        let root = self.roots.cache_root.clone();
        let request = context_request(&job.lease.context);
        let (send, receive) = oneshot::channel();
        library
            .commit(request, "main", move |_| {
                // Only the trusted app cache child can be created; the effects
                // executor anchors all ancestors and rejects symlinks/identity races.
                if !root.exists() {
                    match std::fs::create_dir(&root) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => {
                            return Err(Error::NativeOperation {
                                code: "thumbnail_cache_root",
                                message: error.to_string(),
                            })
                        }
                    }
                }
                let mut receipt =
                    freeshow_thumbnail_effects::apply(&root, &job.prepared).map_err(|error| {
                        Error::NativeOperation {
                            code: error.code,
                            message: error.to_string(),
                        }
                    })?;
                if job.prepared.outcome == ThumbnailOutcome::CapturePending {
                    receipt
                        .acknowledge_preparation(&mut job.prepared)
                        .map_err(|error| Error::NativeOperation {
                            code: error.code,
                            message: error.to_string(),
                        })?;
                }
                for diagnostic in receipt.diagnostics() {
                    eprintln!("thumbnail effects: {diagnostic}");
                }
                if receipt.diagnostics_omitted() > 0 {
                    eprintln!(
                        "thumbnail effects: {} diagnostics omitted",
                        receipt.diagnostics_omitted()
                    );
                }
                let _ = send.send(job);
                Ok(vec![])
            })
            .await
            .map_err(WireError::from)?;
        receive.await.map_err(|_| {
            failed(
                "thumbnail_worker",
                "Thumbnail effects lost their prepared plan",
            )
        })
    }
    pub async fn dispatch(
        &self,
        library: &LibraryService,
        assets: &NativeAssets,
        request: Request,
        window: &str,
        publish: Publish<'_>,
    ) -> Result<Vec<Event>, WireError> {
        if window != "main" {
            return Err(failed(
                "thumbnail_role",
                "Only the main window may request thumbnail captures",
            ));
        }
        library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        let current = request.context.as_ref().unwrap();
        let mut lease = self.begin(current)?;
        self.coordinate(&request, &mut lease).await?;
        library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        let command = request.data["channel"].as_str().unwrap_or("");
        let prepared = self
            .service
            .prepare(
                command,
                &request.data["data"],
                &self.roots,
                lease.cancel.clone(),
            )
            .await
            .map_err(thumbnail_error)?;
        let mut job = self.effects(library, Job { prepared, lease }).await?;
        if job.prepared.capture.is_some() {
            let grants = assets
                .thumbnail_grants(library, current, &job.prepared, true)
                .await?;
            let capture = job.prepared.capture.as_mut().unwrap();
            // The retained helper treats id as opaque and echoes it unchanged.
            // Unique native IDs isolate simultaneous requests for the same file.
            capture.id = job.lease.id.clone();
            let (send, receive) = oneshot::channel();
            {
                let mut registry = self
                    .registry
                    .lock()
                    .map_err(|_| failed("thumbnail_worker", "Thumbnail registry unavailable"))?;
                if registry.closed || job.lease.cancel.is_cancelled() {
                    return Err(failed("cancelled", "Canvas request cancelled"));
                }
                registry.captures.insert(
                    job.lease.id.clone(),
                    PendingCapture {
                        context: current.clone(),
                        response: send,
                    },
                );
            }
            let mut event = Event::reply("MAIN", "CAPTURE_CANVAS", json!(capture), None);
            event.context = Some(current.clone());
            event.native_assets = Some(grants);
            event.native_capture = Some(json!({"id":job.lease.id}));
            library
                .contextualize(&request, &mut [])
                .map_err(WireError::from)?;
            job.prepared
                .mark_capture_started()
                .map_err(thumbnail_error)?;
            publish(event).map_err(|error| failed("thumbnail_publication", error))?;
            let completion = tokio::select! {
                value = tokio::time::timeout(Duration::from_secs(15),receive) => match value {
                    Ok(Ok(value)) => value.map_or(Completion::Failed,Completion::Png),
                    Ok(Err(_)) => return Err(failed("cancelled","Canvas completion cancelled")),
                    Err(_) => Completion::TimedOut,
                },
                _ = job.lease.cancel.cancelled() => return Err(failed("cancelled","Canvas request cancelled")),
            };
            self.registry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .captures
                .remove(&job.lease.id);
            library
                .contextualize(&request, &mut [])
                .map_err(WireError::from)?;
            let completion = match &completion {
                Completion::Png(bytes) => CaptureCompletion::Png(bytes),
                Completion::Failed => CaptureCompletion::Failed,
                Completion::TimedOut => CaptureCompletion::TimedOut,
            };
            job.prepared = self
                .service
                .complete(job.prepared, &job.lease.id, completion)
                .await
                .map_err(thumbnail_error)?;
            job = self.effects(library, job).await?;
        }
        for diagnostic in &job.prepared.diagnostics {
            eprintln!("thumbnail: {}: {}", diagnostic.code, diagnostic.message);
        }
        let grants = assets
            .thumbnail_grants(library, current, &job.prepared, false)
            .await?;
        let mut event = Event::reply(
            "MAIN",
            command,
            job.prepared.reply.clone(),
            request.listener_id.clone(),
        );
        library
            .contextualize(&request, std::slice::from_mut(&mut event))
            .map_err(WireError::from)?;
        event.native_assets = Some(grants);
        Ok(vec![event])
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
            registry.captures.clear();
        }
        self.service.shutdown().await.map_err(thumbnail_error)?;
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
        // Plans released by host drain can enqueue staging cleanup after the
        // first service fence. Await it before native exit.
        self.service.shutdown().await.map_err(thumbnail_error)
    }
}
enum Completion {
    Png(Vec<u8>),
    Failed,
    TimedOut,
}
