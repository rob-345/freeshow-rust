//! Parse imports on fixed workers and install their media under the ordered
//! library owner. A whole-pipeline lease bounds prepared plans waiting to commit.
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
    Error, Result, WireError,
};
use freeshow_imports::{ImportEvent, ImportRoots, ImportService, PreparedImport};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

pub const COMMANDS: &[&str] = &["IMPORT_FILES"];
const MAX_REQUESTS: usize = 2;

#[derive(Debug)]
pub struct Imported {
    pub events: Vec<Event>,
    /// Only final installed or existing destinations, after the ordered commit.
    pub media: Vec<PathBuf>,
    pub context: RuntimeContext,
}

#[derive(Default)]
struct Registry {
    closed: bool,
    next: u64,
    active: HashMap<u64, (RuntimeContext, CancellationToken)>,
}

#[derive(Clone)]
pub struct NativeImports {
    service: Arc<ImportService>,
    slots: Arc<Semaphore>,
    registry: Arc<Mutex<Registry>>,
    changed: Arc<Notify>,
    shutdown: Arc<AsyncMutex<()>>,
}

struct RequestLease {
    id: u64,
    registry: Arc<Mutex<Registry>>,
    changed: Arc<Notify>,
    cancel: CancellationToken,
    _permit: OwnedSemaphorePermit,
}

impl Drop for RequestLease {
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

// Field order matters: private staging cleanup must be enqueued before the host
// lease disappears, including when a stale queued closure is simply dropped.
struct PreparedJob {
    prepared: PreparedImport,
    _lease: RequestLease,
}

impl PreparedJob {
    fn commit(mut self, runtime: &mut Runtime) -> Result<Vec<Event>> {
        let root = runtime.stores.data.join("Imports");
        freeshow_import_effects::apply(&root, &self.prepared.plan).map_err(|error| {
            Error::NativeOperation {
                code: error.code,
                message: error.to_string(),
            }
        })?;
        Ok(events(std::mem::take(&mut self.prepared.events)))
    }
}

fn events(input: Vec<ImportEvent>) -> Vec<Event> {
    input
        .into_iter()
        .map(|event| {
            Event::reply(
                &event.channel,
                &event.message.channel,
                event.message.data,
                None,
            )
        })
        .collect()
}

impl NativeImports {
    pub fn new(service: ImportService) -> Self {
        Self {
            service: Arc::new(service),
            slots: Arc::new(Semaphore::new(MAX_REQUESTS)),
            registry: Arc::new(Mutex::new(Registry::default())),
            changed: Arc::new(Notify::new()),
            shutdown: Arc::new(AsyncMutex::new(())),
        }
    }

    fn begin(&self, request: &Request) -> std::result::Result<RequestLease, WireError> {
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new(
                if self.slots.is_closed() {
                    "import_shutdown"
                } else {
                    "import_busy"
                },
                "Native import requests are closed or at capacity",
            )
        })?;
        let context = request.context.clone().ok_or_else(|| {
            WireError::new("stale_library", "Import request has no library context")
        })?;
        let cancel = CancellationToken::new();
        let mut registry = self.registry.lock().map_err(|_| {
            WireError::new("import_worker", "Native import registry is unavailable")
        })?;
        if registry.closed {
            return Err(WireError::new(
                "import_shutdown",
                "Native imports are closed",
            ));
        }
        let id = registry.next;
        registry.next = id
            .checked_add(1)
            .ok_or_else(|| WireError::new("import_worker", "Native import sequence exhausted"))?;
        registry.active.insert(id, (context, cancel.clone()));
        Ok(RequestLease {
            id,
            registry: self.registry.clone(),
            changed: self.changed.clone(),
            cancel,
            _permit: permit,
        })
    }

    pub fn status(&self) -> Value {
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        json!({"closing":registry.closed,"admittedRequests":registry.active.len()})
    }

    pub fn context_changed(&self, context: &RuntimeContext) {
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (captured, token) in registry.active.values() {
            if captured != context {
                token.cancel();
            }
        }
    }

    pub async fn dispatch(
        &self,
        library: &LibraryService,
        request: Request,
        window: &str,
        sink: impl Fn(Event) -> std::result::Result<(), String> + Send,
    ) -> std::result::Result<Imported, WireError> {
        if window != "main" {
            return Err(WireError::new(
                "import_role",
                "Only the main window may import",
            ));
        }
        let lease = self.begin(&request)?;
        library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        let context = request.context.as_ref().expect("lease validated context");
        let captured = context.clone();
        let prepared = self
            .service
            .prepare_with_cancel(
                request.data["channel"].as_str().unwrap_or(""),
                &request.data["data"],
                ImportRoots {
                    imports_root: context.data_path.join("Imports"),
                },
                lease.cancel.clone(),
            )
            .await;
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                // Specialized native formats announce importing before a later
                // parse failure. Preserve those source events without inventing
                // an IMPORT2 success or attaching a request listener ID.
                let mut prior = events(error.prior_events);
                library
                    .contextualize(&request, &mut prior)
                    .map_err(WireError::from)?;
                for event in prior {
                    sink(event).map_err(|error| WireError::new("import_delivery", error))?;
                }
                return Err(WireError::new(error.code, error.message));
            }
        };
        // Transfer the lease into the queued operation. Once the library admits
        // it, dropping the IPC response wait must not cancel accepted writes.
        let failure_request = Request {
            channel: request.channel.clone(),
            data: Value::Null,
            listener_id: None,
            context: request.context.clone(),
        };
        let prior = prepared
            .events
            .iter()
            .filter(|event| event.message.channel == "ALERT")
            .cloned()
            .collect();
        let root = context.data_path.join("Imports");
        let mut media = prepared
            .plan
            .installs
            .iter()
            .map(|item| root.join(item.destination.as_path()))
            .chain(
                prepared
                    .plan
                    .existing
                    .iter()
                    .map(|item| root.join(item.destination.as_path())),
            )
            .collect::<Vec<_>>();
        media.sort();
        media.dedup();
        let job = PreparedJob {
            prepared,
            _lease: lease,
        };
        match library
            .commit(request, window, move |runtime| job.commit(runtime))
            .await
        {
            Ok(events) => Ok(Imported {
                events,
                media,
                context: captured,
            }),
            Err(error) => {
                let mut prior = events(prior);
                library
                    .contextualize(&failure_request, &mut prior)
                    .map_err(WireError::from)?;
                for event in prior {
                    sink(event).map_err(|error| WireError::new("import_delivery", error))?;
                }
                Err(WireError::from(error))
            }
        }
    }

    pub async fn shutdown(&self) -> std::result::Result<(), WireError> {
        let _serial = self.shutdown.lock().await;
        {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            registry.closed = true;
            self.slots.close();
            for (_, token) in registry.active.values() {
                token.cancel();
            }
        }
        let first = self.service.shutdown().await;
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
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
        // The service's first fence can precede retained host plans dropping.
        // This second fence covers their cleanup before Quit acknowledges drain.
        let final_cleanup = self.service.shutdown().await;
        first
            .and(final_cleanup)
            .map_err(|error| WireError::new(error.code, error.message))
    }
}
