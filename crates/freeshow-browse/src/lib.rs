//! Bounded read-only source-compatible filesystem browsing. No cache/media writes.
mod engine;
mod paths;
mod service;
mod stats;
use serde::Serialize;
use serde_json::Value;
pub use service::BrowseService;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
pub const COMMANDS: &[&str] = &["READ_FOLDER", "FILE_INFO", "GET_SIMILAR"];
#[derive(Clone, Debug)]
pub struct BrowseConfig {
    pub workers: usize,
    pub roots: usize,
    pub entries: usize,
    pub depth: usize,
    pub path_bytes: usize,
    pub name_units: usize,
    pub output_bytes: usize,
    pub operations: u64,
    pub levenshtein_cells: u64,
    pub substring_units: u64,
    pub timeout: Duration,
}
impl Default for BrowseConfig {
    fn default() -> Self {
        Self {
            workers: 2,
            roots: 64,
            entries: 10000,
            depth: 16,
            path_bytes: 32768,
            name_units: 2048,
            output_bytes: 16 * 1024 * 1024,
            operations: 100000,
            levenshtein_cells: 10000000,
            substring_units: 10000000,
            timeout: Duration::from_secs(10),
        }
    }
}
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailIntent {
    pub input: String,
    pub size: u32,
    pub reason: ThumbnailReason,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ThumbnailReason {
    FolderCapture,
    RootMedia,
}
#[derive(Clone, Debug, Serialize)]
pub struct BrowseDiagnostic {
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct BrowseReply {
    pub data: Value,
    pub thumbnail_intents: Vec<ThumbnailIntent>,
    pub diagnostics: Vec<BrowseDiagnostic>,
}
/// A captured listing whose thumbnail intents must be completed before native
/// publication. Retrying with capture disabled changes depth/noMedia semantics.
#[derive(Debug)]
pub struct PreparedListing {
    pub data: Value,
    pub thumbnail_intents: Vec<ThumbnailIntent>,
    pub diagnostics: Vec<BrowseDiagnostic>,
}
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct BrowseError {
    pub code: &'static str,
    pub message: String,
    pub thumbnail_intents: Vec<ThumbnailIntent>,
}
impl BrowseError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            thumbnail_intents: vec![],
        }
    }
    fn resource(message: impl Into<String>) -> Self {
        Self::new("browse_resource_limit", message)
    }
    fn cancelled() -> Self {
        Self::new("cancelled", "Browse request cancelled")
    }
}
pub type Result<T> = std::result::Result<T, BrowseError>;
pub(crate) enum Request {
    FileInfo(String),
    Folder {
        roots: Vec<String>,
        depth: f64,
        generate: bool,
        capture: bool,
    },
    Similar(Vec<String>),
}
impl Request {
    fn parse(command: &str, payload: &Value, config: &BrowseConfig) -> Result<Self> {
        fn path(value: &Value, config: &BrowseConfig) -> Result<String> {
            let path = value.as_str().ok_or_else(|| {
                BrowseError::new("invalid_request", "Browse path must be a string")
            })?;
            if path.len() > config.path_bytes || path.contains('\0') {
                return Err(BrowseError::resource("Browse path length/NUL limit"));
            }
            Ok(path.into())
        }
        fn paths(value: &Value, config: &BrowseConfig) -> Result<Vec<String>> {
            let paths = value.as_array().ok_or_else(|| {
                BrowseError::new("invalid_request", "Browse paths must be an array")
            })?;
            if paths.len() > config.roots {
                return Err(BrowseError::resource("Browse root count limit"));
            }
            paths.iter().map(|value| path(value, config)).collect()
        }
        match command {
            "FILE_INFO" => Ok(Self::FileInfo(path(payload, config)?)),
            "GET_SIMILAR" => Ok(Self::Similar(paths(&payload["paths"], config)?)),
            "READ_FOLDER" => {
                let roots = if payload["path"].is_string() {
                    vec![path(&payload["path"], config)?]
                } else {
                    paths(&payload["path"], config)?
                };
                let depth = match payload.get("depth") {
                    None | Some(Value::Null) => 0.0,
                    Some(value) => value.as_f64().ok_or_else(|| {
                        BrowseError::new("invalid_request", "Browse depth must be numeric")
                    })?,
                };
                if depth > config.depth as f64 {
                    return Err(BrowseError::resource("Browse requested depth limit"));
                }
                for flag in ["generateThumbnails", "captureFolderContent"] {
                    if payload
                        .get(flag)
                        .is_some_and(|value| !value.is_null() && !value.is_boolean())
                    {
                        return Err(BrowseError::new(
                            "invalid_request",
                            "Browse options must be booleans",
                        ));
                    }
                }
                Ok(Self::Folder {
                    roots,
                    depth,
                    generate: payload["generateThumbnails"].as_bool().unwrap_or(false),
                    capture: payload["captureFolderContent"].as_bool().unwrap_or(false),
                })
            }
            _ => Err(BrowseError::new(
                "unsupported_command",
                "Unsupported browse command",
            )),
        }
    }
}
pub(crate) struct Budget {
    config: BrowseConfig,
    cancel: CancellationToken,
    request_cancel: CancellationToken,
    started: Instant,
    operations: u64,
    entries: usize,
    strings: usize,
    cells: u64,
    substrings: u64,
}
impl Budget {
    fn check(&mut self) -> Result<()> {
        if self.cancel.is_cancelled() || self.request_cancel.is_cancelled() {
            return Err(BrowseError::cancelled());
        }
        self.operations += 1;
        if self.operations > self.config.operations || self.started.elapsed() > self.config.timeout
        {
            return Err(BrowseError::resource("Browse operation/deadline limit"));
        }
        Ok(())
    }
    fn entry(&mut self) -> Result<()> {
        self.check()?;
        self.entries += 1;
        if self.entries > self.config.entries {
            return Err(BrowseError::resource("Aggregate directory entry limit"));
        }
        Ok(())
    }
    fn string(&mut self, value: &str) -> Result<()> {
        self.check()?;
        if value.len() > self.config.path_bytes || value.contains('\0') {
            return Err(BrowseError::resource("Discovered path length/NUL limit"));
        }
        self.strings = self.strings.saturating_add(value.len());
        if self.strings > self.config.output_bytes {
            return Err(BrowseError::resource("Aggregate retained path bytes"));
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;

/// Bounded case-sensitive UTF-16 similarity, independent of filesystem/OS.
pub fn similarity(left: &str, right: &str) -> Result<f64> {
    engine::score(
        left,
        right,
        &mut Budget {
            config: BrowseConfig::default(),
            cancel: CancellationToken::new(),
            request_cancel: CancellationToken::new(),
            started: Instant::now(),
            operations: 0,
            entries: 0,
            strings: 0,
            cells: 0,
            substrings: 0,
        },
    )
}
