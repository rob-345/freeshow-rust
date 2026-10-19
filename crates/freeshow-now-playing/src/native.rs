//! Exact native common values; compatibility JSON never drives native formatting.
use crate::{bounded_json, error, num, string, truthy, Error, Picture, Result, MAX_COVER};
use freeshow_audio::{CommonValue, NativeCommon};
use serde_json::Value;
use std::{collections::BTreeSet, sync::Arc};

pub(crate) const COMMON_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) enum RawCommon {
    Parsed(Arc<NativeCommon>),
    Host(Arc<CommonValue>),
}
impl RawCommon {
    pub fn value(&self) -> &CommonValue {
        match self {
            Self::Parsed(value) => value.value(),
            Self::Host(value) => value,
        }
    }
}

pub(crate) enum Common {
    Json(Value),
    Native(RawCommon),
}
impl Common {
    pub fn get(&self, key: &str) -> View<'_> {
        match self {
            Self::Json(value) => View::Json(value).get(key),
            Self::Native(value) => View::Native(value.value()).get(key),
        }
    }
    pub fn json(&self) -> Value {
        match self {
            Self::Json(value) => value.clone(),
            Self::Native(value) => value.value().json(),
        }
    }
    pub fn raw(&self) -> Result<RawCommon> {
        Ok(match self {
            Self::Native(value) => value.clone(),
            Self::Json(value) => {
                let value = CommonValue::from(value.clone());
                validate_object(&value, COMMON_LIMIT, "picture")?;
                RawCommon::Host(Arc::new(value))
            }
        })
    }
    pub fn is_native(&self) -> bool {
        matches!(self, Self::Native(_))
    }
}

#[derive(Clone, Copy)]
pub(crate) enum View<'a> {
    Undefined,
    Json(&'a Value),
    Native(&'a CommonValue),
}
impl<'a> View<'a> {
    pub fn get(self, key: &str) -> Self {
        match self {
            Self::Json(value) => value.get(key).map_or(Self::Undefined, Self::Json),
            Self::Native(value) => Self::Native(value.get(key)),
            Self::Undefined => Self::Undefined,
        }
    }
    pub fn nullish(self) -> bool {
        matches!(self, Self::Undefined | Self::Json(Value::Null))
            || matches!(self, Self::Native(value) if value.nullish())
    }
    pub fn truthy(self) -> bool {
        match self {
            Self::Undefined => false,
            Self::Json(value) => truthy(value),
            Self::Native(value) => value.truthy(),
        }
    }
    pub fn string(self) -> String {
        match self {
            Self::Undefined => "undefined".into(),
            Self::Json(value) => string(value),
            Self::Native(value) => native_string(value),
        }
    }
    pub fn as_str(self) -> Option<&'a str> {
        match self {
            Self::Json(Value::String(value)) | Self::Native(CommonValue::String(value)) => {
                Some(value)
            }
            _ => None,
        }
    }
    pub fn array(self) -> Option<Vec<Self>> {
        match self {
            Self::Json(Value::Array(values)) => Some(values.iter().map(Self::Json).collect()),
            Self::Native(CommonValue::Array(values)) => {
                Some(values.iter().map(Self::Native).collect())
            }
            _ => None,
        }
    }
}

