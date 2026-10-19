//! Bounded line-delimited probe for tests, not a runtime process-per-request backend.
use freeshow_system::{
    architecture_alias, device_name, meminfo_field, performance_mode, platform_alias, Error,
    JobControl, Limits, SystemService,
};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Read},
    time::{Duration, Instant},
};
fn invalid(message: impl Into<String>) -> Error {
    Error {
        code: "system_invalid".into(),
        message: message.into(),
    }
}
fn number(value: &Value) -> Result<f64, Error> {
    value
        .as_f64()
        .or_else(|| match value.as_str() {
            Some("NaN") => Some(f64::NAN),
            Some("Infinity") => Some(f64::INFINITY),
            Some("-Infinity") => Some(f64::NEG_INFINITY),
            Some("-0") => Some(-0.0),
            _ => None,
        })
        .ok_or_else(|| invalid("Number missing"))
}
fn execute(input: &Value, runtime: &tokio::runtime::Runtime) -> Result<Value, Error> {
    match input["op"].as_str().unwrap_or("snapshot") {
        "batch" => {
            let cases = input["cases"]
                .as_array()
                .ok_or_else(|| invalid("Cases missing"))?;
            if cases.len() > 10_000 {
                return Err(invalid("Too many cases"));
            }
            Ok(Value::Array(
                cases
                    .iter()
                    .map(|case| match execute(case, runtime) {
                        Ok(value) => json!({"ok":true,"value":value}),
                        Err(error) => json!({"ok":false,"error":error}),
                    })
                    .collect(),
            ))
        }
        "decision" => Ok(json!(performance_mode(
            number(&input["total"])?,
            number(&input["free"])?
        ))),
        "device" => Ok(json!(device_name(
            input["hostname"]
                .as_str()
                .ok_or_else(|| invalid("Hostname missing"))?
        ))),
        "aliases" => Ok(
            json!({"platform":platform_alias(input["platform"].as_str().unwrap_or("")),"arch":architecture_alias(input["architecture"].as_str().unwrap_or(""),input["littleEndian"].as_bool().unwrap_or(true))}),
        ),
        "meminfo" => Ok(json!(meminfo_field(
            input["text"].as_str().unwrap_or("").as_bytes(),
            input["key"].as_str().unwrap_or("").as_bytes()
        ))),
        operation @ ("snapshot" | "os" | "deviceNative" | "ram") => {
            let service = SystemService::start(Limits::default())?;
            let mut control = JobControl::default();
            if input["cancelled"] == true {
                control.cancel.cancel();
            }
            if let Some(ms) = input["deadlineMs"].as_u64() {
                control.deadline = Instant::now() + Duration::from_millis(ms.min(10_000));
            }
            let result = match operation {
                "os" => runtime
                    .block_on(service.get_os(control))
                    .map(|value| json!(value)),
                "deviceNative" => runtime
                    .block_on(service.get_device_name(control))
                    .map(|value| json!(value)),
                "ram" => runtime
                    .block_on(service.ram_usage(control))
                    .map(|value| json!(value)),
                _ => {
                    let os = runtime.block_on(service.get_os(control.clone()));
                    let device = runtime.block_on(service.get_device_name(control.clone()));
                    let memory = runtime.block_on(service.memory_sample(control));
                    match (os, device, memory) {
                        (Ok(os), Ok(device), Ok(memory)) => Ok(
                            json!({"os":os,"deviceName":device,"ram":memory.usage(),"memory":memory}),
                        ),
                        (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => Err(error),
                    }
                }
            };
            runtime.block_on(service.shutdown())?;
            result
        }
        _ => Err(invalid("Unknown operation")),
    }
}
fn main() {
    // One current-thread driver, with no extra native owner or helper thread.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("Bounded system probe runtime");
    let mut stdin = io::stdin().lock();
    loop {
        let mut line = String::new();
        let read = (&mut stdin).take(1024 * 1024 + 1).read_line(&mut line);
        if matches!(read, Ok(0)) {
            break;
        }
        let result = if read.is_err() || line.len() > 1024 * 1024 {
            Err(invalid("Probe payload exceeds UTF-8/byte bounds"))
        } else {
            serde_json::from_str::<Value>(&line)
                .map_err(|error| invalid(error.to_string()))
                .and_then(|input| execute(&input, &runtime))
        };
        println!(
            "{}",
            match result {
                Ok(value) => json!({"ok":true,"value":value}),
                Err(error) => json!({"ok":false,"error":error}),
            }
        );
        if read.is_err() || line.len() > 1024 * 1024 {
            break;
        }
    }
}
