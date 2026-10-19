//! Bounded test transport, not a runtime sidecar or renderer command registry.
use freeshow_spotify::*;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{self, BufRead, Read, Write},
    path::Path,
    time::{Duration, Instant},
};
fn invalid() -> Error {
    Error {
        code: "spotify_invalid",
        message: "Invalid test command",
    }
}
fn main() {
    // One fixed observer runtime for the private driver, reused for every wait.
    // The product service still owns its own single native IO runtime/thread.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let mut service = None;
    let owner = Owner::new("spotify-test", 1, Path::new("/fixture/library")).unwrap();
    let mut tickets = HashMap::<u64, Ticket>::new();
    let mut commands = HashMap::<u64, CommandReceipt>::new();
    let mut next = 0u64;
    let mut stdin = io::stdin().lock();
    loop {
        let mut line = Vec::new();
        match stdin
            .by_ref()
            .take(2 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut line)
        {
            Ok(0) => break,
            Ok(_) if line.len() <= 2 * 1024 * 1024 => {}
            _ => break,
        }
        let result = (|| -> Result<Value> {
            let input: Value = serde_json::from_slice(&line).map_err(|_| invalid())?;
            match input["op"].as_str().unwrap_or("") {
                "parse" => {
                    let outcome = parse_linux(input["text"].as_str().ok_or_else(invalid)?)?;
                    Ok(json!({"wire":outcome.wire()}))
                }
                "start" => {
                    if service.is_some() {
                        return Err(invalid());
                    }
                    let mut limits = Limits::default();
                    if let Some(ms) = input["timeoutMs"].as_u64() {
                        limits.query_timeout = Duration::from_millis(ms);
                    }
                    if let Some(n) = input["stdoutBytes"].as_u64() {
                        limits.stdout_bytes = usize::try_from(n).map_err(|_| invalid())?;
                    }
                    let binary = std::env::var_os("FREESHOW_SPOTIFY_DBUS_SEND")
                        .unwrap_or_else(|| "/usr/bin/dbus-send".into());
                    let backend = Backend::capture(Path::new(&binary))?;
                    let started = SpotifyService::start(owner.clone(), backend, limits)?;
                    let capability = runtime.block_on(started.ready())?;
                    service = Some(started);
                    Ok(json!(capability))
                }
                "begin" => {
                    let s = service.as_ref().ok_or_else(invalid)?;
                    let ms = input["deadlineMs"].as_u64().unwrap_or(10000).min(10000);
                    let ticket =
                        s.begin_state(&owner, Instant::now() + Duration::from_millis(ms))?;
                    next = next.checked_add(1).ok_or_else(invalid)?;
                    tickets.insert(next, ticket);
                    Ok(json!({"id":next}))
                }
                "finish" => {
                    let ticket = tickets
                        .remove(&input["id"].as_u64().ok_or_else(invalid)?)
                        .ok_or_else(invalid)?;
                    let outcome = runtime.block_on(ticket.finish())?.into_reply(&owner)?;
                    let null_reason = match &outcome {
                        Outcome::SourceNull(reason) => Some(reason),
                        _ => None,
                    };
                    Ok(json!({"wire":outcome.wire(),"nullReason":null_reason}))
                }
                "drop" => {
                    tickets
                        .remove(&input["id"].as_u64().ok_or_else(invalid)?)
                        .ok_or_else(invalid)?;
                    Ok(Value::Null)
                }
                "cancel" => {
                    tickets
                        .get(&input["id"].as_u64().ok_or_else(invalid)?)
                        .ok_or_else(invalid)?
                        .cancel();
                    Ok(Value::Null)
                }
                "prepareCommand" | "command" => {
                    let name = input["command"].as_str().ok_or_else(invalid)?;
                    let value = if !["setVolume", "seek"].contains(&name) {
                        None
                    } else if let Some(bits) = input["valueBits"].as_str() {
                        Some(f64::from_bits(
                            u64::from_str_radix(bits, 16).map_err(|_| invalid())?,
                        ))
                    } else if input.get("value").is_some() {
                        Some(input["value"].as_f64().ok_or_else(invalid)?)
                    } else {
                        None
                    };
                    let command = PreparedCommand::new(name, value)?;
                    if input["op"] == "prepareCommand" {
                        return Ok(json!({"prepared":format!("{command:?}")}));
                    }
                    let s = service.as_ref().ok_or_else(invalid)?;
                    let ms = input["deadlineMs"].as_u64().unwrap_or(10000).min(10000);
                    let receipt = s.begin_command(
                        &owner,
                        command,
                        Instant::now() + Duration::from_millis(ms),
                    )?;
                    let reply = receipt.reply(&owner)?;
                    next = next.checked_add(1).ok_or_else(invalid)?;
                    commands.insert(next, receipt);
                    Ok(json!({"id":next,"reply":reply}))
                }
                "finishCommand" => {
                    let receipt = commands
                        .remove(&input["id"].as_u64().ok_or_else(invalid)?)
                        .ok_or_else(invalid)?;
                    let finished = runtime.block_on(receipt.finish())?;
                    Ok(json!(finished.result(&owner)?))
                }
                "dropCommand" => {
                    commands
                        .remove(&input["id"].as_u64().ok_or_else(invalid)?)
                        .ok_or_else(invalid)?;
                    Ok(Value::Null)
                }
                "cancelCommand" => {
                    commands
                        .get(&input["id"].as_u64().ok_or_else(invalid)?)
                        .ok_or_else(invalid)?
                        .cancel();
                    Ok(Value::Null)
                }
                "status" => Ok(json!(service.as_ref().ok_or_else(invalid)?.status())),
                "close" => {
                    runtime.block_on(service.as_ref().ok_or_else(invalid)?.shutdown())?;
                    Ok(Value::Null)
                }
                _ => Err(invalid()),
            }
        })();
        let result = match result {
            Ok(value) => json!({"ok":true,"value":value}),
            Err(error) => json!({"ok":false,"error":error}),
        };
        println!("{result}");
        io::stdout().flush().unwrap();
    }
    tickets.clear();
    commands.clear();
    if let Some(service) = service {
        let _ = runtime.block_on(service.shutdown());
    }
}
