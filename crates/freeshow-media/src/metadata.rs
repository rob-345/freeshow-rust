use crate::MediaError;
use serde_json::{json, Value};
use std::path::Path;
use tokio::io::AsyncReadExt;

pub(crate) async fn has_mp4_header(path: &str) -> bool {
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    let Ok(file) = tokio::fs::File::open(path).await else {
        return false;
    };
    let mut header = Vec::with_capacity((metadata.len().min(256 * 1024)) as usize);
    if file
        .take(256 * 1024)
        .read_to_end(&mut header)
        .await
        .is_err()
    {
        return false;
    }
    header.windows(4).any(|bytes| bytes == b"ftyp")
}

// This is deliberately the original narrow map, including its lack of audio MIME types.
fn mime_type(path: Option<&str>) -> &'static str {
    let extension = path
        .and_then(|path| Path::new(path).extension())
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();
    match extension.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mov" => "video/quicktime",
        "3gp" => "video/3gpp",
        "3g2" => "video/3gpp2",
        "avi" => "video/x-msvideo",
        "mkv" => "video/x-matroska",
        "flv" => "video/x-flv",
        "ts" => "video/mp2t",
        "dvrms" => "video/x-ms-dvr",
        "mpeg" | "mpg" => "video/mpeg",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

pub(crate) fn fallback(command: &str, payload: &Value) -> Value {
    let mut data = payload.clone();
    let object = data.as_object_mut().expect("validated object");
    if command == "MEDIA_CODEC" {
        let mime_type = mime_type(object.get("path").and_then(Value::as_str));
        object.insert("codecs".into(), json!([]));
        object.insert("mimeType".into(), mime_type.into());
        object.insert("mimeCodec".into(), "".into());
    } else {
        object.insert("tracks".into(), json!([]));
    }
    data
}

pub(crate) fn project(command: &str, payload: Value, probe: &Value) -> Result<Value, MediaError> {
    let mut result = fallback(command, &payload);
    let Some(streams) = probe.get("streams").and_then(Value::as_array) else {
        return Ok(result);
    };
    if command == "MEDIA_CODEC" {
        let codecs: Vec<String> = streams.iter().map(codec).collect::<Result<_, _>>()?;
        if !codecs.is_empty() {
            result["mimeCodec"] = format!(
                "{}; codecs=\"{}\"",
                result["mimeType"].as_str().unwrap_or_default(),
                codecs.join(", ")
            )
            .into();
            result["codecs"] = json!(codecs);
        }
    } else {
        let packets = probe.get("packets").and_then(Value::as_array);
        let mut tracks = vec![];
        for stream in streams {
            // MP4Box classifies wvtt as metadata, so do not expose ffprobe's WebVTT classification.
            if stream.get("codec_tag_string").and_then(Value::as_str) == Some("wvtt") {
                continue;
            }
            let index = stream["index"]
                .as_u64()
                .ok_or_else(|| invalid("subtitle stream index"))?;
            let language = stream
                .pointer("/tags/language")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("und");
            let (numerator, denominator) = stream["time_base"]
                .as_str()
                .and_then(|value| value.split_once('/'))
                .and_then(|(a, b)| Some((a.parse::<f64>().ok()?, b.parse::<f64>().ok()?)))
                .filter(|(a, b)| a.is_finite() && b.is_finite() && *a > 0.0 && *b > 0.0)
                .ok_or_else(|| invalid("subtitle time base"))?;
            let mut lines = vec!["WEBVTT\n".to_owned()];
            let mut cue = 1;
            for packet in packets
                .into_iter()
                .flatten()
                .filter(|packet| packet["stream_index"].as_u64() == Some(index))
            {
                let bytes = decode_hex(
                    packet["data"]
                        .as_str()
                        .ok_or_else(|| invalid("subtitle packet bytes"))?,
                )?;
                // Original: TextDecoder -> remove everything except ASCII + CR/LF/TAB -> trim.
                // Invalid UTF-8 becomes U+FFFD and is removed, equivalent to filtering these bytes.
                // TextDecoder consumes an initial UTF-8 BOM; these non-ASCII bytes are removed too.
                let text: String = bytes
                    .into_iter()
                    .filter(|byte| matches!(byte, b' '..=b'~' | b'\r' | b'\n' | b'\t'))
                    .map(char::from)
                    .collect();
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                let start = packet["pts"]
                    .as_f64()
                    .ok_or_else(|| invalid("subtitle packet timestamp"))?;
                let duration = packet["duration"].as_f64().ok_or_else(|| {
                    MediaError::UnsupportedVariant("subtitle packet without duration".into())
                })?;
                // Match JS operation order rather than decimal-rounded pts_time strings.
                let scale = denominator / numerator;
                lines.push(cue.to_string());
                lines.push(format!(
                    "{} --> {}",
                    timestamp((start / scale) * 1000.0),
                    timestamp(((start + duration) / scale) * 1000.0)
                ));
                lines.push(format!("{text}\n"));
                cue += 1;
            }
            if lines.len() > 1 {
                tracks.push(json!({"lang": language.chars().take(2).collect::<String>(), "name": language, "vtt": lines.join("\n"), "embedded": true}));
            }
        }
        result["tracks"] = json!(tracks);
    }
    Ok(result)
}

