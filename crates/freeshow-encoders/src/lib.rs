//! Encoder preparation only: the host owns executable resolution, app-cache
//! persistence, frozen contexts and capture reconfiguration.
mod json;
mod process;
pub mod profiles;
mod service;
#[cfg(test)]
mod tests;
use serde::Serialize;
use serde_json::{json, Value};
pub use service::EncoderService;
use std::{fmt, path::PathBuf, time::Duration};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct EncoderError {
    pub code: &'static str,
    pub message: String,
}
impl EncoderError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > 4096 {
            let mut end = 4080;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            message.push_str(" [truncated]");
        }
        Self { code, message }
    }
}
impl fmt::Display for EncoderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for EncoderError {}
pub type Result<T> = std::result::Result<T, EncoderError>;

#[derive(Clone, Debug)]
pub struct EncoderConfig {
    pub max_probes: usize,
    pub max_callers: usize,
    pub process_timeout: Duration,
    pub operation_timeout: Duration,
    pub listing_limit: usize,
    pub encode_limit: usize,
    pub cache_limit: usize,
    pub max_settings: usize,
}
impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            max_probes: 2,
            max_callers: 16,
            process_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(35),
            listing_limit: 4 * 1024 * 1024,
            encode_limit: 1024 * 1024,
            cache_limit: 4 * 1024 * 1024,
            max_settings: 256,
        }
    }
}
impl EncoderConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.max_probes == 0
            || self.max_probes > 4
            || self.max_callers == 0
            || self.max_callers > 64
            || self.max_settings == 0
            || self.max_settings > 4096
            || self.process_timeout.is_zero()
            || self.process_timeout > Duration::from_secs(10)
            || self.operation_timeout < self.process_timeout
            || self.operation_timeout > Duration::from_secs(60)
            || [self.listing_limit, self.encode_limit, self.cache_limit]
                .into_iter()
                .any(|v| v == 0 || v > 4 * 1024 * 1024)
        {
            return Err(EncoderError::new(
                "invalid_configuration",
                "Encoder supervision limits are invalid",
            ));
        }
        Ok(())
    }
}
/// Native-only authority. The path is already resolved/trusted by the host;
/// source_path preserves the original cache-key spelling independently.
#[derive(Clone, Debug)]
pub struct FrozenExecutable {
    pub path: PathBuf,
    pub source_path: String,
    pub mtime_ms: f64,
}
#[derive(Clone, Debug)]
pub struct FrozenDetectionInput {
    pub generation: u64,
    pub executable: Option<FrozenExecutable>,
    pub platform: String,
    pub arch: String,
    pub vaapi_device: String,
    pub cache: Option<Vec<u8>>,
}
impl FrozenDetectionInput {
    pub fn cache_key(&self) -> Result<Option<String>> {
        let Some(executable) = &self.executable else {
            return Ok(None);
        };
        if !executable.mtime_ms.is_finite() {
            return Err(EncoderError::new(
                "nonfinite_number",
                "Executable mtimeMs must be finite",
            ));
        }
        Ok(Some(format!(
            "1|{}|{}|{}|{}",
            executable.source_path,
            profiles::number(executable.mtime_ms),
            self.platform,
            self.arch
        )))
    }
    pub(crate) fn validate(&self, config: &EncoderConfig) -> Result<()> {
        if self.platform.len() > 128
            || self.arch.len() > 128
            || self.vaapi_device.len() > 4096
            || self
                .cache
                .as_ref()
                .is_some_and(|v| v.len() > config.cache_limit)
        {
            return Err(EncoderError::new(
                "input_limit",
                "Encoder frozen input exceeds a resource limit",
            ));
        }
        if let Some(executable) = &self.executable {
            if !executable.path.is_absolute()
                || executable.path.as_os_str().as_encoded_bytes().len() > 16384
                || executable.source_path.len() > 16384
            {
                return Err(EncoderError::new(
                    "invalid_executable",
                    "A trusted absolute executable path and bounded source spelling are required",
                ));
            }
        }
        self.cache_key()?;
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionSource {
    Missing,
    Cache,
    Probe,
}
#[derive(Clone, Debug, Serialize)]
pub struct PreparedDetection {
    pub generation: u64,
    pub probe_id: u64,
    pub source: DetectionSource,
    /// Raw cache detection is intentionally not schema-normalized.
    pub detection: Value,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip)]
    pub cache_write: Option<CacheWriteIntent>,
}
#[derive(Clone, Debug, Serialize)]
pub struct PreparedResolution {
    pub encoder: String,
    /// Present when resolving required detection. The host must commit its
    /// cache intent just as for ENCODER_DETECT before discarding this envelope.
    pub detection: Option<PreparedDetection>,
}
/// Not deserializable; a host must validate generation and recomputed binary key
/// under its trusted owner before atomically committing these bytes.
#[derive(Clone, Debug)]
pub struct CacheWriteIntent {
    generation: u64,
    key: String,
    bytes: Vec<u8>,
}
impl CacheWriteIntent {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn ensure_current(&self, generation: u64, key: &str) -> Result<()> {
        if self.generation != generation || self.key != key {
            return Err(EncoderError::new(
                "stale_generation",
                "Encoder cache intent no longer matches its frozen owner/binary",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct SettingIntent {
    pub output_id: String,
    pub encoder: String,
    pub reconfigure_rtmp: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct EncoderStats {
    pub admitted_callers: usize,
    pub active_probes: usize,
    pub active_children: usize,
    pub children_started: u64,
    pub children_reaped: u64,
}

pub fn first_error_line(stderr: &str, killed: bool, message: Option<&str>) -> String {
    let line = stderr
        .split('\n')
        .map(|line| line.trim_matches(profiles::whitespace))
        .find(|line| !line.is_empty());
    let text = if let Some(line) = line {
        line
    } else if killed {
        "Timed out"
    } else {
        message
            .filter(|message| !message.is_empty())
            .unwrap_or("Test encode failed")
    };
    String::from_utf16_lossy(&text.encode_utf16().take(200).collect::<Vec<_>>())
}
pub fn unavailable() -> Value {
    json!({"encoders":profiles::profiles("").iter().map(|p|json!({"id":p.id,"label":p.label,"hardware":p.hardware,"available":false,"reason":"FFmpeg not installed"})).collect::<Vec<_>>(),"recommended":"x264"})
}
/// Mirrors the runtime's `detectEncoders(data?.force)` truthiness. The host still
/// owns command admission and the trusted frozen executable/context.
pub fn force_requested(payload: Option<&Value>) -> bool {
    payload
        .and_then(|value| value.get("force"))
        .is_some_and(truthy)
}
pub fn resolve_detection(detection: &Value, setting: Option<&str>) -> Result<String> {
    if setting.is_none_or(|v| v.is_empty() || v == "auto") {
        return detection
            .get("recommended")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                EncoderError::new(
                    "invalid_cached_detection",
                    "Cached detection.recommended is not an encoder string",
                )
            });
    }
    let encoders = detection
        .get("encoders")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            EncoderError::new(
                "invalid_cached_detection",
                "Cached detection.encoders has no array find operation",
            )
        })?;
    for encoder in encoders {
        if encoder.is_null() {
            return Err(EncoderError::new(
                "invalid_cached_detection",
                "Null cached encoder has no id property",
            ));
        }
        if encoder.get("id").and_then(Value::as_str) == setting {
            return Ok(
                if truthy(encoder.get("available").unwrap_or(&Value::Null)) {
                    setting.unwrap().to_owned()
                } else {
                    "x264".into()
                },
            );
        }
    }
    Ok("x264".into())
}
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_some_and(|n| n != 0.0),
        Value::String(v) => !v.is_empty(),
        _ => true,
    }
}
