//! Small JSON-oriented JS value model. Nonfinite numbers and undefined must
//! survive until final serialization to match the original handler's arithmetic.
use serde_json::Value;

#[derive(Clone, Debug)]
pub enum Js {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Js>),
    Object(Vec<(String, Js)>),
}
impl Js {
    pub fn object() -> Self {
        Self::Object(vec![])
    }
    pub fn get(&self, key: &str) -> &Self {
        if let Self::Object(fields) = self {
            if let Some((_, value)) = fields.iter().find(|(name, _)| name == key) {
                return value;
            }
        }
        &Self::Undefined
    }
    pub fn set(&mut self, key: &str, value: Self) {
        if let Self::Object(fields) = self {
            if let Some((_, current)) = fields.iter_mut().find(|(name, _)| name == key) {
                *current = value;
            } else {
                fields.push((key.into(), value));
            }
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
    pub fn nullish(&self) -> bool {
        matches!(self, Self::Null | Self::Undefined)
    }
    pub fn truthy(&self) -> bool {
        match self {
            Self::Null | Self::Undefined => false,
            Self::Bool(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::String(value) => !value.is_empty(),
            _ => true,
        }
    }
    pub fn number(&self) -> f64 {
        match self {
            Self::Null => 0.0,
            Self::Bool(value) => {
                if *value {
                    1.0
                } else {
                    0.0
                }
            }
            Self::Number(value) => *value,
            Self::String(value) => number(value),
            Self::Array(_) => number(&self.string()),
            _ => f64::NAN,
        }
    }
    pub fn string(&self) -> String {
        match self {
            Self::Undefined => "undefined".into(),
            Self::Null => "null".into(),
            Self::Bool(value) => value.to_string(),
            Self::String(value) => value.clone(),
            Self::Number(value) if value.is_nan() => "NaN".into(),
            Self::Number(value) if *value == f64::INFINITY => "Infinity".into(),
            Self::Number(value) if *value == f64::NEG_INFINITY => "-Infinity".into(),
            Self::Number(value) => {
                if *value == 0.0 {
                    "0".into()
                } else {
                    value.to_string()
                }
            }
            Self::Array(values) => values
                .iter()
                .map(|value| {
                    if value.nullish() {
                        String::new()
                    } else {
                        value.string()
                    }
                })
                .collect::<Vec<_>>()
                .join(","),
            Self::Object(_) => "[object Object]".into(),
        }
    }
    pub fn json(&self) -> Value {
        match self {
            Self::Null | Self::Undefined => Value::Null,
            Self::Bool(value) => (*value).into(),
            Self::Number(value) => serde_json::Number::from_f64(*value)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            Self::String(value) => value.clone().into(),
            Self::Array(values) => Value::Array(values.iter().map(Self::json).collect()),
            Self::Object(fields) => Value::Object(
                ordered(fields)
                    .into_iter()
                    .filter(|(_, value)| !matches!(value, Self::Undefined))
                    .map(|(key, value)| (key.clone(), value.json()))
                    .collect(),
            ),
        }
    }
}
impl From<Value> for Js {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Bool(value),
            Value::Number(value) => Self::Number(value.as_f64().unwrap_or(f64::NAN)),
            Value::String(value) => Self::String(value),
            Value::Array(values) => Self::Array(values.into_iter().map(Self::from).collect()),
            Value::Object(fields) => Self::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key, Self::from(value)))
                    .collect(),
            ),
        }
    }
}
impl From<&str> for Js {
    fn from(value: &str) -> Self {
        Self::String(value.into())
    }
}