fn invalid(message: &str) -> MediaError {
    MediaError::InvalidOutput(message.into())
}

/// Decode ffprobe's hex dump without mistaking its ASCII column for data.
fn decode_hex(dump: &str) -> Result<Vec<u8>, MediaError> {
    let mut output = vec![];
    for line in dump.lines().filter(|line| !line.trim().is_empty()) {
        let (_, data) = line
            .split_once(':')
            .ok_or_else(|| invalid("hex dump offset"))?;
        // Hex data is a fixed 39-column field after a single separator space.
        let data = data.strip_prefix(' ').unwrap_or(data);
        let hex = data
            .get(..data.len().min(39))
            .ok_or_else(|| invalid("hex dump columns"))?;
        for word in hex.split_ascii_whitespace() {
            if !word.is_ascii() || word.len() % 2 != 0 {
                return Err(invalid("hex dump byte count"));
            }
            for offset in (0..word.len()).step_by(2) {
                output.push(
                    u8::from_str_radix(&word[offset..offset + 2], 16)
                        .map_err(|_| invalid("hex dump byte"))?,
                );
            }
        }
    }
    Ok(output)
}

fn codec(stream: &Value) -> Result<String, MediaError> {
    let tag = stream["codec_tag_string"]
        .as_str()
        .filter(|tag| tag.len() == 4 && !tag.contains('['))
        .ok_or_else(|| MediaError::UnsupportedVariant("missing MP4 sample entry tag".into()))?;
    let base = tag.replacen('.', "", 1);
    let extra = stream["extradata"]
        .as_str()
        .map(decode_hex)
        .transpose()?
        .unwrap_or_default();
    match tag {
        "avc1" | "avc2" | "avc3" | "avc4" => {
            if extra.len() < 4 {
                return Err(MediaError::UnsupportedVariant(
                    "AVC without avcC configuration".into(),
                ));
            }
            Ok(format!(
                "{base}.{:02x}{:02x}{:02x}",
                extra[1], extra[2], extra[3]
            ))
        }
        "mp4a" => {
            if stream["codec_name"].as_str() != Some("aac") || extra.is_empty() {
                return Err(MediaError::UnsupportedVariant("mp4a without AAC AudioSpecificConfig (non-AAC ES descriptor OTI is not exposed by ffprobe)".into()));
            }
            let mut kind = (extra[0] >> 3) as u16;
            if kind == 31 && extra.len() >= 2 {
                kind = 32 + ((extra[0] & 7) as u16 * 8) + ((extra[1] >> 5) as u16);
            }
            Ok(if kind == 0 {
                format!("{base}.40")
            } else {
                format!("{base}.40.{kind}")
            })
        }
        "hvc1" | "hvc2" | "hev1" | "hev2" => {
            if extra.len() < 13 {
                return Err(MediaError::UnsupportedVariant(
                    "HEVC without hvcC configuration".into(),
                ));
            }
            let profile_space = ["", "A", "B", "C"][(extra[1] >> 6) as usize];
            let compatibility = u32::from_be_bytes(extra[2..6].try_into().expect("four bytes"))
                .reverse_bits() as i32;
            // MP4Box's JS bitwise reverse is signed, including its unusual negative hex spelling.
            let compatibility = if compatibility < 0 {
                format!("-{:x}", -(compatibility as i64))
            } else {
                format!("{compatibility:x}")
            };
            let mut codec = format!(
                "{base}.{profile_space}{}.{compatibility}.{}{}",
                extra[1] & 31,
                if extra[1] & 32 == 0 { "L" } else { "H" },
                extra[12]
            );
            let constraints = &extra[6..12];
            if let Some(last) = constraints.iter().rposition(|byte| *byte != 0) {
                for byte in &constraints[..=last] {
                    codec.push_str(&format!(".{byte:x}"));
                }
            }
            Ok(codec)
        }
        "av01" => {
            if extra.len() < 3 {
                return Err(MediaError::UnsupportedVariant(
                    "AV1 without av1C configuration".into(),
                ));
            }
            let profile = extra[1] >> 5;
            let depth = if extra[2] & 64 == 0 {
                8
            } else if profile == 2 && extra[2] & 32 != 0 {
                12
            } else {
                10
            };
            Ok(format!(
                "{base}.{profile}.{:02}{}.{depth:02}",
                extra[1] & 31,
                if extra[2] & 128 == 0 { "M" } else { "H" }
            ))
        }
        "vp08" | "vp09" | "vvc1" | "vvi1" | "stxt" | "encv" | "enca" => {
            Err(MediaError::UnsupportedVariant(format!(
                "{tag} configuration is not exposed sufficiently by ffprobe"
            )))
        }
        _ => Ok(base),
    }
}

