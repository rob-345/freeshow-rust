//! Bounded private Spotify Request framing; no IO, runtime, authority or ACK.
use freeshow_core::protocol::{Request, RuntimeContext};
use serde::{
    de::{Error as _, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::{value::RawValue, Map, Value};
use std::{fmt, path::PathBuf};

pub const FRAME_LIMIT: usize = 64 * 1024;
const DEPTH_LIMIT: usize = 16;
const NODE_LIMIT: usize = 4096;
type Result<T> = std::result::Result<T, WireError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError {
    pub code: &'static str,
    pub message: &'static str,
}
impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for WireError {}
fn schema() -> WireError {
    WireError {
        code: "spotify_input_schema",
        message: "Invalid or ambiguous Spotify Request JSON/schema",
    }
}
fn limit() -> WireError {
    WireError {
        code: "spotify_input_limit",
        message: "Spotify Request exceeds bounded bytes, depth or work",
    }
}
fn number_error() -> WireError {
    WireError {
        code: "spotify_input_number",
        message: "Numeric Spotify command requires a finite raw JSON Number",
    }
}

#[derive(Debug)]
pub struct Input {
    pub request: Request,
    /// Only seek/setVolume. Derived from the same raw frame, never Value::as_f64.
    pub number: Option<f64>,
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
            || self.library_generation > 9_007_199_254_740_991
        {
            return Err(WireError {
                code: "spotify_input_context",
                message: "Invalid bounded full RuntimeContext",
            });
        }
        Ok(RuntimeContext {
            runtime_id: self.runtime_id,
            library_generation: self.library_generation,
            data_path: PathBuf::from(self.data_path),
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Envelope<'a> {
    channel: String,
    #[serde(borrow)]
    data: &'a RawValue,
    listener_id: Option<String>,
    #[serde(borrow)]
    context: &'a RawValue,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Route<'a> {
    channel: String,
    #[serde(borrow)]
    data: &'a RawValue,
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
#[derive(Deserialize)]
struct Payload<'a> {
    command: String,
    #[serde(default, borrow)]
    value: Field<'a>,
}

/// Parse one original full Request snapshot. The caller still validates role,
/// actual context and admission, then calls dispatch_with_number for Some.
pub fn parse(body: &[u8]) -> Result<Input> {
    preflight(body)?;
    if body.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(schema());
    }
    let envelope: Envelope<'_> = serde_json::from_slice(body).map_err(|_| schema())?;
    if envelope.channel != "MAIN" {
        return Err(schema());
    }
    if !envelope.context.get().starts_with('{') {
        return Err(schema());
    }
    let context: Context = serde_json::from_str(envelope.context.get()).map_err(|_| schema())?;
    let context = context.checked()?;
    if envelope
        .listener_id
        .as_ref()
        .is_some_and(|id| id.len() > 8192)
    {
        return Err(limit());
    }
    if !envelope.data.get().starts_with('{') {
        return Err(schema());
    }
    let route: Route<'_> = serde_json::from_str(envelope.data.get()).map_err(|_| schema())?;
    if route.channel != "SPOTIFY_COMMAND" {
        return Err(schema());
    }
    if !route.data.get().starts_with('{') {
        return Err(schema());
    }
    // Validate every retained/ignored field, including all duplicate keys, before
    // command selection. This also keeps the constructed Value's numbers exact.
    let data = json_value(
        envelope.data,
        &mut Budget {
            nodes: NODE_LIMIT,
            bytes: FRAME_LIMIT,
            work: FRAME_LIMIT * (DEPTH_LIMIT + 1),
        },
    )?;
    let payload: Payload<'_> = serde_json::from_str(route.data.get()).map_err(|_| schema())?;
    let number = if matches!(payload.command.as_str(), "seek" | "setVolume") {
        let Field::Present(raw) = payload.value else {
            return Err(number_error());
        };
        if !matches!(raw.get().as_bytes().first(), Some(b'-' | b'0'..=b'9')) {
            return Err(number_error());
        }
        Some(exact_number(raw.get())?)
    } else {
        None
    };
    // Use the foundation's pure validation, not a second seek rounding/range
    // implementation. This constructs no owner, child, runtime or IO authority.
    freeshow_spotify::PreparedCommand::new(&payload.command, number).map_err(|e| WireError {
        code: e.code,
        message: e.message,
    })?;
    Ok(Input {
        request: Request {
            channel: envelope.channel,
            data,
            listener_id: envelope.listener_id,
            context: Some(context),
        },
        number,
    })
}

// Iterative, allocation-free structural guard BEFORE any recursive serde parse.
// Serde subsequently validates full JSON grammar/escapes and duplicate fields.
// Tokens count keys as well as values, a conservative bound below host capacity.
fn preflight(body: &[u8]) -> Result<()> {
    if body.is_empty() || body.len() > FRAME_LIMIT {
        return Err(limit());
    }
    std::str::from_utf8(body).map_err(|_| schema())?;
    let mut stack = [0u8; DEPTH_LIMIT];
    let mut depth = 0;
    let mut tokens = 0;
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            b' ' | b'\t' | b'\r' | b'\n' | b':' | b',' => i += 1,
            b'{' | b'[' => {
                if depth == DEPTH_LIMIT {
                    return Err(limit());
                }
                stack[depth] = body[i];
                depth += 1;
                tokens += 1;
                i += 1;
            }
            b'}' | b']' => {
                if depth == 0 || !matches!((stack[depth - 1], body[i]), (b'{', b'}') | (b'[', b']'))
                {
                    return Err(schema());
                }
                depth -= 1;
                i += 1;
            }
            b'"' => {
                tokens += 1;
                i += 1;
                loop {
                    match body.get(i) {
                        Some(b'"') => {
                            i += 1;
                            break;
                        }
                        Some(b'\\') => i += 2,
                        Some(_) => i += 1,
                        None => return Err(schema()),
                    }
                }
            }
            _ => {
                tokens += 1;
                i += 1;
                while body.get(i).is_some_and(|b| {
                    !matches!(
                        b,
                        b' ' | b'\t'
                            | b'\r'
                            | b'\n'
                            | b','
                            | b':'
                            | b'{'
                            | b'}'
                            | b'['
                            | b']'
                            | b'"'
                    )
                }) {
                    i += 1;
                }
            }
        }
        if tokens > NODE_LIMIT {
            return Err(limit());
        }
    }
    if depth != 0 {
        return Err(schema());
    }
    Ok(())
}

struct Budget {
    nodes: usize,
    bytes: usize,
    work: usize,
}
impl Budget {
    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.bytes = self.bytes.checked_sub(bytes).ok_or_else(limit)?;
        Ok(())
    }
}
fn exact_number(text: &str) -> Result<f64> {
    text.parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .ok_or_else(number_error)
}
fn json_value(raw: &RawValue, budget: &mut Budget) -> Result<Value> {
    budget.nodes = budget.nodes.checked_sub(1).ok_or_else(limit)?;
    budget.work = budget.work.checked_sub(raw.get().len()).ok_or_else(limit)?;
    budget.charge(32)?;
    let text = raw.get();
    match text.as_bytes()[0] {
        b'{' | b'[' => {
            struct Children<'a> {
                budget: &'a mut Budget,
                failure: &'a mut Option<WireError>,
            }
            impl<'de> Visitor<'de> for Children<'_> {
                type Value = Value;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("bounded unambiguous Spotify JSON")
                }
                fn visit_map<A: MapAccess<'de>>(
                    self,
                    mut input: A,
                ) -> std::result::Result<Value, A::Error> {
                    let mut values = Map::new();
                    while let Some((key, raw)) = input.next_entry::<String, &RawValue>()? {
                        if values.contains_key(&key) {
                            return Err(A::Error::custom("duplicate Spotify data key"));
                        }
                        let result = self
                            .budget
                            .charge(key.len())
                            .and_then(|()| json_value(raw, self.budget));
                        match result {
                            Ok(value) => {
                                values.insert(key, value);
                            }
                            Err(error) => {
                                *self.failure = Some(error);
                                return Err(A::Error::custom("Spotify data limit"));
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
                        match json_value(raw, self.budget) {
                            Ok(value) => values.push(value),
                            Err(error) => {
                                *self.failure = Some(error);
                                return Err(A::Error::custom("Spotify data limit"));
                            }
                        }
                    }
                    Ok(Value::Array(values))
                }
            }
            let mut decoder = serde_json::Deserializer::from_str(text);
            let mut failure = None;
            let children = Children {
                budget,
                failure: &mut failure,
            };
            let result = if text.starts_with('{') {
                decoder.deserialize_map(children)
            } else {
                decoder.deserialize_seq(children)
            };
            result.map_err(|_| failure.unwrap_or_else(schema))
        }
        b'"' => {
            let value: String = serde_json::from_str(text).map_err(|_| schema())?;
            budget.charge(value.len())?;
            Ok(Value::String(value))
        }
        b'n' => Ok(Value::Null),
        b't' => Ok(Value::Bool(true)),
        b'f' => Ok(Value::Bool(false)),
        _ => serde_json::Number::from_f64(exact_number(text)?)
            .map(Value::Number)
            .ok_or_else(number_error),
    }
}
