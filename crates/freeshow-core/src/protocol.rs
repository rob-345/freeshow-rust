use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub channel: String,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub listener_id: Option<String>,
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
        }
    }
}
