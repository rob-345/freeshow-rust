//! Bounded outbound networking, independent of the desktop IPC/event layer.
//!
//! `EMIT_OSC` has no reply event in Electron. The coordinator can log or route
//! these explicit outcomes without adding a new renderer contract.

pub mod osc;

pub use osc::{
    encode_message, prepare_emit, EmitOutcome, EncodedMessage, OscError, OscErrorKind, OscSender,
    OscSenderConfig, OscTicket, PreparedOsc,
};
