//! Pure native-only audio metadata publication preparation. No parser/path grant proof.
use base64::{engine::general_purpose::STANDARD, Engine};
use freeshow_audio::CommonValue;
use serde::Serialize;
use serde_json::{Map, Value};
use std::{collections::BTreeSet, fmt};

pub const MAX_NODES: usize = 65536;
pub const MAX_DEPTH: usize = 16;
pub const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ENVELOPE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_STRING_BYTES: usize = 6 * 1024 * 1024;
pub const MAX_PATH_BYTES: usize = 1024 * 1024;
pub const MAX_KEY_BYTES: usize = 1024 * 1024;
pub const MAX_DECODED_BYTES: usize = 4 * 1024 * 1024;

/// Borrowed trusted receipt. Fields are ordered source fields excluding data.
/// No Deserialize and no guarantee that the bytes came from a real parser.
pub struct PictureRef<'a> {
    pub fields: &'a CommonValue,
    pub bytes: &'a [u8],
}
#[derive(Debug, Serialize)]
pub struct MetadataWire {
    pub data: Value,
    pub hydration: Hydration,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hydration {
    pub undefined_paths: Vec<String>,
    pub special_numbers: Vec<SpecialNumber>,
    pub byte_buffers: Vec<String>,
    pub object_keys: Vec<ObjectKeys>,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub enum NumberKind {
    #[serde(rename = "NaN")]
    Nan,
    PositiveInfinity,
    NegativeInfinity,
    NegativeZero,
}
impl NumberKind {
    fn name(self) -> &'static str {
        match self {
            Self::Nan => "NaN",
            Self::PositiveInfinity => "PositiveInfinity",
            Self::NegativeInfinity => "NegativeInfinity",
            Self::NegativeZero => "NegativeZero",
        }
    }
}
#[derive(Debug, Serialize)]
pub struct SpecialNumber {
    pub path: String,
    pub kind: NumberKind,
}
#[derive(Debug, Serialize)]
pub struct ObjectKeys {
    pub path: String,
    pub keys: Vec<String>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum WireError {
    Invalid(&'static str),
    Limit(&'static str),
}
impl WireError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "metadata_wire_invalid",
            Self::Limit(_) => "metadata_wire_limit",
        }
    }
}
impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(detail) => write!(f, "Invalid native metadata wire: {detail}"),
            Self::Limit(detail) => write!(f, "Native metadata wire limit: {detail}"),
        }
    }
}
impl std::error::Error for WireError {}
type Result<T> = std::result::Result<T, WireError>;

fn add(value: &mut usize, amount: usize, maximum: usize, label: &'static str) -> Result<()> {
    *value = value.checked_add(amount).ok_or(WireError::Limit(label))?;
    if *value > maximum {
        return Err(WireError::Limit(label));
    }
    Ok(())
}
fn string_size(value: &str) -> usize {
    2 + value
        .bytes()
        .map(|b| match b {
            b'"' | b'\\' | 8 | 9 | 10 | 12 | 13 => 2,
            0..=31 => 6,
            _ => 1,
        })
        .sum::<usize>()
}
fn kind(number: f64) -> Option<NumberKind> {
    if number.is_nan() {
        Some(NumberKind::Nan)
    } else if number == f64::INFINITY {
        Some(NumberKind::PositiveInfinity)
    } else if number == f64::NEG_INFINITY {
        Some(NumberKind::NegativeInfinity)
    } else if number == 0.0 && number.is_sign_negative() {
        Some(NumberKind::NegativeZero)
    } else {
        None
    }
}
fn key_index(key: &str) -> Option<u32> {
    let n = key.parse::<u32>().ok()?;
    (n != u32::MAX && n.to_string() == key).then_some(n)
}
fn push(path: &mut String, key: &str) -> Result<usize> {
    let old = path.len();
    let extra = 1 + key
        .bytes()
        .map(|b| if matches!(b, b'~' | b'/') { 2 } else { 1 })
        .sum::<usize>();
    // Ordinary leaves do not emit a pointer. Keep their traversal scratch bounded
    // by depth/key limits; only generated envelope pointers have the 2048 cap.
    if old + extra > 9 + MAX_DEPTH * (2 * 1024 + 1) {
        return Err(WireError::Limit("pointer scratch"));
    }
    path.push('/');
    for c in key.chars() {
        match c {
            '~' => path.push_str("~0"),
            '/' => path.push_str("~1"),
            _ => path.push(c),
        }
    }
    Ok(old)
}
fn extra_name(extra: Extra<'_>) -> Option<&'static str> {
    match extra {
        Extra::None => None,
        Extra::Pictures(_) => Some("picture"),
        Extra::Bytes(_) => Some("data"),
    }
}
#[derive(Clone, Copy)]
enum Extra<'a> {
    None,
    Pictures(&'a [PictureRef<'a>]),
    Bytes(&'a [u8]),
}

