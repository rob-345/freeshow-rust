//! Stored provider indications read actual global ACCESS on the existing
//! ordered owner. No provider backend, OAuth listener, worker or timer is added.
use freeshow_core::{
    protocol::{Event, Request},
    service::LibraryService,
    Error, WireError,
};
use freeshow_system::{provider_connections as native, CancelToken, JobControl};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::Read,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const SLOTS: usize = 4;
const SNAPSHOT_BYTES: usize = native::MAX_SNAPSHOT_BYTES;

#[derive(Default)]
struct Registry {
    closed: bool,
    next: u64,
    active: HashMap<u64, CancelToken>,
}

#[derive(Clone)]
pub struct ProviderConnections {
    registry: Arc<Mutex<Registry>>,
    slots: Arc<Semaphore>,
}
struct Lease {
    id: u64,
    registry: Arc<Mutex<Registry>>,
    _permit: OwnedSemaphorePermit,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .remove(&self.id);
    }
}
struct Caller(CancelToken);
impl Drop for Caller {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
fn failure(code: &'static str, message: &'static str) -> Error {
    Error::NativeOperation {
        code,
        message: message.into(),
    }
}
fn check(control: &JobControl) -> freeshow_core::Result<()> {
    if control.cancel.is_cancelled() {
        Err(failure(
            "provider_connections_cancelled",
            "Provider connection request cancelled",
        ))
    } else if Instant::now() >= control.deadline {
        Err(failure(
            "provider_connections_deadline",
            "Provider connection request deadline elapsed",
        ))
    } else {
        Ok(())
    }
}
fn resource() -> Error {
    failure(
        "provider_connections_resource_limit",
        "Global provider access exceeds work limits",
    )
}
// Bound actual ACCESS bytes before parsing. The pure projection applies its
// independent compact-byte/node/depth budget to the complete parsed snapshot.
fn snapshot(path: &std::path::Path, control: &JobControl) -> freeshow_core::Result<Value> {
    check(control)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        // Conf's actual missing-file getter returns a successful empty object.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({}))
        }
        Err(_) => {
            return Err(failure(
                "provider_connections_read",
                "Unable to read global provider access",
            ))
        }
    };
    let metadata = file.metadata().map_err(|_| {
        failure(
            "provider_connections_read",
            "Unable to inspect global provider access",
        )
    })?;
    if !metadata.is_file() {
        return Err(failure(
            "provider_connections_unsupported",
            "Global provider access is not a regular file",
        ));
    }
    if metadata.len() > SNAPSHOT_BYTES as u64 {
        return Err(resource());
    }
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(32768));
    loop {
        check(control)?;
        let mut chunk = [0; 32768];
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                if bytes.len() + count > SNAPSHOT_BYTES {
                    return Err(resource());
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                return Err(failure(
                    "provider_connections_read",
                    "Unable to read global provider access",
                ))
            }
        }
    }
    check(control)?;
    // Conservative preallocation bound; punctuation inside strings also counts.
    if bytes
        .iter()
        .filter(|b| matches!(**b, b'{' | b'}' | b'[' | b']' | b',' | b':'))
        .count()
        > 32768
    {
        return Err(resource());
    }
    let value = serde_json::from_slice(&bytes).map_err(|_| {
        failure(
            "provider_connections_parse",
            "Unable to parse global provider access",
        )
    })?;
    check(control)?;
    Ok(value)
}

impl ProviderConnections {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(Mutex::new(Registry::default())),
            slots: Arc::new(Semaphore::new(SLOTS)),
        }
    }
    fn begin(&self, control: &JobControl) -> Result<Arc<Lease>, WireError> {
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new(
                if self.slots.is_closed() {
                    "provider_connections_shutdown"
                } else {
                    "provider_connections_busy"
                },
                "Provider connection admission is closed or at capacity",
            )
        })?;
        let mut registry = self.registry.lock().map_err(|_| {
            WireError::new(
                "provider_connections_worker",
                "Provider connection registry unavailable",
            )
        })?;
        if registry.closed {
            return Err(WireError::new(
                "provider_connections_shutdown",
                "Provider connection admission is closed",
            ));
        }
        let id = registry.next;
        registry.next = id.checked_add(1).ok_or_else(|| {
            WireError::new(
                "provider_connections_worker",
                "Provider connection sequence exhausted",
            )
        })?;
        registry.active.insert(id, control.cancel.clone());
        Ok(Arc::new(Lease {
            id,
            registry: self.registry.clone(),
            _permit: permit,
        }))
    }
    /// Seal admission and request cooperative cancellation. Actual accepted
    /// jobs remain retained; the later LibraryService finish_writes fence must
    /// complete before native exit. This method is not a thread-join receipt.
    pub fn seal(&self) {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.closed = true;
        self.slots.close();
        for token in registry.active.values() {
            token.cancel();
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
                "provider_connections_role",
                "Only the main window may request provider connection",
            ));
        }
        if request.channel != "MAIN" || request.data["channel"] != "PROVIDER_CONNECTIONS" {
            return Err(WireError::new(
                "unsupported_command",
                "Unknown provider connection command",
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
            return Err(WireError::from(resource()));
        }
        // The source handler ignores its payload; do not retain it in the ordered
        // queue. Frozen context and original correlation remain unchanged.
        request.data = serde_json::json!({"channel":"PROVIDER_CONNECTIONS"});
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(WireError::new(
                "provider_connections_runtime",
                "Provider connection admission requires the native async runtime",
            ));
        }
        let control = JobControl {
            deadline: Instant::now() + Duration::from_secs(2),
            cancel: CancelToken::default(),
        };
        let deadline = control.deadline;
        let lease = self.begin(&control)?;
        let _observer_lease = lease.clone();
        let _caller = Caller(control.cancel.clone());
        let reply_request = Request {
            channel: request.channel.clone(),
            data: request.data.clone(),
            listener_id: request.listener_id.clone(),
            context: request.context.clone(),
        };
        let listener_id = request.listener_id.clone();
        let operation = library.commit_without_media_refresh(request, window, move |runtime| {
            let _lease = lease;
            check(&control)?;
            let access = snapshot(&runtime.stores.path("ACCESS")?, &control)?;
            let value =
                native::project(&access).map_err(|error| failure(error.code(), error.message()))?;
            check(&control)?;
            Ok(vec![Event::reply(
                "MAIN",
                "PROVIDER_CONNECTIONS",
                value,
                listener_id,
            )])
        });
        let mut events = tokio::time::timeout_at(deadline.into(), operation)
            .await
            .map_err(|_| {
                WireError::new(
                    "provider_connections_deadline",
                    "Provider connection observer deadline elapsed; accepted work remains retained",
                )
            })?
            .map_err(WireError::from)?;
        library
            .contextualize(&reply_request, &mut events)
            .map_err(WireError::from)?;
        Ok(events)
    }
    #[cfg(test)]
    #[allow(dead_code)] // Shared file also compiles in the binary unit target.
    pub fn retained(&self) -> usize {
        self.registry.lock().unwrap().active.len()
    }
}

impl Default for ProviderConnections {
    fn default() -> Self {
        Self::new()
    }
}
