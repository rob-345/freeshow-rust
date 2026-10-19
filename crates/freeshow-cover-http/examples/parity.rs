//! Bounded test-only JSON adapter, not application IPC or file authority.
use freeshow_cover_http::{
    match_route, options_plan, plan, ErrorRendering, Headers, Method, SourceFileStat, SourceState,
};
use serde::Deserialize;
use serde_json::{json, value::RawValue};
use std::io::{self, Read};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    source: String,
    method: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
    length: u64,
    #[serde(default)]
    mtime_ms: Option<Box<RawValue>>,
    #[serde(default)]
    posix: Option<PosixTime>,
    #[serde(default)]
    production: bool,
    #[serde(default)]
    route_target: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PosixTime {
    seconds: i64,
    nanoseconds: i64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    io::stdin().take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err("parity input exceeds 1 MiB".into());
    }
    let cases: Vec<Input> = serde_json::from_slice(&bytes)?;
    if cases.len() > 1024 {
        return Err("parity input exceeds 1024 cases".into());
    }
    let mut output = Vec::with_capacity(cases.len());
    for input in cases {
        if let Some(target) = input.route_target.as_ref() {
            output.push(match match_route(&input.method, target) {
                Ok(route) => json!({"ok":true,"route":route,"options":options_plan()}),
                Err(err) => json!({"ok":false,"code":err.code}),
            });
            continue;
        }
        let result = (|| {
            let method = match input.method.as_str() {
                "GET" => Method::Get,
                "HEAD" => Method::Head,
                _ => return Err("unsupported parity method".to_owned()),
            };
            let state = match input.source.as_str() {
                "file" => {
                    let stat = match (input.mtime_ms, input.posix) {
                        (Some(ms), None) => {
                            // Preserve original JSON numeric spelling until
                            // Rust's correctly-rounded f64 parse.
                            let ms = ms
                                .get()
                                .parse::<f64>()
                                .map_err(|_| "invalid parity mtime")?;
                            SourceFileStat::new(input.length, ms)
                        }
                        (None, Some(time)) => {
                            SourceFileStat::from_posix(input.length, time.seconds, time.nanoseconds)
                        }
                        _ => return Err("exactly one parity mtime representation required".into()),
                    }
                    .map_err(|err| err.code.to_owned())?;
                    SourceState::File(stat)
                }
                "missing" => SourceState::Missing,
                "failed" => SourceState::Failed,
                _ => return Err("unsupported parity source".to_owned()),
            };
            let headers = Headers::new(input.headers).map_err(|err| err.code.to_owned())?;
            plan(
                state,
                method,
                &headers,
                if input.production {
                    ErrorRendering::Production
                } else {
                    ErrorRendering::Deferred
                },
            )
            .map_err(|err| err.code.to_owned())
        })();
        output.push(match result {
            Ok(plan) => json!({"ok":true,"plan":plan}),
            Err(code) => json!({"ok":false,"code":code}),
        });
    }
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}
