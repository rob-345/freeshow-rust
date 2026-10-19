//! Bounded test-only JSON adapter, not application IPC or file authority.
use freeshow_cover_http::{plan, ErrorRendering, Headers, Method, SourceFileStat, SourceState};
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
    mtime_ms: Box<RawValue>,
    #[serde(default)]
    production: bool,
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
        let result = (|| {
            let method = match input.method.as_str() {
                "GET" => Method::Get,
                "HEAD" => Method::Head,
                _ => return Err("unsupported parity method".to_owned()),
            };
            let state = match input.source.as_str() {
                "file" => {
                    // Preserve the original JSON numeric spelling until Rust's
                    // correctly-rounded f64 parse (no shared serde features).
                    let ms = input
                        .mtime_ms
                        .get()
                        .parse::<f64>()
                        .map_err(|_| "invalid parity mtime")?;
                    SourceState::File(
                        SourceFileStat::new(input.length, ms).map_err(|err| err.code.to_owned())?,
                    )
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
