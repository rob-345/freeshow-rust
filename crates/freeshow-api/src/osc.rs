use crate::{
    broker::Run,
    semantics::{authorized, spread, truthy},
    ApiError,
};
use regex::Regex;
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::net::UdpSocket;

#[derive(Debug)]
pub(crate) struct Message {
    pub address: String,
    pub args: Vec<Value>,
    pub timestamp: Option<i64>,
}
fn invalid(message: &str) -> ApiError {
    ApiError::new("invalid_osc", message)
}
fn read<'a>(bytes: &'a [u8], offset: &mut usize, n: usize) -> Result<&'a [u8], ApiError> {
    let end = offset
        .checked_add(n)
        .ok_or_else(|| invalid("OSC offset overflow"))?;
    let value = bytes
        .get(*offset..end)
        .ok_or_else(|| invalid("OSC packet is truncated"))?;
    *offset = end;
    Ok(value)
}
fn string(bytes: &[u8], offset: &mut usize) -> Result<String, ApiError> {
    let tail = bytes
        .get(*offset..)
        .ok_or_else(|| invalid("OSC string is truncated"))?;
    let length = tail
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| invalid("OSC string is not terminated"))?;
    let value = String::from_utf8_lossy(&tail[..length]).into_owned();
    read(bytes, offset, (length + 4) & !3)?;
    Ok(value)
}
fn i32_value(bytes: &[u8], offset: &mut usize) -> Result<i32, ApiError> {
    Ok(i32::from_be_bytes(
        read(bytes, offset, 4)?.try_into().unwrap(),
    ))
}
fn number(value: f64) -> Value {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}
fn packet(
    bytes: &[u8],
    depth: usize,
    parent: Option<i64>,
    output: &mut Vec<Message>,
) -> Result<(), ApiError> {
    if depth > 16 || output.len() >= 256 {
        return Err(invalid("OSC bundle limits exceeded"));
    }
    let mut offset = 0;
    let address = string(bytes, &mut offset)?;
    if address == "#bundle" {
        let seconds = u32::from_be_bytes(read(bytes, &mut offset, 4)?.try_into().unwrap()) as i64;
        let fractions = u32::from_be_bytes(read(bytes, &mut offset, 4)?.try_into().unwrap()) as u64;
        // osc-js Timetag.timestamp rounds fractions to a whole second.
        let timestamp = (seconds - 2_208_988_800 + i64::from(fractions >= 2_147_483_648)) * 1000;
        if parent.is_some_and(|parent| parent < timestamp) {
            return Err(invalid(
                "OSC parent bundle timestamp is older than its child",
            ));
        }
        while offset < bytes.len() {
            let length = i32_value(bytes, &mut offset)?;
            if length <= 0 {
                return Err(invalid("OSC bundle element length must be positive"));
            }
            packet(
                read(bytes, &mut offset, length as usize)?,
                depth + 1,
                Some(timestamp),
                output,
            )?;
        }
    } else {
        if !address.starts_with('/') {
            return Err(invalid(
                "OSC Message found malformed or missing address string",
            ));
        }
        let tags = string(bytes, &mut offset)?;
        if tags.is_empty() {
            return Err(invalid(
                "OSC Message found malformed or missing type string",
            ));
        }
        let mut args = Vec::new();
        for tag in tags.chars().skip(1) {
            let value = match tag {
                'i' => json!(i32_value(bytes, &mut offset)?),
                'f' => number(
                    f32::from_be_bytes(read(bytes, &mut offset, 4)?.try_into().unwrap()) as f64,
                ),
                'd' => number(f64::from_be_bytes(
                    read(bytes, &mut offset, 8)?.try_into().unwrap(),
                )),
                'h' => json!(i64::from_be_bytes(
                    read(bytes, &mut offset, 8)?.try_into().unwrap()
                )),
                't' => json!(u64::from_be_bytes(
                    read(bytes, &mut offset, 8)?.try_into().unwrap()
                )),
                's' => Value::String(string(bytes, &mut offset)?),
                'b' => {
                    let length = i32_value(bytes, &mut offset)?;
                    if length < 0 {
                        return Err(invalid("OSC blob length is negative"));
                    }
                    let blob = read(bytes, &mut offset, length as usize)?.to_vec();
                    read(
                        bytes,
                        &mut offset,
                        ((length as usize + 3) & !3) - length as usize,
                    )?;
                    json!(blob)
                }
                'T' => Value::Bool(true),
                'F' => Value::Bool(false),
                'N' | 'I' => Value::Null,
                _ => return Err(invalid("OSC Message found unknown argument type")),
            };
            args.push(value);
        }
        output.push(Message {
            address,
            args,
            timestamp: parent,
        });
    }
    Ok(())
}
pub(crate) fn decode(bytes: &[u8]) -> Result<Vec<Message>, ApiError> {
    let mut output = Vec::new();
    packet(bytes, 0, None, &mut output)?;
    Ok(output)
}
fn pattern(address: &str) -> String {
    address
        .replace('.', "\\.")
        .replace('(', "\\(")
        .replace(')', "\\)")
        .replace('{', "(")
        .replace('}', ")")
        .replace(',', "|")
        .replace("[!", "[^")
        .replace('?', ".")
        .replace('*', ".*")
}
fn matches(address: &str) -> Result<bool, ApiError> {
    let normalized = if address.len() > 1 && address.ends_with('/') {
        &address[..address.len() - 1]
    } else {
        address
    };
    let forward =
        Regex::new(&pattern(normalized)).map_err(|_| invalid("OSC address pattern is invalid"))?;
    if forward
        .find("/freeshow/*")
        .is_some_and(|m| m.end() == "/freeshow/*".len())
    {
        return Ok(true);
    }
    Ok(Regex::new("/freeshow/.*")
        .unwrap()
        .find(address)
        .is_some_and(|m| m.end() == address.len()))
}
fn pack_string(value: &str, bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(value.as_bytes());
    bytes.resize(bytes.len() + ((value.len() + 4) & !3) - value.len(), 0);
}
fn encode_string(address: &str, value: &str) -> Vec<u8> {
    let address = if address.len() > 1 && address.ends_with('/') {
        &address[..address.len() - 1]
    } else {
        address
    };
    let mut bytes = Vec::new();
    pack_string(address, &mut bytes);
    pack_string(",s", &mut bytes);
    pack_string(value, &mut bytes);
    bytes
}
async fn handle(run: Arc<Run>, socket: Arc<UdpSocket>, message: Message) -> Result<(), ApiError> {
    if let Some(timestamp) = message.timestamp {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        if timestamp > now {
            tokio::time::sleep(Duration::from_millis((timestamp - now) as u64)).await;
        }
    }
    let first = message.args.first().unwrap_or(&Value::Null);
    let args = match first {
        Value::String(text) if !text.is_empty() => match crate::semantics::parse_json(text) {
            Ok(value) => value,
            Err(error) if error.code == "json_number_overflow" => return Err(error),
            Err(_) => {
                run.diagnostic("invalid_osc_json", "OSC: Could not parse JSON!");
                json!({})
            }
        },
        other => {
            if truthy(other) {
                other.clone()
            } else {
                json!({})
            }
        }
    };
    if !authorized(
        &run.password,
        &[args.clone(), Value::Array(message.args.clone())],
    ) {
        let bytes = encode_string(
            &message.address,
            "{\"status\":401,\"error\":\"Unauthorized\"}",
        );
        socket
            .send_to(&bytes, run.config.osc_response_target)
            .await
            .map_err(|e| ApiError::new("osc_send_failed", e.to_string()))?;
        return Ok(());
    }
    let mut data = serde_json::Map::new();
    data.insert(
        "action".into(),
        Value::String(message.address.replacen("/freeshow", "", 1)),
    );
    if matches!(args, Value::Object(_) | Value::Array(_) | Value::Null) {
        data.extend(spread(args));
    }
    if run
        .received(Value::Object(data), &run.cancel)
        .await?
        .is_some()
    {
        // apiReturnData always returns its object envelope. osc-js Message
        // rejects that value; preserving this bug avoids inventing JSON replies.
        return Err(ApiError::new(
            "osc_response_encoding",
            "OSC typeTag() found unknown value type",
        ));
    }
    Ok(())
}
pub(crate) async fn serve(socket: UdpSocket, run: Arc<Run>) {
    let socket = Arc::new(socket);
    let mut buffer = vec![0; 65_535];
    loop {
        let received = tokio::select! {biased;_=run.cancel.cancelled()=>break,result=socket.recv_from(&mut buffer)=>result};
        let (length, _) = match received {
            Ok(value) => value,
            Err(error) => {
                run.diagnostic("osc_receive_failed", error.to_string());
                break;
            }
        };
        let messages = match decode(&buffer[..length]) {
            Ok(messages) => messages,
            Err(error) => {
                run.diagnostic(error.code, error.message);
                continue;
            }
        };
        for message in messages {
            match matches(&message.address) {
                Ok(false) => continue,
                Err(error) => {
                    run.diagnostic(error.code, error.message);
                    continue;
                }
                Ok(true) => {}
            }
            let permit = match run.inflight.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    run.diagnostic("request_limit", "OSC request concurrency limit reached");
                    continue;
                }
            };
            let task_run = run.clone();
            let socket = socket.clone();
            let _=run.spawn(async move{let _permit=permit;tokio::select!{biased;_=task_run.cancel.cancelled()=>{},result=handle(task_run.clone(),socket,message)=>if let Err(error)=result{task_run.diagnostic(error.code,error.message)}}});
        }
    }
}
