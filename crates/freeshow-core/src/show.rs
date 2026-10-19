//! Compatibility with the pinned Electron show helpers. Keep object insertion order:
//! the slide/line order affects lyrics and the search index.
use serde_json::{Map, Value};

/// Upstream retries a broken show up to its first `}}]` terminator.
pub fn parse_json(text: &str) -> Option<Value> {
    serde_json::from_str(text).ok().or_else(|| {
        let end = text.find("}}]").map(|index| index + 3).unwrap_or(2);
        serde_json::from_str(text.get(..end).unwrap_or(text)).ok()
    })
}

pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    }
}

/// JavaScript enumerates canonical array-index keys first, then other keys in
/// insertion order. serde_json's preserve_order alone does not cover numeric IDs.
pub fn values(value: &Value) -> Vec<&Value> {
    match value {
        Value::Object(map) => {
            let mut indexed = Vec::new();
            let mut others = Vec::new();
            for (key, value) in map {
                if let Ok(index) = key.parse::<u32>() {
                    if index != u32::MAX && index.to_string() == *key {
                        indexed.push((index, value));
                        continue;
                    }
                }
                others.push(value);
            }
            indexed.sort_unstable_by_key(|(index, _)| *index);
            indexed
                .into_iter()
                .map(|(_, value)| value)
                .chain(others)
                .collect()
        }
        Value::Array(array) => array.iter().collect(),
        _ => Vec::new(),
    }
}

pub fn trim(show: &Value) -> Value {
    if !truthy(show) {
        return Value::Null;
    }
    let mut result = Map::new();
    for key in ["name", "category", "timestamps"] {
        if let Some(value) = show.get(key) {
            result.insert(key.into(), value.clone());
        }
    }
    result.insert(
        "quickAccess".into(),
        show.get("quickAccess")
            .filter(|value| truthy(value))
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new())),
    );
    if let Some(origin) = show.get("origin").filter(|value| truthy(value)) {
        result.insert("origin".into(), origin.clone());
    }
    for key in ["private", "locked"] {
        if show.get(key).is_some_and(truthy) {
            result.insert(key.into(), Value::Bool(true));
        }
    }
    Value::Object(result)
}

fn js_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(array) => array
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    js_string(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        _ => value.to_string(),
    }
}

pub fn text_content(show: &Value) -> String {
    let mut result = String::new();
    for slide in values(&show["slides"]) {
        let Some(items) = slide["items"].as_array() else {
            continue;
        };
        for item in items {
            let Some(lines) = item["lines"].as_array() else {
                continue;
            };
            for line in lines {
                let Some(texts) = line["text"].as_array() else {
                    continue;
                };
                for text in texts {
                    if let Some(value) = text.get("value").filter(|value| truthy(value)) {
                        if let Value::String(text) = value {
                            result.push_str(text);
                        } else {
                            result.push_str(&js_string(value));
                        }
                    }
                }
            }
        }
    }
    result
}

pub fn is_empty(show: &Value) -> bool {
    !values(&show["layouts"]).iter().any(|layout| {
        layout["slides"]
            .as_array()
            .is_some_and(|slides| !slides.is_empty())
    }) && text_content(show).is_empty()
}

pub fn search_text(show: &Value) -> String {
    if !truthy(&show["slides"]) || truthy(&show["reference"]["type"]) {
        return String::new();
    }
    let mut result = Vec::new();
    for slide in values(&show["slides"]) {
        for item in slide["items"].as_array().into_iter().flatten() {
            for line in item["lines"].as_array().into_iter().flatten() {
                let text = line["text"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|text| {
                        text.get("value")
                            .filter(|value| truthy(value))
                            .map(js_string)
                            .unwrap_or_default()
                    })
                    .collect::<String>();
                result.push(text);
            }
        }
    }
    result.join(" ").to_lowercase()
}