pub(crate) fn ordered(fields: &[(String, Js)]) -> Vec<&(String, Js)> {
    fn index(key: &str) -> Option<u32> {
        let index = key.parse::<u32>().ok()?;
        (index != u32::MAX && index.to_string() == key).then_some(index)
    }
    let mut fields: Vec<_> = fields.iter().collect();
    fields.sort_by_key(|(key, _)| index(key).map(|index| (0, index)).unwrap_or((1, 0)));
    fields
}
pub(crate) fn whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
pub(crate) fn parse_float(value: &str) -> f64 {
    let value = value.trim_start_matches(whitespace);
    if value.starts_with("Infinity") || value.starts_with("+Infinity") {
        return f64::INFINITY;
    }
    if value.starts_with("-Infinity") {
        return f64::NEG_INFINITY;
    }
    let bytes = value.as_bytes();
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let start = end;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
    }
    if end == start || (end == start + 1 && bytes.get(start) == Some(&b'.')) {
        return f64::NAN;
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let exponent = end;
        end += 1;
        if matches!(bytes.get(end), Some(b'+' | b'-')) {
            end += 1;
        }
        let digits = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == digits {
            end = exponent;
        }
    }
    value[..end].parse().unwrap_or(f64::NAN)
}
pub(crate) fn parse_int(value: &str) -> f64 {
    let value = value.trim_start_matches(whitespace);
    let bytes = value.as_bytes();
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let start = end;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if end == start {
        f64::NAN
    } else {
        value[..end].parse().unwrap_or(f64::NAN)
    }
}
fn number(value: &str) -> f64 {
    let value = value.trim_matches(whitespace);
    if value.is_empty() {
        return 0.0;
    }
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return u64::from_str_radix(hex, 16)
            .map(|x| x as f64)
            .unwrap_or(f64::NAN);
    }
    if let Some(bits) = value
        .strip_prefix("0b")
        .or_else(|| value.strip_prefix("0B"))
    {
        return u64::from_str_radix(bits, 2)
            .map(|x| x as f64)
            .unwrap_or(f64::NAN);
    }
    if let Some(oct) = value
        .strip_prefix("0o")
        .or_else(|| value.strip_prefix("0O"))
    {
        return u64::from_str_radix(oct, 8)
            .map(|x| x as f64)
            .unwrap_or(f64::NAN);
    }
    if matches!(value, "Infinity" | "+Infinity") {
        return f64::INFINITY;
    }
    if value == "-Infinity" {
        return f64::NEG_INFINITY;
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | '+' | '-' | 'e' | 'E'))
    {
        return f64::NAN;
    }
    value.parse().unwrap_or(f64::NAN)
}

pub(crate) fn project(metadata: &Js) -> Js {
    if matches!(metadata, Js::Null | Js::Undefined) {
        return Js::Null;
    }
    let (mut gain, mut peak) = (Js::Null, Js::Null);
    find(metadata, &mut gain, &mut peak);
    let mut result = Js::object();
    result.set("common", metadata.get("common").clone());
    if !gain.is_null() {
        let mut multiplier = 10f64.powf(gain.number() / 20.0);
        if !peak.is_null() && peak.number() > 0.0 {
            let max = 1.0 / peak.number();
            if multiplier > max {
                multiplier = max;
            }
        }
        result.set("replayGainMultiplier", Js::Number(multiplier));
    }
    result
}
fn parse_value(value: &Js, field: &str) -> Js {
    match value {
        Js::Number(_) => value.clone(),
        Js::Object(_) | Js::Array(_) => {
            let first = value.get(field);
            if !first.nullish() {
                return first.clone();
            }
            let ratio = value.get("ratio");
            if !ratio.nullish() {
                ratio.clone()
            } else {
                value.get("dB").clone()
            }
        }
        Js::String(text) => {
            let filtered: String = text
                .chars()
                .filter(|c| c.is_ascii_digit() || matches!(c, '.' | '-'))
                .collect();
            let value = parse_float(&filtered);
            if value.is_nan() {
                Js::Null
            } else {
                Js::Number(value)
            }
        }
        _ => Js::Null,
    }
}
fn find(object: &Js, gain: &mut Js, peak: &mut Js) {
    if !gain.is_null() && !peak.is_null() {
        return;
    }
    let visit = |key: &str, value: &Js, gain: &mut Js, peak: &mut Js| {
        let key: String = key
            .to_lowercase()
            .chars()
            .filter(|c| *c != '_' && !whitespace(*c))
            .collect();
        if gain.is_null() && matches!(key.as_str(), "replaygaintrackgain" | "replaygainalbumgain") {
            *gain = parse_value(value, "dB");
        } else if peak.is_null()
            && matches!(key.as_str(), "replaygaintrackpeak" | "replaygainalbumpeak")
        {
            *peak = parse_value(value, "ratio");
        } else {
            find(value, gain, peak);
        }
    };
    match object {
        Js::Object(fields) => {
            for (key, value) in ordered(fields) {
                visit(key, value, gain, peak);
            }
        }
        Js::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                visit(&index.to_string(), value, gain, peak);
            }
        }
        _ => {}
    }
}
