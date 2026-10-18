pub mod archive;
pub mod backup;
pub mod dialog;
pub mod protocol;
pub mod restore;
pub mod runtime;
pub mod show;
pub mod storage;

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
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
pub struct WireError {
    pub code: &'static str,
    pub message: String,
}

impl From<Error> for WireError {
    fn from(error: Error) -> Self {
        Self {
            code: match &error {
                Error::Unsupported(_) => "unsupported_command",
                Error::Invalid(_) => "invalid_request",
                Error::Json(_) => "invalid_json",
                Error::Io(_) => "io_error",
                Error::Archive(_) => "archive_error",
            },
            message: error.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
