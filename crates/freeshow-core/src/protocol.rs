use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

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
}

impl Event {
    pub fn reply(outer: &str, inner: &str, data: Value, listener_id: Option<String>) -> Self {
        Self {
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
        }
    }
}
