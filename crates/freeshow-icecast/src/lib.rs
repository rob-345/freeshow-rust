//! Bounded, single-owner transport for already encoded Opus packets.
//! Connection status means locally written headers, not server authorization.
mod config;
mod ogg;
mod resolver;
mod service;

pub use config::Config;
pub use ogg::{crc32, opus_tags, OggState, OPUS_HEAD, SILENCE};
use serde::Serialize;
pub use service::IcecastService;
use std::time::Duration;

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    pub(crate) fn new(code: &'static str, text: impl ToString) -> Self {
        let text = text.to_string();
        let mut end = text.len().min(2048);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            code,
            message: text[..end].into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ContextId(pub u64, pub u64);
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Status {
    pub connected: bool,
    pub connecting: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct StatusEvent {
    pub context: ContextId,
    pub run: u64,
    pub data: Status,
    /// Native cause metadata, never part of the original ICECAST_STATUS payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<Error>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Disposition {
    Ignored,
    Disabled,
    ConnectingDropped,
    CooldownDropped,
    Written,
    Updated,
    Disconnected,
}
/// Local socket effects only. No IPC acknowledgement or remote acceptance claim.
#[derive(Debug, Serialize)]
pub struct Receipt {
    context: ContextId,
    run: u64,
    disposition: Disposition,
}
impl Receipt {
    pub fn context(&self) -> ContextId {
        self.context
    }
    pub fn run(&self) -> u64 {
        self.run
    }
    pub fn disposition(&self) -> Disposition {
        self.disposition
    }
}

#[derive(Clone, Debug)]
pub struct Limits {
    pub queue: usize,
    pub queued_bytes: usize,
    pub packet_bytes: usize,
    pub metadata_bytes: usize,
    pub header_bytes: usize,
    pub response_bytes: usize,
    pub events: usize,
    pub connect_timeout: Duration,
    pub write_timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            queue: 16,
            queued_bytes: 256 * 1024,
            packet_bytes: 16384,
            metadata_bytes: 8192,
            header_bytes: 16384,
            response_bytes: 65536,
            events: 64,
            connect_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(2),
        }
    }
}
impl Limits {
    fn validate(&self) -> Result<()> {
        if !(1..=64).contains(&self.queue)
            || !(1..=65024).contains(&self.packet_bytes)
            || self.queued_bytes < self.packet_bytes + self.metadata_bytes
            || self.queued_bytes > 4 * 1024 * 1024
            || !(256..=32768).contains(&self.metadata_bytes)
            || !(1024..=65536).contains(&self.header_bytes)
            || !(1024..=1024 * 1024).contains(&self.response_bytes)
            || !(4..=1024).contains(&self.events)
            || self.connect_timeout.is_zero()
            || self.connect_timeout > Duration::from_secs(30)
            || self.write_timeout.is_zero()
            || self.write_timeout > Duration::from_secs(10)
        {
            return Err(Error::new(
                "invalid_limits",
                "Icecast limits exceed hard bounds",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
