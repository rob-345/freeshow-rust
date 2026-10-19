//! Trusted native cover/image ownership. No deserializable path or renderer API.
mod codec;
mod extract;
mod service;
use serde::Serialize;
use serde_json::Value;
pub use service::{CoverImageService, Observation, Session, Status, Ticket};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
pub const MAX_SESSIONS: usize = 4;
pub const WORKERS: usize = 2;
pub const SESSION_RESERVATION: usize = 128 * 1024 * 1024;
pub const MAX_ENCODED: usize = 4 * 1024 * 1024;
pub const MAX_METADATA: usize = 8 * 1024 * 1024;
pub const MAX_PIXELS: usize = 4 * 1024 * 1024;
pub const MAX_SIDE: u32 = 4096;
pub const MAX_OPERATIONS: usize = 128;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > 1024 {
            let mut at = 1024;
            while !message.is_char_boundary(at) {
                at -= 1
            }
            message.truncate(at);
        }
        Self { code, message }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
pub(crate) fn resource(message: impl Into<String>) -> Error {
    Error::new("cover_resource_limit", message)
}
pub(crate) fn unsupported(message: impl Into<String>) -> Error {
    Error::new("cover_unsupported", message)
}
pub(crate) fn invalid(message: impl Into<String>) -> Error {
    Error::new("cover_invalid", message)
}
#[derive(Clone, Debug)]
pub struct Context {
    pub runtime: String,
    pub generation: u64,
    pub data_path: String,
    seal: Arc<()>,
}
impl Context {
    pub fn from_host(
        runtime: impl Into<String>,
        generation: u64,
        data_path: impl Into<String>,
    ) -> Result<Self> {
        let runtime = runtime.into();
        let data_path = data_path.into();
        if runtime.is_empty()
            || runtime.len() > 256
            || data_path.is_empty()
            || data_path.len() > 32768
            || runtime.contains('\0')
            || data_path.contains('\0')
        {
            return Err(resource("Invalid full native context"));
        }
        Ok(Self {
            runtime,
            generation,
            data_path,
            seal: Arc::new(()),
        })
    }
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.seal, &other.seal)
            && self.runtime == other.runtime
            && self.generation == other.generation
            && self.data_path == other.data_path
    }
}
/// Host must authorize this path from a committed/granted source, not renderer text.
#[derive(Debug)]
pub struct TrustedMediaFile {
    context: Context,
    path: PathBuf,
    opened: Option<std::fs::File>,
}
impl TrustedMediaFile {
    /// Preferred when an existing native authorizer has opened and validated
    /// canonical identity/protected roots. The owned descriptor follows the job.
    pub fn from_opened_host(
        context: &Context,
        source_path: impl Into<PathBuf>,
        file: std::fs::File,
    ) -> Result<Self> {
        let mut trusted = Self::from_host(context, source_path)?;
        trusted.opened = Some(file);
        Ok(trusted)
    }
    pub fn from_host(context: &Context, path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let s = path
            .to_str()
            .ok_or_else(|| unsupported("Non-Unicode native path"))?;
        if !path.is_absolute() || s.len() > 32768 || s.contains('\0') {
            return Err(resource("Expected bounded authorized absolute media path"));
        }
        Ok(Self {
            context: context.clone(),
            path,
            opened: None,
        })
    }
}
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);
impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release)
    }
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
/// JPEG decoder, partial-alpha and alternate resize are deliberately not certified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImagePolicy {
    VerifiedPngPixels,
    AllowNativeResizeDifference,
}
#[derive(Debug, Clone, Serialize)]
pub struct Picture {
    pub fields: Value,
    pub data: Vec<u8>,
    pub undefined_fields: Vec<&'static str>,
}
#[derive(Debug, Serialize)]
pub enum Reply {
    Covers(Vec<Picture>),
    Image {
        width: u32,
        height: u32,
    },
    Bytes {
        data: Vec<u8>,
        diagnostics: Vec<&'static str>,
    },
    Released,
}
#[cfg(test)]
mod tests;
