//! Source-compatible native import transport. All filesystem writes remain private
//! staging; only the host's ordered/context-validated owner may install media.
mod archive;
mod prepare;
mod service;
pub use service::ImportService;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Component, Path, PathBuf},
    time::{Duration, Instant, UNIX_EPOCH},
};
use tempfile::TempDir;
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;

pub const COMMANDS: &[&str] = &["IMPORT_FILES"];
pub const FORMATS: &[&str] = &[
    "freeshow",
    "freeshow_project",
    "freeshow_template",
    "freeshow_overlay",
    "freeshow_theme",
    "freeshow_action",
    "freeshow_stage",
    "txt",
    "chordpro",
    "csv",
    "openlp",
    "opensong",
    "mediashout",
    "quelea",
    "songbeamer",
    "easyslides",
    "verseview",
    "videopsalm",
];
#[derive(Clone, Debug)]
pub struct ImportConfig {
    pub workers: usize,
    pub max_prepared: usize,
    pub max_files: usize,
    pub max_archives: usize,
    pub max_entries: usize,
    pub max_depth: usize,
    pub text_bytes: u64,
    pub input_bytes: u64,
    pub archive_bytes: u64,
    pub entry_bytes: u64,
    pub expanded_bytes: u64,
    pub output_bytes: usize,
    pub expansion_ratio: u64,
    pub operations: u64,
    pub timeout: Duration,
    pub staging_root: Option<PathBuf>,
}
impl Default for ImportConfig {
    fn default() -> Self {
        Self {
            workers: 2,
            max_prepared: 2,
            max_files: 64,
            max_archives: 16,
            max_entries: 2048,
            max_depth: 16,
            text_bytes: 16 * 1024 * 1024,
            input_bytes: 128 * 1024 * 1024,
            archive_bytes: 64 * 1024 * 1024,
            entry_bytes: 64 * 1024 * 1024,
            expanded_bytes: 128 * 1024 * 1024,
            output_bytes: 32 * 1024 * 1024,
            expansion_ratio: 200,
            operations: 100_000,
            timeout: Duration::from_secs(15),
            staging_root: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct ImportRoots {
    pub imports_root: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImportEvent {
    pub channel: String,
    pub message: ImportMessage,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImportMessage {
    pub channel: String,
    pub data: Value,
}
impl ImportEvent {
    pub(crate) fn new(channel: &str, data: Value) -> Self {
        Self {
            channel: "MAIN".into(),
            message: ImportMessage {
                channel: channel.into(),
                data,
            },
        }
    }
}
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ImportError {
    pub code: &'static str,
    pub message: String,
    pub prior_events: Vec<ImportEvent>,
}
impl ImportError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            prior_events: vec![],
        }
    }
    pub(crate) fn resource(message: impl Into<String>) -> Self {
        Self::new("import_resource_limit", message)
    }
    pub(crate) fn io(error: std::io::Error) -> Self {
        Self::new("import_io_error", error.to_string())
    }
    pub(crate) fn cancelled() -> Self {
        Self::new("cancelled", "Import request cancelled")
    }
}
pub type Result<T> = std::result::Result<T, ImportError>;

/// Always relative to ImportRoots.imports_root, never supplied by the request.
#[derive(Clone, Debug, Serialize)]
pub struct ImportRelativePath(PathBuf);
impl ImportRelativePath {
    pub(crate) fn new(path: PathBuf) -> Result<Self> {
        if path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            || !matches!(
                path.components()
                    .next()
                    .and_then(|c| c.as_os_str().to_str()),
                Some("Projects" | "Templates" | "Overlays")
            )
        {
            return Err(ImportError::new(
                "import_invalid_path",
                "Invalid media destination",
            ));
        }
        Ok(Self(path))
    }
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct FileIdentity {
    pub length: u64,
    pub modified_nanos: Option<u128>,
    #[cfg(unix)]
    pub device: u64,
    #[cfg(unix)]
    pub inode: u64,
}
impl FileIdentity {
    pub fn read(path: &Path) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(path).map_err(ImportError::io)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(ImportError::new(
                "import_invalid_path",
                "Expected a regular non-symlink media file",
            ));
        }
        Ok(Self {
            length: metadata.len(),
            modified_nanos: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|time| time.as_nanos()),
            #[cfg(unix)]
            device: {
                use std::os::unix::fs::MetadataExt;
                metadata.dev()
            },
            #[cfg(unix)]
            inode: {
                use std::os::unix::fs::MetadataExt;
                metadata.ino()
            },
        })
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DestinationGuard {
    Absent,
}
#[derive(Clone, Debug, Serialize)]
pub struct MediaInstall {
    pub staged_path: PathBuf,
    pub destination: ImportRelativePath,
    pub expected_source: FileIdentity,
    pub expected_destination: DestinationGuard,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct ExistingMediaGuard {
    pub destination: ImportRelativePath,
    pub expected: FileIdentity,
}
/// The host must revalidate runtime context, cancellation, root confinement,
/// staged identity/hash and destination absence under its ordered library owner.
/// This crate deliberately provides no final-destination write/commit operation.
pub struct MediaCommitPlan {
    pub imports_root: PathBuf,
    pub directories: Vec<ImportRelativePath>,
    pub installs: Vec<MediaInstall>,
    pub existing: Vec<ExistingMediaGuard>,
    pub unresolved: Vec<ImportRelativePath>,
    pub(crate) stage: Option<TempDir>,
    pub(crate) cancel: CancellationToken,
    pub(crate) request_cancel: CancellationToken,
    pub(crate) lease: Option<OwnedSemaphorePermit>,
    pub(crate) cleanup: service::CleanupSender,
}
impl MediaCommitPlan {
    pub fn ensure_current(&self) -> Result<()> {
        if self.cancel.is_cancelled() || self.request_cancel.is_cancelled() {
            Err(ImportError::cancelled())
        } else {
            Ok(())
        }
    }
    pub fn staging_directory(&self) -> Option<&Path> {
        self.stage.as_ref().map(TempDir::path)
    }
    pub fn manifest(&self) -> Value {
        json!({"importsRoot":self.imports_root,"directories":self.directories,"installs":self.installs,"existing":self.existing,"unresolved":self.unresolved,"cancelled":self.ensure_current().is_err()})
    }
}
impl Drop for MediaCommitPlan {
    fn drop(&mut self) {
        if let Some(stage) = self.stage.take() {
            self.cleanup.remove(stage, self.lease.take());
        }
    }
}
/// Dropping a prepared result queues private staging cleanup on a fixed reaper;
/// its retained lease is released only after cleanup, bounding disk retention.
pub struct PreparedImport {
    pub events: Vec<ImportEvent>,
    pub plan: MediaCommitPlan,
}

pub(crate) struct Budget {
    config: ImportConfig,
    cancel: CancellationToken,
    started: Instant,
    pub input: u64,
    pub expanded: u64,
    ops: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub entries: usize,
    pub references: usize,
    pub archives: usize,
    output: usize,
}
impl Budget {
    pub fn new(config: ImportConfig, cancel: CancellationToken) -> Self {
        Self {
            config,
            cancel,
            started: Instant::now(),
            input: 0,
            expanded: 0,
            ops: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            entries: 0,
            references: 0,
            archives: 0,
            output: 0,
        }
    }
    pub fn check(&mut self) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(ImportError::cancelled());
        }
        let operations = self.ops.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if operations >= self.config.operations || self.started.elapsed() > self.config.timeout {
            return Err(ImportError::resource("Import operation/deadline limit"));
        }
        Ok(())
    }
    pub fn io_guard(&self) -> IoGuard {
        IoGuard {
            cancel: self.cancel.clone(),
            started: self.started,
            ops: self.ops.clone(),
            limit: self.config.operations,
            timeout: self.config.timeout,
        }
    }
    pub fn charge_input(&mut self, bytes: u64) -> Result<()> {
        self.check()?;
        self.input = self
            .input
            .checked_add(bytes)
            .ok_or_else(|| ImportError::resource("Input byte overflow"))?;
        if self.input > self.config.input_bytes {
            return Err(ImportError::resource("Aggregate import input bytes"));
        }
        Ok(())
    }
    pub fn charge_output(&mut self, value: &Value) -> Result<()> {
        struct Count(usize);
        impl std::io::Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self.0.saturating_add(bytes.len());
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut count = Count(0);
        serde_json::to_writer(&mut count, value)
            .map_err(|error| ImportError::new("import_serialization", error.to_string()))?;
        self.output = self.output.saturating_add(count.0);
        if self.output > self.config.output_bytes {
            return Err(ImportError::resource("Aggregate import data bytes"));
        }
        Ok(())
    }
    pub fn charge_expanded(&mut self, bytes: u64) -> Result<()> {
        self.check()?;
        self.expanded = self
            .expanded
            .checked_add(bytes)
            .ok_or_else(|| ImportError::resource("Expanded byte overflow"))?;
        if self.expanded > self.config.expanded_bytes {
            return Err(ImportError::resource("Aggregate expanded import bytes"));
        }
        Ok(())
    }
}

/// Shared with compressed ZIP reads/seeks, so a decompressor consuming input
/// without producing output still observes the same request operation/deadline.
pub(crate) struct IoGuard {
    cancel: CancellationToken,
    started: Instant,
    ops: std::sync::Arc<std::sync::atomic::AtomicU64>,
    limit: u64,
    timeout: Duration,
}
impl IoGuard {
    fn check(&self) -> std::io::Result<()> {
        if self.cancel.is_cancelled()
            || self.ops.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= self.limit
            || self.started.elapsed() > self.timeout
        {
            Err(std::io::Error::other(
                "Import cancelled or IO budget exceeded",
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod text_tests;
