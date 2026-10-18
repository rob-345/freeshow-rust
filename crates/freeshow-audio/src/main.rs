//! JSONL feature parity harness; desktop callers use AudioService directly.
use freeshow_audio::{project_metadata, AudioConfig, AudioService};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let service = AudioService::new(AudioConfig::default())?;
    let mut output = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let input: Value = serde_json::from_str(&line?)?;
        let reply = if input["op"] == "project" {
            json!({"ok":true,"value":project_metadata(input["metadata"].clone())})
        } else {
            match runtime.block_on(service.dispatch(
                input["command"].as_str().unwrap_or("READ_AUDIO_METADATA"),
                input.get("data").cloned().unwrap_or(Value::Null),
            )) {
                Ok(reply) => json!({"ok":true,"value":reply.data,"diagnostics":reply.diagnostics}),
                Err(error) => {
                    json!({"ok":false,"error":{"code":error.code(),"message":error.to_string()}})
                }
            }
        };
        serde_json::to_writer(&mut output, &reply)?;
        writeln!(output)?;
        output.flush()?;
    }
    runtime.block_on(service.shutdown());
    Ok(())
}
