//! Readonly filesystem queries have their own bounded workers and cancellation
//! registry; they never wait behind library compression or mutation admission.
use crate::{
    assets::NativeAssets,
    thumbnails::{NativeThumbnails, Publish},
};
use freeshow_browse::BrowseService;
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::LibraryService,
    WireError,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

pub use freeshow_browse::COMMANDS;

#[derive(Default)]
struct Registry {
    closed: bool,
    next: u64,
    active: HashMap<u64, (RuntimeContext, CancellationToken)>,
}

#[derive(Clone)]
pub struct NativeBrowse {
    service: BrowseService,
    registry: Arc<Mutex<Registry>>,
    slots: Arc<Semaphore>,
}

struct Lease {
    id: u64,
    registry: Arc<Mutex<Registry>>,
    cancel: CancellationToken,
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

impl NativeBrowse {
    pub fn new(service: BrowseService) -> Self {
        Self {
            service,
            registry: Arc::new(Mutex::new(Registry::default())),
            slots: Arc::new(Semaphore::new(2)),
        }
    }

    fn begin(&self, request: &Request) -> Result<Lease, WireError> {
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new(
                if self.slots.is_closed() {
                    "browse_shutdown"
                } else {
                    "browse_busy"
                },
                "Native browse requests are closed or at capacity",
            )
        })?;
        let context = request.context.clone().ok_or_else(|| {
            WireError::new("stale_library", "Browse request has no library context")
        })?;
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| WireError::new("browse_worker_error", "Browse registry is unavailable"))?;
        if registry.closed {
            return Err(WireError::new(
                "browse_shutdown",
                "Native browsing is closed",
            ));
        }
        let id = registry.next;
        registry.next = id
            .checked_add(1)
            .ok_or_else(|| WireError::new("browse_worker_error", "Browse sequence exhausted"))?;
        let cancel = CancellationToken::new();
        registry.active.insert(id, (context, cancel.clone()));
        Ok(Lease {
            id,
            registry: self.registry.clone(),
            cancel,
            _permit: permit,
        })
    }

    pub fn context_changed(&self, context: &RuntimeContext) {
        let registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        for (captured, cancel) in registry.active.values() {
            if captured != context {
                cancel.cancel();
            }
        }
    }

    pub async fn dispatch(
        &self,
        library: &LibraryService,
        request: Request,
        window: &str,
    ) -> Result<Vec<Event>, WireError> {
        self.dispatch_internal(library, request, window, None).await
    }

    pub async fn dispatch_with_thumbnails(
        &self,
        library: &LibraryService,
        request: Request,
        window: &str,
        thumbnails: &NativeThumbnails,
        assets: &NativeAssets,
        publish: Publish<'_>,
    ) -> Result<Vec<Event>, WireError> {
        self.dispatch_internal(
            library,
            request,
            window,
            Some((thumbnails, assets, publish)),
        )
        .await
    }

    async fn dispatch_internal(
        &self,
        library: &LibraryService,
        request: Request,
        window: &str,
        capture: Option<(&NativeThumbnails, &NativeAssets, Publish<'_>)>,
    ) -> Result<Vec<Event>, WireError> {
        if window != "main" {
            return Err(WireError::new(
                "browse_role",
                "Only the main window may browse files",
            ));
        }
        let lease = self.begin(&request)?;
        library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        let command = request.data["channel"].as_str().unwrap_or("");
        let result = async {
            let error = |error: freeshow_browse::BrowseError| WireError::new(error.code, error.message);
            if command == "READ_FOLDER" {
                if let Some((thumbnails, assets, publish)) = capture {
                    let mut listing = self.service.prepare_listing_with_cancel(&request.data["data"], lease.cancel.clone()).await.map_err(error)?;
                    if listing.thumbnail_intents.len() > 32 {
                        return Err(WireError::new("browse_resource_limit", "Folder listing exceeds 32 thumbnail captures"));
                    }
                    tokio::time::timeout(std::time::Duration::from_secs(30), async {
                        for intent in listing.thumbnail_intents {
                            library.contextualize(&request, &mut []).map_err(WireError::from)?;
                            if lease.cancel.is_cancelled() {
                                return Err(WireError::new("cancelled", "Folder capture cancelled"));
                            }
                            let generated = thumbnails.dispatch(library, assets, Request {
                                channel: "MAIN".into(),
                                data: serde_json::json!({"channel":"GET_THUMBNAIL","data":{"input":intent.input,"size":intent.size}}),
                                context: request.context.clone(),
                                listener_id: None,
                            }, window, publish).await?;
                            let output = generated.first().and_then(|event| event.message.data.as_ref()).and_then(|data| data["output"].as_str()).ok_or_else(|| WireError::new("thumbnail_worker", "Thumbnail has no source-compatible output"))?;
                            let entry = listing.data.get_mut(&intent.input).and_then(serde_json::Value::as_object_mut).ok_or_else(|| WireError::new("thumbnail_worker", "Prepared listing lost its thumbnail slot"))?;
                            entry.insert("thumbnailPath".into(), serde_json::json!(output));
                        }
                        Ok::<_, WireError>(freeshow_browse::BrowseReply { data:listing.data, thumbnail_intents:vec![], diagnostics:listing.diagnostics })
                    }).await.map_err(|_| WireError::new("browse_resource_limit", "Folder capture exceeded its 30-second deadline"))?
                } else {
                    self.service.dispatch_with_cancel(command, &request.data["data"], lease.cancel.clone()).await.map_err(error)
                }
            } else {
                self.service.dispatch_with_cancel(command, &request.data["data"], lease.cancel.clone()).await.map_err(error)
            }
        }.await;
        // Suppress errors and results belonging to a library that has changed.
        library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        if lease.cancel.is_cancelled() {
            return Err(WireError::new("cancelled", "Browse request cancelled"));
        }
        let reply = result?;
        for diagnostic in reply.diagnostics {
            eprintln!(
                "filesystem browse: {}: {}",
                diagnostic.code, diagnostic.message
            );
        }
        let mut events = vec![Event::reply(
            "MAIN",
            command,
            reply.data,
            request.listener_id.clone(),
        )];
        library
            .contextualize(&request, &mut events)
            .map_err(WireError::from)?;
        Ok(events)
    }

    pub async fn shutdown(&self) -> Result<(), WireError> {
        {
            let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
            registry.closed = true;
            self.slots.close();
            for (_, cancel) in registry.active.values() {
                cancel.cancel();
            }
        }
        self.service
            .shutdown()
            .await
            .map_err(|error| WireError::new(error.code, error.message))
    }
}
