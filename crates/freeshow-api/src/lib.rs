//! Inbound FreeShow REST, genuine Socket.IO, and OSC API service.
//! Renderer event routing and application context validation belong to the host.
mod broker;
mod json;
mod osc;
mod semantics;
mod server;

pub use semantics::{auth_token, parse_query, rest_data};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use server::{ApiService, ApiStats};
use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4},
    time::Duration,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApiGeneration {
    pub context: u64,
    pub run: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct BoundEndpoints {
    pub generation: ApiGeneration,
    pub websocket: SocketAddr,
    pub rest: SocketAddr,
    pub osc: SocketAddr,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApiEvent {
    pub generation: ApiGeneration,
    #[serde(flatten)]
    pub kind: ApiEventKind,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApiEventKind {
    Connected,
    Trigger { data: Value },
    DynamicValues { listener_id: String, keys: Value },
    Diagnostic { code: &'static str, message: String },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
}
impl ApiError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ApiError {}

#[derive(Clone, Debug)]
pub struct ApiConfig {
    /// Production TCP defaults to Node's dual-stack wildcard; tests use IPv4 loopback.
    pub bind_ip: IpAddr,
    pub event_capacity: usize,
    pub max_tcp_connections: usize,
    pub max_engine_sessions: usize,
    pub max_inflight: usize,
    pub max_client_inflight: usize,
    pub max_pending_replies: usize,
    pub max_socket_payload: usize,
    pub socket_buffer_packets: usize,
    pub max_http_body: usize,
    pub request_timeout: Duration,
    pub dynamic_timeout: Duration,
    pub body_timeout: Duration,
    pub handshake_timeout: Duration,
    /// Original osc-js sends all responses to localhost:41235, not the sender.
    pub osc_response_target: SocketAddrV4,
}
impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            bind_ip: Ipv6Addr::UNSPECIFIED.into(),
            event_capacity: 256,
            max_tcp_connections: 128,
            max_engine_sessions: 64,
            max_inflight: 128,
            max_client_inflight: 8,
            max_pending_replies: 128,
            max_socket_payload: 1_000_000,
            socket_buffer_packets: 32,
            max_http_body: 102_400,
            request_timeout: Duration::from_secs(10),
            dynamic_timeout: Duration::from_secs(15),
            body_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(45),
            osc_response_target: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 41235),
        }
    }
}
impl ApiConfig {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        for (name, value, max) in [
            ("event_capacity", self.event_capacity, 65536),
            ("max_tcp_connections", self.max_tcp_connections, 4096),
            ("max_engine_sessions", self.max_engine_sessions, 4096),
            ("max_inflight", self.max_inflight, 4096),
            ("max_client_inflight", self.max_client_inflight, 256),
            ("max_pending_replies", self.max_pending_replies, 4096),
            ("max_socket_payload", self.max_socket_payload, 16_777_216),
            ("socket_buffer_packets", self.socket_buffer_packets, 256),
            ("max_http_body", self.max_http_body, 1_048_576),
        ] {
            if value == 0 || value > max {
                return Err(ApiError::new(
                    "invalid_config",
                    format!("{name} must be in 1..={max}"),
                ));
            }
        }
        for duration in [
            self.request_timeout,
            self.dynamic_timeout,
            self.body_timeout,
            self.handshake_timeout,
        ] {
            if duration.is_zero() || duration > Duration::from_secs(60) {
                return Err(ApiError::new(
                    "invalid_config",
                    "timeouts must be in (0,60s]",
                ));
            }
        }
        if !self.osc_response_target.ip().is_loopback() || self.osc_response_target.port() == 0 {
            return Err(ApiError::new(
                "invalid_config",
                "OSC responses require a nonzero loopback destination",
            ));
        }
        Ok(())
    }
}
