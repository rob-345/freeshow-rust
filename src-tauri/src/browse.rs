//! Readonly filesystem queries have their own bounded workers and cancellation
//! registry; they never wait behind library compression or mutation admission.
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
        let result = self
            .service
            .dispatch_with_cancel(command, &request.data["data"], lease.cancel.clone())
            .await;
        // Suppress errors and results belonging to a library that has changed.
        library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        if lease.cancel.is_cancelled() {
            return Err(WireError::new("cancelled", "Browse request cancelled"));
        }
        let reply = result.map_err(|error| WireError::new(error.code, error.message))?;
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
