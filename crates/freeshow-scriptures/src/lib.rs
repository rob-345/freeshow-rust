//! Read-only local .fsb preparation. Trusted host owns directory effects/context.
mod engine;
#[cfg(target_os = "linux")]
mod json;
mod service;
use serde::Serialize;
use serde_json::Value;
pub use service::{ScriptureService, Status};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
pub use tokio_util::sync::CancellationToken;
pub const COMMAND: &str = "BIBLE";
pub const WORKERS: usize = 2;
const MAX_PATH: usize = 32768;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn limit(message: &str) -> Self {
        Self::new("scripture_limit", message)
    }
    fn unsupported(message: &str) -> Self {
        Self::new("scripture_unsupported", message)
    }
    fn cancelled() -> Self {
        Self::new("scripture_cancelled", "Scripture request cancelled")
    }
    fn shutdown() -> Self {
        Self::new("scripture_shutdown", "Scripture admission is closed")
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Debug)]
pub struct Limits {
    pub queued_jobs: usize,
    pub path_bytes: usize,
    /// Aggregate across both original loadFile reads, not per file.
    pub read_bytes: usize,
    pub allocation_bytes: usize,
    pub output_bytes: usize,
    pub nodes: usize,
    pub depth: usize,
    pub string_bytes: usize,
    pub verses: usize,
    pub operations: u64,
    pub timeout: Duration,
    pub shutdown_timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            queued_jobs: 8,
            path_bytes: MAX_PATH,
            read_bytes: 16 * 1024 * 1024,
            allocation_bytes: 128 * 1024 * 1024,
            output_bytes: 32 * 1024 * 1024,
            nodes: 500000,
            depth: 32,
            string_bytes: 4 * 1024 * 1024,
            verses: 100000,
            operations: 40000000,
            timeout: Duration::from_secs(10),
            shutdown_timeout: Duration::from_secs(3),
        }
    }
}
impl Limits {
    fn validate(&self) -> Result<()> {
        if !(1..=32).contains(&self.queued_jobs)
            || !(1..=MAX_PATH).contains(&self.path_bytes)
            || !(1..=32 * 1024 * 1024).contains(&self.read_bytes)
            || !(4096..=256 * 1024 * 1024).contains(&self.allocation_bytes)
            || !(2..=64 * 1024 * 1024).contains(&self.output_bytes)
            || !(1..=1000000).contains(&self.nodes)
            || !(1..=64).contains(&self.depth)
            || !(1..=8 * 1024 * 1024).contains(&self.string_bytes)
            || !(1..=200000).contains(&self.verses)
            || !(1..=100000000).contains(&self.operations)
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(120)
            || self.shutdown_timeout.is_zero()
            || self.shutdown_timeout > Duration::from_secs(30)
        {
            return Err(Error::new(
                "invalid_configuration",
                "Invalid scripture resource limits",
            ));
        }
        Ok(())
    }
}
/// Native-only trusted roots, never deserialize these from renderer input.
#[derive(Debug, Clone)]
pub struct ScriptureContext {
    bibles: PathBuf,
    legacy: PathBuf,
    directory_attempted: bool,
}
impl ScriptureContext {
    pub fn new(data_root: impl AsRef<Path>, documents_root: impl AsRef<Path>) -> Result<Self> {
        fn root(path: &Path) -> Result<&str> {
            let text = path
                .to_str()
                .ok_or_else(|| Error::unsupported("Non-Unicode trusted root"))?;
            if !path.is_absolute() || text.contains('\0') {
                return Err(Error::new(
                    "invalid_context",
                    "Scripture roots must be absolute without NUL",
                ));
            }
            if text.len() > MAX_PATH - 8 {
                return Err(Error::limit("Trusted scripture root length"));
            }
            Ok(text)
        }
        let data = root(data_root.as_ref())?;
        let docs = root(documents_root.as_ref())?;
        Ok(Self {
            bibles: join(data, "Bibles").into(),
            legacy: join(docs, "Bibles").into(),
            directory_attempted: false,
        })
    }
    pub fn bibles_root(&self) -> &Path {
        &self.bibles
    }
    pub fn legacy_root(&self) -> &Path {
        &self.legacy
    }
    /// Host calls only after the ordered mkdir attempt (including failure/ALERT).
    /// A failed mkdir must still let the original legacy fallback run.
    pub fn after_directory_attempt(&self) -> Self {
        let mut next = self.clone();
        next.directory_attempted = true;
        next
    }
}
fn join(parent: &str, child: &str) -> String {
    #[cfg(target_os = "linux")]
    {
        let text = format!("{parent}/{child}");
        let mut parts = Vec::new();
        for part in text.split('/') {
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
    #[cfg(not(target_os = "linux"))]
    {
        Path::new(parent).join(child).to_string_lossy().into_owned()
    }
}
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FolderIntent {
    EnsureBiblesDirectory { path: PathBuf },
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub code: &'static str,
    pub operation: &'static str,
    pub os_code: Option<i32>,
    pub source_error_logged: bool,
}
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub load_paths: Vec<String>,
    pub read_bytes: usize,
    pub allocated_bytes: usize,
    pub parsed_nodes: usize,
    pub visited_verses: usize,
    pub migrated_verses: usize,
    pub operations: u64,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedScriptureReply {
    /// None only while directory preparation is required; never publish that.
    pub data: Option<Value>,
    pub diagnostics: Vec<Diagnostic>,
    pub folder_intents: Vec<FolderIntent>,
    pub stats: Stats,
}
impl PreparedScriptureReply {
    pub fn is_complete(&self) -> bool {
        self.data.is_some() && self.folder_intents.is_empty()
    }
}
struct Request {
    context: ScriptureContext,
    id: Value,
    name: String,
}
struct Budget {
    limits: Limits,
    cancel: CancellationToken,
    caller: CancellationToken,
    service: CancellationToken,
    deadline: Instant,
    stats: Stats,
    allocated: usize,
    #[cfg(all(test, target_os = "linux"))]
    hooks: std::sync::Arc<service::Hooks>,
}
impl Budget {
    fn check(&mut self) -> Result<()> {
        if self.service.is_cancelled() {
            return Err(Error::shutdown());
        }
        if self.cancel.is_cancelled() || self.caller.is_cancelled() {
            return Err(Error::cancelled());
        }
        if Instant::now() >= self.deadline {
            return Err(Error::new(
                "scripture_deadline",
                "Scripture deadline elapsed",
            ));
        }
        self.stats.operations = self.stats.operations.saturating_add(1);
        if self.stats.operations > self.limits.operations {
            return Err(Error::limit("Scripture operation bound"));
        }
        Ok(())
    }
    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.check()?;
        if bytes > self.limits.allocation_bytes.saturating_sub(self.allocated) {
            return Err(Error::limit("Scripture allocation budget"));
        }
        self.allocated += bytes;
        self.stats.allocated_bytes = self.stats.allocated_bytes.max(self.allocated);
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_some_and(|v| v != 0.0),
        Value::String(v) => !v.is_empty(),
        _ => true,
    }
}
#[cfg(all(test, target_os = "linux"))]
mod tests;
