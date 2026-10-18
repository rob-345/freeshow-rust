//! Source MAIN.IP, using the shared fixed native OS owner rather than frozen TXT.
use crate::network_snapshot::{NetworkSnapshots, SnapshotControl};
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::LibraryService,
    WireError,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
pub const COMMANDS: &[&str] = &["IP"];
const CAPACITY: usize = 8;
const INPUT_BYTES: usize = 64 * 1024;
const OUTPUT_BYTES: usize = 128 * 1024;
#[derive(Default)]
struct Registry {
    closed: bool,
    next: u64,
    active: HashMap<u64, (RuntimeContext, CancellationToken)>,
}
struct Inner {
    library: LibraryService,
    snapshots: NetworkSnapshots,
    registry: Arc<Mutex<Registry>>,
    changed: Arc<Notify>,
    #[cfg(test)]
    output_limit: usize,
    shutdown_timeout: Duration,
}
impl Drop for Inner {
    fn drop(&mut self) {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.closed = true;
        for (_, cancel) in registry.active.values() {
            cancel.cancel();
        }
    }
}
#[derive(Clone)]
pub struct NativeNetworkQueries {
    inner: Arc<Inner>,
}
struct Lease {
    id: u64,
    registry: Arc<Mutex<Registry>>,
    changed: Arc<Notify>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .remove(&self.id);
        self.changed.notify_waiters();
    }
}
fn error(code: &'static str, message: &str) -> WireError {
    WireError::new(code, message)
}
fn closed() -> WireError {
    error(
        "network_query_shutdown",
        "Native network query admission is closed",
    )
}
impl NativeNetworkQueries {
    pub fn new(library: LibraryService, snapshots: NetworkSnapshots) -> Self {
        Self {
            inner: Arc::new(Inner {
                library,
                snapshots,
                registry: Arc::new(Mutex::new(Registry::default())),
                changed: Arc::new(Notify::new()),
                #[cfg(test)]
                output_limit: OUTPUT_BYTES,
                shutdown_timeout: Duration::from_secs(3),
            }),
        }
    }
    fn begin(&self, context: &RuntimeContext) -> Result<(Lease, CancellationToken), WireError> {
        let mut registry = self
            .inner
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if registry.closed {
            return Err(closed());
        }
        if registry.active.len() == CAPACITY {
            return Err(error(
                "network_query_busy",
                "Native network queries are at capacity",
            ));
        }
        let id = registry.next;
        registry.next = id
            .checked_add(1)
            .ok_or_else(|| error("network_query_limit", "Native network query IDs exhausted"))?;
        let cancel = CancellationToken::new();
        registry
            .active
            .insert(id, (context.clone(), cancel.clone()));
        Ok((
            Lease {
                id,
                registry: self.inner.registry.clone(),
                changed: self.inner.changed.clone(),
            },
            cancel,
        ))
    }
    pub async fn dispatch(&self, request: Request, window: &str) -> Result<Vec<Event>, WireError> {
        if request.channel != "MAIN"
            || !COMMANDS.contains(&request.data["channel"].as_str().unwrap_or(""))
        {
            return Err(error("unsupported_command", "Unknown native network query"));
        }
        if window != "main" {
            return Err(error(
                "window_error",
                "Only main queries native network addresses",
            ));
        }
        if !bounded_input(&request.data)
            || request
                .listener_id
                .as_ref()
                .is_some_and(|id| id.len() > 256)
            || request
                .context
                .as_ref()
                .is_some_and(|c| c.runtime_id.len() > 256 || c.data_path.as_os_str().len() > 32768)
        {
            return Err(error(
                "network_query_limit",
                "Native network query input exceeds limits",
            ));
        }
        self.inner
            .library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        let (lease, cancel) = self.begin(request.context.as_ref().unwrap())?;
        // Only the native lease enters the OS worker. Renderer data/paths/ports
        // are neither cloned nor passed as interface or executable authority.
        let captured = self
            .inner
            .snapshots
            .addresses_owned(
                SnapshotControl {
                    cancel,
                    ..SnapshotControl::default()
                },
                lease,
            )
            .await;
        // A stale completion cannot be retagged with the latest library context,
        // even when the source data is process-global rather than library data.
        self.inner
            .library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        let captured = captured?;
        let data = json!(captured.value());
        let mut event = Event::reply("MAIN", "IP", data, request.listener_id.clone());
        event.context = request.context.clone();
        #[cfg(test)]
        let maximum = self.inner.output_limit;
        #[cfg(not(test))]
        let maximum = OUTPUT_BYTES;
        if !within_encoded(&event, maximum) {
            return Err(error(
                "network_query_limit",
                "Native network reply exceeds encoded output bound",
            ));
        }
        // Keep accepted request provenance/admission through all final checks.
        self.inner
            .library
            .contextualize(&request, std::slice::from_mut(&mut event))
            .map_err(WireError::from)?;
        drop(captured);
        Ok(vec![event])
    }
    /// Cancels obsolete reads without closing the shared snapshot owner. Real OS
    /// work and completed/uncollected replies retain their native leases.
    pub fn context_changed(&self, current: &RuntimeContext) {
        let registry = self
            .inner
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (context, cancel) in registry.active.values() {
            if context != current {
                cancel.cancel();
            }
        }
    }
    pub fn status(&self) -> Value {
        let registry = self
            .inner
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        json!({"closing":registry.closed,"requests":registry.active.len(),"capacity":CAPACITY,"snapshots":self.inner.snapshots.status()})
    }
    /// Drain this consumer only. The coordinator shuts down the shared worker
    /// after discovery and every live-query consumer have finished their cleanup.
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
        let deadline = Instant::now() + self.inner.shutdown_timeout;
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self
                .inner
                .registry
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active
                .is_empty()
            {
                break;
            }
            tokio::time::timeout_at(deadline.into(), changed)
                .await
                .map_err(|_| {
                    error(
                        "network_query_shutdown_timeout",
                        "Real native network work still owns query leases; drain can be retried",
                    )
                })?;
        }
        self.inner.snapshots.terminal_error().map_or(Ok(()), Err)
    }
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "Exercised by the dedicated network_queries integration target; the binary unit harness does not invoke every fixture hook"
    )]
    pub(crate) fn test_limits(
        library: LibraryService,
        snapshots: NetworkSnapshots,
        output: usize,
        shutdown: Duration,
    ) -> Self {
        let mut native = Self::new(library, snapshots);
        let inner = Arc::get_mut(&mut native.inner).unwrap();
        inner.output_limit = output;
        inner.shutdown_timeout = shutdown;
        native
    }
}
fn bounded_input(value: &Value) -> bool {
    let mut stack = vec![(value, 0usize)];
    let mut nodes = 0usize;
    let mut bytes = 0usize;
    while let Some((value, depth)) = stack.pop() {
        nodes += 1;
        if depth > 16 || nodes > 1024 {
            return false;
        }
        match value {
            Value::String(text) => bytes = bytes.saturating_add(text.len()),
            Value::Array(items) => {
                if items.len() > 1024usize.saturating_sub(nodes + stack.len()) {
                    return false;
                }
                stack.extend(items.iter().map(|v| (v, depth + 1)));
            }
            Value::Object(items) => {
                if items.len() > 1024usize.saturating_sub(nodes + stack.len()) {
                    return false;
                }
                for (key, value) in items {
                    bytes = bytes.saturating_add(key.len());
                    stack.push((value, depth + 1));
                }
            }
            _ => bytes = bytes.saturating_add(16),
        }
        if bytes > INPUT_BYTES {
            return false;
        }
    }
    within_encoded(value, INPUT_BYTES)
}
fn within_encoded(value: &impl serde::Serialize, maximum: usize) -> bool {
    struct Counter {
        bytes: usize,
        maximum: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.maximum.saturating_sub(self.bytes) {
                return Err(std::io::Error::other("network wire limit"));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter { bytes: 0, maximum }, value).is_ok()
}
