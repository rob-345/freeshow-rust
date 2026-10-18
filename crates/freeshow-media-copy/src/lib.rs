//! Source-driven media copy preparation. Only private staging is written here.
//! The host ordered owner resolves guarded final effects and deferred IPC replies.
mod engine;
mod paths;
mod service;
pub use paths::{file_path_hash, parent_folder_id};
use serde::Serialize;
use serde_json::{json, Value};
pub use service::MediaCopyService;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
pub use tokio_util::sync::CancellationToken;
pub const COMMANDS: &[&str] = &["BUNDLE_MEDIA_FILES", "MEDIA_FOLDER_COPY"];
pub const WORKERS: usize = 2;
#[derive(Clone, Debug)]
pub struct CopyConfig {
    pub queued_jobs: usize,
    pub max_prepared: usize,
    pub max_files: usize,
    pub max_entries: usize,
    pub max_nodes: usize,
    pub max_depth: usize,
    pub path_bytes: usize,
    pub show_bytes: usize,
    pub text_bytes: usize,
    pub store_bytes: usize,
    pub file_bytes: u64,
    pub copied_bytes: u64,
    pub output_bytes: usize,
    pub operations: u64,
    pub timeout: Duration,
    pub prepared_ttl: Duration,
    pub staging_root: Option<PathBuf>,
}
impl Default for CopyConfig {
    fn default() -> Self {
        Self {
            queued_jobs: 4,
            max_prepared: 4,
            max_files: 256,
            max_entries: 2048,
            max_nodes: 50000,
            max_depth: 32,
            path_bytes: 32768,
            show_bytes: 2 * 1024 * 1024,
            text_bytes: 16 * 1024 * 1024,
            store_bytes: 4 * 1024 * 1024,
            file_bytes: 32 * 1024 * 1024,
            copied_bytes: 64 * 1024 * 1024,
            output_bytes: 4 * 1024 * 1024,
            operations: 500000,
            timeout: Duration::from_secs(15),
            prepared_ttl: Duration::from_secs(60),
            staging_root: None,
        }
    }
}
impl CopyConfig {
    fn validate(&self) -> Result<()> {
        if !(1..=16).contains(&self.queued_jobs)
            || !(1..=8).contains(&self.max_prepared)
            || !(1..=1024).contains(&self.max_files)
            || !(1..=8192).contains(&self.max_entries)
            || !(1..=100000).contains(&self.max_nodes)
            || !(1..=64).contains(&self.max_depth)
            || !(1..=32768).contains(&self.path_bytes)
            || !(1..=8 * 1024 * 1024).contains(&self.show_bytes)
            || !(1..=32 * 1024 * 1024).contains(&self.text_bytes)
            || !(1..=8 * 1024 * 1024).contains(&self.store_bytes)
            || self.file_bytes == 0
            || self.file_bytes > 64 * 1024 * 1024
            || self.copied_bytes == 0
            || self.copied_bytes > 128 * 1024 * 1024
            || !(1..=16 * 1024 * 1024).contains(&self.output_bytes)
            || !(1..=2_000_000).contains(&self.operations)
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(60)
            || self.prepared_ttl < self.timeout
            || self.prepared_ttl > Duration::from_secs(300)
            || self.staging_root.as_ref().is_some_and(|p| {
                !p.is_absolute() || p.as_os_str().len() > 32768 || p.to_str().is_none()
            })
        {
            return Err(CopyError::new(
                "invalid_configuration",
                "Invalid media copy limits",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct StoreSnapshot {
    projects: Value,
    overlays: Value,
    templates: Value,
}
impl StoreSnapshot {
    pub fn projects(&self) -> &Value {
        &self.projects
    }
    pub fn overlays(&self) -> &Value {
        &self.overlays
    }
    pub fn templates(&self) -> &Value {
        &self.templates
    }
    /// Freeze original full getStore() values, including PROJECTS.projects.
    pub fn new(projects: Value, overlays: Value, templates: Value) -> Self {
        Self {
            projects,
            overlays,
            templates,
        }
    }
}
#[derive(Clone, Debug)]
pub enum MediaSyncRoot {
    Default,
    Configured(PathBuf),
}
#[derive(Clone, Debug)]
pub struct CopyContext {
    data_root: String,
    shows_root: String,
    default_media: String,
    sync_root: String,
    sync_default: bool,
    approved_bundle_root: Option<String>,
    stores: Arc<StoreSnapshot>,
}
impl CopyContext {
    pub fn new(
        data_root: impl AsRef<Path>,
        sync: MediaSyncRoot,
        approved_bundle_root: Option<PathBuf>,
        stores: Arc<StoreSnapshot>,
    ) -> Result<Self> {
        fn root(path: &Path) -> Result<String> {
            let text = path.to_str().ok_or_else(|| {
                CopyError::new("unsupported_path_encoding", "Copy root is not Unicode")
            })?;
            if !path.is_absolute() || text.contains('\0') {
                return Err(CopyError::new(
                    "invalid_context",
                    "Trusted copy roots must be absolute without NUL",
                ));
            }
            if text.len() > 32768 {
                return Err(CopyError::resource("Copy root path limit"));
            }
            Ok(text.into())
        }
        let data_root = root(data_root.as_ref())?;
        let shows_root = paths::join(&data_root, "Shows");
        let default_media = paths::join(&data_root, "Media");
        if shows_root.len() > 32768 || default_media.len() > 32768 {
            return Err(CopyError::resource("Derived copy root path limit"));
        }
        let (sync_root, sync_default) = match sync {
            MediaSyncRoot::Default => (default_media.clone(), true),
            MediaSyncRoot::Configured(p) => (root(&p)?, false),
        };
        let approved_bundle_root = approved_bundle_root.as_deref().map(root).transpose()?;
        Ok(Self {
            data_root,
            shows_root,
            default_media,
            sync_root,
            sync_default,
            approved_bundle_root,
            stores,
        })
    }
    pub fn uses_default_sync_root(&self) -> bool {
        self.sync_default
    }
    /// Source-normalized default root, derived only from the trusted data root.
    pub fn default_media_root(&self) -> &Path {
        Path::new(&self.default_media)
    }
    /// Original configured spelling, or the derived default root.
    pub fn sync_root(&self) -> &Path {
        Path::new(&self.sync_root)
    }
    pub fn stores(&self) -> &StoreSnapshot {
        &self.stores
    }
    pub fn data_root(&self) -> &Path {
        Path::new(&self.data_root)
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct MediaRelativePath(String);
impl MediaRelativePath {
    #[cfg(target_os = "linux")]
    fn new(path: String) -> Result<Self> {
        if path.is_empty()
            || Path::new(&path).is_absolute()
            || Path::new(&path)
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(CopyError::new(
                "unsupported_destination_path",
                "Copy destination is not safely relative",
            ));
        }
        Ok(Self(path))
    }
    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
    pub length: u64,
    pub mode: u32,
    pub mtime_seconds: i64,
    pub mtime_nanos: i64,
    pub ctime_seconds: i64,
    pub ctime_nanos: i64,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "expected", rename_all = "camelCase")]
pub enum PathGuard {
    Absent,
    Existing(FileIdentity),
}
/// Directory safety identity ignores entry-count/ctime/mtime churn; target
/// absence and source listing guards separately protect relevant contents.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DirectoryIdentity {
    pub device: u64,
    pub inode: u64,
    pub mode: u32,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", content = "expected", rename_all = "camelCase")]
pub enum DirectoryGuard {
    Absent,
    Existing(DirectoryIdentity),
}
#[derive(Clone, Debug, Serialize)]
pub struct AncestorGuard {
    path: PathBuf,
    expected: DirectoryGuard,
}
impl AncestorGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn expected(&self) -> &DirectoryGuard {
        &self.expected
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct SourceGuard {
    path: String,
    link: FileIdentity,
    target: FileIdentity,
}
impl SourceGuard {
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn link(&self) -> &FileIdentity {
        &self.link
    }
    pub fn target(&self) -> &FileIdentity {
        &self.target
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Install {
    destination: MediaRelativePath,
    expected_destination: PathGuard,
    staged_path: PathBuf,
    expected_staged: FileIdentity,
    source: SourceGuard,
    sha256: String,
}
impl Install {
    pub fn expected_destination(&self) -> &PathGuard {
        &self.expected_destination
    }
    pub fn destination(&self) -> &MediaRelativePath {
        &self.destination
    }
    pub fn staged_path(&self) -> &Path {
        &self.staged_path
    }
    pub fn expected_staged(&self) -> &FileIdentity {
        &self.expected_staged
    }
    pub fn source(&self) -> &SourceGuard {
        &self.source
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CopyEffect {
    EnsureShowsRoot {
        path: PathBuf,
        ancestors: Vec<AncestorGuard>,
        listing_guard: PathGuard,
    },
    EnsureDestinationRoot,
    EnsureDirectory {
        destination: MediaRelativePath,
        expected: DirectoryGuard,
    },
    Install {
        file: Box<Install>,
    },
    /// Source sets changed=true before copyFile, even if this ordinary attempt fails.
    CopyAttemptFailure {
        destination: MediaRelativePath,
        source: String,
        os_code: Option<i32>,
    },
    SkipExisting {
        destination: MediaRelativePath,
        expected: FileIdentity,
        followed: Option<FileIdentity>,
    },
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CopyObservation {
    PrefixSkipped {
        source: String,
    },
    Missing {
        source: String,
    },
    EnsuredDirectoryDestination {
        source: String,
        destination: Option<MediaRelativePath>,
    },
    DuplicateDestination {
        source: String,
        destination: MediaRelativePath,
    },
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DeferredReply {
    NoReply,
    MediaFolderCopy { changed_if_effects_resolved: bool },
}
#[derive(Clone, Debug, Serialize)]
pub struct CopyDiagnostic {
    pub code: &'static str,
    pub os_code: Option<i32>,
    pub source_error_logged: bool,
}
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct CopyError {
    pub code: &'static str,
    pub message: String,
    pub diagnostics: Vec<CopyDiagnostic>,
    pub omitted_diagnostics: usize,
}
impl CopyError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            diagnostics: vec![],
            omitted_diagnostics: 0,
        }
    }
    fn resource(message: impl Into<String>) -> Self {
        Self::new("copy_resource_limit", message)
    }
    fn cancelled() -> Self {
        Self::new("cancelled", "Media copy preparation cancelled")
    }
    fn shutdown() -> Self {
        Self::new("copy_shutdown", "Media copy service is shutting down")
    }
}
pub type Result<T> = std::result::Result<T, CopyError>;
struct StageState {
    status: AtomicU8,
}
const LIVE: u8 = 0;
const PINNED: u8 = 1;
const RELEASED: u8 = 2;
const DEAD: u8 = 3;
/// Single-use trusted ordered-owner pin. Holds private staging through accepted
/// final effects; not a renderer command, acknowledgement, or successful copy.
pub struct CommitLease {
    state: Arc<StageState>,
    cleanup: service::Cleanup,
}
impl Drop for CommitLease {
    fn drop(&mut self) {
        self.cleanup.release_pin(&self.state);
    }
}
/// Opaque prepared operation. No deserialization or final-destination mutation API.
/// A retained lease lives in the cleanup owner until private files are removed.
pub struct PreparedMediaCopy {
    data_root: String,
    destination_root: String,
    destination_ancestors: Vec<AncestorGuard>,
    show_guards: Vec<SourceGuard>,
    effects: Vec<CopyEffect>,
    observations: Vec<CopyObservation>,
    candidates: Vec<String>,
    diagnostics: Vec<CopyDiagnostic>,
    omitted_diagnostics: usize,
    deferred_reply: DeferredReply,
    open_folder: bool,
    stage_path: PathBuf,
    state: Arc<StageState>,
    cleanup: service::Cleanup,
    cancel: CancellationToken,
    request_cancel: CancellationToken,
    created: Instant,
    ttl: Duration,
}
impl PreparedMediaCopy {
    pub fn ensure_current(&self) -> Result<()> {
        if self.cancel.is_cancelled() || self.request_cancel.is_cancelled() {
            return Err(CopyError::cancelled());
        }
        let status = self.state.status.load(Ordering::Acquire);
        if status >= RELEASED || (status == LIVE && self.created.elapsed() >= self.ttl) {
            return Err(CopyError::new(
                "copy_plan_expired",
                "Media copy plan expired",
            ));
        }
        Ok(())
    }
    pub fn pin_for_commit(&self) -> Result<CommitLease> {
        self.ensure_current()?;
        self.state
            .status
            .compare_exchange(LIVE, PINNED, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                CopyError::new(
                    "invalid_commit_lease",
                    "Copy plan expired or already claimed",
                )
            })?;
        if self.created.elapsed() >= self.ttl {
            self.cleanup.release_pin(&self.state);
            return Err(CopyError::new(
                "copy_plan_expired",
                "Copy plan expired before commit admission",
            ));
        }
        Ok(CommitLease {
            state: self.state.clone(),
            cleanup: self.cleanup.clone(),
        })
    }
    /// Verify a pin belongs to this exact opaque plan. Once accepted, request
    /// cancellation/TTL does not revoke physical staging from the ordered owner.
    pub fn validate_commit_lease(&self, lease: &CommitLease) -> Result<()> {
        if !Arc::ptr_eq(&self.state, &lease.state)
            || self.state.status.load(Ordering::Acquire) != PINNED
        {
            return Err(CopyError::new(
                "invalid_commit_lease",
                "Foreign or released copy commit lease",
            ));
        }
        Ok(())
    }
    pub fn captured_data_root(&self) -> &Path {
        Path::new(&self.data_root)
    }
    pub fn destination_root(&self) -> &Path {
        Path::new(&self.destination_root)
    }
    pub fn destination_ancestors(&self) -> &[AncestorGuard] {
        &self.destination_ancestors
    }
    pub fn show_guards(&self) -> &[SourceGuard] {
        &self.show_guards
    }
    pub fn effects(&self) -> &[CopyEffect] {
        &self.effects
    }
    pub fn observations(&self) -> &[CopyObservation] {
        &self.observations
    }
    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }
    pub fn deferred_reply(&self) -> &DeferredReply {
        &self.deferred_reply
    }
    pub fn diagnostics(&self) -> &[CopyDiagnostic] {
        &self.diagnostics
    }
    pub fn omitted_diagnostics(&self) -> usize {
        self.omitted_diagnostics
    }
    pub fn open_folder_intent(&self) -> bool {
        self.open_folder
    }
    pub fn staging_directory(&self) -> &Path {
        &self.stage_path
    }
    fn projection(&self) -> Manifest<'_> {
        Manifest {
            data_root: &self.data_root,
            destination_root: &self.destination_root,
            destination_ancestors: &self.destination_ancestors,
            show_guards: &self.show_guards,
            effects: &self.effects,
            observations: &self.observations,
            candidates: &self.candidates,
            diagnostics: &self.diagnostics,
            omitted_diagnostics: self.omitted_diagnostics,
            deferred_reply: &self.deferred_reply,
            open_folder_intent: self.open_folder,
        }
    }
    pub fn manifest(&self) -> Value {
        serde_json::to_value(self.projection()).expect("Validated copy manifest")
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest<'a> {
    data_root: &'a str,
    destination_root: &'a str,
    destination_ancestors: &'a [AncestorGuard],
    show_guards: &'a [SourceGuard],
    effects: &'a [CopyEffect],
    observations: &'a [CopyObservation],
    candidates: &'a [String],
    diagnostics: &'a [CopyDiagnostic],
    omitted_diagnostics: usize,
    deferred_reply: &'a DeferredReply,
    open_folder_intent: bool,
}
impl Drop for PreparedMediaCopy {
    fn drop(&mut self) {
        self.cleanup.remove(&self.state);
    }
}
struct Budget {
    config: CopyConfig,
    service_cancel: CancellationToken,
    cancel: CancellationToken,
    request_cancel: CancellationToken,
    started: Instant,
    operations: u64,
    #[cfg(target_os = "linux")]
    copied: u64,
    #[cfg(target_os = "linux")]
    show_guards: Vec<SourceGuard>,
    #[cfg(target_os = "linux")]
    text: usize,
    #[cfg(target_os = "linux")]
    diagnostics: Vec<CopyDiagnostic>,
    #[cfg(target_os = "linux")]
    omitted: usize,
    #[cfg(test)]
    hooks: Arc<service::Hooks>,
}
impl Budget {
    fn check(&mut self) -> Result<()> {
        if self.service_cancel.is_cancelled() {
            return Err(CopyError::shutdown());
        }
        if self.cancel.is_cancelled() || self.request_cancel.is_cancelled() {
            return Err(CopyError::cancelled());
        }
        if self.started.elapsed() >= self.config.timeout {
            return Err(CopyError::new(
                "copy_timeout",
                "Media copy preparation deadline exceeded",
            ));
        }
        self.operations += 1;
        if self.operations > self.config.operations {
            return Err(CopyError::resource("Copy operation limit"));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    fn diagnostic(&mut self, code: &'static str, error: Option<i32>, logged: bool) {
        if self.diagnostics.len() < 16 {
            self.diagnostics.push(CopyDiagnostic {
                code,
                os_code: error,
                source_error_logged: logged,
            });
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }
}
