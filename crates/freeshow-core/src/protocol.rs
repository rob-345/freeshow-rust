use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc};

/// Rust-only publication resource; never serialized or accepted from JSON.
#[derive(Clone)]
pub struct NativeLifetime {
    _hold: Arc<dyn Send + Sync>,
}
impl NativeLifetime {
    pub fn new<T: Send + Sync + 'static>(value: T) -> Self {
        Self {
            _hold: Arc::new(value),
        }
    }
}
impl std::fmt::Debug for NativeLifetime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeLifetime(..)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContext {
    pub runtime_id: String,
    pub library_generation: u64,
    pub data_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub channel: String,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub listener_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<RuntimeContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub channel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(rename = "autoProfile", skip_serializing_if = "Option::is_none")]
    pub auto_profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// Retains native resources through event clones/encoding and actual Drop.
    #[serde(skip)]
    pub native_lifetime: Option<NativeLifetime>,
    pub channel: String,
    pub message: Message,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listener_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<RuntimeContext>,
    /// Bounded host-issued media capabilities. Kept outside the retained
    /// application payload and accepted only after the event's context check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_assets: Option<Value>,
    /// Private correlation for retained canvas capture; source payload stays
    /// available to the original handler, binary completion uses native IPC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_capture: Option<Value>,
    /// Trusted READ_EXIF restoration of own undefined, IEEE special numbers and bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_exif: Option<Value>,
    /// Private remote provenance and delivery receipt; retained messages are raw JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_remote: Option<Value>,
    /// Ordinary MIDI restoration and private synchronous-delivery provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_midi: Option<Value>,
    /// Private native audio status restoration and synchronous delivery receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_audio: Option<Value>,
    /// Private timecode bytes/IEEE restoration and one bounded delivery receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_timecode: Option<Value>,
    /// Private now-playing metadata restoration and bounded delivery receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_now_playing: Option<Value>,
    /// Private fixed-cover folder notification; source callback stays unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_cover: Option<Value>,
    /// Private exact Spotify undefined/IEEE restoration, paired with this reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_spotify: Option<Value>,
}

impl Event {
    pub fn reply(outer: &str, inner: &str, data: Value, listener_id: Option<String>) -> Self {
        Self {
            native_lifetime: None,
            channel: outer.into(),
            message: Message {
                channel: inner.into(),
                data: Some(data),
                auto_profile: None,
            },
            listener_id,
            context: None,
            native_assets: None,
            native_capture: None,
            native_exif: None,
            native_remote: None,
            native_midi: None,
            native_audio: None,
            native_timecode: None,
            native_now_playing: None,
            native_cover: None,
            native_spotify: None,
        }
    }
}
