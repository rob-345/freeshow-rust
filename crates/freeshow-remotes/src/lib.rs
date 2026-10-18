//! Native REMOTE/STAGE/CONTROLLER transport. The host owns main-window IPC,
//! frozen library context, retained renderer authorization and client assets.
mod json;
mod semantics;
mod server;
mod state;
mod static_files;
pub use semantics::{get_os, parse_json, prepare_start};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use server::RemoteService;
pub use state::RemoteEvents;
use std::{
    collections::BTreeMap,
    fmt,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum ServerKind {
    REMOTE,
    STAGE,
    CONTROLLER,
}
impl ServerKind {
    pub const ALL: [Self; 3] = [Self::REMOTE, Self::STAGE, Self::CONTROLLER];
    pub fn channel(self) -> &'static str {
        match self {
            Self::REMOTE => "REMOTE",
            Self::STAGE => "STAGE",
            Self::CONTROLLER => "CONTROLLER",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::REMOTE => "remote",
            Self::STAGE => "stage",
            Self::CONTROLLER => "controller",
        }
    }
    pub fn port(self) -> u16 {
        match self {
            Self::REMOTE => 5510,
            Self::STAGE => 5511,
            Self::CONTROLLER => 5512,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct RemoteGeneration {
    pub context: u64,
    pub run: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct BoundServers {
    pub generation: RemoteGeneration,
    pub endpoints: BTreeMap<ServerKind, SocketAddr>,
}
#[derive(Clone, Debug, Serialize)]
pub struct RemoteEvent {
    pub generation: RemoteGeneration,
    pub sequence: u64,
    pub server: ServerKind,
    #[serde(flatten)]
    pub kind: RemoteEventKind,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteEventKind {
    Renderer {
        source_socket: Option<String>,
        message: Value,
    },
    Diagnostic {
        code: &'static str,
        message: String,
    },
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RemoteError {
    pub code: &'static str,
    pub message: String,
}
impl RemoteError {
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
impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for RemoteError {}
pub type Result<T> = std::result::Result<T, RemoteError>;
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;
#[derive(Clone)]
pub struct RemoteConfig {
    pub bind_ip: IpAddr,
    pub max_tcp_connections: usize,
    pub max_engine_sessions: usize,
    pub max_clients: usize,
    pub data_events: usize,
    pub command_capacity: usize,
    pub max_socket_payload: usize,
    pub socket_buffer_packets: usize,
    pub handshake_timeout: Duration,
    pub body_timeout: Duration,
    pub max_response_ids: usize,
    pub static_roots: BTreeMap<ServerKind, PathBuf>,
    pub static_workers: usize,
    pub static_queue: usize,
    pub max_static_bytes: u64,
    pub clock: Clock,
}
impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            bind_ip: Ipv6Addr::UNSPECIFIED.into(),
            max_tcp_connections: 256,
            max_engine_sessions: 192,
            max_clients: 192,
            data_events: 256,
            command_capacity: 64,
            max_socket_payload: 1_000_000,
            socket_buffer_packets: 32,
            handshake_timeout: Duration::from_secs(45),
            body_timeout: Duration::from_secs(10),
            max_response_ids: 256,
            static_roots: BTreeMap::new(),
            static_workers: 2,
            static_queue: 8,
            max_static_bytes: 16 * 1024 * 1024,
            clock: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u64::MAX as u128) as u64
            }),
        }
    }
}
impl RemoteConfig {
    fn validate(&self) -> Result<()> {
        for (name, value, max) in [
            ("tcp", self.max_tcp_connections, 4096),
            ("engine_sessions", self.max_engine_sessions, 4096),
            ("clients", self.max_clients, 4096),
            ("data_events", self.data_events, 65536),
            ("commands", self.command_capacity, 256),
            ("payload", self.max_socket_payload, 16_777_216),
            ("socket_queue", self.socket_buffer_packets, 256),
            ("response_ids", self.max_response_ids, 4096),
            ("static_workers", self.static_workers, 4),
            ("static_queue", self.static_queue, 64),
        ] {
            if value == 0 || value > max {
                return Err(RemoteError::new(
                    "invalid_configuration",
                    format!("Invalid {name} limit"),
                ));
            }
        }
        if self.max_static_bytes == 0
            || self.max_static_bytes > 64 * 1024 * 1024
            || [self.handshake_timeout, self.body_timeout]
                .into_iter()
                .any(|v| v.is_zero() || v > Duration::from_secs(60))
        {
            return Err(RemoteError::new(
                "invalid_configuration",
                "Invalid static/time limit",
            ));
        }
        for root in self.static_roots.values() {
            static_files::validate_root(root)?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct StartSpec {
    pub ports: BTreeMap<ServerKind, u16>,
    pub max: Option<Value>,
    pub data: BTreeMap<ServerKind, Value>,
}
#[derive(Debug, Serialize)]
pub struct RemoteStats {
    pub generation: RemoteGeneration,
    pub tcp_connections: usize,
    pub engine_sessions: usize,
    pub clients: BTreeMap<ServerKind, usize>,
    pub subscribers: usize,
    pub queued_commands: usize,
    pub runtime_tasks: usize,
    pub dropped_diagnostics: u64,
    pub static_requests: usize,
}
