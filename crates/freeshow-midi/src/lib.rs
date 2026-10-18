//! Pure MIDI/MTC compatibility. This crate does not enumerate or open OS devices.
mod bindings;
mod message;
mod mtc;
mod scalar;
pub use bindings::{InputDecision, PortBindings, PortToken};
pub use message::{device_names, prepare_send, receive, MessageKind, SendPlan};
pub use mtc::{mtc_packets, MtcReceiver, MtcSnapshot, Status, StatusGate, TimecodeEffect};
pub use scalar::{MidiValue, Number};
use serde::Serialize;
use serde_json::Value;

pub const COMMANDS: &[&str] = &[
    "GET_MIDI_INPUTS",
    "GET_MIDI_OUTPUTS",
    "SEND_MIDI",
    "RECEIVE_MIDI",
    "CLOSE_MIDI",
];
pub const MAX_MESSAGE_BYTES: usize = 1024;
pub const MAX_PORTS: usize = 32;
pub const MAX_NAME_BYTES: usize = 4096;
const MAX_JSON_BYTES: usize = 65536;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub name: &'static str,
    pub message: String,
}
impl Error {
    fn new(code: &'static str, name: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            name,
            message: message.into(),
        }
    }
    fn resource() -> Self {
        Self::new(
            "midi_resource_limit",
            "ResourceError",
            "MIDI input/work/resource bound exceeded",
        )
    }
    fn invalid(message: &str) -> Self {
        Self::new("midi_invalid", "TypeError", message)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Hydration {
    pub undefined_paths: Vec<String>,
    pub numbers: Vec<NumberField>,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NumberField {
    pub path: String,
    pub number: Number,
}
/// JSON plus data-only instructions for original own undefined/non-finite values.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WireData {
    pub data: Value,
    pub hydration: Hydration,
}
fn bounded_name(value: &str) -> Result<(), Error> {
    if value.len() > MAX_NAME_BYTES {
        Err(Error::resource())
    } else {
        Ok(())
    }
}
fn truthy_json(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        _ => true,
    }
}
/// Bound typed JSON work before extracting or cloning fields. Unknown fields are
/// retained in-place and only visited for resource accounting, never as paths.
fn bounded_json(root: &Value) -> Result<(), Error> {
    let mut stack = vec![(root, 0)];
    let mut nodes = 0usize;
    let mut bytes = 0usize;
    while let Some((value, depth)) = stack.pop() {
        nodes += 1;
        if nodes > 1024 || depth > 8 {
            return Err(Error::resource());
        }
        match value {
            Value::String(text) => bytes = bytes.saturating_add(text.len()),
            Value::Array(items) => {
                if items.len() > 1024 - nodes - stack.len() {
                    return Err(Error::resource());
                }
                for item in items {
                    stack.push((item, depth + 1));
                }
            }
            Value::Object(items) => {
                if items.len() > 1024 - nodes - stack.len() {
                    return Err(Error::resource());
                }
                for (key, item) in items {
                    bytes = bytes.saturating_add(key.len());
                    stack.push((item, depth + 1));
                }
            }
            _ => bytes = bytes.saturating_add(8),
        }
        if bytes > MAX_JSON_BYTES || stack.len() > 1024 {
            return Err(Error::resource());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests;
