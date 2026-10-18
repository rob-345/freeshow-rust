use crate::{RemoteError, Result, ServerKind, StartSpec};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

pub fn parse_json(text: &str) -> Result<Value> {
    crate::json::parse(text)
}
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_some_and(|n| n != 0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}
fn trim(value: &str) -> &str {
    value.trim_matches(|c|matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'))
}
fn string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => ryu_js::Buffer::new().format(v.as_f64().unwrap()).into(),
        Value::String(v) => v.clone(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(v) => v
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
    }
}
pub(crate) fn number(value: Option<&Value>) -> f64 {
    match value {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(v)) => {
            if *v {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(v)) => v.as_f64().unwrap_or(f64::NAN),
        Some(value) => {
            let owned = string(value);
            let text = trim(&owned);
            if text.is_empty() {
                return 0.0;
            }
            for (prefix, radix) in [
                ("0x", 16),
                ("0X", 16),
                ("0b", 2),
                ("0B", 2),
                ("0o", 8),
                ("0O", 8),
            ] {
                if let Some(digits) = text.strip_prefix(prefix) {
                    return if digits.is_empty() {
                        f64::NAN
                    } else {
                        digits
                            .chars()
                            .try_fold(0.0, |n, c| {
                                c.to_digit(radix).map(|d| n * radix as f64 + d as f64)
                            })
                            .unwrap_or(f64::NAN)
                    };
                }
            }
            match text {
                "Infinity" | "+Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                _ => {
                    if !text
                        .bytes()
                        .all(|c| c.is_ascii_digit() || b"+-.eE".contains(&c))
                    {
                        f64::NAN
                    } else {
                        text.parse::<f64>().unwrap_or(f64::NAN)
                    }
                }
            }
        }
    }
}
pub fn prepare_start(payload: &Value) -> Result<StartSpec> {
    if payload.is_null() || !payload.is_object() {
        return Err(RemoteError::new(
            "invalid_server_data",
            "START requires an object",
        ));
    }
    let disabled = payload
        .get("disabled")
        .filter(|v| !v.is_null())
        .ok_or_else(|| {
            RemoteError::new("invalid_server_data", "START disabled value is required")
        })?;
    if disabled.get("output_stream") == Some(&Value::Bool(false)) {
        return Err(RemoteError::new(
            "unsupported_output_stream",
            "OUTPUT_STREAM capture transport is outside this service",
        ));
    }
    let mut ports = BTreeMap::new();
    let mut data = BTreeMap::new();
    for kind in ServerKind::ALL {
        if disabled.get(kind.key()) != Some(&Value::Bool(false)) {
            continue;
        }
        let port_map = payload
            .get("ports")
            .filter(|v| !v.is_null())
            .ok_or_else(|| {
                RemoteError::new(
                    "invalid_server_data",
                    "START ports value is required for enabled servers",
                )
            })?;
        let data_map = payload
            .get("data")
            .filter(|v| !v.is_null())
            .ok_or_else(|| {
                RemoteError::new(
                    "invalid_server_data",
                    "START data value is required for enabled servers",
                )
            })?;
        let port = match port_map.get(kind.key()).filter(|v| truthy(v)) {
            None => kind.port(),
            Some(value) => {
                if !matches!(value, Value::Number(_) | Value::String(_)) {
                    return Err(RemoteError::new(
                        "unsupported_port",
                        "Only TCP numeric/string ports are supported",
                    ));
                }
                let n = number(Some(value));
                if matches!(value, Value::String(_)) && (!n.is_finite() || n < 0.0) {
                    return Err(RemoteError::new(
                        "unsupported_named_pipe",
                        "Named-pipe listen paths are unsupported",
                    ));
                }
                if !n.is_finite()
                    || n.fract() != 0.0
                    || !(0.0..=65535.0).contains(&n)
                    || matches!(value,Value::String(v)if trim(v).is_empty())
                {
                    return Err(RemoteError::new(
                        "invalid_port",
                        "TCP port must be an integer in 0..65535",
                    ));
                }
                n as u16
            }
        };
        ports.insert(kind, port);
        data.insert(
            kind,
            data_map
                .get(kind.key())
                .filter(|v| truthy(v))
                .cloned()
                .unwrap_or(json!({})),
        );
    }
    Ok(StartSpec {
        ports,
        max: payload.get("max").cloned(),
        data,
    })
}
pub fn get_os(ua: &str) -> &'static str {
    let ua = ua.to_ascii_lowercase();
    for (name, needles) in [
        ("Generic Linux", &["linux"][..]),
        ("Android", &["android"]),
        ("BlackBerry", &["blackberry"]),
        ("Bluebird", &["ef500"]),
        ("Chrome OS", &["cros"]),
        ("Datalogic", &["dl-axis"]),
        ("Honeywell", &["ct50"]),
        ("iPad", &["ipad"]),
        ("iPhone", &["iphone"]),
        ("iPod", &["ipod"]),
        ("macOS", &["macintosh"]),
        ("Windows", &["iemobile", "windows"]),
        ("Zebra", &["tc70", "tc55"]),
    ] {
        if needles.iter().any(|needle| ua.contains(needle)) {
            return name;
        }
    }
    "Unknown"
}
pub(crate) enum Target {
    Broadcast,
    Ids(HashSet<String>),
}
pub(crate) fn target(message: &Value) -> Target {
    match message.get("id").filter(|v| truthy(v)) {
        None => Target::Broadcast,
        Some(Value::Array(values)) if values.is_empty() => Target::Broadcast,
        Some(Value::Array(values)) => Target::Ids(
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        ),
        Some(Value::String(id)) => Target::Ids(HashSet::from([id.clone()])),
        Some(_) => Target::Ids(HashSet::new()),
    }
}
pub(crate) fn stream_key(message: &Value) -> Result<String> {
    let data = message
        .get("data")
        .filter(|v| !v.is_null())
        .ok_or_else(|| {
            RemoteError::new(
                "invalid_stream_data",
                "STREAM/STREAM_DONE requires non-null data",
            )
        })?;
    let key = data
        .get("id")
        .map(string)
        .unwrap_or_else(|| "undefined".into());
    if key.len() > 1024 {
        return Err(RemoteError::new(
            "response_id_limit",
            "Stream response id exceeds limit",
        ));
    }
    Ok(key)
}
pub(crate) fn normalize(value: &Value) -> Value {
    match value {
        Value::Number(n) => serde_json::Number::from_f64(n.as_f64().unwrap())
            .map(Value::Number)
            .unwrap(),
        Value::Array(v) => Value::Array(v.iter().map(normalize).collect()),
        Value::Object(v) => {
            Value::Object(v.iter().map(|(k, v)| (k.clone(), normalize(v))).collect())
        }
        other => other.clone(),
    }
}
pub(crate) fn wire(value: &Value) -> Value {
    match value {
        Value::Number(n) if n.as_f64() == Some(0.0) => json!(0),
        Value::Array(v) => Value::Array(v.iter().map(wire).collect()),
        Value::Object(v) => Value::Object(v.iter().map(|(k, v)| (k.clone(), wire(v))).collect()),
        other => other.clone(),
    }
}
pub(crate) fn check_size(value: &Value, limit: usize) -> Result<()> {
    fn depth(value: &Value, level: usize, nodes: &mut usize) -> Result<()> {
        *nodes += 1;
        if level >= 128 || *nodes > 1_000_000 {
            return Err(RemoteError::new(
                "json_depth_limit",
                "Remote JSON depth/node limit",
            ));
        }
        match value {
            Value::Array(values) => {
                for value in values {
                    depth(value, level + 1, nodes)?;
                }
            }
            Value::Object(values) => {
                for value in values.values() {
                    depth(value, level + 1, nodes)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    depth(value, 0, &mut 0)?;
    struct Count {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self.bytes.saturating_add(bytes.len());
            if self.bytes > self.limit {
                return Err(std::io::Error::other("payload limit"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Count { bytes: 0, limit }, value)
        .map_err(|_| RemoteError::new("payload_limit", "Remote message exceeds output bound"))
}
