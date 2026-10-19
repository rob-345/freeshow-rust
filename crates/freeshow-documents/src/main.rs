//! Standalone line protocol for differential tests; never a runtime sidecar.
use freeshow_documents::{DocumentConfig, DocumentContext, DocumentService};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let service = DocumentService::new(DocumentConfig::default()).unwrap();
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    loop {
        let mut line = String::new();
        let size = (&mut input)
            .take(128 * 1024 + 1)
            .read_line(&mut line)
            .unwrap();
        if size == 0 || line.len() > 128 * 1024 {
            break;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        let context = DocumentContext::new(request["dataRoot"].as_str().unwrap_or(""));
        let result = match context {
            Ok(context) => runtime.block_on(service.dispatch(
                request["command"].as_str().unwrap_or(""),
                &request["data"],
                &context,
            )),
            Err(error) => Err(error),
        };
        let response = match result {
            Ok(reply) => {
                json!({"data":reply.data,"complete":reply.is_complete(),"diagnostics":reply.diagnostics,"folderIntents":reply.folder_intents})
            }
            Err(error) => json!({"error":{"code":error.code,"message":error.message}}),
        };
        serde_json::to_writer(&mut output, &response).unwrap();
        writeln!(&mut output).unwrap();
        output.flush().unwrap();
    }
    runtime.block_on(service.shutdown()).unwrap();
}
