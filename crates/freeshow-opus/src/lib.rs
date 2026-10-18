//! Owned fixed-format Opus encoding for the retained AUDIO → Icecast pipeline.
//! Library selection is a trusted native host decision, never a renderer command.
mod native;
mod service;
pub use native::LibrarySource;
use serde::Serialize;
pub use service::{EncodedPacket, OpusService};
use std::time::Duration;

pub const RATE: i32 = 48000;
pub const CHANNELS: i32 = 2;
pub const SAMPLES: usize = 960;
pub const PCM_BYTES: usize = SAMPLES * 2 * 2;
pub const MAX_PACKET: usize = 3 * 1276;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_code: Option<i32>,
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
            native_code: None,
        }
    }
    pub(crate) fn native(code: &'static str, value: i32) -> Self {
        Self {
            code,
            message: format!("libopus returned {value}"),
            native_code: Some(value),
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
pub struct NativeInfo {
    pub version: String,
    pub library: String,
    pub state_bytes: usize,
    pub lookahead: i32,
    pub default_bitrate: i32,
    pub rate: i32,
    pub channels: i32,
    pub frame_samples: usize,
    pub max_packet_bytes: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Capability {
    Available(NativeInfo),
    Unavailable(Error),
}
impl Capability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }
}
#[derive(Clone, Debug)]
pub struct Limits {
    pub queue: usize,
    pub retained: usize,
    pub call_budget: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            queue: 4,
            retained: 8,
            call_budget: Duration::from_millis(250),
        }
    }
}
impl Limits {
    fn validate(&self) -> Result<()> {
        if !(1..=16).contains(&self.queue)
            || !(1..=32).contains(&self.retained)
            || self.call_budget.is_zero()
            || self.call_budget > Duration::from_secs(2)
        {
            return Err(Error::new(
                "invalid_limits",
                "Opus limits exceed hard bounds",
            ));
        }
        Ok(())
    }
}
pub(crate) trait Codec: Send {
    fn info(&self) -> NativeInfo;
    fn encode(&mut self, pcm: &[u8; PCM_BYTES]) -> Result<Vec<u8>>;
    fn dispose(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

#[cfg(any(test, feature = "test-harness"))]
pub mod testing {
    //! Codec evidence helpers only; never part of desktop IPC.
    use super::*;
    pub struct Decoder(native::Decoder);
    impl Decoder {
        pub fn open(source: LibrarySource) -> Result<Self> {
            native::Decoder::open(source).map(Self)
        }
        pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>> {
            self.0.decode(packet)
        }
        pub fn version(&self) -> &str {
            self.0.version()
        }
    }
}
