//! Trusted test-host control over stdin. The HTTP endpoint has no path API.
use freeshow_assets::{
    spec, AssetServer, Config, Context, Control, Error, NativeGrant, Protection, Provenance,
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
fn invalid(message: impl Into<String>) -> Error {
    Error {
        code: "assets_invalid".into(),
        message: message.into(),
    }
}
fn context(value: &Value) -> Result<Context, Error> {
    serde_json::from_value(value.clone()).map_err(|error| invalid(error.to_string()))
}
fn grant(value: &Value) -> Result<NativeGrant, Error> {
    let path = value["path"]
        .as_str()
        .ok_or_else(|| invalid("Native grant path missing"))?;
    Ok(NativeGrant {
        path: PathBuf::from(path),
        source_path: value["sourcePath"].as_str().unwrap_or(path).into(),
        provenance: Provenance::CommittedLibrary,
        protection: if value["protected"] == true {
            Protection::Protected
        } else {
            Protection::Plain
        },
    })
}
fn config(value: &Value) -> Config {
    let mut config = Config::default();
    for (field, value) in [
        (&mut config.max_connections, &value["connections"]),
        (&mut config.max_grants, &value["grants"]),
        (&mut config.max_registry_bytes, &value["registryBytes"]),
        (&mut config.max_batch, &value["batch"]),
        (&mut config.chunk_bytes, &value["chunkBytes"]),
        (&mut config.file_workers, &value["workers"]),
        (&mut config.file_queue_capacity, &value["queue"]),
        (&mut config.header_bytes, &value["headerBytes"]),
        (&mut config.socket_send_bytes, &value["socketSendBytes"]),
    ] {
        if let Some(value) = value.as_u64() {
            *field = value.try_into().unwrap_or(usize::MAX);
        }
    }
    for (field, value) in [
        (&mut config.max_file_bytes, &value["fileBytes"]),
        (&mut config.max_response_bytes, &value["responseBytes"]),
    ] {
        if let Some(value) = value.as_u64() {
            *field = value;
        }
    }
    for (field, value) in [
        (&mut config.header_timeout, &value["headerMs"]),
        (&mut config.request_timeout, &value["requestMs"]),
        (&mut config.shutdown_timeout, &value["shutdownMs"]),
    ] {
        if let Some(value) = value.as_u64() {
            *field = Duration::from_millis(value.min(10_000_000));
        }
    }
    if let Some(origins) = value["origins"].as_array() {
        config.allowed_origins = origins
            .iter()
            .map(|origin| origin.as_str().unwrap_or("").to_string())
            .collect();
    }
    config
}
fn control(value: &Value) -> Control {
    let mut control = Control::default();
    if let Some(ms) = value["deadlineMs"].as_u64() {
        control.deadline = Instant::now() + Duration::from_millis(ms.min(10_000));
    }
    if value["cancelled"] == true {
        control.cancel.cancel();
    }
    control
}
fn pure(value: &Value) -> Result<Value, Error> {
    match value["op"].as_str() {
        Some("range") => Ok(json!(spec::parse_range(
            value["header"].as_str(),
            value["length"]
                .as_u64()
                .ok_or_else(|| invalid("Length missing"))?
        ))),
        Some("mime") => Ok(json!(spec::mime_type(Path::new(
            value["path"]
                .as_str()
                .ok_or_else(|| invalid("Path missing"))?
        )))),
        Some("specs") => {
            let cases = value["cases"]
                .as_array()
                .ok_or_else(|| invalid("Cases missing"))?;
            if cases.len() > 10_000 {
                return Err(invalid("Too many specification cases"));
            }
            cases
                .iter()
                .map(pure)
                .collect::<Result<Vec<_>, _>>()
                .map(|values| json!(values))
        }
        _ => Err(invalid("Unknown pure operation")),
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut server: Option<AssetServer> = None;
    let mut current: Option<Context> = None;
    loop {
        let mut line = String::new();
        let read = (&mut stdin)
            .take(4 * 1024 * 1024 + 1)
            .read_line(&mut line)
            .await;
        if matches!(read, Ok(0)) {
            break;
        }
        let input = if read.is_err() || line.len() > 4 * 1024 * 1024 {
            Err(invalid("Probe input exceeds byte/UTF-8 bounds"))
        } else {
            serde_json::from_str::<Value>(&line).map_err(|error| invalid(error.to_string()))
        };
        let quit = input.as_ref().is_ok_and(|value| value["op"] == "quit");
        let result = async {
            let input = input?;
            let op = input["op"].as_str().unwrap_or("");
            if matches!(op, "range" | "mime" | "specs") {
                return pure(&input);
            }
            if op == "start" {
                if server.is_some() {
                    return Err(invalid("Server is already running"));
                }
                let captured = context(&input["context"])?;
                server =
                    Some(AssetServer::start(config(&input["config"]), captured.clone()).await?);
                current = Some(captured);
                return Ok(json!({"ready":true}));
            }
            if op == "quit" && server.is_none() {
                return Ok(json!(true));
            }
            let service = server
                .as_ref()
                .ok_or_else(|| invalid("Server is not started"))?;
            let captured = if input.get("context").is_some() {
                context(&input["context"])?
            } else {
                current.clone().ok_or_else(|| invalid("Context missing"))?
            };
            match op {
                "grant" => service
                    .authorize(&captured, grant(&input)?, control(&input))
                    .await
                    .map(|grant| json!(grant)),
                "batch" => {
                    let values = input["grants"]
                        .as_array()
                        .ok_or_else(|| invalid("Grants missing"))?;
                    let grants = values.iter().map(grant).collect::<Result<Vec<_>, _>>()?;
                    service
                        .authorize_batch(&captured, grants, control(&input))
                        .await
                        .map(|batch| json!(batch))
                }
                "context" => {
                    service.activate_context(captured.clone())?;
                    current = Some(captured);
                    Ok(json!(true))
                }
                "revoke" => {
                    service.revoke(input["id"].as_str().unwrap_or(""));
                    Ok(json!(true))
                }
                "status" => Ok(json!(service.status())),
                "stop" | "quit" => {
                    service.shutdown().await?;
                    Ok(json!(service.status()))
                }
                _ => Err(invalid("Unknown operation")),
            }
        }
        .await;
        let output = match result {
            Ok(value) => json!({"ok":true,"value":value}),
            Err(error) => json!({"ok":false,"error":error}),
        };
        if stdout
            .write_all((output.to_string() + "\n").as_bytes())
            .await
            .is_err()
        {
            break;
        }
        let _ = stdout.flush().await;
        if quit || read.is_err() || line.len() > 4 * 1024 * 1024 {
            break;
        }
    }
    if let Some(server) = server {
        let _ = server.shutdown().await;
    }
}
