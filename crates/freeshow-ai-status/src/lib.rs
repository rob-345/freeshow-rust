//! Fixed Nemotron integrity query. Call only on an owned filesystem worker.
//! This crate creates no thread/runtime and cannot certify its host's IO drain.
use serde::Serialize;
use std::{
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
pub use tokio_util::sync::CancellationToken as CancelToken;
mod engine;
mod stamp;

pub const REVISION: &str = "cba1c96ca5ef0e8393b50584ae153a79145dc492";
pub const FILES: [(&str, &str); 5] = [
    (
        "encoder.int8.onnx",
        "2fff2166acaa535bd969fb223c1f0783d71029f143cb298bc54c2afe85abf772",
    ),
    (
        "decoder.int8.onnx",
        "19f9c98fc6d0a2c33a65a43b36fdb2e914c26c0aa9764be3aebc502a1e982fb0",
    ),
    (
        "joiner.int8.onnx",
        "4101c7c679a0bc30483794b27a059e34e79232aa2068d78d51231a22c8b0d7ce",
    ),
    (
        "tokens.txt",
        "729cc103155bafa785f9cd45746cd41cabe97eab7182fc04d594129587958f8a",
    ),
    (
        "silero_vad.onnx",
        "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
    ),
];
pub const MAX_PATH: usize = 32768;
pub const STAMP_BYTES: usize = 65536;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: &'static str,
}
impl Error {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
    fn limit() -> Self {
        Self::new(
            "ai_status_limit",
            "Nemotron status exceeds bounded native work",
        )
    }
    fn unsupported() -> Self {
        Self::new(
            "ai_status_unsupported",
            "Nemotron status representation is not certified",
        )
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}

/// Native app userData, never selected library dataPath or renderer customPath.
pub struct TrustedUserData(PathBuf);
impl TrustedUserData {
    pub fn from_host(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let text = path.to_str().ok_or_else(Error::unsupported)?;
        if text.len() > MAX_PATH {
            return Err(Error::limit());
        }
        if !path.is_absolute()
            || text.contains('\0')
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
        {
            return Err(Error::unsupported());
        }
        #[cfg(windows)]
        {
            if !matches!(path.components().next(),Some(Component::Prefix(p)) if matches!(p.kind(),std::path::Prefix::Disk(_)|std::path::Prefix::VerbatimDisk(_)))
            {
                return Err(Error::unsupported());
            }
            for component in path.components().filter_map(|c| {
                if let Component::Normal(n) = c {
                    n.to_str()
                } else {
                    None
                }
            }) {
                let stem = component
                    .split('.')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_uppercase();
                if component.ends_with(['.', ' '])
                    || component
                        .chars()
                        .any(|c| c <= '\u{1f}' || ":\"<>|?*".contains(c))
                    || component.encode_utf16().count() > 255
                    || matches!(
                        stem.as_str(),
                        "CON"
                            | "PRN"
                            | "AUX"
                            | "NUL"
                            | "CONIN$"
                            | "CONOUT$"
                            | "COM¹"
                            | "COM²"
                            | "COM³"
                            | "LPT¹"
                            | "LPT²"
                            | "LPT³"
                    )
                    || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                        && stem.len() == 4
                        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
                {
                    return Err(Error::unsupported());
                }
            }
        }
        Ok(Self(path))
    }
}
/// Typed native selection preserves JavaScript falsiness before JSON projection.
pub enum ModelId<'a> {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    Text(&'a str),
    Other,
}
pub struct NemotronNoModel(());
impl NemotronNoModel {
    pub fn explicit(model: ModelId<'_>) -> Result<Self> {
        let falsy = match model {
            ModelId::Undefined | ModelId::Null | ModelId::Bool(false) => true,
            ModelId::Number(n) => n == 0.0 || n.is_nan(),
            ModelId::Text(s) => s.is_empty(),
            _ => false,
        };
        if falsy {
            Ok(Self(()))
        } else {
            Err(Error::unsupported())
        }
    }
    pub fn from_payload(data: &serde_json::Value) -> Result<Self> {
        fn bound(v: &serde_json::Value, d: usize, n: &mut usize, b: &mut usize) -> Result<()> {
            *n += 1;
            if d > 16 || *n > 4096 {
                return Err(Error::limit());
            }
            match v {
                serde_json::Value::String(s) => {
                    *b = b.checked_add(s.len()).ok_or_else(Error::limit)?
                }
                serde_json::Value::Array(a) => {
                    for v in a {
                        bound(v, d + 1, n, b)?
                    }
                }
                serde_json::Value::Object(o) => {
                    for (k, v) in o {
                        *b = b.checked_add(k.len()).ok_or_else(Error::limit)?;
                        bound(v, d + 1, n, b)?
                    }
                }
                _ => {}
            }
            if *b > STAMP_BYTES {
                return Err(Error::limit());
            }
            Ok(())
        }
        bound(data, 0, &mut 0, &mut 0)?;
        if data.get("engineId").and_then(serde_json::Value::as_str) != Some("nemotron") {
            return Err(Error::unsupported());
        }
        Self::explicit(match data.get("modelId") {
            None => ModelId::Undefined,
            Some(serde_json::Value::Null) => ModelId::Null,
            Some(serde_json::Value::Bool(v)) => ModelId::Bool(*v),
            Some(serde_json::Value::Number(n)) => {
                ModelId::Number(n.as_f64().ok_or_else(Error::unsupported)?)
            }
            Some(serde_json::Value::String(s)) => ModelId::Text(s),
            _ => ModelId::Other,
        })
    }
}
#[derive(Clone, Copy)]
pub enum Platform {
    Linux,
    MacArm,
    MacIntel,
    Windows,
    Unsupported,
}
impl Platform {
    pub fn native() -> Self {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", _) => Self::Linux,
            ("macos", "aarch64") => Self::MacArm,
            ("macos", "x86_64") => Self::MacIntel,
            ("windows", _) => Self::Windows,
            _ => Self::Unsupported,
        }
    }
}
#[derive(Clone)]
pub struct Limits {
    pub hash_bytes: u64,
    pub operations: usize,
    pub timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            hash_bytes: 1024 * 1024 * 1024,
            operations: 32768,
            timeout: Duration::from_secs(10),
        }
    }
}
#[derive(Clone)]
pub struct Control {
    pub cancel: CancelToken,
    pub deadline: Instant,
}
impl Default for Control {
    fn default() -> Self {
        Self {
            cancel: CancelToken::new(),
            deadline: Instant::now() + Duration::from_secs(10),
        }
    }
}
struct Config {
    root: PathBuf,
    limits: Limits,
}
#[derive(Clone)]
pub struct Checker(Arc<Config>);
fn slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(4))).clone()
}
impl Checker {
    pub fn new(root: TrustedUserData, limits: Limits) -> Result<Self> {
        if limits.hash_bytes == 0
            || limits.hash_bytes > 1024 * 1024 * 1024
            || limits.operations == 0
            || limits.operations > 32768
            || limits.timeout.is_zero()
            || limits.timeout > Duration::from_secs(60)
        {
            return Err(Error::limit());
        }
        Ok(Self(Arc::new(Config {
            root: root.0,
            limits,
        })))
    }
    pub fn retained(&self) -> usize {
        4 - slots().available_permits()
    }
    pub fn try_begin(
        &self,
        _query: NemotronNoModel,
        platform: Platform,
        mut control: Control,
    ) -> Result<Job> {
        control.deadline = control.deadline.min(Instant::now() + self.0.limits.timeout);
        let permit = slots().try_acquire_owned().map_err(|_| {
            Error::new("ai_status_busy", "Whole Nemotron status capacity exhausted")
        })?;
        control.cancel = control.cancel.child_token();
        let job = Job {
            config: self.0.clone(),
            control,
            platform,
            _permit: permit,
            operations: 0,
            hash_bytes: 0,
            evidence: Evidence::default(),
            #[cfg(test)]
            after_chunk: None,
        };
        job.check()?;
        Ok(job)
    }
}
#[cfg_attr(
    not(feature = "test-harness"),
    doc = r#"
