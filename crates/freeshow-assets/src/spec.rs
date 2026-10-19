use serde::Serialize;
use std::{collections::HashMap, path::Path, sync::OnceLock};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
    pub status_code: u16,
}

/// Source protected.ts::parseRange policy, including its unanchored first
/// match, suffix-zero and empty-file behavior. Asset sizes stay below 2^53.
pub fn parse_range(header: Option<&str>, length: u64) -> ByteRange {
    let full = ByteRange {
        start: 0,
        end: length.saturating_sub(1),
        status_code: 200,
    };
    let Some(header) = header.filter(|header| !header.is_empty()) else {
        return full;
    };
    if length == 0 {
        return full;
    }
    let matched = header.match_indices("bytes=").find_map(|(index, _)| {
        let tail = &header[index + 6..];
        let count = tail.bytes().take_while(u8::is_ascii_digit).count();
        (tail.as_bytes().get(count) == Some(&b'-')).then_some((tail, count))
    });
    let Some((tail, start_length)) = matched else {
        return full;
    };
    let end_text = &tail[start_length + 1..];
    let end_length = end_text.bytes().take_while(u8::is_ascii_digit).count();
    let number = |text: &str| text.parse::<f64>().unwrap_or(f64::INFINITY);
    let mut start = if start_length > 0 {
        number(&tail[..start_length])
    } else {
        0.0
    };
    let mut end = if end_length > 0 {
        number(&end_text[..end_length])
    } else {
        (length - 1) as f64
    };
    if start_length == 0 && end_length > 0 {
        start = ((length as f64) - end).max(0.0);
        end = (length - 1) as f64;
    }
    if end >= length as f64 {
        end = (length - 1) as f64;
    }
    if start >= length as f64 || start > end {
        return ByteRange {
            start: 0,
            end: 0,
            status_code: 416,
        };
    }
    ByteRange {
        start: start as u64,
        end: end as u64,
        status_code: 206,
    }
}

pub fn mime_type(path: &Path) -> &'static str {
    static TYPES: OnceLock<HashMap<String, String>> = OnceLock::new();
    let types = TYPES.get_or_init(|| {
        serde_json::from_str(include_str!("mime.json")).expect("checked source MIME table")
    });
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();
    let standard = match extension.as_str() {
        "mp3" => Some("audio/mpeg"),
        "wav" => Some("audio/wav"),
        "ogg" | "oga" | "opus" => Some("audio/ogg"),
        "m4a" => Some("audio/x-m4a"),
        "aac" => Some("audio/aac"),
        "flac" => Some("audio/flac"),
        "weba" => Some("audio/webm"),
        "wma" => Some("audio/x-ms-wma"),
        "aif" | "aiff" => Some("audio/x-aiff"),
        "mid" | "midi" => Some("audio/midi"),
        "svg" => Some("image/svg+xml"),
        "bmp" => Some("image/bmp"),
        "tif" | "tiff" => Some("image/tiff"),
        "jfif" => Some("image/jpeg"),
        "heic" => Some("image/heif"),
        "wmv" => Some("video/x-ms-wmv"),
        "avif" => Some("image/avif"),
        "pdf" => Some("application/pdf"),
        _ => None,
    };
    if let Some(standard) = standard {
        return standard;
    }
    types
        .get(&extension)
        .map(String::as_str)
        .unwrap_or("application/octet-stream")
}
