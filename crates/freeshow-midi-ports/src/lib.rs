//! Fixed-owner native MIDI IO. Host integration remains separate.
mod backend;
mod ports;
mod select;
mod service;
#[cfg(feature = "native")]
pub use backend::NativeBackend;
pub use backend::{Backend, Device, Direction, Input, InputCallback, Output};
use freeshow_midi::WireData;
pub use select::{select_devices, Selector};
use serde::Serialize;
use serde_json::Value;
pub use service::{EventStream, MidiService, Ticket};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
pub use tokio_util::sync::CancellationToken;
pub type Result<T> = std::result::Result<T, Error>;
pub const COMMAND_LIMIT: usize = 32;
pub const EVENT_LIMIT: usize = 256;
pub const DEVICE_LIMIT: usize = 256;
pub const INPUT_LIMIT: usize = 8;
pub const OUTPUT_LIMIT: usize = 16;
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    pub fn new(code: &'static str, message: &str) -> Self {
        let mut end = message.len().min(1024);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            code,
            message: message[..end].into(),
        }
    }
    fn limit() -> Self {
        Self::new("midi_limit", "Native MIDI resource bound exceeded")
    }
    fn closing() -> Self {
        Self::new("midi_shutdown", "Native MIDI owner is closing")
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
impl From<freeshow_midi::Error> for Error {
    fn from(e: freeshow_midi::Error) -> Self {
        Self::new(e.code, &e.message)
    }
}
fn pure_error(e: Error) -> freeshow_midi::Error {
    freeshow_midi::Error {
        code: e.code,
        name: "NativeError",
        message: e.message,
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Owner {
    runtime: String,
    generation: u64,
    data_path: String,
    run: u64,
}
impl Owner {
    /// Native-only context/run capability. Never deserialize from renderer data.
    pub fn new(runtime: &str, generation: u64, data_path: &str, run: u64) -> Result<Self> {
        if runtime.is_empty()
            || runtime.len() > 256
            || data_path.len() > 32768
            || runtime.contains('\0')
            || data_path.contains('\0')
            || generation > 9007199254740991
            || run > 9007199254740991
        {
            return Err(Error::limit());
        }
        Ok(Self {
            runtime: runtime.into(),
            generation,
            data_path: data_path.into(),
            run,
        })
    }
}
#[derive(Debug, Clone)]
pub struct Config {
    pub idle_poll: Duration,
    pub shutdown_timeout: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            idle_poll: Duration::from_millis(100),
            shutdown_timeout: Duration::from_secs(5),
        }
    }
}
pub enum Command {
    Devices(Direction),
    Send(Value),
    Listen(Value),
    CloseInputs(String),
    Stop,
    MtcListen(Selector),
    MtcClose(u64),
    MtcSend {
        selector: Selector,
        time_ms: f64,
        framerate: f64,
    },
    MtcStop,
}
impl Command {
    /// Original CLOSE_MIDI receives `data.id`; preserve its truthiness and JS
    /// property-key coercion rather than silently dropping numeric IDs.
    pub fn close_inputs_field(value: Option<&Value>) -> Result<Self> {
        if let Some(value) = value {
            bounded_value(value)?;
        }
        let truthy = match value {
            None | Some(Value::Null) => false,
            Some(Value::Bool(v)) => *v,
            Some(Value::Number(v)) => v.as_f64().is_some_and(|v| v != 0.0),
            Some(Value::String(v)) => !v.is_empty(),
            _ => true,
        };
        if !truthy {
            return Ok(Self::CloseInputs(String::new()));
        }
        let mut data = serde_json::json!({"output":value.unwrap()});
        let key = freeshow_midi::prepare_send(&mut data)?.output_key()?;
        if key.is_empty() {
            return Err(Error::new(
                "midi_unsupported",
                "Truthy MIDI close ID with an empty coerced key is unsupported",
            ));
        }
        Ok(Self::CloseInputs(key))
    }
}
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum Reply {
    Done,
    Devices(WireData),
    MtcListener(u64),
    Diagnostics(Vec<Error>),
}
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Event {
    Midi {
        owner: Owner,
        listener: u64,
        data: WireData,
    },
    Mtc {
        owner: Owner,
        listener: u64,
        bytes: Vec<u8>,
        backend_timestamp_us: u64,
        received_unix_ms: f64,
    },
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub closing: bool,
    pub admitted: usize,
    pub inputs: usize,
    pub outputs: usize,
    pub worker_finished: bool,
    pub failure: Option<Error>,
}
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
pub(crate) struct Control {
    caller: CancellationToken,
    deadline: Instant,
}
impl Control {
    fn check(&self) -> Result<()> {
        if self.caller.is_cancelled() {
            return Err(Error::new(
                "midi_cancelled",
                "Native MIDI command cancelled",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(Error::new(
                "midi_deadline",
                "Native MIDI command deadline elapsed",
            ));
        }
        Ok(())
    }
}
pub(crate) fn bounded_value(value: &Value) -> Result<()> {
    let mut stack = vec![(value, 0)];
    let mut nodes = 0usize;
    let mut bytes = 0usize;
    while let Some((value, depth)) = stack.pop() {
        nodes += 1;
        if nodes > 1024 || depth > 8 {
            return Err(Error::limit());
        }
        match value {
            Value::String(v) => bytes = bytes.saturating_add(v.len()),
            Value::Array(v) => {
                if v.len() > 1024usize.saturating_sub(nodes + stack.len()) {
                    return Err(Error::limit());
                }
                for x in v {
                    stack.push((x, depth + 1));
                }
            }
            Value::Object(v) => {
                if v.len() > 1024usize.saturating_sub(nodes + stack.len()) {
                    return Err(Error::limit());
                }
                for (k, x) in v {
                    bytes = bytes.saturating_add(k.len());
                    stack.push((x, depth + 1));
                }
            }
            _ => bytes = bytes.saturating_add(8),
        }
        if bytes > 65536 {
            return Err(Error::limit());
        }
    }
    Ok(())
}
#[cfg(test)]
mod controlled;
#[cfg(test)]
mod tests;