fn native_string(value: &CommonValue) -> String {
    match value {
        CommonValue::Undefined => "undefined".into(),
        CommonValue::Null => "null".into(),
        CommonValue::Bool(value) => value.to_string(),
        CommonValue::Number(value) => num(*value),
        CommonValue::String(value) => value.clone(),
        CommonValue::Array(values) => values
            .iter()
            .map(|value| {
                if value.nullish() {
                    String::new()
                } else {
                    native_string(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        CommonValue::Object(_) => "[object Object]".into(),
    }
}

/// Actual ordered source picture fields, separate from compact typed bytes.
/// `Undefined` must come from authoritative native evidence, never JSON null.
#[derive(Clone, Debug)]
pub struct NativePicture {
    fields: Arc<CommonValue>,
    bytes: Arc<[u8]>,
}
impl NativePicture {
    pub fn from_host(fields: CommonValue, bytes: Vec<u8>) -> Result<Self> {
        validate_object(&fields, 65_536, "data")?;
        if bytes.len() > MAX_COVER {
            return Err(error("now_playing_picture"));
        }
        Ok(Self {
            fields: Arc::new(fields),
            bytes: bytes.into(),
        })
    }
    pub fn fields(&self) -> &CommonValue {
        &self.fields
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn projection(&self) -> Picture {
        Picture {
            fields: self.fields.json(),
            bytes: self.bytes.to_vec(),
        }
    }
    pub(crate) fn from_legacy(value: &Picture) -> Result<Self> {
        Self::from_host(CommonValue::from(value.fields.clone()), value.bytes.clone())
    }
}

/// A native observation attached to the issued PublishMetadata effect. It is
/// not serializable and carries no renderer/publication authority of its own.
#[derive(Clone, Debug)]
pub struct NativeMetadataPublication {
    file_path: String,
    common: RawCommon,
    pictures: Arc<[NativePicture]>,
}
impl NativeMetadataPublication {
    pub(crate) fn new(file_path: String, common: RawCommon, pictures: Vec<NativePicture>) -> Self {
        Self {
            file_path,
            common,
            pictures: pictures.into(),
        }
    }
    pub fn file_path(&self) -> &str {
        &self.file_path
    }
    pub fn common(&self) -> &CommonValue {
        self.common.value()
    }
    /// Present only for the actual parser-owned receipt, not trusted host fixtures.
    pub fn parsed_common(&self) -> Option<&NativeCommon> {
        match &self.common {
            RawCommon::Parsed(common) => Some(common),
            RawCommon::Host(_) => None,
        }
    }
    pub fn pictures(&self) -> &[NativePicture] {
        &self.pictures
    }
}

pub(crate) fn validate_object(value: &CommonValue, limit: usize, excluded: &str) -> Result<()> {
    validate(value, limit)?;
    let CommonValue::Object(fields) = value else {
        return Err(error("now_playing_metadata"));
    };
    if fields.iter().any(|(key, _)| key == excluded) {
        return Err(error("now_playing_metadata"));
    }
    Ok(())
}

// Conservative accounting includes undefined/nonfinite values before any
// projection, escaping, allocation of another tree, or recursive formatting.
fn validate(value: &CommonValue, mut limit: usize) -> Result<()> {
    fn visit(
        value: &CommonValue,
        depth: usize,
        nodes: &mut usize,
        bytes: &mut usize,
    ) -> Result<()> {
        if depth > 8 {
            return Err(error("now_playing_input_limit"));
        }
        *nodes = nodes
            .checked_sub(1)
            .ok_or_else(|| error("now_playing_input_limit"))?;
        *bytes = bytes
            .checked_sub(32)
            .ok_or_else(|| error("now_playing_input_limit"))?;
        let text = |value: &str, bytes: &mut usize| -> Result<()> {
            *bytes = bytes
                .checked_sub(
                    value
                        .len()
                        .checked_mul(6)
                        .ok_or_else(|| error("now_playing_input_limit"))?,
                )
                .ok_or_else(|| error("now_playing_input_limit"))?;
            Ok(())
        };
        match value {
            CommonValue::String(value) => text(value, bytes)?,
            CommonValue::Array(values) => {
                if values.len() > *nodes {
                    return Err(error("now_playing_input_limit"));
                }
                for value in values {
                    visit(value, depth + 1, nodes, bytes)?;
                }
            }
            CommonValue::Object(fields) => {
                if fields.len() > *nodes {
                    return Err(error("now_playing_input_limit"));
                }
                let mut keys = BTreeSet::new();
                let mut last_index = None;
                let mut named_key_seen = false;
                for (key, value) in fields {
                    text(key, bytes)?;
                    if !keys.insert(key.as_str())
                        || matches!(key.as_str(), "toString" | "valueOf" | "__proto__")
                    {
                        return Err(Error::native(
                            "now_playing_unsupported_common",
                            "Duplicate keys or custom primitive coercion are not supported",
                        ));
                    }
                    let index = key
                        .parse::<u32>()
                        .ok()
                        .filter(|index| *index != u32::MAX && index.to_string() == *key);
                    if let Some(index) = index {
                        if named_key_seen || last_index.is_some_and(|last| index <= last) {
                            return Err(Error::native(
                                "now_playing_unsupported_common",
                                "Native object keys must use actual JavaScript enumeration order",
                            ));
                        }
                        last_index = Some(index);
                    } else {
                        named_key_seen = true;
                    }
                    visit(value, depth + 1, nodes, bytes)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    visit(value, 0, &mut 1024, &mut limit)
}

pub(crate) fn validate_legacy(pictures: &[Picture]) -> Result<()> {
    if pictures.len() > 8 || pictures.iter().map(|p| p.bytes.len()).sum::<usize>() > MAX_COVER {
        return Err(error("now_playing_picture"));
    }
    for picture in pictures {
        bounded_json(&picture.fields, 65_536)?;
        if !picture.fields.is_object() || picture.fields.get("data").is_some() {
            return Err(error("now_playing_picture"));
        }
    }
    Ok(())
}
