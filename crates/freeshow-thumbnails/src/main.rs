//! Standalone parity harness, never a runtime sidecar. No final cache effects.
use freeshow_thumbnails::*;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use tokio_util::sync::CancellationToken;
fn main() {
    let pure = std::env::args().any(|arg| arg == "--hash");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let service = ThumbnailService::new(ThumbnailConfig::default()).unwrap();
    let input = std::io::stdin();
    let mut input = input.lock();
    let mut output = std::io::stdout();
    loop {
        let mut line = String::new();
        let n = (&mut input)
            .take(20 * 1024 * 1024 + 1)
            .read_line(&mut line)
            .unwrap();
        if n == 0 || line.len() > 20 * 1024 * 1024 {
            break;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        if pure {
            serde_json::to_writer(
                &mut output,
                &json!({"data":file_path_hash_code(request["input"].as_str().unwrap())}),
            )
            .unwrap();
            writeln!(&mut output).unwrap();
            continue;
        }
        // Harness fixture reads occur outside the runtime; runtime code uses
        // bounded worker completion bytes, never a renderer-supplied path.
        let png = request["pngPath"].as_str().map(|path| {
            let file = std::fs::File::open(path).unwrap();
            let mut bytes = vec![];
            file.take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .unwrap();
            bytes
        });
        let result = runtime.block_on(async {
            let roots = ThumbnailRoots {
                cache_root: request["cacheRoot"].as_str().unwrap().into(),
            };
            let prepared = service
                .prepare(
                    request["command"].as_str().unwrap(),
                    &request["data"],
                    &roots,
                    CancellationToken::new(),
                )
                .await?;
            if let Some(bytes) = png {
                let id = prepared.capture.as_ref().unwrap().id.clone();
                return service
                    .complete(prepared, &id, CaptureCompletion::Png(&bytes))
                    .await;
            }
            if let Some(finish) = request["finish"].as_str() {
                let id = prepared.capture.as_ref().unwrap().id.clone();
                return service
                    .complete(
                        prepared,
                        &id,
                        if finish == "failed" {
                            CaptureCompletion::Failed
                        } else {
                            CaptureCompletion::TimedOut
                        },
                    )
                    .await;
            }
            Ok(prepared)
        });
        let reply = match result {
            Ok(plan) => plan.manifest(),
            Err(e) => json!({"error":{"code":e.code,"message":e.message}}),
        };
        serde_json::to_writer(&mut output, &reply).unwrap();
        writeln!(&mut output).unwrap();
        output.flush().unwrap();
    }
    runtime.block_on(service.shutdown()).unwrap();
}
