pub mod archive;
pub mod backup;
pub mod dialog;
pub mod location;
pub mod output;
pub mod protocol;
pub mod restore;
pub mod runtime;
pub mod service;
pub mod show;
pub mod storage;
mod transaction;

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("library worker unavailable: {0}")]
    WorkerUnavailable(String),
    #[error("request belongs to a previous library or runtime session")]
    StaleLibrary {
        recovery_path: Option<std::path::PathBuf>,
        recovery_error: Option<String>,
    },
    #[error("unsupported command: {0}")]
    Unsupported(String),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("file operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive operation failed: {0}")]
    Archive(#[from] zip::result::ZipError),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireError {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_path: Option<std::path::PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_error: Option<String>,
}

impl WireError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            recovery_path: None,
            recovery_error: None,
        }
    }
}

impl From<Error> for WireError {
    fn from(error: Error) -> Self {
        let mut wire = Self::new(
            match &error {
                Error::WorkerUnavailable(_) => "library_worker_unavailable",
                Error::StaleLibrary { .. } => "stale_library",
                Error::Unsupported(_) => "unsupported_command",
                Error::Invalid(_) => "invalid_request",
                Error::Json(_) => "invalid_json",
                Error::Io(_) => "io_error",
                Error::Archive(_) => "archive_error",
            },
            error.to_string(),
        );
        if let Error::StaleLibrary {
            recovery_path,
            recovery_error,
        } = error
        {
            wire.recovery_path = recovery_path;
            wire.recovery_error = recovery_error;
        }
        wire
    }
}

pub type Result<T> = std::result::Result<T, Error>;
