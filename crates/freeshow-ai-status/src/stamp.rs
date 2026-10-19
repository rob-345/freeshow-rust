use crate::*;
use serde_json::value::RawValue;
use std::collections::HashMap;
type Object = HashMap<String, Box<RawValue>>;
pub(crate) struct Stamp(Object);
pub(crate) struct Entry(Object);
fn object(raw: &str) -> Option<Object> {
    serde_json::from_str(raw).ok()
}
fn string(raw: &RawValue) -> Option<String> {
    serde_json::from_str(raw.get()).ok()
}
fn number(raw: &RawValue) -> Option<f64> {
    // Raw JSON number -> correctly rounded binary64. serde_json's ordinary
    // decimal Value projection is not the original Node numeric observation.
    let raw = raw.get();
    if !raw.starts_with('-') && !raw.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    raw.parse().ok()
}

// Preflight BEFORE RawValue/object allocation. Strings are scalar Unicode;
// source permits lone UTF16 surrogates, which this native subset rejects.
fn preflight(bytes: &[u8]) -> Result<bool> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::unsupported())?;
    let b = text.as_bytes();
    let (mut i, mut depth, mut tokens) = (0, 0usize, 0usize);
    while i < b.len() {
        match b[i] {
            b'"' => {
                tokens += 1;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                        if i >= b.len() {
                            return Ok(false);
                        }
                        if b[i] == b'u' {
                            let Some(hex) = text.get(i + 1..i + 5) else {
                                return Ok(false);
                            };
                            let Ok(code) = u16::from_str_radix(hex, 16) else {
                                return Ok(false);
                            };
                            if (0xd800..=0xdbff).contains(&code) {
                                if b.get(i + 5..i + 7) != Some(b"\\u") {
                                    return Err(Error::unsupported());
                                }
                                let Some(low) = text.get(i + 7..i + 11) else {
                                    return Err(Error::unsupported());
                                };
                                if !u16::from_str_radix(low, 16)
                                    .is_ok_and(|n| (0xdc00..=0xdfff).contains(&n))
                                {
                                    return Err(Error::unsupported());
                                }
                                i += 6;
                            } else if (0xdc00..=0xdfff).contains(&code) {
                                return Err(Error::unsupported());
                            }
                            i += 4;
                        }
                    }
                    i += 1;
                }
                if i == b.len() {
                    return Ok(false);
                }
            }
            b'{' | b'[' => {
                depth += 1;
                tokens += 1;
                if depth > 16 {
                    return Err(Error::limit());
                }
            }
            b'}' | b']' => {
                if depth == 0 {
                    return Ok(false);
                }
                depth -= 1
            }
            b',' | b':' => tokens += 1,
            _ => {}
        }
        if tokens > 4096 {
            return Err(Error::limit());
        }
        i += 1;
    }
    Ok(true)
}
pub(crate) fn parse(bytes: &[u8]) -> Result<Option<Stamp>> {
    if !preflight(bytes)? {
        return Ok(None);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Error::unsupported())?;
    let Some(mut top) = object(text) else {
        return Ok(None);
    };
    if top.get("revision").and_then(|r| string(r)).as_deref() != Some(REVISION) {
        return Ok(None);
    }
    let Some(files) = top.remove("files").and_then(|v| object(v.get())) else {
        return Ok(None);
    };
    Ok(Some(Stamp(files)))
}
impl Stamp {
    pub(crate) fn entry(&self, name: &str, sha: &str) -> Option<Entry> {
        let entry = object(self.0.get(name)?.get())?;
        if entry.get("sha256").and_then(|r| string(r)).as_deref() != Some(sha) {
            return None;
        }
        Some(Entry(entry))
    }
}
impl Entry {
    pub(crate) fn numbers(&self) -> Option<(f64, f64)> {
        Some((
            number(self.0.get("size")?)?,
            number(self.0.get("mtimeMs")?)?,
        ))
    }
}
pub(crate) fn body(records: &[(&str, String, f64, f64)]) -> Vec<u8> {
    let mut body = format!("{{\n    \"revision\": \"{REVISION}\",\n    \"files\": {{\n");
    for (index, (name, sha, size, mtime)) in records.iter().enumerate() {
        use std::fmt::Write;
        let mut number = ryu_js::Buffer::new();
        let size = number.format(*size).to_owned();
        let mtime = number.format(*mtime);
        write!(body,"        \"{name}\": {{\n            \"sha256\": \"{sha}\",\n            \"size\": {size},\n            \"mtimeMs\": {mtime}\n        }}{}\n",if index==4{""}else{","}).expect("String writer");
    }
    body.push_str("    }\n}");
    body.into_bytes()
}
