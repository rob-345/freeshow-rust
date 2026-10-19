//! Controlled native-file parity driver, not a renderer command or path authorizer.
use freeshow_audio::{AudioConfig, AudioService, CommonValue, FormatHint};
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{self, Read},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

fn raw(value: &CommonValue) -> Value {
    match value {
        CommonValue::Undefined => json!(["undefined"]),
        CommonValue::Null => json!(["null"]),
        CommonValue::Bool(v) => json!(["boolean", v]),
        CommonValue::String(v) => json!(["string", v]),
        CommonValue::Number(v) => {
            if v.is_nan() {
                json!(["number", "NaN"])
            } else if *v == f64::INFINITY {
                json!(["number", "Infinity"])
            } else if *v == f64::NEG_INFINITY {
                json!(["number", "-Infinity"])
            } else if *v == 0.0 && v.is_sign_negative() {
                json!(["number", "-0"])
            } else {
                json!(["number", v])
            }
        }
        CommonValue::Array(v) => json!(["array", v.iter().map(raw).collect::<Vec<_>>()]),
        CommonValue::Object(v) => json!([
            "object",
            v.iter()
                .map(|(k, v)| json!([k, raw(v)]))
                .collect::<Vec<_>>()
        ]),
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_string(&mut input)?;
    if input.len() > 1024 * 1024 {
        return Err("driver input limit".into());
    }
    let cases: Vec<Value> = serde_json::from_str(&input)?;
    if cases.len() > 128 {
        return Err("driver case limit".into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let service = AudioService::new(AudioConfig::default())?;
    let mut output = Vec::new();
    for case in cases {
        let path = case["path"].as_str().ok_or("fixture path")?;
        let file = match File::open(path) {
            Ok(v) => v,
            Err(_) => {
                output.push(json!({"ok":false,"error":"audio_input_rejected"}));
                continue;
            }
        };
        if let Some(rename) = case["rename"].as_str() {
            std::fs::rename(path, rename)?;
            std::fs::copy(case["replacement"].as_str().ok_or("replacement")?, path)?;
        }
        let mut file = file;
        let mut answers = Vec::new();
        for _ in 0..case["passes"].as_u64().unwrap_or(1).clamp(1, 3) {
            let hint = if case["detect"] == true {
                FormatHint::Detect
            } else {
                FormatHint::FromSourceName
            };
            let ticket = service.reserve_opened()?.submit(
                file,
                case["source"].as_str().unwrap_or(path),
                hint,
                CancellationToken::new(),
            )?;
            let read = runtime.block_on(ticket.finish())?;
            let answer = match read.common() {
                Ok(common) => {
                    json!({"ok":true,"raw":raw(common.value()),"projection":common.json_projection(),"covers":"unknown","observations":read.observations(),"unchanged":read.observations().unchanged()})
                }
                Err(e) => json!({"ok":false,"error":e.code(),"observations":read.observations()}),
            };
            let (_, returned, _) = read.into_parts();
            file = returned;
            answers.push(answer);
        }
        drop(file);
        output.push(json!({"passes":answers}));
    }
    // Real owners are joined before the fixture results are published.
    runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), service.shutdown()).await })
        .map_err(|_| "driver real shutdown timeout")?;
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}