The fixture composition method is unavailable in ordinary builds:
```compile_fail
fn unavailable(job: freeshow_ai_status::Job) {
    job.prepare_fixed_tiny_fixture();
}
```
"#
)]
pub struct Job {
    config: Arc<Config>,
    control: Control,
    platform: Platform,
    _permit: OwnedSemaphorePermit,
    operations: usize,
    hash_bytes: u64,
    evidence: Evidence,
    #[cfg(test)]
    after_chunk: Option<fn(&mut Job)>,
}
impl Job {
    fn check(&self) -> Result<()> {
        if self.control.cancel.is_cancelled() {
            Err(Error::new(
                "ai_status_cancelled",
                "Nemotron status cancelled",
            ))
        } else if Instant::now() >= self.control.deadline {
            Err(Error::new(
                "ai_status_deadline",
                "Accepted Nemotron status deadline elapsed",
            ))
        } else {
            Ok(())
        }
    }
    fn step(&mut self) -> Result<()> {
        self.check()?;
        self.operations += 1;
        if self.operations > self.config.limits.operations {
            Err(Error::limit())
        } else {
            Ok(())
        }
    }
    fn path(&self, name: Option<&str>) -> Result<PathBuf> {
        let mut path = self.config.root.join("bin").join("nemotron").join("models");
        if let Some(name) = name {
            path.push(name)
        }
        if path.to_str().is_none_or(|s| s.len() > MAX_PATH) {
            Err(Error::limit())
        } else {
            Ok(path)
        }
    }
    pub fn prepare(self) -> Result<Prepared> {
        engine::prepare(self, None)
    }
    /// Test-only composition boundary. Reads/hashes every fixed file, bypassing
    /// stamp reuse. Only ASCII filename bytes without newline classify as the
    /// pinned model digests. Never enable for a normal application build/root.
    #[cfg(feature = "test-harness")]
    pub fn prepare_fixed_tiny_fixture(self) -> Result<Prepared> {
        engine::prepare_fixed_tiny_fixture(self)
    }
    fn ready(self, status: EngineStatus) -> Result<StatusRead> {
        self.check()?;
        Ok(StatusRead {
            reply: Reply { nemotron: status },
            job: self,
            write_diagnostic: None,
        })
    }
}
pub enum Prepared {
    Ready(StatusRead),
    Stamp(StampIntent),
}
#[derive(Serialize)]
struct EngineStatus {
    ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
    #[serde(rename = "localPath", skip_serializing_if = "Option::is_none")]
    local_path: Option<String>,
}
impl EngineStatus {
    fn failed(error: &'static str) -> Self {
        Self {
            ready: false,
            error: Some(error),
            local_path: None,
        }
    }
    fn ok(path: &Path) -> Self {
        Self {
            ready: true,
            error: None,
            local_path: Some(path.to_str().expect("validated path").into()),
        }
    }
}
#[derive(Serialize)]
pub struct Reply {
    nemotron: EngineStatus,
}
#[derive(Default, Serialize)]
pub struct Evidence {
    pub fast_path: bool,
    pub hashed: Vec<&'static str>,
    pub stats: Vec<StatObservation>,
}
#[derive(Serialize)]
pub struct StatObservation {
    pub file: &'static str,
    pub size_bits: String,
    pub mtime_bits: String,
}
pub struct StatusRead {
    reply: Reply,
    job: Job,
    write_diagnostic: Option<WriteDiagnostic>,
}
impl StatusRead {
    pub fn reply(&self) -> &Reply {
        &self.reply
    }
    pub fn evidence(&self) -> &Evidence {
        &self.job.evidence
    }
    pub fn write_diagnostic(&self) -> Option<&WriteDiagnostic> {
        self.write_diagnostic.as_ref()
    }
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub enum WriteDiagnostic {
    Failed,
}
pub struct StampIntent {
    job: Job,
    body: Vec<u8>,
}
impl StampIntent {
    pub fn user_data(&self) -> &Path {
        &self.job.config.root
    }
    /// Must run on the host's actual ordered filesystem owner. The accepted
    /// control/deadline cannot be replaced; no ready result exists before IO.
    pub fn apply(mut self) -> Result<StatusRead> {
        self.job.step()?;
        let path = self.job.path(Some("model.json"))?;
        engine::namespace(&mut self.job, &path)?;
        let failed = engine::write_stamp(&path, &self.body).is_err();
        self.job.check()?;
        let model_dir = self.job.path(None)?;
        let mut read = self.job.ready(EngineStatus::ok(&model_dir))?;
        read.write_diagnostic = failed.then_some(WriteDiagnostic::Failed);
        Ok(read)
    }
}
#[cfg(test)]
mod tests;
