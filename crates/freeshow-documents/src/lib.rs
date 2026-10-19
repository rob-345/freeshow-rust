//! Bounded, read-only preparation for the original READ_FILE / READ_BIBLES_FOLDER.
//! Host context validation, root creation and reply publication stay host-owned.
mod engine;
mod service;
use serde::Serialize;
use serde_json::Value;
pub use service::DocumentService;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
pub use tokio_util::sync::CancellationToken;
pub const COMMANDS: &[&str] = &["READ_FILE", "READ_BIBLES_FOLDER"];
pub const WORKERS: usize = 2;
const MAX_PATH_BYTES: usize = 32768;

#[derive(Clone, Debug)]
pub struct DocumentConfig {
    pub queued_jobs: usize,
    pub path_bytes: usize,
    pub read_bytes: usize,
    pub output_bytes: usize,
    pub entries: usize,
    pub operations: u64,
    pub timeout: Duration,
}
impl Default for DocumentConfig {
    fn default() -> Self {
        Self {
            queued_jobs: 8,
            path_bytes: MAX_PATH_BYTES,
            read_bytes: 8 * 1024 * 1024,
            output_bytes: 32 * 1024 * 1024,
            entries: 10000,
            operations: 500000,
            timeout: Duration::from_secs(10),
        }
    }
}
impl DocumentConfig {
    fn validate(&self) -> Result<()> {
        if !(1..=32).contains(&self.queued_jobs)
            || !(1..=MAX_PATH_BYTES).contains(&self.path_bytes)
            || !(1..=32 * 1024 * 1024).contains(&self.read_bytes)
            || !(2..=64 * 1024 * 1024).contains(&self.output_bytes)
            || !(1..=50000).contains(&self.entries)
            || !(1..=1_000_000).contains(&self.operations)
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(120)
        {
            return Err(DocumentError::new(
                "invalid_configuration",
                "Invalid document resource limits",
            ));
        }
        Ok(())
    }
}
/// Frozen trusted host configuration, never constructed from renderer payloads.
/// Original Bibles paths are derived with Node POSIX join semantics on Linux.
#[derive(Clone, Debug)]
pub struct DocumentContext {
    data_root: PathBuf,
    bibles_root: PathBuf,
}
impl DocumentContext {
    pub fn new(data_root: impl AsRef<Path>) -> Result<Self> {
        let root = data_root.as_ref();
        let text = root.to_str().ok_or_else(|| {
            DocumentError::new(
                "unsupported_path_encoding",
                "Trusted document root is not Unicode",
            )
        })?;
        if !root.is_absolute() || text.contains('\0') {
            return Err(DocumentError::new(
                "invalid_context",
                "Trusted data root must be absolute without NUL",
            ));
        }
        if text.len() > MAX_PATH_BYTES {
            return Err(DocumentError::resource("Trusted data root length limit"));
        }
        #[cfg(target_os = "linux")]
        let bibles = posix_join(text, "Bibles");
        #[cfg(not(target_os = "linux"))]
        let bibles = root
            .join("Bibles")
            .to_str()
            .ok_or_else(|| {
                DocumentError::new(
                    "unsupported_path_encoding",
                    "Trusted Bibles root is not Unicode",
                )
            })?
            .to_owned();
        if bibles.len() > MAX_PATH_BYTES {
            return Err(DocumentError::resource("Trusted Bibles root length limit"));
        }
        Ok(Self {
            data_root: root.to_owned(),
            bibles_root: bibles.into(),
        })
    }
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }
    pub fn bibles_root(&self) -> &Path {
        &self.bibles_root
    }
}
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FolderIntent {
    /// Original getDataFolderPath calls recursive mkdir before reading this root.
    /// Host must resolve this intent under its ordered, current-context owner.
    EnsureBiblesDirectory { path: PathBuf },
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentDiagnostic {
    pub code: &'static str,
    pub operation: &'static str,
    pub os_code: Option<i32>,
    pub source_error_logged: bool,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedDocumentReply {
    /// Exact source reply, ready to publish only when is_complete() and host
    /// context validation both succeed. An empty array can still require mkdir.
    pub data: Value,
    pub diagnostics: Vec<DocumentDiagnostic>,
    pub folder_intents: Vec<FolderIntent>,
}
impl PreparedDocumentReply {
    pub fn is_complete(&self) -> bool {
        self.folder_intents.is_empty()
    }
}
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct DocumentError {
    pub code: &'static str,
    pub message: String,
}
impl DocumentError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn resource(message: impl Into<String>) -> Self {
        Self::new("document_resource_limit", message)
    }
    fn cancelled() -> Self {
        Self::new("cancelled", "Document request cancelled")
    }
    fn shutdown() -> Self {
        Self::new("documents_shutdown", "Document service is shutting down")
    }
}
pub type Result<T> = std::result::Result<T, DocumentError>;
enum Request {
    File(String),
    Bibles(DocumentContext),
}
struct Budget {
    config: DocumentConfig,
    service_cancel: CancellationToken,
    cancel: CancellationToken,
    request_cancel: CancellationToken,
    started: Instant,
    operations: u64,
    #[cfg(test)]
    hooks: std::sync::Arc<service::Hooks>,
}
impl Budget {
    fn check(&mut self) -> Result<()> {
        if self.service_cancel.is_cancelled() {
            return Err(DocumentError::shutdown());
        }
        if self.cancel.is_cancelled() || self.request_cancel.is_cancelled() {
            return Err(DocumentError::cancelled());
        }
        if self.started.elapsed() >= self.config.timeout {
            return Err(DocumentError::new(
                "document_timeout",
                "Document request deadline exceeded",
            ));
        }
        self.operations += 1;
        if self.operations > self.config.operations {
            return Err(DocumentError::resource("Document operation limit"));
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn posix_join(parent: &str, name: &str) -> String {
    let joined = format!("{parent}/{name}");
    let mut parts = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}
