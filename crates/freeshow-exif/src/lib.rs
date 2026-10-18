//! Source-compatible metadata values with bounded fixed-worker file/parsing IO.
mod parser;
mod service;
mod tags;
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use service::{ExifService, Status};
use std::{
    fmt,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    pub runtime_id: String,
    pub library_generation: u64,
    pub data_path: String,
}
#[derive(Debug, Clone)]
pub struct Control {
    pub deadline: Instant,
    pub cancel: CancellationToken,
}
impl Default for Control {
    fn default() -> Self {
        Self {
            deadline: Instant::now() + Duration::from_secs(2),
            cancel: CancellationToken::new(),
        }
    }
}
impl Control {
    pub(crate) fn check(&self, closing: &AtomicBool) -> Result<(), Error> {
        if closing.load(Ordering::Acquire) {
            return Err(Error::new("exif_shutdown", "EXIF service is shut down"));
        }
        if self.cancel.is_cancelled() {
            return Err(Error::new("exif_cancelled", "EXIF request cancelled"));
        }
        if Instant::now() >= self.deadline {
            return Err(Error::new("exif_deadline", "EXIF request deadline elapsed"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct Limits {
    pub queue_capacity: usize,
    pub input_bytes: usize,
    pub path_bytes: usize,
    pub tags: usize,
    pub components: usize,
    pub depth: usize,
    pub allocation_bytes: usize,
    pub output_bytes: usize,
    pub operations: usize,
    pub max_job_duration: Duration,
    pub shutdown_timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            queue_capacity: 8,
            input_bytes: 16 * 1024 * 1024,
            path_bytes: 32768,
            tags: 1024,
            components: 65536,
            depth: 3,
            allocation_bytes: 64 * 1024 * 1024,
            output_bytes: 2 * 1024 * 1024,
            operations: 1_000_000,
            max_job_duration: Duration::from_secs(10),
            shutdown_timeout: Duration::from_secs(2),
        }
    }
}
impl Limits {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        for (value, hard) in [
            (self.queue_capacity, 64),
            (self.input_bytes, 64 * 1024 * 1024),
            (self.path_bytes, 32768),
            (self.tags, 8192),
            (self.components, 1_048_576),
            (self.depth, 8),
            (self.allocation_bytes, 128 * 1024 * 1024),
            (self.output_bytes, 8 * 1024 * 1024),
            (self.operations, 4_000_000),
        ] {
            if value == 0 || value > hard {
                return Err(Error::new("exif_invalid", "EXIF limits exceed hard bounds"));
            }
        }
        if self.max_job_duration.is_zero()
            || self.max_job_duration > Duration::from_secs(60)
            || self.shutdown_timeout.is_zero()
            || self.shutdown_timeout > Duration::from_secs(10)
        {
            return Err(Error::new(
                "exif_invalid",
                "EXIF duration limits exceed hard bounds",
            ));
        }
        Ok(())
    }
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
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: &'static str,
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum NumberKind {
    NaN,
    PositiveInfinity,
    NegativeInfinity,
    NegativeZero,
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NumberHydration {
    pub path: String,
    pub kind: NumberKind,
}
#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Hydration {
    pub undefined_paths: Vec<String>,
    pub special_numbers: Vec<NumberHydration>,
    pub byte_buffers: Vec<String>,
}
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub input_bytes: usize,
    pub tags: usize,
    pub directories: usize,
    pub operations: usize,
    pub allocated_bytes: usize,
    pub output_bytes: usize,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExifOutcome {
    pub context: Context,
    pub reply: Value,
    pub hydration: Hydration,
    pub diagnostics: Vec<Diagnostic>,
    pub source_compatible: bool,
    pub stats: Stats,
}

pub(crate) fn missing(
    context: Context,
    id: String,
    code: &'static str,
    message: &'static str,
    compatible: bool,
    stats: Stats,
) -> ExifOutcome {
    ExifOutcome {
        context,
        reply: serde_json::json!({"id":id}),
        hydration: Hydration {
            undefined_paths: vec!["/exif".into()],
            ..Hydration::default()
        },
        diagnostics: vec![Diagnostic { code, message }],
        source_compatible: compatible,
        stats,
    }
}
/// Bounded synchronous parsing for fixtures or already bounded host bytes.
/// Production path reads use ExifService's two dedicated workers.
pub fn parse_bytes(
    id: String,
    context: Context,
    data: &[u8],
    limits: &Limits,
    mut control: Control,
) -> Result<ExifOutcome, Error> {
    limits.validate()?;
    service::validate_input(&id, &context, limits)?;
    control.deadline = control
        .deadline
        .min(Instant::now() + limits.max_job_duration);
    let closing = AtomicBool::new(false);
    let outcome = parser::parse(id, context, data, limits, &control, &closing)?;
    parser::finish(outcome, limits, &control, &closing)
}

#[cfg(test)]
mod tests;
