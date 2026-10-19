//! Linux desktop Spotify state and bounded command transport, not the Spotify Web API.
mod commands;
mod parser;
mod process;
mod service;
pub use commands::{CommandOutcome, PreparedCommand};
pub use parser::{parse_linux, NumberKind, NumberRestoration, State, WireState};
use serde::Serialize;
pub use service::{
    CommandControl, CommandFinished, CommandReceipt, Receipt, SpotifyService, Ticket,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub type Result<T> = std::result::Result<T, Error>;
pub const JOB_CAPACITY: usize = 4;
pub const OBSERVER_CAPACITY: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: &'static str,
}
impl Error {
    pub(crate) fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}

/// A full native provenance snapshot plus a private per-owner seal. No Deserialize.
#[derive(Clone)]
pub struct Owner {
    context: Context,
    seal: Arc<()>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    pub runtime_id: String,
    pub library_generation: u64,
    pub data_path: PathBuf,
}
impl Owner {
    pub fn new(runtime_id: &str, library_generation: u64, data_path: &Path) -> Result<Self> {
        if runtime_id.is_empty()
            || runtime_id.len() > 128
            || runtime_id.contains('\0')
            || data_path.as_os_str().len() > 32768
            || data_path.to_str().is_none_or(|p| p.contains('\0'))
        {
            return Err(Error::new("spotify_invalid", "Invalid native context"));
        }
        Ok(Self {
            context: Context {
                runtime_id: runtime_id.into(),
                library_generation,
                data_path: data_path.into(),
            },
            seal: Arc::new(()),
        })
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
    pub(crate) fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.seal, &other.seal)
    }
}

/// Construct only at the trusted native boundary, never from command payloads.
/// Captures existing session transport and locale once; does not log them.
#[derive(Clone)]
pub struct Backend {
    pub(crate) executable: PathBuf,
    pub(crate) env: Vec<(&'static str, OsString)>,
    pub(crate) unavailable: Option<Error>,
}
impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Backend { native configuration: <redacted> }")
    }
}
impl Backend {
    pub fn capture(executable: &Path) -> Result<Self> {
        if !executable.is_absolute()
            || executable.as_os_str().len() > 32768
            || executable.to_str().is_none_or(|p| p.contains('\0'))
        {
            return Err(Error::new(
                "spotify_invalid",
                "Executable must be an absolute native Unicode path",
            ));
        }
        let mut env = Vec::new();
        let mut bytes = 0usize;
        for key in [
            "DBUS_SESSION_BUS_ADDRESS",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "HOME",
        ] {
            if let Some(value) = std::env::var_os(key) {
                bytes = bytes.saturating_add(value.len());
                if bytes > 16384 || value.to_str().is_none_or(|v| v.contains('\0')) {
                    return Err(Error::new(
                        "spotify_environment",
                        "Native session environment exceeds supported bounds",
                    ));
                }
                env.push((key, value));
            }
        }
        let address = env
            .iter()
            .find(|(k, _)| *k == "DBUS_SESSION_BUS_ADDRESS")
            .and_then(|(_, v)| v.to_str());
        let unavailable = if !cfg!(target_os = "linux") {
            Some(Error::new(
                "spotify_unsupported",
                "Only Linux desktop Spotify transport is implemented",
            ))
        } else if address.is_none_or(|v| !v.starts_with("unix:") || v.contains(';')) {
            Some(Error::new(
                "spotify_session_unavailable",
                "A captured single existing Unix session bus is required",
            ))
        } else {
            None
        };
        Ok(Self {
            executable: executable.into(),
            env,
            unavailable,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Limits {
    pub query_timeout: Duration,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            query_timeout: Duration::from_secs(2),
            stdout_bytes: 256 * 1024,
            stderr_bytes: 64 * 1024,
        }
    }
}
impl Limits {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.query_timeout.is_zero()
            || self.query_timeout > Duration::from_secs(10)
            || self.stdout_bytes == 0
            || self.stdout_bytes > 1024 * 1024
            || self.stderr_bytes == 0
            || self.stderr_bytes > 256 * 1024
        {
            return Err(Error::new("spotify_invalid", "Invalid resource limits"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Capability {
    Pending,
    Available,
    Unavailable { code: &'static str },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NullReason {
    MissingTitle,
    ServiceUnknown,
    ProcessFailure,
}
#[derive(Debug)]
pub enum Outcome {
    State(State),
    SourceNull(NullReason),
}
impl Outcome {
    pub fn wire(&self) -> WireState {
        match self {
            Self::State(state) => state.wire(),
            Self::SourceNull(_) => WireState::null(),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub context: Context,
    pub closing: bool,
    pub running: bool,
    pub joined: bool,
    /// Actual native thread join and all spawned direct children waited.
    /// Completed/uncollected results remain separately charged in retained.
    pub drained: bool,
    pub retained: usize,
    pub observers: usize,
    pub active_pid: u32,
    /// Bounded actual command child registry, separate from the sequential query lane.
    pub command_pids: Vec<u32>,
    pub commands: u64,
    pub commands_completed: u64,
    pub command_failures: u64,
    pub last_command_error: Option<Error>,
    pub spawned: u64,
    pub reaped: u64,
    pub queries: u64,
    pub completed: u64,
    pub capability: Capability,
    pub terminal_error: Option<Error>,
}