#[derive(Default)]
struct Counts {
    nodes: usize,
    metadata: usize,
    envelope: usize,
    keys: usize,
    paths: usize,
    decoded: usize,
    undefined: usize,
    special: usize,
    buffers: usize,
    orders: usize,
}
impl Counts {
    fn node(&mut self, n: usize) -> Result<()> {
        add(&mut self.nodes, n, MAX_NODES, "shared nodes")
    }
    fn meta(&mut self, n: usize) -> Result<()> {
        add(&mut self.metadata, n, MAX_METADATA_BYTES, "metadata JSON")
    }
    fn env(&mut self, n: usize) -> Result<()> {
        add(&mut self.envelope, n, MAX_ENVELOPE_BYTES, "envelope JSON")
    }
    fn key(&mut self, key: &str) -> Result<()> {
        if key.len() > 1024 {
            return Err(WireError::Limit("individual key"));
        }
        if matches!(key, "__proto__" | "constructor" | "prototype") {
            return Err(WireError::Invalid("dangerous key"));
        }
        add(&mut self.keys, key.len(), MAX_KEY_BYTES, "aggregate keys")
    }
    fn path(&mut self, path: &str) -> Result<()> {
        if path.len() > 2048 {
            return Err(WireError::Limit("individual pointer"));
        }
        add(
            &mut self.paths,
            path.len(),
            MAX_PATH_BYTES,
            "aggregate pointers",
        )
    }
    fn start() -> Result<Self> {
        let mut s = Self::default();
        s.node(2 + 1 + 4)?; // payload+filePath, envelope root+four arrays
        s.env(2 + 3 + 4 * 2)?;
        for key in [
            "undefinedPaths",
            "specialNumbers",
            "byteBuffers",
            "objectKeys",
        ] {
            s.key(key)?;
            s.env(string_size(key) + 1)?;
        }
        Ok(s)
    }
    fn undefined(&mut self, path: &str) -> Result<()> {
        self.path(path)?;
        self.node(1)?;
        self.env(usize::from(self.undefined > 0) + string_size(path))?;
        self.undefined += 1;
        Ok(())
    }
    fn special(&mut self, path: &str, kind: NumberKind) -> Result<()> {
        self.path(path)?;
        self.node(3)?;
        self.key("path")?;
        self.key("kind")?;
        self.env(
            usize::from(self.special > 0)
                + 3
                + string_size("path")
                + 1
                + string_size(path)
                + string_size("kind")
                + 1
                + string_size(kind.name()),
        )?;
        self.special += 1;
        Ok(())
    }
    fn buffer(&mut self, path: &str) -> Result<()> {
        self.path(path)?;
        self.node(1)?;
        self.env(usize::from(self.buffers > 0) + string_size(path))?;
        self.buffers += 1;
        Ok(())
    }
    fn object(
        &mut self,
        fields: &[(String, CommonValue)],
        extra: Extra<'_>,
        path: &mut String,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(WireError::Limit("metadata depth"));
        }
        let len = fields
            .len()
            .checked_add(usize::from(extra_name(extra).is_some()))
            .ok_or(WireError::Limit("object fields"))?;
        // Reserve a lower bound before allocating the bounded borrowed-key set.
        if len > (MAX_NODES.saturating_sub(self.nodes).saturating_sub(4)) / 2 {
            return Err(WireError::Limit("shared nodes"));
        }
        self.node(1 + 3 + len)?; // metadata object, order record/path/array/key strings
        self.path(path)?;
        self.key("path")?;
        self.key("keys")?;
        self.meta(2 + len.saturating_sub(1))?;
        self.env(
            usize::from(self.orders > 0)
                + 3
                + string_size("path")
                + 1
                + string_size(path)
                + string_size("keys")
                + 1
                + 2
                + len.saturating_sub(1),
        )?;
        self.orders += 1;
        let mut unique = BTreeSet::new();
        let mut non_index = false;
        let mut last_index = None;
        for key in fields
            .iter()
            .map(|(key, _)| key.as_str())
            .chain(extra_name(extra))
        {
            self.key(key)?;
            self.key(key)?;
            if !unique.insert(key) {
                return Err(WireError::Invalid("duplicate object key"));
            }
            match key_index(key) {
                Some(n) if non_index || last_index.is_some_and(|last| n <= last) => {
                    return Err(WireError::Invalid("nonrepresentable integer-key order"))
                }
                Some(n) => last_index = Some(n),
                None => non_index = true,
            }
            self.meta(string_size(key) + 1)?;
            self.env(string_size(key))?;
        }
        for (key, value) in fields {
            let old = push(path, key)?;
            let result = self.value(value, path, depth + 1);
            path.truncate(old);
            result?;
        }
        if let Some(key) = extra_name(extra) {
            let old = push(path, key)?;
            let result = match extra {
                Extra::Pictures(pictures) => self.pictures(pictures, path, depth + 1),
                Extra::Bytes(bytes) => self.bytes(bytes, path, depth + 1),
                Extra::None => unreachable!(),
            };
            path.truncate(old);
            result?;
        }
        Ok(())
    }
    fn value(&mut self, value: &CommonValue, path: &mut String, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(WireError::Limit("metadata depth"));
        }
        if let CommonValue::Object(fields) = value {
            return self.object(fields, Extra::None, path, depth);
        }
        self.node(1)?;
        match value {
            CommonValue::Undefined => {
                self.meta(4)?;
                self.undefined(path)?;
            }
            CommonValue::Null => self.meta(4)?,
            CommonValue::Bool(_) => self.meta(5)?,
            CommonValue::Number(n) => match kind(*n) {
                Some(k) => {
                    self.meta(if matches!(k, NumberKind::NegativeZero) {
                        32
                    } else {
                        4
                    })?;
                    self.special(path, k)?;
                }
                None => self.meta(32)?,
            },
            CommonValue::String(s) => {
                if s.len() > MAX_STRING_BYTES - 2 {
                    return Err(WireError::Limit("individual metadata string"));
                }
                let size = string_size(s);
                if size > MAX_STRING_BYTES {
                    return Err(WireError::Limit("individual metadata string"));
                }
                self.meta(size)?;
            }
            CommonValue::Array(values) => {
                if values.len() > MAX_NODES.saturating_sub(self.nodes) {
                    return Err(WireError::Limit("shared nodes"));
                }
                self.meta(2 + values.len().saturating_sub(1))?;
                for (i, value) in values.iter().enumerate() {
                    let old = push(path, &i.to_string())?;
                    let result = self.value(value, path, depth + 1);
                    path.truncate(old);
                    result?;
                }
            }
            CommonValue::Object(_) => unreachable!(),
        }
        Ok(())
    }
    fn pictures(
        &mut self,
        pictures: &[PictureRef<'_>],
        path: &mut String,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(WireError::Limit("metadata depth"));
        }
        self.node(1)?;
        self.meta(2 + pictures.len().saturating_sub(1))?;
        for (i, picture) in pictures.iter().enumerate() {
            let CommonValue::Object(fields) = picture.fields else {
                return Err(WireError::Invalid("picture fields must be object"));
            };
            if fields.len() > MAX_NODES {
                return Err(WireError::Limit("object fields"));
            }
            if fields.iter().any(|(key, _)| key == "data") {
                return Err(WireError::Invalid("picture fields already contain data"));
            }
            let old = push(path, &i.to_string())?;
            let result = self.object(fields, Extra::Bytes(picture.bytes), path, depth + 1);
            path.truncate(old);
            result?;
        }
        Ok(())
    }
    fn bytes(&mut self, bytes: &[u8], path: &str, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(WireError::Limit("metadata depth"));
        }
        add(
            &mut self.decoded,
            bytes.len(),
            MAX_DECODED_BYTES,
            "decoded picture bytes",
        )?;
        let encoded = bytes.len().div_ceil(3) * 4;
        if encoded + 2 > MAX_STRING_BYTES {
            return Err(WireError::Limit("individual metadata string"));
        }
        self.node(1)?;
        self.meta(encoded + 2)?;
        self.buffer(path)
    }
}

