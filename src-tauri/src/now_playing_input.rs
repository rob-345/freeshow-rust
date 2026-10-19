//! Private original-command framing. No filesystem authority, effects or ACK.
use freeshow_core::protocol::RuntimeContext;
use serde::{
    de::{Error as _, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::{value::RawValue, Map, Value};
use std::{fmt, path::PathBuf};
pub const COMMAND_LIMIT: usize = 262144;
type Result<T> = std::result::Result<T, NowPlayingInputError>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NowPlayingInputError {
    pub code: &'static str,
    pub message: &'static str,
}
impl fmt::Display for NowPlayingInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for NowPlayingInputError {}
fn error(code: &'static str, message: &'static str) -> NowPlayingInputError {
    NowPlayingInputError { code, message }
}
fn invalid(_: serde_json::Error) -> NowPlayingInputError {
    error(
        "now_playing_input_schema",
        "Invalid or ambiguous now-playing JSON/schema",
    )
}
#[derive(Debug)]
pub enum Command {
    Set {
        payload: Value,
        duration_number: Option<f64>,
    },
    Unset,
    Open,
}
#[derive(Debug)]
pub struct CommandInput {
    pub context: RuntimeContext,
    pub command: Command,
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
                "now_playing_input_context",
                "Invalid bounded full context",
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
            Self::Absent => Err(error("now_playing_input_schema", "Required field absent")),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCommand<'a> {
    kind: String,
    #[serde(default, borrow)]
    data: Field<'a>,
    #[serde(default, borrow)]
    duration: Field<'a>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Number<'a> {
    kind: String,
    #[serde(default, borrow)]
    value: Field<'a>,
}
fn number(raw: &RawValue) -> Result<f64> {
    let wire: Number<'_> = serde_json::from_str(raw.get()).map_err(invalid)?;
    if wire.kind == "Finite" {
        let token = wire.value.required()?.get();
        if !matches!(token.as_bytes().first(), Some(b'-' | b'0'..=b'9')) {
            return Err(error(
                "now_playing_input_number",
                "Finite requires a JSON Number",
            ));
        }
        let value = token
            .parse::<f64>()
            .map_err(|_| error("now_playing_input_number", "Invalid binary64 Number"))?;
        if !value.is_finite() || (value == 0.0 && value.is_sign_negative()) {
            return Err(error(
                "now_playing_input_number",
                "Nonfinite/negative zero requires its exact tag",
            ));
        }
        return Ok(value);
    }
    if wire.value.present() {
        return Err(error(
            "now_playing_input_schema",
            "This Number tag has no value",
        ));
    }
    match wire.kind.as_str() {
        "NaN" => Ok(f64::NAN),
        "PositiveInfinity" => Ok(f64::INFINITY),
        "NegativeInfinity" => Ok(f64::NEG_INFINITY),
        "NegativeZero" => Ok(-0.0),
        _ => Err(error("now_playing_input_number", "Unknown Number tag")),
    }
}
pub fn parse(body: &[u8]) -> Result<CommandInput> {
    if body.is_empty() || body.len() > COMMAND_LIMIT {
        return Err(error(
            "now_playing_input_limit",
            "Command must contain1..262144 bytes",
        ));
    }
    let envelope: Envelope<'_> = serde_json::from_slice(body).map_err(invalid)?;
    if envelope.version != 1 {
        return Err(error(
            "now_playing_input_version",
            "Unsupported private command version",
        ));
    }
    let context = envelope.context.checked()?;
    let wire: WireCommand<'_> = serde_json::from_str(envelope.command.get()).map_err(invalid)?;
    let command = match wire.kind.as_str() {
        "set" => {
            let raw = wire.data.required()?;
            if raw.get().len() > 65536 {
                return Err(error(
                    "now_playing_input_limit",
                    "Data exceeds65536 encoded bytes",
                ));
            }
            let payload = json_value(
                raw,
                0,
                &mut Budget {
                    nodes: 1024,
                    bytes: 65536,
                },
            )?;
            if !payload.is_object() || !payload.get("filePath").is_some_and(Value::is_string) {
                return Err(error(
                    "now_playing_input_schema",
                    "SET requires an object with a string filePath",
                ));
            }
            let duration_number = match wire.duration {
                Field::Absent => {
                    if payload.get("duration").is_some_and(Value::is_number) {
                        return Err(error(
                            "now_playing_input_schema",
                            "Numeric duration requires its private Number tag",
                        ));
                    }
                    None
                }
                Field::Present(value) => {
                    if payload.get("duration") != Some(&Value::Null) {
                        return Err(error(
                            "now_playing_input_schema",
                            "Tagged duration requires an own null placeholder",
                        ));
                    }
                    Some(number(value)?)
                }
            };
            Command::Set {
                payload,
                duration_number,
            }
        }
        "unset" | "open" => {
            if wire.data.present() || wire.duration.present() {
                return Err(error(
                    "now_playing_input_schema",
                    "This command has no data fields",
                ));
            }
            if wire.kind == "unset" {
                Command::Unset
            } else {
                Command::Open
            }
        }
        _ => {
            return Err(error(
                "now_playing_input_unsupported",
                "Unknown private command",
            ))
        }
    };
    Ok(CommandInput { context, command })
}

struct Budget {
    nodes: usize,
    bytes: usize,
}
impl Budget {
    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.bytes = self.bytes.checked_sub(bytes).ok_or_else(|| {
            error(
                "now_playing_input_limit",
                "Data exceeds host byte/work bound",
            )
        })?;
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
        .ok_or_else(|| error("now_playing_input_limit", "Data exceeds 1024 nodes"))?;
    if depth > 8 {
        return Err(error("now_playing_input_limit", "Data exceeds depth8"));
    }
    let text = raw.get();
    match text.as_bytes()[0] {
        b'{' | b'[' => {
            struct Children<'a> {
                depth: usize,
                budget: &'a mut Budget,
                failure: &'a mut Option<NowPlayingInputError>,
            }
            impl<'de> Visitor<'de> for Children<'_> {
                type Value = Value;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("bounded unambiguous now-playing JSON")
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
                        "now_playing_input_number",
                        "JSON data requires finite binary64 numbers",
                    )
                })?;
            Ok(Value::Number(value))
        }
    }
}
