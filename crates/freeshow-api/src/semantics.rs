//! JavaScript/Express request semantics, tested against the actual source.
use indexmap::IndexMap;
use serde_json::{Map, Value};

pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_some_and(|n| n != 0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}
pub(crate) fn trim(value: &str) -> &str {
    value.trim_matches(|c| matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'))
}
pub(crate) fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => ryu_js::Buffer::new()
            .format(v.as_f64().unwrap_or_default())
            .to_owned(),
        Value::String(v) => v.clone(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(v) => v
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    js_string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
    }
}
/// Exact getAuthToken precedence. Object tokens are stringified/trimmed but
/// never bearer-decoded; an incorrect truthy object field blocks later sources.
pub fn auth_token(password: &str, sources: &[Value]) -> Option<String> {
    for source in sources {
        if !truthy(source) {
            continue;
        }
        match source {
            Value::String(value) => {
                if value.to_ascii_lowercase().starts_with("bearer ") {
                    return Some(trim(&value[7..]).to_owned());
                }
                if value == password {
                    return Some(value.clone());
                }
            }
            Value::Array(values) => {
                for value in values.iter().filter_map(Value::as_str) {
                    if value.to_ascii_lowercase().starts_with("bearer ") {
                        return Some(trim(&value[7..]).to_owned());
                    }
                    if value == password {
                        return Some(value.to_owned());
                    }
                }
            }
            Value::Object(values) => {
                for key in [
                    "auth",
                    "password",
                    "token",
                    "key",
                    "apiKey",
                    "authKey",
                    "authorization",
                    "x-api-key",
                ] {
                    if let Some(value) = values.get(key).filter(|value| truthy(value)) {
                        return Some(trim(&js_string(value)).to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    None
}
pub(crate) fn authorized(password: &str, sources: &[Value]) -> bool {
    password.is_empty() || auth_token(password, sources).as_deref() == Some(password)
}
pub(crate) fn spread(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Array(values) => values
            .into_iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        Value::String(value) => value
            .encode_utf16()
            .enumerate()
            .map(|(i, c)| (i.to_string(), Value::String(String::from_utf16_lossy(&[c]))))
            .collect(),
        _ => Map::new(),
    }
}
/// Express root route's body precedence and query JSON spread, including a data
/// object's ability to override the query's action.
pub fn rest_data(body: Value, query: &Value) -> Result<Value, crate::ApiError> {
    let data = if truthy(&body) {
        body
    } else {
        Value::Object(Map::new())
    };
    if !data.get("action").is_some_and(truthy) {
        if let Some(action) = query.get("action").filter(|v| truthy(v)) {
            let mut merged = Map::new();
            merged.insert("action".into(), action.clone());
            let raw = query
                .get("data")
                .filter(|v| truthy(v))
                .cloned()
                .unwrap_or(Value::String("{}".into()));
            match parse_json(&js_string(&raw)) {
                Ok(value) => {
                    if matches!(&value,Value::String(text) if text.chars().any(|c|c as u32>0xffff))
                    {
                        return Err(crate::ApiError::new("json_surrogate_spread","JS string spread creates unpaired UTF-16 values unsupported by native JSON"));
                    }
                    merged.extend(spread(value));
                }
                Err(error) if error.code == "json_number_overflow" => return Err(error),
                Err(_) => {}
            }
            return Ok(Value::Object(merged));
        }
    }
    Ok(data)
}

pub(crate) fn normalize(value: Value) -> Value {
    match value {
        Value::Number(value) => value
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Value::Array(values) => Value::Array(values.into_iter().map(normalize).collect()),
        Value::Object(values) => {
            Value::Object(values.into_iter().map(|(k, v)| (k, normalize(v))).collect())
        }
        other => other,
    }
}
pub(crate) fn wire(value: &Value) -> Value {
    match value {
        Value::Number(number) if number.as_f64() == Some(0.0) => serde_json::json!(0),
        Value::Array(values) => Value::Array(values.iter().map(wire).collect()),
        Value::Object(values) => {
            Value::Object(values.iter().map(|(k, v)| (k.clone(), wire(v))).collect())
        }
        other => other.clone(),
    }
}
pub(crate) fn parse_json(text: &str) -> Result<Value, crate::ApiError> {
    crate::json::parse(text)
}

#[derive(Clone, Debug)]
enum Q {
    Scalar(String),
    Array(Vec<Option<Q>>),
    Object(IndexMap<String, Q>, bool),
}
impl Q {
    fn object() -> Self {
        Self::Object(IndexMap::new(), false)
    }
    fn array_to_object(values: Vec<Option<Self>>, overflow: bool) -> Self {
        Self::Object(
            values
                .into_iter()
                .enumerate()
                .filter_map(|(i, v)| v.map(|v| (i.to_string(), v)))
                .collect(),
            overflow,
        )
    }
    fn value(self) -> Value {
        match self {
            Q::Scalar(s) => Value::String(s),
            Q::Array(a) => Value::Array(a.into_iter().flatten().map(Q::value).collect()),
            Q::Object(o, _) => Value::Object(o.into_iter().map(|(k, v)| (k, v.value())).collect()),
        }
    }
}
fn index_key(value: f64) -> String {
    ryu_js::Buffer::new().format(value).to_owned()
}
fn query_index(key: &str) -> Option<f64> {
    let value = key.parse::<f64>().ok()?;
    (value.is_finite() && value >= 0.0 && value.fract() == 0.0 && index_key(value) == key)
        .then_some(value)
}
fn next_index(map: &IndexMap<String, Q>) -> f64 {
    map.keys()
        .filter_map(|key| query_index(key))
        .max_by(f64::total_cmp)
        .map_or(0.0, |n| n + 1.0)
}
fn concat(left: Q, right: Q) -> Q {
    if let Q::Object(mut map, true) = left {
        let next = next_index(&map);
        let values = match right {
            Q::Array(values) => values,
            other => vec![Some(other)],
        };
        values.into_iter().flatten().fold(next, |next, value| {
            map.insert(index_key(next), value);
            next + 1.0
        });
        return Q::Object(map, true);
    }

    let mut list = match left {
        Q::Array(a) => a,
        other => vec![Some(other)],
    };
    match right {
        Q::Array(a) => list.extend(a),
        other => list.push(Some(other)),
    }
    if list.len() > 20 {
        Q::array_to_object(list, true)
    } else {
        Q::Array(list)
    }
}
fn merge(target: Q, source: Q) -> Q {
    if matches!(&source, Q::Scalar(value) if value.is_empty()) {
        return target;
    }
    match (target, source) {
        (Q::Object(mut a, overflow), Q::Object(b, other)) => {
            for (key, value) in b {
                if let Some(previous) = a.get_mut(&key) {
                    let old = std::mem::replace(previous, Q::Scalar(String::new()));
                    *previous = merge(old, value);
                } else {
                    a.insert(key, value);
                }
            }
            Q::Object(a, overflow || other)
        }
        (Q::Array(mut a), Q::Array(b)) => {
            for (index, value) in b.into_iter().enumerate() {
                let Some(value) = value else { continue };
                if index < a.len() && a[index].is_some() {
                    let previous = a[index].take().unwrap();
                    if matches!(previous, Q::Object(..) | Q::Array(_))
                        && matches!(value, Q::Object(..) | Q::Array(_))
                    {
                        a[index] = Some(merge(previous, value));
                    } else {
                        a[index] = Some(previous);
                        a.push(Some(value));
                    }
                } else {
                    if a.len() <= index {
                        a.resize_with(index + 1, || None);
                    }
                    a[index] = Some(value);
                }
            }
            if a.len() > 20 {
                Q::array_to_object(a, true)
            } else {
                Q::Array(a)
            }
        }
        (Q::Array(a), b @ Q::Object(..)) => merge(Q::array_to_object(a, false), b),
        (a @ Q::Object(..), Q::Array(b)) => merge(a, Q::array_to_object(b, false)),
        (Q::Object(mut a, true), Q::Scalar(value)) => {
            let next = next_index(&a);
            a.insert(index_key(next), Q::Scalar(value));
            Q::Object(a, true)
        }
        (a, Q::Object(source, true)) if matches!(a, Q::Scalar(_)) => {
            let mut map = IndexMap::new();
            map.insert("0".into(), a);
            for (key, value) in source {
                let index = key.parse::<f64>().unwrap_or(f64::NAN) + 1.0;
                map.insert(index_key(index), value);
            }
            Q::Object(map, true)
        }
        (a, b) => concat(a, b),
    }
}
fn decode(value: &str) -> String {
    let value = value.replace('+', " ");
    // qs uses decodeURIComponent and retains the original on any invalid escape
    // or malformed UTF-8. percent_decode alone would partially decode it.
    let bytes = value.as_bytes();
    if bytes.iter().enumerate().any(|(i, b)| {
        *b == b'%'
            && (i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit())
    }) {
        return value;
    }
    percent_encoding::percent_decode_str(&value)
        .decode_utf8()
        .map(|v| v.into_owned())
        .unwrap_or(value)
}
fn segments(key: &str) -> Vec<String> {
    let mut result = Vec::new();
    let Some(first) = key.find('[') else {
        return vec![key.to_owned()];
    };
    if first > 0 {
        result.push(key[..first].to_owned());
    }
    let mut open = Some(first);
    let mut depth = 0;
    while let Some(start) = open {
        if depth >= 5 {
            result.push(format!("[{}]", &key[start..]));
            break;
        }
        let mut level = 1;
        let mut close = None;
        for (i, c) in key[start + 1..].char_indices() {
            if c == '[' {
                level += 1;
            } else if c == ']' {
                level -= 1;
                if level == 0 {
                    close = Some(start + 1 + i);
                    break;
                }
            }
        }
        let Some(end) = close else {
            result.push(format!("[{}]", &key[start..]));
            break;
        };
        result.push(key[start..=end].to_owned());
        depth += 1;
        open = key[end + 1..].find('[').map(|i| end + 1 + i);
    }
    result
}
/// Express 4 extended (qs) query semantics: duplicate combining, nesting,
/// sparse-array compaction, 20-element arrays, depth 5 and 1000 parameters.
pub fn parse_query(raw: &str) -> Value {
    let mut values = IndexMap::<String, Q>::new();
    let brackets = raw
        .replace("%5B", "[")
        .replace("%5b", "[")
        .replace("%5D", "]")
        .replace("%5d", "]");
    for part in brackets.split('&').take(1000) {
        let equal = part.find("]=").map(|i| i + 1).or_else(|| part.find('='));
        let (key, value) = match equal {
            Some(i) => (decode(&part[..i]), decode(&part[i + 1..])),
            None => (decode(part), String::new()),
        };
        let value = Q::Scalar(value);
        if let Some(previous) = values.get_mut(&key) {
            let old = std::mem::replace(previous, Q::Scalar(String::new()));
            *previous = concat(old, value);
        } else {
            values.insert(key, value);
        }
    }
    let mut root = Q::object();
    for (key, mut leaf) in values {
        if key.is_empty() {
            continue;
        }
        for segment in segments(&key).into_iter().rev() {
            if segment == "[]" {
                leaf = match leaf {
                    Q::Array(_) => leaf,
                    Q::Object(_, true) => leaf,
                    other => Q::Array(vec![Some(other)]),
                };
            } else {
                let bracket = segment.starts_with('[') && segment.ends_with(']');
                let clean = if bracket {
                    &segment[1..segment.len() - 1]
                } else {
                    &segment
                };
                let index = query_index(clean).filter(|_| bracket);
                if let Some(index) = index.filter(|index| *index < 20.0).map(|n| n as usize) {
                    let mut a = Vec::new();
                    a.resize_with(index + 1, || None);
                    a[index] = Some(leaf);
                    leaf = Q::Array(a);
                } else {
                    let mut map = IndexMap::new();
                    if clean != "__proto__" {
                        map.insert(clean.to_owned(), leaf);
                    }
                    leaf = Q::Object(map, index.is_some());
                }
            }
        }
        root = merge(root, leaf);
    }
    root.value()
}

pub(crate) fn socket_query(raw: &str) -> Value {
    let mut map = Map::new();
    for (key, value) in form_urlencoded::parse(raw.as_bytes()).take(1000) {
        let value = Value::String(value.into_owned());
        match map.get_mut(key.as_ref()) {
            None => {
                map.insert(key.into_owned(), value);
            }
            Some(Value::Array(a)) => a.push(value),
            Some(previous) => {
                *previous = Value::Array(vec![previous.take(), value]);
            }
        }
    }
    Value::Object(map)
}