struct Builder {
    hydration: Hydration,
}
impl Builder {
    fn object(
        &mut self,
        fields: &[(String, CommonValue)],
        extra: Extra<'_>,
        path: &mut String,
    ) -> Value {
        let mut keys: Vec<_> = fields.iter().map(|(k, _)| k.clone()).collect();
        if let Some(key) = extra_name(extra) {
            keys.push(key.into());
        }
        self.hydration.object_keys.push(ObjectKeys {
            path: path.clone(),
            keys,
        });
        let mut object =
            Map::with_capacity(fields.len() + usize::from(extra_name(extra).is_some()));
        for (key, value) in fields {
            let old = push(path, key).expect("preflight pointer");
            let value = self.value(value, path);
            path.truncate(old);
            object.insert(key.clone(), value);
        }
        if let Some(key) = extra_name(extra) {
            let old = push(path, key).expect("preflight pointer");
            let value = match extra {
                Extra::Pictures(pictures) => Value::Array(
                    pictures
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let old = push(path, &i.to_string()).expect("preflight pointer");
                            let CommonValue::Object(fields) = p.fields else {
                                unreachable!()
                            };
                            let value = self.object(fields, Extra::Bytes(p.bytes), path);
                            path.truncate(old);
                            value
                        })
                        .collect(),
                ),
                Extra::Bytes(bytes) => {
                    self.hydration.byte_buffers.push(path.clone());
                    Value::String(STANDARD.encode(bytes))
                }
                Extra::None => unreachable!(),
            };
            path.truncate(old);
            object.insert(key.into(), value);
        }
        Value::Object(object)
    }
    fn value(&mut self, value: &CommonValue, path: &mut String) -> Value {
        match value {
            CommonValue::Undefined => {
                self.hydration.undefined_paths.push(path.clone());
                Value::Null
            }
            CommonValue::Null => Value::Null,
            CommonValue::Bool(v) => Value::Bool(*v),
            CommonValue::Number(n) => match kind(*n) {
                Some(kind) => {
                    self.hydration.special_numbers.push(SpecialNumber {
                        path: path.clone(),
                        kind,
                    });
                    if matches!(kind, NumberKind::NegativeZero) {
                        Value::from(0)
                    } else {
                        Value::Null
                    }
                }
                None => {
                    Value::Number(serde_json::Number::from_f64(*n).expect("finite source number"))
                }
            },
            CommonValue::String(s) => Value::String(s.clone()),
            CommonValue::Array(values) => Value::Array(
                values
                    .iter()
                    .enumerate()
                    .map(|(i, value)| {
                        let old = push(path, &i.to_string()).expect("preflight pointer");
                        let v = self.value(value, path);
                        path.truncate(old);
                        v
                    })
                    .collect(),
            ),
            CommonValue::Object(fields) => self.object(fields, Extra::None, path),
        }
    }
}

