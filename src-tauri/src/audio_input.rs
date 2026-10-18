//! Pure native AUDIO raw framing. No transport, window authority or sink state.
use freeshow_audio_pipeline::{AudioMetadata, JsInput};
use freeshow_core::protocol::RuntimeContext;
use serde::{
    de::{Error as _, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::{value::RawValue, Map, Number, Value};
use std::{collections::HashSet, fmt, path::PathBuf};

const PREFIX_LIMIT: usize = 49152;
const PCM_LIMIT: usize = 32768;
const METADATA_LIMIT: usize = 8192;
type Result<T> = std::result::Result<T, AudioInputError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioInputError {
    pub code: &'static str,
    pub message: String,
}
impl fmt::Display for AudioInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for AudioInputError {}
fn error(code: &'static str, message: &str) -> AudioInputError {
    AudioInputError {
        code,
        message: message.into(),
    }
}
fn invalid(_: serde_json::Error) -> AudioInputError {
    error("audio_input_json", "Invalid or ambiguous AUDIO prefix JSON")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioOrigin {
    #[serde(rename = "port")]
    Port,
    #[serde(rename = "PCM")]
    Pcm,
    #[serde(rename = "CAPTURE")]
    Capture,
}
#[derive(Debug)]
pub struct AudioInput<'a> {
    pub context: RuntimeContext,
    pub origin: AudioOrigin,
    pub metadata: AudioMetadata,
    pub pcm: &'a [u8],
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Context {
    runtime_id: String,
    library_generation: u64,
    data_path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Prefix<'a> {
    version: u8,
    context: Context,
    origin: AudioOrigin,
    #[serde(borrow)]
    metadata: &'a RawValue,
    #[serde(default)]
    negative_zero_paths: Vec<String>,
}

/// Borrows the exact PCM suffix. All schema, numeric, path and resource checks
/// finish before restoration; no partially restored result escapes on failure.
pub fn parse(body: &[u8]) -> Result<AudioInput<'_>> {
    if body.len() > 4 + PREFIX_LIMIT + PCM_LIMIT {
        return Err(error(
            "audio_input_limit",
            "AUDIO raw body exceeds its bound",
        ));
    }
    let header: [u8; 4] = body
        .get(..4)
        .ok_or_else(|| error("audio_input_framing", "AUDIO length header is truncated"))?
        .try_into()
        .unwrap();
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 || length > PREFIX_LIMIT {
        return Err(error(
            "audio_input_limit",
            "AUDIO prefix must contain 1..49152 bytes",
        ));
    }
    let end = 4 + length;
    let json = body
        .get(4..end)
        .ok_or_else(|| error("audio_input_framing", "AUDIO JSON prefix is truncated"))?;
    let pcm = &body[end..];
    if pcm.len() > PCM_LIMIT {
        return Err(error("audio_input_limit", "AUDIO PCM exceeds 32768 bytes"));
    }
    let prefix: Prefix<'_> = serde_json::from_slice(json).map_err(invalid)?;
    if prefix.version != 1 {
        return Err(error(
            "audio_input_version",
            "Unsupported AUDIO framing version",
        ));
    }
    let context = prefix.context;
    if context.runtime_id.is_empty()
        || context.runtime_id.len() > 256
        || context.runtime_id.contains('\0')
        || context.data_path.is_empty()
        || context.data_path.len() > 32768
        || context.data_path.contains('\0')
        || context.library_generation > 9007199254740991
    {
        return Err(error(
            "audio_input_context",
            "Invalid bounded full AUDIO runtime context",
        ));
    }
    if prefix.metadata.get().len() > METADATA_LIMIT {
        return Err(error(
            "audio_input_limit",
            "AUDIO metadata exceeds 8192 encoded bytes",
        ));
    }
    let mut nodes = 0;
    let metadata = json_value(prefix.metadata, 0, &mut nodes)?;
    let Value::Object(metadata) = metadata else {
        return Err(error(
            "audio_input_metadata",
            "AUDIO metadata must be an object",
        ));
    };
    if metadata
        .keys()
        .any(|key| !matches!(key.as_str(), "sampleRate" | "id" | "icecast"))
    {
        return Err(error(
            "audio_input_metadata",
            "Unknown top-level AUDIO metadata field",
        ));
    }
    let mut value = Value::Object(metadata);
    let paths = validate_paths(&value, &prefix.negative_zero_paths)?;
    // All paths were checked against the original tree. No user key is ever
    // interpreted as a prototype, executable property or filesystem authority.
    for path in paths {
        *target_mut(&mut value, &path).unwrap() = Value::Number(Number::from_f64(-0.0).unwrap());
    }
    let Value::Object(mut metadata) = value else {
        unreachable!()
    };
    Ok(AudioInput {
        context: RuntimeContext {
            runtime_id: context.runtime_id,
            library_generation: context.library_generation,
            data_path: PathBuf::from(context.data_path),
        },
        origin: prefix.origin,
        metadata: AudioMetadata {
            sample_rate: metadata
                .remove("sampleRate")
                .map_or(JsInput::Undefined, JsInput::Json),
            target_id: metadata
                .remove("id")
                .map_or(JsInput::Undefined, JsInput::Json),
            icecast: metadata.remove("icecast"),
        },
        pcm,
    })
}

// Use the exact raw number token and Rust's binary64 parser locally. Enabling
// serde_json float_roundtrip/arbitrary_precision would alter shared parsers.
fn json_value(raw: &RawValue, depth: usize, nodes: &mut usize) -> Result<Value> {
    *nodes += 1;
    if depth > 8 || *nodes > 256 {
        return Err(error(
            "audio_input_limit",
            "AUDIO metadata depth/nodes exceed 8/256",
        ));
    }
    let text = raw.get();
    match text.as_bytes()[0] {
        b'{' | b'[' => {
            struct Children<'a> {
                depth: usize,
                nodes: &'a mut usize,
                failure: &'a mut Option<AudioInputError>,
            }
            impl<'de> Visitor<'de> for Children<'_> {
                type Value = Value;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("bounded AUDIO JSON")
                }
                fn visit_map<A: MapAccess<'de>>(
                    self,
                    mut input: A,
                ) -> std::result::Result<Self::Value, A::Error> {
                    let mut values = Map::new();
                    while let Some((key, raw)) = input.next_entry::<String, &RawValue>()? {
                        if values.contains_key(&key) {
                            *self.failure =
                                Some(error("audio_input_json", "Duplicate AUDIO metadata key"));
                            return Err(A::Error::custom("invalid AUDIO metadata"));
                        }
                        match json_value(raw, self.depth + 1, self.nodes) {
                            Ok(value) => {
                                values.insert(key, value);
                            }
                            Err(error) => {
                                *self.failure = Some(error);
                                return Err(A::Error::custom("invalid AUDIO metadata"));
                            }
                        }
                    }
                    Ok(Value::Object(values))
                }
                fn visit_seq<A: SeqAccess<'de>>(
                    self,
                    mut input: A,
                ) -> std::result::Result<Self::Value, A::Error> {
                    let mut values = Vec::new();
                    while let Some(raw) = input.next_element::<&RawValue>()? {
                        match json_value(raw, self.depth + 1, self.nodes) {
                            Ok(value) => values.push(value),
                            Err(error) => {
                                *self.failure = Some(error);
                                return Err(A::Error::custom("invalid AUDIO metadata"));
                            }
                        }
                    }
                    Ok(Value::Array(values))
                }
            }
            let mut decoder = serde_json::Deserializer::from_str(text);
            let mut failure = None;
            let children = Children {
                depth,
                nodes,
                failure: &mut failure,
            };
            let result = if text.starts_with('{') {
                decoder.deserialize_map(children)
            } else {
                decoder.deserialize_seq(children)
            };
            result.map_err(|json| failure.unwrap_or_else(|| invalid(json)))
        }
        b'"' => serde_json::from_str::<String>(text)
            .map(Value::String)
            .map_err(invalid),
        b'n' => Ok(Value::Null),
        b't' => Ok(Value::Bool(true)),
        b'f' => Ok(Value::Bool(false)),
        _ => {
            let number = text
                .parse::<f64>()
                .ok()
                .and_then(Number::from_f64)
                .ok_or_else(|| {
                    error(
                        "audio_input_number",
                        "AUDIO metadata must contain finite binary64 numbers",
                    )
                })?;
            Ok(Value::Number(number))
        }
    }
}

