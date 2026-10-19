mod access;
mod io;
pub mod spec;
mod transport;
pub use access::{
    AuthorizedMediaRead, MediaAuthority, MediaObservation, RegisteredMedia, NATIVE_READ_LIMIT,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    pub runtime_id: String,
    pub library_generation: u64,
    pub data_path: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Error {
    pub code: String,
    pub message: String,
}
impl Error {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    pub(crate) fn os(error: std::io::Error) -> Self {
        Self::new(
            match error.kind() {
                std::io::ErrorKind::NotFound => "assets_missing",
                std::io::ErrorKind::PermissionDenied => "assets_permission",
                _ => "assets_io",
            },
            error.to_string(),
        )
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}

/// This type deliberately has no Deserialize implementation. Grants are
/// constructed only after the host has validated a native provenance boundary.
#[derive(Debug, Clone)]
pub struct NativeGrant {
    pub source_path: String,
    pub path: PathBuf,
    pub provenance: Provenance,
    pub protection: Protection,
}
#[derive(Debug, Clone, Copy)]
pub enum Provenance {
    CommittedLibrary,
    NativeSelection,
    NativeDrop,
    LocatedMedia,
    GeneratedThumbnail,
    CaptureSource,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    Plain,
    Protected,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetGrant {
    pub id: String,
    pub source_path: String,
    pub url: String,
    pub mime_type: String,
    pub length: u64,
    pub context: Context,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantFailure {
    pub index: usize,
    pub source_path: String,
    pub error: Error,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantBatch {
    pub grants: Vec<AssetGrant>,
    pub rejected: Vec<GrantFailure>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub allowed_origins: Vec<String>,
    /// Trusted canonical protected-cache roots. Ciphertext must never acquire
    /// a plaintext capability through an ordinary path or a native symlink.
    pub protected_roots: Vec<PathBuf>,
    pub max_connections: usize,
    pub max_grants: usize,
    pub max_registry_bytes: usize,
    pub max_batch: usize,
    pub file_workers: usize,
    pub file_queue_capacity: usize,
    pub chunk_bytes: usize,
    pub max_file_bytes: u64,
    pub max_response_bytes: u64,
    pub header_bytes: usize,
    pub header_timeout: Duration,
    pub request_timeout: Duration,
    pub shutdown_timeout: Duration,
    pub socket_send_bytes: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            protected_roots: Vec::new(),
            max_connections: 8,
            max_grants: 1024,
            max_registry_bytes: 8 * 1024 * 1024,
            max_batch: 128,
            file_workers: 2,
            file_queue_capacity: 8,
            chunk_bytes: 16 * 1024,
            max_file_bytes: 1 << 40,
            max_response_bytes: 1 << 38,
            header_bytes: 16 * 1024,
            header_timeout: Duration::from_secs(2),
            request_timeout: Duration::from_secs(120),
            shutdown_timeout: Duration::from_secs(2),
            socket_send_bytes: 64 * 1024,
        }
    }
}
impl Config {
    fn validate(&self) -> Result<(), Error> {
        if !(1..=64).contains(&self.max_connections)
            || !(1..=16_384).contains(&self.max_grants)
            || !(1..=64 * 1024 * 1024).contains(&self.max_registry_bytes)
            || !(1..=512).contains(&self.max_batch)
            || !(1..=4).contains(&self.file_workers)
            || !(1..=64).contains(&self.file_queue_capacity)
            || !(1024..=65536).contains(&self.chunk_bytes)
            || self.max_file_bytes > 1 << 48
            || self.max_response_bytes > 1 << 48
            || !(8192..=65536).contains(&self.header_bytes)
            || self.header_timeout.is_zero()
            || self.header_timeout > Duration::from_secs(10)
            || self.request_timeout.is_zero()
            || self.request_timeout > Duration::from_secs(3600)
            || self.shutdown_timeout.is_zero()
            || self.shutdown_timeout > Duration::from_secs(10)
            || !(1024..=1024 * 1024).contains(&self.socket_send_bytes)
            || self.allowed_origins.len() > 16
            || self.allowed_origins.iter().any(|origin| {
                origin.is_empty()
                    || origin.len() > 512
                    || !origin.is_ascii()
                    || origin.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
            })
            || self.protected_roots.len() > 32
            || self.protected_roots.iter().any(|root| {
                !root.is_absolute()
                    || root
                        .to_str()
                        .is_none_or(|text| text.len() > 32768 || text.contains('\0'))
            })
        {
            return Err(Error::new(
                "assets_invalid",
                "Asset transport configuration exceeds hard bounds",
            ));
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct Control {
    pub deadline: Instant,
    pub cancel: CancellationToken,
    pub(crate) epoch: Option<CancellationToken>,
}
impl Default for Control {
    fn default() -> Self {
        Self {
            deadline: Instant::now() + Duration::from_secs(2),
            cancel: CancellationToken::new(),
            epoch: None,
        }
    }
}
impl Control {
    pub fn new(deadline: Instant, cancel: CancellationToken) -> Self {
        Self {
            deadline,
            cancel,
            epoch: None,
        }
    }
    pub(crate) fn check(&self, closing: &AtomicBool) -> Result<(), Error> {
        if closing.load(Ordering::Acquire) {
            return Err(Error::new("assets_shutdown", "Asset service is shut down"));
        }
        if self.cancel.is_cancelled() {
            return Err(Error::new("assets_cancelled", "Asset request cancelled"));
        }
        if self
            .epoch
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(Error::new(
                "assets_stale_context",
                "Asset request belongs to an obsolete library",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(Error::new(
                "assets_deadline",
                "Asset request deadline elapsed",
            ));
        }
        Ok(())
    }
}
struct CancelOnDrop(Option<CancellationToken>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(token) = self.0.take() {
            token.cancel();
        }
    }
}

#[derive(Default)]
pub(crate) struct Stats {
    pub active_io: AtomicUsize,
    pub queued_io: AtomicUsize,
    pub open_files: AtomicUsize,
    pub read_bytes: AtomicU64,
    pub buffered: AtomicUsize,
    pub peak_buffered: AtomicUsize,
    pub rejected_connections: AtomicU64,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub closing: bool,
    pub listener_running: bool,
    pub connections: usize,
    pub grants: usize,
    pub registry_bytes: usize,
    pub active_io: usize,
    pub queued_io: usize,
    pub open_files: usize,
    pub read_bytes: u64,
    pub buffered_bytes: usize,
    pub peak_buffered_bytes: usize,
    pub rejected_connections: u64,
    pub native_reads: usize,
    pub file_workers_joined: bool,
}
#[derive(Clone)]
pub(crate) struct Entry {
    pub public: AssetGrant,
    pub path: PathBuf,
    pub identity: io::Identity,
    pub bytes: usize,
}
pub(crate) struct Connection {
    pub grant: Option<String>,
    pub cancel: CancellationToken,
}
pub(crate) struct Registry {
    pub context: Context,
    revision: Revision,
    pub epoch: CancellationToken,
    pub grants: HashMap<String, Entry>,
    pub connections: HashMap<u64, Connection>,
    pub bytes: usize,
}

/// Native-only publication identity. Retaining this token prevents allocation
/// reuse, so unchanged replay needs no path cloning or wrapping counter.
#[derive(Clone)]
pub struct Revision(Arc<()>);
pub(crate) struct Shared {
    pub config: Config,
    pub authority: String,
    pub base: String,
    pub token: String,
    pub registry: Mutex<Registry>,
    pub stats: Arc<Stats>,
    pub pool: io::Pool,
    pub closing: AtomicBool,
    pub stop: CancellationToken,
    pub done: watch::Sender<bool>,
    pub next_connection: AtomicU64,
    pub native_reads: Arc<access::ReadState>,
}
struct Inner {
    shared: Arc<Shared>,
    completion: watch::Receiver<bool>,
}
impl Inner {
    fn close(&self) {
        self.shared.closing.store(true, Ordering::Release);
        self.shared.stop.cancel();
        self.shared.pool.close();
        self.shared.native_reads.close();
        if let Ok(mut registry) = self.shared.registry.lock() {
            registry.epoch.cancel();
            for connection in registry.connections.values() {
                connection.cancel.cancel();
            }
            registry.grants.clear();
            registry.bytes = 0;
        }
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.close();
    }
}
#[derive(Clone)]
pub struct AssetServer(Arc<Inner>);
struct BatchRollback {
    server: AssetServer,
    ids: Vec<String>,
}
impl Drop for BatchRollback {
    fn drop(&mut self) {
        for id in &self.ids {
            self.server.revoke(id);
        }
    }
}
fn random_hex(bytes: usize) -> Result<String, Error> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).map_err(|error| Error::new("assets_random", error.to_string()))?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn validate_context(context: &Context) -> Result<(), Error> {
    if context.runtime_id.is_empty()
        || context.runtime_id.len() > 128
        || context.runtime_id.contains('\0')
        || context.library_generation > (1u64 << 53) - 1
        || context.data_path.len() > 32768
        || context.data_path.contains('\0')
    {
        return Err(Error::new(
            "assets_invalid",
            "Invalid captured asset context",
        ));
    }
    Ok(())
}

impl AssetServer {
    pub async fn start(config: Config, context: Context) -> Result<Self, Error> {
        config.validate()?;
        validate_context(&context)?;
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(Error::os)?;
        let authority = listener.local_addr().map_err(Error::os)?.to_string();
        let token = random_hex(32)?;
        let base = format!("http://{authority}/{token}");
        let stats = Arc::new(Stats::default());
        let pool = io::Pool::start(
            config.file_workers,
            config.file_queue_capacity,
            config.max_connections + config.file_queue_capacity + 2,
            config.chunk_bytes,
            config.max_file_bytes,
            stats.clone(),
        )?;
        let (done, completion) = watch::channel(false);
        let shared = Arc::new(Shared {
            config,
            authority,
            base,
            token,
            registry: Mutex::new(Registry {
                context,
                revision: Revision(Arc::new(())),
                epoch: CancellationToken::new(),
                grants: HashMap::new(),
                connections: HashMap::new(),
                bytes: 0,
            }),
            stats,
            pool,
            closing: AtomicBool::new(false),
            stop: CancellationToken::new(),
            done,
            next_connection: AtomicU64::new(1),
            native_reads: access::ReadState::new(),
        });
        tokio::spawn(transport::serve(listener, shared.clone()));
        Ok(Self(Arc::new(Inner { shared, completion })))
    }
    pub fn activate_context(&self, next: Context) -> Result<(), Error> {
        validate_context(&next)?;
        if self.0.shared.closing.load(Ordering::Acquire) {
            return Err(Error::new("assets_shutdown", "Asset service is shut down"));
        }
        let mut registry = self
            .0
            .shared
            .registry
            .lock()
            .map_err(|_| Error::new("assets_worker", "Asset registry lock poisoned"))?;
        if self.0.shared.closing.load(Ordering::Acquire) {
            return Err(Error::new("assets_shutdown", "Asset service is shut down"));
        }
        if next == registry.context {
            return Ok(());
        }
        if next.runtime_id != registry.context.runtime_id
            || next.library_generation <= registry.context.library_generation
        {
            return Err(Error::new(
                "assets_stale_context",
                "Asset library context is obsolete",
            ));
        }
        for connection in registry.connections.values() {
            connection.cancel.cancel();
        }
        registry.epoch.cancel();
        registry.epoch = CancellationToken::new();
        registry.context = next;
        registry.revision = Revision(Arc::new(()));
        registry.grants.clear();
        registry.bytes = 0;
        Ok(())
    }
    fn context_control(&self, context: &Context, control: &Control) -> Result<(), Error> {
        control.check(&self.0.shared.closing)?;
        if &self
            .0
            .shared
            .registry
            .lock()
            .map_err(|_| Error::new("assets_worker", "Asset registry lock poisoned"))?
            .context
            != context
        {
            return Err(Error::new(
                "assets_stale_context",
                "Asset request belongs to an obsolete library",
            ));
        }
        Ok(())
    }
    pub async fn authorize(
        &self,
        context: &Context,
        grant: NativeGrant,
        control: Control,
    ) -> Result<AssetGrant, Error> {
        self.authorize_inner(context, grant, control)
            .await
            .map(|(grant, _)| grant)
    }
    async fn authorize_inner(
        &self,
        context: &Context,
        grant: NativeGrant,
        mut control: Control,
    ) -> Result<(AssetGrant, bool), Error> {
        self.context_control(context, &control)?;
        {
            let registry = self
                .0
                .shared
                .registry
                .lock()
                .map_err(|_| Error::new("assets_worker", "Asset registry lock poisoned"))?;
            if &registry.context != context {
                return Err(Error::new(
                    "assets_stale_context",
                    "Asset request belongs to an obsolete library",
                ));
            }
            control.epoch = Some(registry.epoch.clone());
        }
        control.deadline = control
            .deadline
            .min(Instant::now() + Duration::from_secs(10));
        if grant.protection == Protection::Protected
            || grant.source_path.starts_with("freeshow-protected://")
        {
            return Err(Error::new(
                "assets_protected",
                "Protected media decryption/license authorization is not implemented",
            ));
        }
        let path_text = grant.path.to_str().ok_or_else(|| {
            Error::new("assets_encoding", "Non-Unicode asset paths are unsupported")
        })?;
        if !grant.path.is_absolute()
            || path_text.len() > 32768
            || path_text.contains('\0')
            || grant.source_path.is_empty()
            || grant.source_path.len() > 32768
            || grant.source_path.contains('\0')
        {
            return Err(Error::new(
                "assets_invalid",
                "Asset grants require bounded absolute native paths and source keys",
            ));
        }
        let mime_type = spec::mime_type(&grant.path).to_string();
        let mut guard = CancelOnDrop(Some(control.cancel.clone()));
        let opened = self
            .0
            .shared
            .pool
            .open(grant.path, None, control.clone())
            .await;
        guard.0.take();
        let opened = opened?;
        self.context_control(context, &control)?;
        if self
            .0
            .shared
            .config
            .protected_roots
            .iter()
            .any(|root| opened.path.starts_with(root))
        {
            return Err(Error::new(
                "assets_protected",
                "Protected cache bytes require decryption and license authorization",
            ));
        }
        let canonical = opened.path.to_str().ok_or_else(|| {
            Error::new(
                "assets_encoding",
                "Non-Unicode canonical asset paths are unsupported",
            )
        })?;
        if canonical.len() > 32768 {
            return Err(Error::new(
                "assets_limit",
                "Canonical asset path exceeds its byte limit",
            ));
        }
        let bytes = canonical.len()
            + grant.source_path.len()
            + mime_type.len()
            + context.runtime_id.len()
            + context.data_path.len()
            + 256;
        let mut registry = self
            .0
            .shared
            .registry
            .lock()
            .map_err(|_| Error::new("assets_worker", "Asset registry lock poisoned"))?;
        control.check(&self.0.shared.closing)?;
        if &registry.context != context {
            return Err(Error::new(
                "assets_stale_context",
                "Library changed during asset authorization",
            ));
        }
        if let Some(entry) = registry.grants.values_mut().find(|entry| {
            entry.public.source_path == grant.source_path
                && entry.path == opened.path
                && entry.identity == opened.identity
                && entry.public.mime_type == mime_type
        }) {
            let changed = entry.public.length != opened.length;
            entry.public.length = opened.length;
            let public = entry.public.clone();
            if changed {
                registry.revision = Revision(Arc::new(()));
            }
            return Ok((public, false));
        }
        let replaced = registry
            .grants
            .values()
            .filter(|entry| entry.public.source_path == grant.source_path)
            .map(|entry| (entry.public.id.clone(), entry.bytes))
            .collect::<Vec<_>>();
        let replaced_bytes = replaced.iter().map(|(_, bytes)| *bytes).sum::<usize>();
        if registry.grants.len() - replaced.len() >= self.0.shared.config.max_grants
            || registry.bytes - replaced_bytes + bytes > self.0.shared.config.max_registry_bytes
        {
            return Err(Error::new(
                "assets_limit",
                "Asset grant registry budget exhausted",
            ));
        }
        let id = random_hex(16)?;
        if registry.grants.contains_key(&id) {
            return Err(Error::new("assets_random", "Random grant ID collision"));
        }
        let public = AssetGrant {
            id: id.clone(),
            source_path: grant.source_path,
            url: format!("{}/{id}", self.0.shared.base),
            mime_type,
            length: opened.length,
            context: context.clone(),
        };
        for (old, _) in replaced {
            registry.grants.remove(&old);
            for connection in registry.connections.values() {
                if connection.grant.as_deref() == Some(old.as_str()) {
                    connection.cancel.cancel();
                }
            }
        }
        registry.bytes = registry.bytes - replaced_bytes + bytes;
        registry.revision = Revision(Arc::new(()));
        registry.grants.insert(
            id,
            Entry {
                public: public.clone(),
                path: opened.path,
                identity: opened.identity,
                bytes,
            },
        );
        Ok((public, true))
    }
    pub async fn authorize_batch(
        &self,
        context: &Context,
        grants: Vec<NativeGrant>,
        control: Control,
    ) -> Result<GrantBatch, Error> {
        self.context_control(context, &control)?;
        if grants.len() > self.0.shared.config.max_batch
            || grants
                .iter()
                .map(|grant| grant.source_path.len() + grant.path.as_os_str().len())
                .sum::<usize>()
                > 1024 * 1024
        {
            return Err(Error::new(
                "assets_limit",
                "Asset batch exceeds admission bounds",
            ));
        }
        let mut batch = GrantBatch {
            grants: Vec::new(),
            rejected: Vec::new(),
        };
        let mut rollback = BatchRollback {
            server: self.clone(),
            ids: Vec::new(),
        };
        for (index, grant) in grants.into_iter().enumerate() {
            self.context_control(context, &control)?;
            let source_path = grant.source_path.clone();
            match self.authorize_inner(context, grant, control.clone()).await {
                Ok((grant, created)) => {
                    if created {
                        rollback.ids.push(grant.id.clone());
                    }
                    batch.grants.push(grant);
                }
                Err(error)
                    if matches!(
                        error.code.as_str(),
                        "assets_stale_context"
                            | "assets_shutdown"
                            | "assets_cancelled"
                            | "assets_deadline"
                    ) =>
                {
                    return Err(error)
                }
                Err(error) => batch.rejected.push(GrantFailure {
                    index,
                    source_path,
                    error,
                }),
            }
        }
        self.context_control(context, &control)?;
        rollback.ids.clear();
        Ok(batch)
    }
    pub fn revoke(&self, id: &str) {
        if let Ok(mut registry) = self.0.shared.registry.lock() {
            if let Some(entry) = registry.grants.remove(id) {
                registry.bytes -= entry.bytes;
                registry.revision = Revision(Arc::new(()));
            }
            for connection in registry.connections.values() {
                if connection.grant.as_deref() == Some(id) {
                    connection.cancel.cancel();
                }
            }
        }
    }
    /// Bounded replay for native output windows; never authorizes new paths.
    pub fn snapshot(&self, context: &Context) -> Result<Vec<AssetGrant>, Error> {
        Ok(self.snapshot_since(context, None)?.1.unwrap())
    }
    pub fn snapshot_since(
        &self,
        context: &Context,
        previous: Option<&Revision>,
    ) -> Result<(Revision, Option<Vec<AssetGrant>>), Error> {
        self.context_control(context, &Control::default())?;
        let registry = self
            .0
            .shared
            .registry
            .lock()
            .map_err(|_| Error::new("assets_worker", "Asset registry lock poisoned"))?;
        if &registry.context != context {
            return Err(Error::new(
                "assets_stale_context",
                "Asset replay belongs to an obsolete library",
            ));
        }
        let revision = registry.revision.clone();
        if previous.is_some_and(|previous| Arc::ptr_eq(&previous.0, &revision.0)) {
            return Ok((revision, None));
        }
        let mut grants = registry
            .grants
            .values()
            .map(|entry| entry.public.clone())
            .collect::<Vec<_>>();
        grants.sort_unstable_by(|left, right| left.source_path.cmp(&right.source_path));
        Ok((revision, Some(grants)))
    }
    pub fn status(&self) -> Status {
        let registry = self
            .0
            .shared
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let stats = &self.0.shared.stats;
        Status {
            closing: self.0.shared.closing.load(Ordering::Acquire),
            listener_running: !*self.0.completion.borrow(),
            connections: registry.connections.len(),
            grants: registry.grants.len(),
            registry_bytes: registry.bytes,
            active_io: stats.active_io.load(Ordering::Acquire),
            queued_io: stats.queued_io.load(Ordering::Acquire),
            open_files: stats.open_files.load(Ordering::Acquire),
            read_bytes: stats.read_bytes.load(Ordering::Relaxed),
            buffered_bytes: stats.buffered.load(Ordering::Acquire),
            peak_buffered_bytes: stats.peak_buffered.load(Ordering::Acquire),
            rejected_connections: stats.rejected_connections.load(Ordering::Relaxed),
            native_reads: self.0.shared.native_reads.count(),
            file_workers_joined: self.0.shared.pool.joined(),
        }
    }
    pub async fn shutdown(&self) -> Result<(), Error> {
        self.0.close();
        let mut completion = self.0.completion.clone();
        let wait = async {
            while !*completion.borrow_and_update() {
                completion.changed().await.map_err(|_| {
                    Error::new("assets_worker", "Asset listener completion channel closed")
                })?;
            }
            let pool = self.0.shared.pool.drain().await;
            let native_reads = self.0.shared.native_reads.drain().await;
            pool.and(native_reads)
        };
        tokio::time::timeout(self.0.shared.config.shutdown_timeout, wait)
            .await
            .map_err(|_| {
                Error::new(
                    "assets_shutdown_timeout",
                    "Asset listener/file IO did not drain before the shutdown deadline",
                )
            })?
    }
}

#[cfg(test)]
mod access_tests;
#[cfg(test)]
mod tests;
