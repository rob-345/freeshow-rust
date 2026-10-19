use crate::{Error, NullReason, Outcome, Result};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::OnceLock;

pub struct State {
    pub is_playing: bool,
    pub title: String,
    pub artist: String,
    pub position_sec: f64,
    pub duration_sec: f64,
    pub album_art: Option<String>,
    pub volume: f64,
}
impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("metadata", &"<redacted>")
            .finish()
    }
}
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum NumberKind {
    NaN,
    PositiveInfinity,
    NegativeInfinity,
    NegativeZero,
}
#[derive(Debug, Serialize)]
pub struct NumberRestoration {
    pub field: &'static str,
    pub kind: NumberKind,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireState {
    pub data: Value,
    pub undefined_fields: Vec<&'static str>,
    pub special_numbers: Vec<NumberRestoration>,
}
impl WireState {
    pub(crate) fn null() -> Self {
        Self {
            data: Value::Null,
            undefined_fields: vec![],
            special_numbers: vec![],
        }
    }
}
impl State {
    pub fn wire(&self) -> WireState {
        // Field insertion order is the actual Linux source DTO order.
        let mut data = json!({"isPlaying":self.is_playing,"title":self.title,"artist":self.artist,"positionSec":null,"durationSec":null,"albumArt":self.album_art,"volume":null,"platform":"linux"});
        let mut special_numbers = vec![];
        for (field, value) in [
            ("positionSec", self.position_sec),
            ("durationSec", self.duration_sec),
            ("volume", self.volume),
        ] {
            let kind = if value.is_nan() {
                Some(NumberKind::NaN)
            } else if value == f64::INFINITY {
                Some(NumberKind::PositiveInfinity)
            } else if value == f64::NEG_INFINITY {
                Some(NumberKind::NegativeInfinity)
            } else if value == 0.0 && value.is_sign_negative() {
                Some(NumberKind::NegativeZero)
            } else {
                None
            };
            if let Some(kind) = kind {
                special_numbers.push(NumberRestoration { field, kind });
            } else {
                data[field] = json!(value);
            }
        }
        WireState {
            data,
            undefined_fields: if self.album_art.is_none() {
                vec!["albumArt"]
            } else {
                vec![]
            },
            special_numbers,
        }
    }
}
struct Patterns {
    title: Regex,
    artist: Regex,
    len: Regex,
    art: Regex,
    position: Regex,
    volume: Regex,
}
fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        // Exact ECMAScript \s and ASCII \d/\b, not Rust Unicode \d/\b.
        let ws = r"[\t\n\x0b\x0c\r \u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]";
        let re = |p: &str| Regex::new(&p.replace("\\s", ws)).expect("fixed reviewed regular expression");
        Patterns {
            title: re(r#"string\s+"xesam:title"\s+variant\s+string\s+"([^"]+)""#),
            artist: re(r#"string\s+"xesam:artist"\s+variant\s+array\s+\[\s+string\s+"([^"]+)""#),
            len: re(r#"string\s+"mpris:length"\s+variant\s+uint64\s+([0-9]+)"#),
            art: re(r#"string\s+"mpris:artUrl"\s+variant\s+string\s+"([^"]+)""#),
            position: re(r"(?-u:\b)int64\s+([0-9]+)"),
            volume: re(r"double\s+([0-9.]+)"),
        }
    })
}
fn capture(re: &Regex, text: &str) -> Option<String> {
    re.captures(text).map(|c| c[1].into())
}
fn integer(text: Option<String>, fallback: &str) -> f64 {
    text.as_deref()
        .unwrap_or(fallback)
        .parse::<f64>()
        .unwrap_or(f64::NAN)
}
fn float(text: Option<String>) -> f64 {
    let text = text.as_deref().unwrap_or("1");
    let mut dot = false;
    let end = text
        .bytes()
        .position(|b| {
            if b == b'.' {
                if dot {
                    true
                } else {
                    dot = true;
                    false
                }
            } else {
                false
            }
        })
        .unwrap_or(text.len());
    text[..end].parse::<f64>().unwrap_or(f64::NAN)
}
/// The immutable Linux helper's combined printed-output parser. No D-Bus calls.
pub fn parse_linux(text: &str) -> Result<Outcome> {
    if text.len() > 1024 * 1024 {
        return Err(Error::new(
            "spotify_output_limit",
            "Printed metadata exceeds supported bounds",
        ));
    }
    let p = patterns();
    let Some(title) = capture(&p.title, text) else {
        return Ok(Outcome::SourceNull(NullReason::MissingTitle));
    };
    Ok(Outcome::State(State {
        is_playing: text.contains("Playing"),
        title,
        artist: capture(&p.artist, text).unwrap_or_else(|| "Unknown".into()),
        position_sec: integer(capture(&p.position, text), "0") / 1_000_000.0,
        duration_sec: integer(capture(&p.len, text), "0") / 1_000_000.0,
        album_art: capture(&p.art, text),
        volume: float(capture(&p.volume, text)),
    }))
}
