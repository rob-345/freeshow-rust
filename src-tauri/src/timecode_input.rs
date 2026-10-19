//! Bounded private timecode command/LTC parsing. No IPC, authority, IO or ACK.
use freeshow_core::protocol::RuntimeContext;
use freeshow_midi::Number;
use freeshow_timecode::{Command, Data, Scalar};
use serde::{
    de::{Error as _, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::{value::RawValue, Map, Value};
use std::{fmt, path::PathBuf};

pub const COMMAND_LIMIT: usize = 262144;
pub const PREFIX_LIMIT: usize = 49152;
pub const AUDIO_LIMIT: usize = 65536;
type Result<T> = std::result::Result<T, TimecodeInputError>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimecodeInputError {
    pub code: &'static str,
    pub message: &'static str,
}
impl fmt::Display for TimecodeInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for TimecodeInputError {}
fn error(code: &'static str, message: &'static str) -> TimecodeInputError {
    TimecodeInputError { code, message }
}
fn invalid(_: serde_json::Error) -> TimecodeInputError {
    error(
        "timecode_input_schema",
        "Invalid or ambiguous timecode JSON/schema",
    )
}
#[derive(Debug)]
pub struct CommandInput {
    pub context: RuntimeContext,
    pub command: Command,
}
#[derive(Debug)]
pub struct LtcInput<'a> {
    pub context: RuntimeContext,
    pub mode: &'static str,
    pub bytes: &'a [u8],
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Context {
    runtime_id: String,
    library_generation: u64,
    data_path: String,
}
impl Context {
    fn checked(self) -> Result<RuntimeContext> {
        if self.runtime_id.is_empty()
            || self.runtime_id.len() > 256
            || self.runtime_id.contains('\0')
            || self.data_path.is_empty()
            || self.data_path.len() > 32768
            || self.data_path.contains('\0')
            || self.library_generation > 9007199254740991
        {
            return Err(error(
                "timecode_input_context",
                "Invalid bounded full timecode context",
            ));
        }
        Ok(RuntimeContext {
            runtime_id: self.runtime_id,
            library_generation: self.library_generation,
            data_path: PathBuf::from(self.data_path),
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<'a> {
    version: u8,
    context: Context,
    #[serde(borrow)]
    command: &'a RawValue,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Prefix {
    version: u8,
    context: Context,
    mode: String,
    byte_length: usize,
}
// Distinguish an omitted field from an explicit JSON null while borrowing its
// exact token. Option<T> would erase that distinction during schema validation.
#[derive(Default)]
enum Field<'a> {
    #[default]
    Absent,
    Present(&'a RawValue),
}
impl<'de: 'a, 'a> Deserialize<'de> for Field<'a> {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> std::result::Result<Self, D::Error> {
        <&RawValue>::deserialize(decoder).map(Self::Present)
    }
}
impl<'a> Field<'a> {
    fn present(&self) -> bool {
        matches!(self, Self::Present(_))
    }
    fn required(self) -> Result<&'a RawValue> {
        match self {
            Self::Present(value) => Ok(value),
            Self::Absent => Err(error("timecode_input_schema", "Required field absent")),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCommand<'a> {
    kind: String,
    #[serde(default, borrow)]
    r#type: Field<'a>,
    #[serde(default, borrow)]
    mode: Field<'a>,
    #[serde(default, borrow)]
    framerate: Field<'a>,
    #[serde(default, borrow)]
    data: Field<'a>,
    #[serde(default, borrow)]
    value: Field<'a>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tagged<'a> {
    kind: String,
    #[serde(default, borrow)]
    value: Field<'a>,
}
fn version(version: u8) -> Result<()> {
    if version != 1 {
        return Err(error(
            "timecode_input_version",
            "Unsupported timecode version",
        ));
    }
    Ok(())
}
fn tagged(raw: &RawValue) -> Result<Tagged<'_>> {
    serde_json::from_str(raw.get()).map_err(invalid)
}
fn no_value(value: Field<'_>) -> Result<()> {
    if value.present() {
        return Err(error(
            "timecode_input_schema",
            "This tag must not contain a value",
        ));
    }
    Ok(())
}
fn text(raw: &RawValue, max: usize) -> Result<String> {
    let text: String = serde_json::from_str(raw.get()).map_err(invalid)?;
    if text.len() > max {
        return Err(error(
            "timecode_input_limit",
            "Timecode string exceeds host bound",
        ));
    }
    Ok(text)
}
fn number(raw: &RawValue) -> Result<Number> {
    let tag = tagged(raw)?;
    Ok(match tag.kind.as_str() {
        "Finite" => {
            let raw = tag.value.required()?.get();
            if !matches!(raw.as_bytes().first(), Some(b'-' | b'0'..=b'9')) {
                return Err(error(
                    "timecode_input_schema",
                    "Finite Number requires a JSON number",
                ));
            }
            let value = raw
                .parse::<f64>()
                .map_err(|_| error("timecode_input_number", "Invalid binary64 Number"))?;
            if !value.is_finite() || (value == 0.0 && value.is_sign_negative()) {
                return Err(error(
                    "timecode_input_number",
                    "Nonfinite/negative-zero Number requires its exact tag",
                ));
            }
            Number::new(value)
        }
        "NaN" | "PositiveInfinity" | "NegativeInfinity" | "NegativeZero" => {
            no_value(tag.value)?;
            match tag.kind.as_str() {
                "NaN" => Number::NaN,
                "PositiveInfinity" => Number::PositiveInfinity,
                "NegativeInfinity" => Number::NegativeInfinity,
                _ => Number::NegativeZero,
            }
        }
        _ => return Err(error("timecode_input_schema", "Unknown Number tag")),
    })
}
fn scalar(raw: &RawValue) -> Result<Scalar> {
    let tag = tagged(raw)?;
    Ok(match tag.kind.as_str() {
        "undefined" => {
            no_value(tag.value)?;
            Scalar::Undefined
        }
        "null" => {
            no_value(tag.value)?;
            Scalar::Null
        }
        "bool" => Scalar::Bool(serde_json::from_str(tag.value.required()?.get()).map_err(invalid)?),
        "text" => Scalar::Text(text(tag.value.required()?, 4096)?),
        "number" => Scalar::Number(number(tag.value.required()?)?),
        _ => return Err(error("timecode_input_schema", "Unknown framerate tag")),
    })
}
fn data(raw: &RawValue) -> Result<Data> {
    let tag = tagged(raw)?;
    match tag.kind.as_str() {
        "undefined" => {
            no_value(tag.value)?;
            Ok(Data::Undefined)
        }
        "json" => Ok(Data::Json(json_value(
            tag.value.required()?,
            0,
            &mut Budget {
                nodes: 1024,
                bytes: 65536,
            },
        )?)),
        _ => Err(error("timecode_input_schema", "Unknown data tag")),
    }
}
/// Strict bounded schema, complete typed source command. Authority/context
/// freshness is still checked by NativeTimecode at its trusted effects boundary.
pub fn parse_command(body: &[u8]) -> Result<CommandInput> {
    if body.is_empty() || body.len() > COMMAND_LIMIT {
        return Err(error(
            "timecode_input_limit",
            "Command must contain 1..262144 bytes",
        ));
    }
    let envelope: Envelope<'_> = serde_json::from_slice(body).map_err(invalid)?;
    version(envelope.version)?;
    let context = envelope.context.checked()?;
    let wire: WireCommand<'_> = serde_json::from_str(envelope.command.get()).map_err(invalid)?;
    let mask = u8::from(wire.r#type.present())
        | (u8::from(wire.mode.present()) << 1)
        | (u8::from(wire.framerate.present()) << 2)
        | (u8::from(wire.data.present()) << 3)
        | (u8::from(wire.value.present()) << 4);
    let expected = match wire.kind.as_str() {
        "start" => 15,
        "stop" => 0,
        "value" => 16,
        "status" => 8,
        _ => {
            return Err(error(
                "timecode_input_unsupported",
                "Unknown private command",
            ))
        }
    };
    if mask != expected {
        return Err(error(
            "timecode_input_schema",
            "Command fields do not match exact variant",
        ));
    }
    let command = match wire.kind.as_str() {
        "start" => Command::Start {
            kind: text(wire.r#type.required()?, 32)?,
            mode: text(wire.mode.required()?, 4096)?,
            framerate: scalar(wire.framerate.required()?)?,
            data: data(wire.data.required()?)?,
        },
        "stop" => Command::Stop,
        "value" => Command::Value(number(wire.value.required()?)?),
        "status" => Command::Status(data(wire.data.required()?)?),
        _ => unreachable!(),
    };
    Ok(CommandInput { context, command })
}
/// Validates the whole prefix and exact announced size before exposing a borrowed
/// suffix. This is 64KiB LTC, never the unrelated 32KiB AUDIO PCM frame.
pub fn parse_ltc(body: &[u8]) -> Result<LtcInput<'_>> {
    if body.len() > 4 + PREFIX_LIMIT + AUDIO_LIMIT {
        return Err(error("timecode_input_limit", "LTC frame exceeds bound"));
    }
    let header: [u8; 4] = body
        .get(..4)
        .ok_or_else(|| error("timecode_input_framing", "Truncated LTC length header"))?
        .try_into()
        .unwrap();
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 || length > PREFIX_LIMIT {
        return Err(error(
            "timecode_input_limit",
            "LTC prefix must contain 1..49152 bytes",
        ));
    }
    let end = 4 + length;
    let json = body
        .get(4..end)
        .ok_or_else(|| error("timecode_input_framing", "Truncated LTC prefix"))?;
    let bytes = &body[end..];
    if bytes.len() > AUDIO_LIMIT {
        return Err(error("timecode_input_limit", "LTC exceeds 65536 raw bytes"));
    }
    let prefix: Prefix = serde_json::from_slice(json).map_err(invalid)?;
    version(prefix.version)?;
    let context = prefix.context.checked()?;
    if prefix.mode != "LTC" {
        return Err(error(
            "timecode_input_unsupported",
            "Raw framing supports explicit LTC only",
        ));
    }
    if prefix.byte_length != bytes.len() {
        return Err(error(
            "timecode_input_framing",
            "Announced LTC size differs from suffix",
        ));
    }
    Ok(LtcInput {
        context,
        mode: "LTC",
        bytes,
    })
}
struct Budget {
    nodes: usize,
    bytes: usize,
}
impl Budget {
    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_sub(bytes)
            .ok_or_else(|| error("timecode_input_limit", "Data exceeds host byte/work bound"))?;
        Ok(())
    }
}
// Parse exact number tokens through Rust's correctly rounded binary64 parser.
// Generic serde_json numeric parsing/serialization must not erase raw -0.0 or
// introduce a one-ULP difference. No shared parser feature is changed here.
fn json_value(raw: &RawValue, depth: usize, budget: &mut Budget) -> Result<Value> {
    budget.nodes = budget
        .nodes
        .checked_sub(1)
        .ok_or_else(|| error("timecode_input_limit", "Data exceeds 1024 nodes"))?;
    if depth > 8 {
        return Err(error("timecode_input_limit", "Data exceeds depth8"));
    }
    let text = raw.get();
    match text.as_bytes()[0] {
        b'{' | b'[' => {
            struct Children<'a> {
                depth: usize,
                budget: &'a mut Budget,
                failure: &'a mut Option<TimecodeInputError>,
            }
            impl<'de> Visitor<'de> for Children<'_> {
                type Value = Value;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("bounded unambiguous timecode JSON")
                }
                fn visit_map<A: MapAccess<'de>>(
                    self,
                    mut input: A,
                ) -> std::result::Result<Value, A::Error> {
                    let mut values = Map::new();
                    while let Some((key, raw)) = input.next_entry::<String, &RawValue>()? {
                        if values.contains_key(&key) {
                            return Err(A::Error::custom("duplicate data key"));
                        }
                        if let Err(error) = self.budget.charge(key.len()) {
                            *self.failure = Some(error);
                            return Err(A::Error::custom("data byte limit"));
                        }
                        match json_value(raw, self.depth + 1, self.budget) {
                            Ok(value) => {
                                values.insert(key, value);
                            }
                            Err(error) => {
                                *self.failure = Some(error);
                                return Err(A::Error::custom("data work limit"));
                            }
                        }
                    }
                    Ok(Value::Object(values))
                }
                fn visit_seq<A: SeqAccess<'de>>(
                    self,
                    mut input: A,
                ) -> std::result::Result<Value, A::Error> {
                    let mut values = Vec::new();
                    while let Some(raw) = input.next_element::<&RawValue>()? {
                        match json_value(raw, self.depth + 1, self.budget) {
                            Ok(value) => values.push(value),
                            Err(error) => {
                                *self.failure = Some(error);
                                return Err(A::Error::custom("data work limit"));
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
                budget,
                failure: &mut failure,
            };
            let result = if text.starts_with('{') {
                decoder.deserialize_map(children)
            } else {
                decoder.deserialize_seq(children)
            };
            result.map_err(|json| failure.unwrap_or_else(|| invalid(json)))
        }
        b'"' => {
            let value: String = serde_json::from_str(text).map_err(invalid)?;
            budget.charge(value.len())?;
            Ok(Value::String(value))
        }
        b'n' | b't' | b'f' => {
            budget.charge(16)?;
            Ok(match text {
                "null" => Value::Null,
                "true" => Value::Bool(true),
                _ => Value::Bool(false),
            })
        }
        _ => {
            budget.charge(16)?;
            let value = text
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .ok_or_else(|| {
                    error(
                        "timecode_input_number",
                        "JSON data requires finite binary64 numbers",
                    )
                })?;
            Ok(Value::Number(value))
        }
    }
}
