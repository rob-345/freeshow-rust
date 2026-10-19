//! JSON-lines harness for differential tests. This executable is not the
//! desktop runtime and deliberately permits network sends only to loopback.

use freeshow_network::{encode_message, prepare_emit, OscSender, OscSenderConfig};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

fn pure(input: &Value) -> Result<Value, Value> {
    match input["op"].as_str().unwrap_or_default() {
        "encode" => encode_message(
            input["data"]
                .as_str()
                .ok_or(json!({"kind":"invalid_payload"}))?,
        )
        .map(|value| json!(value))
        .map_err(|error| json!(error)),
        "prepare" => prepare_emit(&input["payload"])
            .map(|value| json!(value))
            .map_err(|error| json!(error)),
        "batch" => input["cases"]
            .as_array()
            .ok_or(json!({"kind":"invalid_payload"}))?
            .iter()
            .map(pure)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        _ => Err(json!({"kind":"unsupported_operation"})),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sender = OscSender::start(OscSenderConfig::default())?;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for line in io::stdin().lock().lines() {
        let line = line?;
        let result = match serde_json::from_str::<Value>(&line) {
            Err(error) => Err(json!({"kind":"invalid_json","message":error.to_string()})),
            Ok(input) if input["op"] == "send" => {
                let payload = &input["payload"];
                match prepare_emit(payload) {
                    Err(error) => Err(json!(error)),
                    Ok(Some(prepared))
                        if prepared.host() != "localhost"
                            && !prepared
                                .host()
                                .parse::<std::net::Ipv4Addr>()
                                .is_ok_and(|ip| ip.is_loopback()) =>
                    {
                        Err(json!({"kind":"test_destination_not_loopback"}))
                    }
                    _ => sender
                        .emit(payload)
                        .await
                        .map(|outcome| json!(outcome))
                        .map_err(|error| json!(error)),
                }
            }
            Ok(input) => pure(&input),
        };
        let response = match result {
            Ok(value) => json!({"ok":true,"value":value}),
            Err(error) => json!({"ok":false,"error":error}),
        };
        writeln!(output, "{response}")?;
        output.flush()?;
    }
    sender.shutdown().await;
    Ok(())
}