/// Prepare one bounded publication from trusted typed receipts. This does not
/// establish complete covers, file authorization, live context or source parsing.
/// A separate pictures array appends root picture; its original common position
/// is not recovered. Input fields exclude data, which is appended last per picture.
pub fn build(
    file_path: &str,
    common: &CommonValue,
    pictures: &[PictureRef<'_>],
) -> Result<MetadataWire> {
    if file_path.len() > 32768 {
        return Err(WireError::Limit("filePath"));
    }
    if pictures.len() > 8 {
        return Err(WireError::Limit("picture count"));
    }
    let CommonValue::Object(fields) = common else {
        return Err(WireError::Invalid("common must be object"));
    };
    if fields.len() > MAX_NODES {
        return Err(WireError::Limit("object fields"));
    }
    if fields.iter().any(|(key, _)| key == "picture") {
        return Err(WireError::Invalid("common picture must be separate"));
    }
    let extra = if pictures.is_empty() {
        Extra::None
    } else {
        Extra::Pictures(pictures)
    };
    let mut path = String::from("/metadata");
    let mut counts = Counts::start()?;
    counts.object(fields, extra, &mut path, 0)?;
    // All native input and generated envelope charges have passed before large
    // JSON/base64 allocations. This second pass borrows the same immutable input.
    let mut builder = Builder {
        hydration: Hydration {
            undefined_paths: Vec::with_capacity(counts.undefined),
            special_numbers: Vec::with_capacity(counts.special),
            byte_buffers: Vec::with_capacity(counts.buffers),
            object_keys: Vec::with_capacity(counts.orders),
        },
    };
    let metadata = builder.object(fields, extra, &mut path);
    let mut data = Map::with_capacity(2);
    data.insert("filePath".into(), Value::String(file_path.into()));
    data.insert("metadata".into(), metadata);
    Ok(MetadataWire {
        data: Value::Object(data),
        hydration: builder.hydration,
    })
}

#[cfg(test)]
mod tests;
