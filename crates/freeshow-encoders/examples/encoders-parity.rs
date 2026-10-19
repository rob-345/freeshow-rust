//! Controlled test driver and deterministic failing child fixtures. Fixture
//! names never claim a working software or hardware encoder.
use freeshow_encoders::{
    profiles, EncoderConfig, EncoderError, EncoderService, FrozenDetectionInput, FrozenExecutable,
};
use serde_json::{json, Value};
use std::{io::Write, path::PathBuf, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

fn fixture(args: &[String]) -> bool {
    let executable = std::env::current_exe().unwrap();
    let name = executable.file_stem().unwrap().to_string_lossy();
    let Some(mode) = name.strip_prefix("encoders-fixture-") else {
        return false;
    };
    let directory = std::env::var_os("FREESHOW_ENCODER_WITNESS_DIR").map(PathBuf::from);
    if let Some(root) = &directory {
        std::fs::write(root.join(std::process::id().to_string()), args.join("\n")).unwrap();
    }
    let listing = args.iter().any(|v| v == "-encoders");
    match mode {
        "gate" if listing => {
            let root = directory.unwrap();
            let end = std::time::Instant::now() + Duration::from_secs(30);
            while !root
                .join(format!("release-{}", std::process::id()))
                .exists()
                && std::time::Instant::now() < end
            {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        "timeout" => std::thread::sleep(Duration::from_secs(30)),
        "stdout" => {
            let _ = std::io::stdout().write_all(&vec![b'x'; 6 * 1024 * 1024]);
        }
        "stderr" => {
            let _ = std::io::stderr().write_all(&vec![b'x'; 6 * 1024 * 1024]);
        }
        "listing-failure" => {
            eprintln!("listing deliberately failed");
            std::process::exit(9)
        }
        _ if listing => {
            for id in profiles::ENCODER_IDS {
                println!(
                    " V....D {} fixture encoder; availability still requires encode",
                    profiles::profile(id, "").codec
                );
            }
        }
        "encode-timeout" => std::thread::sleep(Duration::from_secs(30)),
        _ => {
            eprintln!("\n\u{feff} fixture GPU/device unavailable été 漢字\r");
            std::process::exit(7)
        }
    }
    true
}
fn input(value: &Value) -> FrozenDetectionInput {
    FrozenDetectionInput {
        generation: value["generation"].as_u64().unwrap_or(1),
        executable: value["executable"].as_str().map(|path| FrozenExecutable {
            path: PathBuf::from(path),
            source_path: value["sourcePath"].as_str().unwrap_or(path).into(),
            mtime_ms: value["mtimeMs"].as_f64().unwrap_or(0.0),
        }),
        platform: value["platform"]
            .as_str()
            .unwrap_or(std::env::consts::OS)
            .replace("macos", "darwin")
            .replace("windows", "win32"),
        arch: value["arch"].as_str().unwrap_or("x64").into(),
        vaapi_device: value["device"]
            .as_str()
            .unwrap_or(profiles::DEFAULT_VAAPI_DEVICE)
            .into(),
        cache: value["cache"].as_str().map(|v| v.as_bytes().to_vec()),
    }
}
async fn execute(service: EncoderService, request: Value) -> freeshow_encoders::Result<Value> {
    let device = request["device"]
        .as_str()
        .unwrap_or(profiles::DEFAULT_VAAPI_DEVICE);
    let id = request["encoder"].as_str().unwrap_or("x264");
    Ok(match request["op"].as_str().unwrap_or_default() {
        "profiles" => json!(profiles::profiles(device)),
        "supported" => json!(profiles::supported(
            id,
            request["platform"].as_str().unwrap_or("linux")
        )),
        "parse" => json!(profiles::parse_available_encoders(
            request["stdout"].as_str().unwrap_or_default()
        )),
        "args" => json!(profiles::encoder_args(
            id,
            request["bitrate"].as_f64().unwrap_or(500.0),
            request["gop"].as_f64().unwrap_or(60.0),
            request["mode"].as_str()
        )?),
        "test_command" => json!(profiles::test_encode_command(id, device)?),
        "command" => json!(profiles::encoder_command(
            &serde_json::from_value(request["options"].clone()).map_err(|e| EncoderError {
                code: "invalid_options",
                message: e.to_string()
            })?,
            device
        )?),
        "relay" => json!(profiles::relay_command(
            request["url"].as_str().unwrap_or_default()
        )),
        "error" => json!(freeshow_encoders::first_error_line(
            request["stderr"].as_str().unwrap_or_default(),
            request["killed"].as_bool().unwrap_or(false),
            request["message"].as_str()
        )),
        "key" => json!(input(&request).cache_key()?),
        "detect" => {
            let prepared = service
                .detect(input(&request), request["force"].as_bool().unwrap_or(false))
                .await?;
            let mut value = json!(prepared);
            if let Some(intent) = &prepared.cache_write {
                intent.ensure_current(prepared.generation, intent.key())?;
                value["cacheIntent"] =
                    json!({"key":intent.key(),"bytes":String::from_utf8_lossy(intent.bytes())});
            }
            value
        }
        "drop_detect" => {
            let child = service.clone();
            let request_input = input(&request);
            let task = tokio::spawn(async move { child.detect(request_input, true).await });
            tokio::time::timeout(Duration::from_secs(2), async {
                while service.stats().active_children == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| EncoderError {
                code: "fixture_timeout",
                message: "Child did not start".into(),
            })?;
            task.abort();
            let _ = task.await;
            json!(service.stats())
        }
        "resolve" => json!(
            service
                .resolve(
                    input(&request),
                    request["setting"].as_str(),
                    request["disableHardware"].as_bool().unwrap_or(false)
                )
                .await?
                .encoder
        ),
        "set" => json!(service.set_rtmp_encoder(
            request["outputId"].as_str().unwrap_or_default(),
            request["encoder"].as_str().unwrap_or_default()
        )?),
        "get" => json!(service.rtmp_encoder(request["outputId"].as_str().unwrap_or_default())?),
        "stats" => json!(service.stats()),
        "shutdown" => {
            service.shutdown().await?;
            json!(service.stats())
        }
        _ => {
            return Err(EncoderError {
                code: "unsupported_operation",
                message: "Unknown test operation".into(),
            })
        }
    })
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    if fixture(&std::env::args().skip(1).collect::<Vec<_>>()) {
        return Ok(());
    }
    let limits: Value = std::env::var("FREESHOW_ENCODER_LIMITS")
        .ok()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or(json!({}));
    let mut config = EncoderConfig::default();
    for (key, target) in [
        ("probes", &mut config.max_probes),
        ("callers", &mut config.max_callers),
        ("listing", &mut config.listing_limit),
        ("encode", &mut config.encode_limit),
        ("cache", &mut config.cache_limit),
        ("settings", &mut config.max_settings),
    ] {
        if let Some(value) = limits[key].as_u64() {
            *target = value as usize;
        }
    }
    if let Some(value) = limits["process_ms"].as_u64() {
        config.process_timeout = Duration::from_millis(value);
    }
    if let Some(value) = limits["operation_ms"].as_u64() {
        config.operation_timeout = Duration::from_millis(value);
    }
    let service = EncoderService::new(config)?;
    let mut tasks = tokio::task::JoinSet::new();
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    loop {
        tokio::select! {
            result=tasks.join_next(),if !tasks.is_empty()=>{if let Some(Ok(value))=result {stdout.write_all(format!("{value}\n").as_bytes()).await?;stdout.flush().await?;}},
            line=lines.next_line(),if tasks.len()<32=>{let Some(line)=line? else {break};if line.len()>5*1024*1024 {return Err("test input limit".into());}let request:Value=serde_json::from_str(&line)?;let service=service.clone();tasks.spawn(async move {let id=request["id"].clone();match execute(service,request).await {Ok(value)=>json!({"id":id,"ok":true,"value":value}),Err(error)=>json!({"id":id,"ok":false,"error":error})}});}
        }
    }
    service.shutdown().await?;
    while tasks.join_next().await.is_some() {}
    Ok(())
}
