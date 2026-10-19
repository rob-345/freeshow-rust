use crate::{Error, Result};
use regex::Regex;
use std::sync::OnceLock;

/// Native preparation only; never deserialized into executable or bus authority.
#[derive(Debug)]
pub struct PreparedCommand(pub(crate) Command);
#[derive(Debug)]
pub(crate) enum Command {
    PlayPause,
    Next,
    Previous,
    Pause,
    SetVolume(String),
    Seek(String),
}
impl PreparedCommand {
    /// `None` means source undefined, not JSON null/string/bool coercion. The
    /// host must reject those unsupported numeric payload shapes explicitly.
    pub fn new(name: &str, value: Option<f64>) -> Result<Self> {
        Ok(Self(match name {
            "playpause" => Command::PlayPause,
            "next" => Command::Next,
            "prev" => Command::Previous,
            "pause" => Command::Pause,
            "setVolume" => {
                let number = numeric(value)?;
                Command::SetVolume(ryu_js::Buffer::new().format(number).into())
            }
            "seek" => {
                let scaled = numeric(value)? * 1_000_000.0;
                if !scaled.is_finite() || scaled.abs() > 9_007_199_254_740_991.0 {
                    return Err(unsupported(
                        "Seek exceeds the exact integer microsecond range",
                    ));
                }
                // Math.round ties toward +infinity, without the incorrect
                // floating-point x+0.5 shortcut. -0 interpolates as source "0".
                let floor = scaled.floor();
                let rounded = if scaled - floor >= 0.5 {
                    floor + 1.0
                } else {
                    floor
                };
                Command::Seek((rounded as i64).to_string())
            }
            _ => return Err(unsupported("Unsupported native Spotify command")),
        }))
    }
    pub fn name(&self) -> &'static str {
        match self.0 {
            Command::PlayPause => "playpause",
            Command::Next => "next",
            Command::Previous => "prev",
            Command::Pause => "pause",
            Command::SetVolume(_) => "setVolume",
            Command::Seek(_) => "seek",
        }
    }
}
fn numeric(value: Option<f64>) -> Result<f64> {
    value
        .filter(|n| n.is_finite())
        .ok_or_else(|| unsupported("A finite source Number is required for this Spotify command"))
}
pub(crate) fn unsupported(message: &'static str) -> Error {
    Error::new("spotify_command_unsupported", message)
}
pub(crate) fn track_id(text: &str) -> Result<Option<&str>> {
    if text.len() > 1024 * 1024 {
        return Err(Error::new(
            "spotify_output_limit",
            "Native metadata exceeds the output bound",
        ));
    }
    // Explicit ECMAScript WhiteSpace/LineTerminator set, including FEFF and
    // excluding Unicode NEL, unlike regex crate's broader Unicode \\s.
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(concat!(
        "string[\\u0009-\\u000d \\u00a0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000\\ufeff]+",
        "\"mpris:trackid\"[\\u0009-\\u000d \\u00a0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000\\ufeff]+",
        "variant[\\u0009-\\u000d \\u00a0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000\\ufeff]+",
        "string[\\u0009-\\u000d \\u00a0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000\\ufeff]+\"([^\"]+)\""
    )).expect("fixed source track-id regex"));
    let Some(captured) = re.captures(text).and_then(|c| c.get(1)) else {
        return Ok(None);
    };
    let id = captured.as_str();
    if id.len() > 1024
        || !id.starts_with('/')
        || id.len() > 1
            && id[1..].split('/').any(|segment| {
                segment.is_empty()
                    || !segment
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
    {
        return Err(unsupported(
            "Source track ID is outside the safe D-Bus object-path subset",
        ));
    }
    Ok(Some(id))
}

/// Actual direct-child completion, never a remote server success assertion.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CommandOutcome {
    Sent,
    MissingTrackId,
    SourceProcessFailure { service_unknown: bool },
}
