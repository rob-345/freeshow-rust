//! Bounded source-compatible FreeShow service publication, independent of IPC.
mod dns;
mod records;
mod service;
pub use dns::{decode, encode, Packet, Question, Record, RecordData, RecordType};
pub use records::{dns_sd_args, get_local_ips, service_records};
use serde::{Deserialize, Serialize};
pub use service::{DiscoveryEvents, DiscoveryService};
use std::{
    fmt,
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
    time::Duration,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryGeneration {
    pub context: u64,
    pub run: String,
}
impl DiscoveryGeneration {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.run.len() != 32 || !self.run.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(DiscoveryError::new(
                "invalid_discovery_generation",
                "Run ID must be 32 hexadecimal characters",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DiscoveryName {
    Remote,
    Stage,
    Controller,
    OutputStream,
}
impl DiscoveryName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Remote => "REMOTE",
            Self::Stage => "STAGE",
            Self::Controller => "CONTROLLER",
            Self::OutputStream => "OUTPUT_STREAM",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InterfaceAddress {
    pub address: String,
    pub family: serde_json::Value,
    pub internal: bool,
    #[serde(default)]
    pub mac: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkInterface {
    pub name: String,
    pub addresses: Vec<InterfaceAddress>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkSnapshot {
    pub hostname: String,
    pub interfaces: Vec<NetworkInterface>,
}
#[derive(Clone, Debug)]
pub enum DiscoveryTransport {
    /// Real IPv4 mDNS, matching the non-mac source dependency. The host grants
    /// LAN publication authority by choosing this transport.
    Multicast,
    /// Explicit controlled unicast transport; both endpoints must be loopback.
    Loopback {
        bind: SocketAddrV4,
        target: SocketAddrV4,
    },
}
#[derive(Clone, Debug)]
pub enum DiscoveryPlatform {
    Other,
    /// Only an absolute, trusted native executable. Never accepts IPC argv.
    Darwin {
        dns_sd: Option<PathBuf>,
    },
}
#[derive(Clone, Debug)]
pub struct DiscoveryConfig {
    pub platform: DiscoveryPlatform,
    pub transport: DiscoveryTransport,
    pub instance_id: [u8; 3],
    pub command_capacity: usize,
    pub event_capacity: usize,
    pub responses_per_second: usize,
    /// Source Math.random()*250 jitter, frozen by host for reproducibility.
    pub probe_jitter: Duration,
}
impl DiscoveryConfig {
    pub fn new(instance_id: [u8; 3]) -> Self {
        Self {
            platform: if cfg!(target_os = "macos") {
                DiscoveryPlatform::Darwin { dns_sd: None }
            } else {
                DiscoveryPlatform::Other
            },
            transport: DiscoveryTransport::Multicast,
            instance_id,
            command_capacity: 16,
            event_capacity: 64,
            responses_per_second: 64,
            probe_jitter: Duration::from_millis(125),
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoveryError {
    pub code: &'static str,
    pub message: String,
}
impl DiscoveryError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > 2048 {
            let mut n = 2030;
            while !message.is_char_boundary(n) {
                n -= 1;
            }
            message.truncate(n);
            message.push_str(" [truncated]");
        }
        Self { code, message }
    }
}
impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for DiscoveryError {}
pub type Result<T> = std::result::Result<T, DiscoveryError>;
#[derive(Clone, Debug, Serialize)]
pub struct BeginReceipt {
    pub generation: DiscoveryGeneration,
    pub endpoint: Option<SocketAddrV4>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishReceipt {
    Scheduled,
    AlreadyPresent,
}
#[derive(Clone, Debug, Serialize)]
pub struct DiscoveryEvent {
    pub generation: DiscoveryGeneration,
    pub name: Option<DiscoveryName>,
    #[serde(flatten)]
    pub kind: DiscoveryEventKind,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscoveryEventKind {
    Published,
    Spawned,
    Suppressed { reason: &'static str },
    Diagnostic { code: &'static str, message: String },
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct DiscoveryStats {
    pub generation: Option<DiscoveryGeneration>,
    pub services: usize,
    pub children: usize,
    pub socket_open: bool,
    pub admitted_jobs: usize,
    pub dropped_events: u64,
    pub dropped_responses: u64,
}
impl InterfaceAddress {
    pub(crate) fn is_v4(&self) -> bool {
        self.family.as_str() == Some("IPv4") || self.family.as_f64() == Some(4.0)
    }
}
impl NetworkSnapshot {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.interfaces.len() > 32
            || self.interfaces.iter().any(|i| {
                i.name.len() > 128
                    || i.addresses.len() > 16
                    || i.addresses.iter().any(|a| {
                        a.address.len() > 128
                            || a.mac.len() > 32
                            || !matches!(&a.family,serde_json::Value::String(s) if s.len()<=16)
                                && !a.family.is_number()
                    })
            })
        {
            return Err(DiscoveryError::new(
                "discovery_snapshot_limit",
                "Network snapshot exceeds limits",
            ));
        }
        for (index, interface) in self.interfaces.iter().enumerate() {
            if self.interfaces[..index]
                .iter()
                .any(|previous| previous.name == interface.name)
            {
                return Err(DiscoveryError::new(
                    "invalid_discovery_snapshot",
                    "Interface names must be unique",
                ));
            }
        }
        dns::validate_name(&self.hostname)?;
        Ok(())
    }
    pub(crate) fn source_order(&self) -> Vec<&NetworkInterface> {
        let mut interfaces: Vec<_> = self.interfaces.iter().enumerate().collect();
        interfaces.sort_by_key(|(index, interface)| {
            let number = interface
                .name
                .parse::<u32>()
                .ok()
                .filter(|n| *n != u32::MAX && n.to_string() == interface.name);
            (number.is_none(), number.unwrap_or(0), *index)
        });
        interfaces
            .into_iter()
            .map(|(_, interface)| interface)
            .collect()
    }
    pub(crate) fn memberships(&self) -> Vec<Ipv4Addr> {
        self.source_order()
            .into_iter()
            .filter_map(|i| {
                i.addresses
                    .iter()
                    .find(|a| a.is_v4())
                    .and_then(|a| a.address.parse().ok())
            })
            .collect()
    }
}