fn validate_paths(metadata: &Value, paths: &[String]) -> Result<Vec<Vec<String>>> {
    if paths.len() > 256 {
        return Err(error(
            "audio_input_limit",
            "AUDIO has more than 256 negative-zero paths",
        ));
    }
    let mut unique = HashSet::new();
    let mut decoded: Vec<Vec<String>> = Vec::new();
    for path in paths {
        if path.len() > 1024 {
            return Err(error(
                "audio_input_limit",
                "AUDIO negative-zero path exceeds 1024 bytes",
            ));
        }
        if !path.starts_with('/') || !unique.insert(path) {
            return Err(error(
                "audio_input_path",
                "Invalid or duplicate AUDIO negative-zero path",
            ));
        }
        let mut tokens = Vec::new();
        for token in path[1..].split('/') {
            if tokens.len() == 8 {
                return Err(error(
                    "audio_input_limit",
                    "AUDIO negative-zero path exceeds eight segments",
                ));
            }
            let mut output = String::new();
            let mut chars = token.chars();
            while let Some(ch) = chars.next() {
                output.push(match ch {
                    '~' => match chars.next() {
                        Some('0') => '~',
                        Some('1') => '/',
                        _ => return Err(error("audio_input_path", "Invalid RFC6901 AUDIO escape")),
                    },
                    ch => ch,
                });
            }
            tokens.push(output);
        }
        if decoded
            .iter()
            .any(|other| other.starts_with(&tokens) || tokens.starts_with(other))
        {
            return Err(error(
                "audio_input_path",
                "Overlapping AUDIO negative-zero paths",
            ));
        }
        if target(metadata, &tokens).and_then(Value::as_f64) != Some(0.0) {
            return Err(error(
                "audio_input_path",
                "AUDIO negative-zero target must be an actual numeric zero",
            ));
        }
        decoded.push(tokens);
    }
    Ok(decoded)
}
fn array_index(token: &str) -> Option<usize> {
    if token.is_empty()
        || (token.len() > 1 && token.starts_with('0'))
        || !token.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    token.parse().ok()
}
fn target<'a>(mut value: &'a Value, path: &[String]) -> Option<&'a Value> {
    for token in path {
        value = match value {
            Value::Object(object) => object.get(token)?,
            Value::Array(array) => array.get(array_index(token)?)?,
            _ => return None,
        };
    }
    Some(value)
}
fn target_mut<'a>(mut value: &'a mut Value, path: &[String]) -> Option<&'a mut Value> {
    for token in path {
        value = match value {
            Value::Object(object) => object.get_mut(token)?,
            Value::Array(array) => array.get_mut(array_index(token)?)?,
            _ => return None,
        };
    }
    Some(value)
}
