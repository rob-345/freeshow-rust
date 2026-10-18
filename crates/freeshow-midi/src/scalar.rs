use crate::{Error, MAX_NAME_BYTES};
use serde::{ser::SerializeMap, Serialize, Serializer};
use serde_json::Value;
/// Exact JavaScript special-number identity, never implicitly encoded as null.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Number {
    Finite(f64),
    NaN,
    PositiveInfinity,
    NegativeInfinity,
    NegativeZero,
}
impl Serialize for Number {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = Self::new(self.value());
        let mut map = serializer.serialize_map(None)?;
        let kind = match value {
            Self::Finite(_) => "Finite",
            Self::NaN => "NaN",
            Self::PositiveInfinity => "PositiveInfinity",
            Self::NegativeInfinity => "NegativeInfinity",
            Self::NegativeZero => "NegativeZero",
        };
        map.serialize_entry("kind", kind)?;
        if let Self::Finite(number) = value {
            map.serialize_entry("value", &number)?;
        }
        map.end()
    }
}
impl Number {
    pub fn new(value: f64) -> Self {
        if value.is_nan() {
            Self::NaN
        } else if value == f64::INFINITY {
            Self::PositiveInfinity
        } else if value == f64::NEG_INFINITY {
            Self::NegativeInfinity
        } else if value == 0.0 && value.is_sign_negative() {
            Self::NegativeZero
        } else {
            Self::Finite(value)
        }
    }
    pub fn value(self) -> f64 {
        match self {
            Self::Finite(n) => n,
            Self::NaN => f64::NAN,
            Self::PositiveInfinity => f64::INFINITY,
            Self::NegativeInfinity => f64::NEG_INFINITY,
            Self::NegativeZero => -0.0,
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum MidiValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
    Coercible(String),
}
impl MidiValue {
    pub fn from_json(value: Option<&Value>) -> Result<Self, Error> {
        Ok(match value {
            None => Self::Undefined,
            Some(Value::Null) => Self::Null,
            Some(Value::Bool(b)) => Self::Bool(*b),
            Some(Value::Number(n)) => Self::Number(n.as_f64().unwrap()),
            Some(Value::String(s)) => {
                if s.len() > MAX_NAME_BYTES {
                    return Err(Error::resource());
                }
                Self::Text(s.clone())
            }
            Some(value) => {
                crate::bounded_json(value)?;
                Self::Coercible(json_string(value)?)
            }
        })
    }
    pub(crate) fn truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(b) => *b,
            Self::Number(n) => *n != 0.0 && !n.is_nan(),
            Self::Text(s) => !s.is_empty(),
            Self::Coercible(_) => true,
        }
    }
    pub(crate) fn number(&self) -> f64 {
        match self {
            Self::Undefined => f64::NAN,
            Self::Null => 0.0,
            Self::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            Self::Number(n) => *n,
            Self::Text(s) | Self::Coercible(s) => to_number(s),
        }
    }
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Undefined => "undefined".into(),
            Self::Null => "null".into(),
            Self::Bool(b) => b.to_string(),
            Self::Number(n) => number_string(*n),
            Self::Text(s) | Self::Coercible(s) => s.clone(),
        }
    }
    pub(crate) fn integer(&self, max: u8, channel: bool) -> Result<u8, Error> {
        let number = self.number();
        let parsed = parse_int(&self.text());
        if number != parsed || !(0.0..=f64::from(max)).contains(&number) {
            return Err(Error::new(
                "midi_value",
                "RangeError",
                if channel {
                    format!(
                        "Bad channel value (must not be less than 0 or more than 15): {}",
                        self.text()
                    )
                } else {
                    format!("Bad MIDI value: {}", self.text())
                },
            ));
        }
        Ok(parsed as u8)
    }
    pub(crate) fn note(&self) -> Result<u8, Error> {
        if matches!(self, Self::Null) {
            return Err(Error::new(
                "midi_value",
                "TypeError",
                "Cannot read properties of null (reading 'toString')",
            ));
        }
        let text = self.text();
        if let Some(number) = note_value(&text) {
            if (0..128).contains(&number) {
                return Ok(number as u8);
            }
        }
        Err(Error::new(
            "midi_value",
            "RangeError",
            format!("Bad MIDI value: {text}"),
        ))
    }
}
fn json_string(value: &Value) -> Result<String, Error> {
    fn append(value: &Value, out: &mut String, depth: usize) -> Result<(), Error> {
        if depth > 8 {
            return Err(Error::resource());
        }
        match value {
            Value::Array(items) => {
                if items.len() > 1024 {
                    return Err(Error::resource());
                }
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    if !item.is_null() {
                        append(item, out, depth + 1)?;
                    }
                    if out.len() > MAX_NAME_BYTES {
                        return Err(Error::resource());
                    }
                }
            }
            Value::Object(fields) => {
                if fields.contains_key("toString") {
                    return Err(Error::new(
                        "midi_unsupported",
                        "UnsupportedError",
                        "Custom object coercion is unsupported",
                    ));
                }
                out.push_str("[object Object]");
            }
            Value::String(s) => {
                if s.len() > MAX_NAME_BYTES.saturating_sub(out.len()) {
                    return Err(Error::resource());
                }
                out.push_str(s)
            }
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => out.push_str(&number_string(n.as_f64().unwrap())),
        };
        if out.len() > MAX_NAME_BYTES {
            return Err(Error::resource());
        }
        Ok(())
    }
    let mut out = String::new();
    append(value, &mut out, 0)?;
    Ok(out)
}
pub(crate) fn number_string(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n == f64::INFINITY {
        "Infinity".into()
    } else if n == f64::NEG_INFINITY {
        "-Infinity".into()
    } else if n == 0.0 {
        "0".into()
    } else {
        ryu_js::Buffer::new().format(n).into()
    }
}
fn whitespace(c: char) -> bool {
    matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
fn to_number(s: &str) -> f64 {
    let s = s.trim_matches(whitespace);
    if s.is_empty() {
        return 0.0;
    }
    if matches!(s, "Infinity" | "+Infinity") {
        return f64::INFINITY;
    }
    if s == "-Infinity" {
        return f64::NEG_INFINITY;
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0b", 2),
        ("0B", 2),
        ("0o", 8),
        ("0O", 8),
    ] {
        if let Some(digits) = s.strip_prefix(prefix) {
            if digits.is_empty() {
                return f64::NAN;
            }
            let mut n = 0.0;
            for c in digits.chars() {
                let Some(digit) = c.to_digit(radix) else {
                    return f64::NAN;
                };
                n = n * radix as f64 + digit as f64;
            }
            return n;
        }
    }
    let bytes = s.as_bytes();
    let mut i = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let mut digits = 0;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
        digits += 1;
    }
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return f64::NAN;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return f64::NAN;
        }
    }
    if i != bytes.len() {
        f64::NAN
    } else {
        s.parse().unwrap_or(f64::NAN)
    }
}
fn parse_int(s: &str) -> f64 {
    let mut s = s.trim_start_matches(whitespace);
    let negative = s.starts_with('-');
    if s.starts_with(['+', '-']) {
        s = &s[1..];
    }
    let radix = if s.starts_with("0x") || s.starts_with("0X") {
        s = &s[2..];
        16
    } else {
        10
    };
    let mut number = 0.0;
    let mut any = false;
    for c in s.chars() {
        let Some(digit) = c.to_digit(radix) else {
            break;
        };
        number = number * radix as f64 + digit as f64;
        any = true;
    }
    if !any {
        f64::NAN
    } else if negative {
        -number
    } else {
        number
    }
}
fn note_value(text: &str) -> Option<i32> {
    if text.len() > MAX_NAME_BYTES {
        return None;
    }
    let text = text.to_lowercase();
    if let Ok(number) = text.parse::<i32>() {
        if (0..128).contains(&number) && text == number.to_string() {
            return Some(number);
        }
    }
    let base = match text.chars().next()? {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' | 'h' => 11,
        _ => return None,
    };
    let rest = &text[1..];
    let index = rest.find(|c: char| c.is_ascii_digit())?;
    let octave = &rest[index..];
    let octave = octave
        .parse::<i32>()
        .ok()
        .filter(|n| *n < 12 && octave == n.to_string())?;
    let value = base + octave * 12;
    if value > 127 {
        return None;
    }
    match &rest[..index] {
        "" | "♮" => Some(value),
        "b" | "♭" if value > 0 => Some(value - 1),
        "bb" | "♭♭" | "𝄫" if value > 0 => Some(value - 2),
        "#" | "♯" if value < 127 => Some(value + 1),
        "##" | "♯♯" | "𝄪" if value < 127 => Some(value + 2),
        _ => None,
    }
}
pub(crate) fn int32(value: f64) -> i32 {
    if !value.is_finite() || value == 0.0 {
        return 0;
    }
    let value = value.trunc().rem_euclid(4294967296.0);
    if value >= 2147483648.0 {
        (value - 4294967296.0) as i32
    } else {
        value as i32
    }
}
