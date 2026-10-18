//! Standalone parity harness; desktop integration calls MediaService directly.
use freeshow_media::{MediaConfig, MediaService};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = MediaConfig::default();
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--ffmpeg" => config
                .ffmpeg_candidates
                .push(PathBuf::from(args.next().ok_or("missing ffmpeg path")?)),
            "--ffprobe" => config
                .ffprobe_candidates
                .push(PathBuf::from(args.next().ok_or("missing ffprobe path")?)),
            "--local-bin" => {
                config.local_bin_dir = Some(PathBuf::from(
                    args.next().ok_or("missing local bin directory")?,
                ))
            }
            "--no-discover" => config.discover_system = false,
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let service = MediaService::new(config)?;
    let mut out = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let input: Value = serde_json::from_str(&line?)?;
        let command = input["command"].as_str().ok_or("missing command")?;
        let result = runtime
            .block_on(service.dispatch(command, input.get("data").cloned().unwrap_or(Value::Null)));
        let value = match result {
            Ok(reply) => json!({"ok":true,"value":reply.data,"diagnostics":reply.diagnostics}),
            Err(error) => {
                json!({"ok":false,"error":{"code":error.code(),"message":error.to_string()}})
            }
        };
        serde_json::to_writer(&mut out, &value)?;
        writeln!(out)?;
        out.flush()?;
    }
    runtime.block_on(service.shutdown());
    Ok(())
}