fn timestamp(milliseconds: f64) -> String {
    fn pad(value: f64, width: usize) -> String {
        let value = value.floor();
        let text = if value == 0.0 {
            "0".to_owned()
        } else {
            format!("{value:.0}")
        };
        format!("{text:0>width$}")
    }
    format!(
        "{}:{}:{}.{}",
        pad(milliseconds / 3_600_000.0, 2),
        pad((milliseconds % 3_600_000.0) / 60_000.0, 2),
        pad((milliseconds % 60_000.0) / 1000.0, 2),
        pad(milliseconds % 1000.0, 3)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mime_map_retains_audio_defaults_case_and_payload_fields() {
        for (path, mime) in [
            ("song.m4a", "application/octet-stream"),
            ("x.MP4", "video/mp4"),
            (".mp4", "application/octet-stream"),
            ("x.mp4?query", "application/octet-stream"),
            ("x.mov", "video/quicktime"),
        ] {
            assert_eq!(
                fallback(
                    "MEDIA_CODEC",
                    &json!({"path":path,"future":42,"codecs":["stale"]})
                ),
                json!({"path":path,"future":42,"codecs":[],"mimeType":mime,"mimeCodec":""})
            );
        }
    }
    #[test]
    fn hex_ascii_column_is_never_parsed() {
        assert_eq!(
            decode_hex("\n00000000: 000b 4865 6c6c 6f20 776f 726c 64         ..Hello world\n")
                .unwrap(),
            b"\0\x0bHello world"
        );
        assert!(decode_hex("00000000: zzzz").is_err());
    }
    #[test]
    fn configuration_codecs_and_timestamp_floor() {
        assert_eq!(
            codec(&json!({"codec_tag_string":"avc1","extradata":"00000000: 0164 000a"})).unwrap(),
            "avc1.64000a"
        );
        assert_eq!(
            codec(
                &json!({"codec_tag_string":"mp4a","codec_name":"aac","extradata":"00000000: 1588"})
            )
            .unwrap(),
            "mp4a.40.2"
        );
        assert_eq!(timestamp(3_661_002.99), "01:01:01.002");
    }
}
