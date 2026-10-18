//! Standalone parity harness, never used as a runtime sidecar.
use freeshow_browse::{BrowseConfig, BrowseService};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
fn main() {
    let pure = std::env::args().any(|arg| arg == "--similarity");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    let input = std::io::stdin();
    let mut input = input.lock();
    let mut output = std::io::stdout();
    loop {
        let mut line = String::new();
        let read = (&mut input)
            .take(2 * 1024 * 1024 + 1)
            .read_line(&mut line)
            .unwrap();
        if read == 0 || line.len() > 2 * 1024 * 1024 {
            break;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        if pure {
            let result = match (request["left"].as_str(), request["right"].as_str()) {
                (Some(left), Some(right)) => {
                    freeshow_browse::similarity(left, right).map(|score| json!({"data":score}))
                }
                _ => Err(freeshow_browse::BrowseError {
                    code: "invalid_request",
                    message: "Similarity names must be strings".into(),
                    thumbnail_intents: vec![],
                }),
            };
            let reply = result.unwrap_or_else(|error| json!({"error":{"code":error.code}}));
            serde_json::to_writer(&mut output, &reply).unwrap();
            writeln!(&mut output).unwrap();
            continue;
        }
        let result = runtime.block_on(
            service.dispatch(request["command"].as_str().unwrap_or(""), &request["data"]),
        );
        let reply = match result {
            Ok(reply) => {
                json!({"data":reply.data,"diagnostics":reply.diagnostics,"thumbnailIntents":reply.thumbnail_intents})
            }
            Err(error) => {
                json!({"error":{"code":error.code,"message":error.message,"thumbnailIntents":error.thumbnail_intents}})
            }
        };
        serde_json::to_writer(&mut output, &reply).unwrap();
        writeln!(&mut output).unwrap();
        output.flush().unwrap();
    }
    runtime.block_on(service.shutdown()).unwrap();
}
