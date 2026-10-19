//! Bounded, test-only JSON harness. Never part of the desktop runtime.
use base64::Engine;
use freeshow_icecast::*;
use serde_json::{json, Value};
use std::io::Read;
use tokio_util::sync::CancellationToken;
fn bytes(v: &Value) -> Result<Vec<u8>> {
    let a = v.as_array().ok_or_else(|| Error {
        code: "harness",
        message: "Expected bytes".into(),
    })?;
    if a.len() > 65025 {
        return Err(Error {
            code: "harness",
            message: "Byte cap".into(),
        });
    }
    a.iter()
        .map(|v| {
            v.as_u64()
                .filter(|n| *n <= 255)
                .map(|n| n as u8)
                .ok_or_else(|| Error {
                    code: "harness",
                    message: "Bad byte".into(),
                })
        })
        .collect()
}
fn encoded(v: Vec<u8>) -> Value {
    json!(base64::engine::general_purpose::STANDARD.encode(v))
}
async fn execute(v: Value) -> Result<Value> {
    match v["mode"].as_str().unwrap_or("pages") {
        "config" => {
            let parsed = Config::parse(v.get("config"), &Limits::default())?;
            Ok(match parsed {
                Some(c) => {
                    json!({"config":c,"header":encoded(c.request_header(v["title"].as_str().unwrap_or(""),&Limits::default())?)})
                }
                None => Value::Null,
            })
        }
        "pages" => {
            let mut state = OggState::new(v["serial"].as_u64().unwrap_or(0) as u32);
            let mut pages = Vec::new();
            pages.push(encoded(state.headers(v["title"].as_str().unwrap_or(""))?));
            if let Some(a) = v["packets"].as_array() {
                if a.len() > 64 {
                    return Err(Error {
                        code: "harness",
                        message: "Packet count".into(),
                    });
                }
                for p in a {
                    pages.push(encoded(state.audio(&bytes(p)?)?));
                }
            }
            pages.push(encoded(state.eos()?));
            Ok(json!(pages))
        }
        "network" => {
            let context = ContextId(1, 1);
            let service = IcecastService::start(context, Limits::default())?;
            let mut receipts = Vec::new();
            let mut events = Vec::new();
            let commands = v["commands"].as_array().ok_or_else(|| Error {
                code: "harness",
                message: "Commands required".into(),
            })?;
            if commands.len() > 128 {
                return Err(Error {
                    code: "harness",
                    message: "Command count".into(),
                });
            }
            for command in commands {
                let result = match command["op"].as_str().unwrap_or("") {
                    "send" => {
                        service
                            .send_audio(
                                context,
                                &bytes(&command["packet"])?,
                                command.get("config"),
                                CancellationToken::new(),
                            )
                            .await
                    }
                    "metadata" => {
                        service
                            .update_metadata(
                                context,
                                command["title"].as_str().unwrap_or(""),
                                CancellationToken::new(),
                            )
                            .await
                    }
                    "disconnect" => service.disconnect(context, CancellationToken::new()).await,
                    "wait_connected" => {
                        let until = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
                        let mut connected = false;
                        while tokio::time::Instant::now() < until {
                            let batch = service.take_events();
                            connected |= batch.iter().any(|e| e.data.connected);
                            events.extend(batch);
                            if connected {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                        }
                        receipts.push(json!({"connected":connected}));
                        continue;
                    }
                    "wait" => {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            command["ms"].as_u64().unwrap_or(0).min(3500),
                        ))
                        .await;
                        continue;
                    }
                    _ => {
                        return Err(Error {
                            code: "harness",
                            message: "Unknown operation".into(),
                        })
                    }
                };
                receipts.push(match result {
                    Ok(r) => json!(r),
                    Err(e) => json!({"error":e}),
                });
            }
            let shutdown = service.shutdown().await;
            events.extend(service.take_events());
            Ok(json!({"receipts":receipts,"events":events,"shutdown":shutdown.err()}))
        }
        _ => Err(Error {
            code: "harness",
            message: "Unknown mode".into(),
        }),
    }
}
fn main() {
    let mut input = Vec::new();
    std::io::stdin()
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut input)
        .unwrap();
    if input.len() > 2 * 1024 * 1024 {
        std::process::exit(2);
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        match serde_json::from_slice::<Value>(&input) {
            Ok(v) => execute(v).await,
            Err(e) => Err(Error {
                code: "harness",
                message: e.to_string(),
            }),
        }
    });
    let output = match result {
        Ok(v) => json!({"ok":v}),
        Err(e) => json!({"error":e}),
    };
    println!("{output}");
}
