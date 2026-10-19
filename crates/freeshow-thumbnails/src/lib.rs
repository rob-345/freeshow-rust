//! Bounded read-only cache preparation and retained-canvas completion staging.
//! The trusted host owns canvas routing, final cache effects and context validation.
mod png_data;
mod prepare;
mod service;
mod stat;
use serde::Serialize;
use serde_json::Value;
pub use service::ThumbnailService;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;
pub const COMMANDS: &[&str] = &["GET_THUMBNAIL", "DOES_MEDIA_EXIST"];
#[derive(Clone, Debug)]
pub struct ThumbnailConfig {
    pub workers: usize,
    pub max_prepared: usize,
    pub path_bytes: usize,
    pub max_size: u32,
    pub input_bytes: u64,
    pub max_pixels: u64,
    pub png_bytes: usize,
    pub chunks: usize,
    pub timeout: Duration,
    pub staging_root: Option<PathBuf>,
}
impl Default for ThumbnailConfig {
    fn default() -> Self {
        Self {
            workers: 2,
            max_prepared: 4,
            path_bytes: 32768,
            max_size: 2048,
            input_bytes: 128 * 1024 * 1024,
            max_pixels: 4_194_304,
            png_bytes: 8 * 1024 * 1024,
            chunks: 256,
            timeout: Duration::from_secs(10),
            staging_root: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct ThumbnailRoots {
    pub cache_root: PathBuf,
}
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ThumbnailError {
    pub code: &'static str,
    pub message: String,
}
pub type Result<T> = std::result::Result<T, ThumbnailError>;
impl ThumbnailError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn resource(message: impl Into<String>) -> Self {
        Self::new("thumbnail_resource_limit", message)
    }
    fn io(error: std::io::Error) -> Self {
        Self::new("thumbnail_io_error", error.to_string())
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct CacheRelativePath(String);
impl CacheRelativePath {
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
    pub mtime_nanos: u32,
    pub ctime_seconds: i64,
    pub ctime_nanos: u32,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "kind", content = "expected")]
pub enum CacheGuard {
    Absent,
    Existing(FileIdentity),
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SourceGuard {
    pub path: String,
    pub expected: FileIdentity,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum CacheEffect {
    Invalidate {
        destination: CacheRelativePath,
        expected: FileIdentity,
    },
    Install {
        destination: CacheRelativePath,
        expected_destination: CacheGuard,
        staged_path: PathBuf,
        expected_staged: FileIdentity,
        sha256: String,
        source: Box<SourceGuard>,
        width: u32,
        height: u32,
        pixel_sha256: String,
    },
}
#[derive(Clone, Debug, Serialize)]
pub struct CaptureSize {
    pub width: u32,
}
#[derive(Clone, Debug, Serialize)]
pub struct CaptureConfig {
    pub seek: f64,
}
/// Exact retained CAPTURE_CANVAS shape. `output` is private staging, never cache.
#[derive(Clone, Debug, Serialize)]
pub struct CaptureCanvas {
    pub input: String,
    pub output: PathBuf,
    pub size: CaptureSize,
    pub extension: String,
    pub config: CaptureConfig,
    pub id: String,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ThumbnailOutcome {
    MediaExists,
    Missing,
    ChangeTimeMismatch,
    CacheHitUnverified,
    CapturePending,
    CaptureFailed,
    TimedOutPrediction,
    GeneratedPng,
}
#[derive(Clone, Debug, Serialize)]
pub struct ThumbnailDiagnostic {
    pub code: String,
    pub message: String,
}
/// Opaque native plan identity; no deserialization or public constructor.
#[derive(Clone, Debug)]
pub struct PlanIdentity(std::sync::Arc<()>);
impl PartialEq for PlanIdentity {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for PlanIdentity {}
/// Keeps staging and a separately bounded prepared lease alive until host commit.
/// Host validates context, identity/hash/guards and ordered effects before reply.
pub struct PreparedThumbnail {
    pub reply: Value,
    pub outcome: ThumbnailOutcome,
    pub capture: Option<CaptureCanvas>,
    pub effects: Vec<CacheEffect>,
    pub diagnostics: Vec<ThumbnailDiagnostic>,
    pub cache_root: PathBuf,
    stage: Option<TempDir>,
    lease: Option<OwnedSemaphorePermit>,
    cleanup: service::Cleanup,
    cancel: CancellationToken,
    request_cancel: CancellationToken,
    created: Instant,
    capture_started: bool,
    source: Option<SourceGuard>,
    destination: Option<CacheRelativePath>,
    guard: CacheGuard,
    failure_matches_request: bool,
    captured_cache_root: PathBuf,
    identity: PlanIdentity,
    preparation_acknowledged: bool,
}
impl PreparedThumbnail {
    pub fn identity(&self) -> PlanIdentity {
        self.identity.clone()
    }
    pub fn captured_cache_root(&self) -> &Path {
        &self.captured_cache_root
    }
    /// Trusted native owner only: call after a successful invalidation receipt.
    /// Foreign/duplicate/wrong-state proofs fail before changing any plan state.
    pub fn acknowledge_pre_capture_invalidation(
        &mut self,
        identity: &PlanIdentity,
        root: &Path,
        destination: &CacheRelativePath,
        expected: &FileIdentity,
    ) -> Result<()> {
        self.ensure_current()?;
        if identity != &self.identity
            || self.preparation_acknowledged
            || self.outcome != ThumbnailOutcome::CapturePending
            || root.as_os_str() != self.captured_cache_root.as_os_str()
            || self.cache_root.as_os_str() != self.captured_cache_root.as_os_str()
            || self.destination.as_ref() != Some(destination)
            || self.guard != CacheGuard::Existing(expected.clone())
            || self.effects.len() != 1
            || !matches!(&self.effects[0],CacheEffect::Invalidate{destination:d,expected:e} if d==destination&&e==expected)
        {
            return Err(ThumbnailError::new(
                "invalid_preparation_acknowledgement",
                "Invalid/foreign/duplicate preparation receipt",
            ));
        }
        self.effects.clear();
        self.guard = CacheGuard::Absent;
        self.preparation_acknowledged = true;
        Ok(())
    }
    /// Trusted native host only: call once immediately before publishing the
    /// capture, after preparation effects and input authorization have completed.
    /// Unmarked plans retain their original preparation-time completion expiry.
    pub fn mark_capture_started(&mut self) -> Result<()> {
        self.ensure_current()?;
        if self.capture_started
            || self.outcome != ThumbnailOutcome::CapturePending
            || self.capture.is_none()
            || self.stage.is_none()
            || self.lease.is_none()
            || self.source.is_none()
            || self.destination.is_none()
        {
            return Err(ThumbnailError::new(
                "invalid_capture_start",
                "Capture must be live, pending and not already started",
            ));
        }
        self.created = Instant::now();
        self.capture_started = true;
        Ok(())
    }
    pub fn ensure_current(&self) -> Result<()> {
        if self.cancel.is_cancelled() || self.request_cancel.is_cancelled() {
            Err(ThumbnailError::new("cancelled", "Thumbnail plan cancelled"))
        } else {
            Ok(())
        }
    }
    pub fn source_guard(&self) -> Option<&SourceGuard> {
        self.source.as_ref()
    }
    pub fn staging_directory(&self) -> Option<&Path> {
        self.stage.as_ref().map(TempDir::path)
    }
    pub fn manifest(&self) -> Value {
        serde_json::json!({"reply":self.reply,"outcome":self.outcome,"capture":self.capture,"effects":self.effects,"diagnostics":self.diagnostics,"cacheRoot":self.cache_root})
    }
}
impl Drop for PreparedThumbnail {
    fn drop(&mut self) {
        if let Some(stage) = self.stage.take() {
            self.cleanup.remove(stage, self.lease.take());
        }
    }
}
pub enum CaptureCompletion<'a> {
    Png(&'a [u8]),
    Failed,
    TimedOut,
}
/// Original signed-32-bit, UTF-16 hash (including the minimum i32 value).
pub fn file_path_hash_code(input: &str) -> String {
    let value = input.encode_utf16().fold(0_i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(unit as i32)
    });
    if value < 0 {
        format!("i{}", -(value as i64))
    } else {
        format!("a{value}")
    }
}
struct Budget {
    config: ThumbnailConfig,
    cancel: CancellationToken,
    request_cancel: CancellationToken,
    started: Instant,
}
impl Budget {
    fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() || self.request_cancel.is_cancelled() {
            return Err(ThumbnailError::new("cancelled", "Thumbnail work cancelled"));
        }
        if self.started.elapsed() > self.config.timeout {
            return Err(ThumbnailError::resource("Thumbnail work deadline"));
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;

/// Trusted native synchronous owner helper. Direct Linux statx on the already
/// anchored handle preserves the exact thumbnail identity/ctime projection.
/// No content read, filesystem fallback, runtime or worker is created here.
pub fn trusted_file_identity(file: &std::fs::File) -> Result<FileIdentity> {
    stat::identity_from_file(file)
}
