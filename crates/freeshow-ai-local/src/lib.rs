//! Read-only AI_GET_BIN inventory. Native code alone supplies the app-data root.
//! No model setup, download, deletion, executable probing or network authority.
use serde::Serialize;
use std::{
    fmt,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
mod engine;
mod service;
pub use service::{InventoryRead, InventoryService, Status, Ticket};
pub use tokio_util::sync::CancellationToken as CancelToken;

pub const MAX_EXACT_SIZE: u64 = (1u64 << 53) - 1;
const MAX_PATH: usize = 32768;
const MAX_RESULT: usize = 65536;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub message: &'static str,
}
impl Error {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
    fn resource() -> Self {
        Self::new(
            "ai_local_limit",
            "Local inventory exceeds bounded filesystem work",
        )
    }
    fn unsupported() -> Self {
        Self::new(
            "ai_local_unsupported",
            "Local inventory path or metadata representation is not certified",
        )
    }
    fn closed() -> Self {
        Self::new("ai_local_shutdown", "Local inventory admission is closed")
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}

/// Opaque native setup authority. No Deserialize, Clone or renderer path input.
pub struct TrustedUserData(PathBuf);
impl TrustedUserData {
    pub fn from_host(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let text = path.to_str().ok_or_else(Error::unsupported)?;
        if text.len() > MAX_PATH {
            return Err(Error::resource());
        }
        if text.contains('\0')
            || !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
        {
            return Err(Error::unsupported());
        }
        #[cfg(windows)]
        if !matches!(path.components().next(),Some(Component::Prefix(prefix)) if matches!(prefix.kind(),std::path::Prefix::Disk(_)|std::path::Prefix::VerbatimDisk(_)))
        {
            return Err(Error::unsupported());
        }
        Ok(Self(path))
    }
}
#[derive(Debug, Clone)]
pub struct Limits {
    pub capacity: usize,
    pub directories: usize,
    pub entries: usize,
    pub depth: usize,
    pub steps: usize,
    pub path_bytes: usize,
    pub job_timeout: Duration,
    pub shutdown_timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            capacity: 4,
            directories: 1024,
            entries: 16384,
            depth: 32,
            steps: 65536,
            path_bytes: 8 * 1024 * 1024,
            job_timeout: Duration::from_secs(2),
            shutdown_timeout: Duration::from_secs(2),
        }
    }
}
impl Limits {
    fn validate(&self) -> Result<()> {
        if !(1..=4).contains(&self.capacity)
            || !(1..=1024).contains(&self.directories)
            || !(1..=16384).contains(&self.entries)
            || self.depth > 32
            || !(1..=65536).contains(&self.steps)
            || !(1..=8 * 1024 * 1024).contains(&self.path_bytes)
            || self.job_timeout.is_zero()
            || self.job_timeout > Duration::from_secs(2)
            || self.shutdown_timeout.is_zero()
            || self.shutdown_timeout > Duration::from_secs(2)
        {
            return Err(Error::new(
                "ai_local_invalid",
                "Local inventory limits exceed hard bounds",
            ));
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct Control {
    pub cancel: CancellationToken,
    pub deadline: Instant,
}
impl Default for Control {
    fn default() -> Self {
        Self {
            cancel: CancellationToken::new(),
            deadline: Instant::now() + Duration::from_secs(2),
        }
    }
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct BinFile {
    pub path: String,
    pub name: &'static str,
    pub size: u64,
}

fn path_text(path: &Path) -> Result<&str> {
    let text = path.to_str().ok_or_else(Error::unsupported)?;
    if text.len() > MAX_PATH {
        Err(Error::resource())
    } else {
        Ok(text)
    }
}
#[cfg(test)]
mod tests;
