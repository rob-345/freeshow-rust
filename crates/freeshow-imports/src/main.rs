//! Standalone bounded line-oriented parity harness, never a desktop sidecar.
use freeshow_imports::{ImportConfig, ImportRoots, ImportService};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let service = ImportService::new(ImportConfig::default()).unwrap();
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
        let default_root = std::env::temp_dir().join("freeshow-import-oracle");
        let root = request["importsRoot"]
            .as_str()
            .map(std::path::PathBuf::from)
            .unwrap_or(default_root);
        let result = runtime.block_on(service.prepare(
            request["command"].as_str().unwrap_or("IMPORT_FILES"),
            &request["data"],
            ImportRoots { imports_root: root },
        ));
        let response = match result {
            Ok(prepared) => {
                let mut proof_bytes = 0u64;
                let media:Vec<_>=prepared.plan.installs.iter().map(|install|{
                    // Tiny deterministic fixtures expose bytes. Large staged media
                    // retains real identity/hash evidence without huge JSON arrays.
                    let include=install.expected_source.length<=1024*1024&&proof_bytes+install.expected_source.length<=4*1024*1024;
                    let content=if include{proof_bytes+=install.expected_source.length;Some(std::fs::read(&install.staged_path).unwrap())}else{None};
                    json!({"destination":install.destination,"content":content,"sha256":install.sha256,"length":install.expected_source.length})
                }).collect();
                json!({"events":prepared.events,"plan":prepared.plan.manifest(),"media":media})
            }
            Err(error) => {
                json!({"error":{"code":error.code,"message":error.message,"priorEvents":error.prior_events}})
            }
        };
        serde_json::to_writer(&mut output, &response).unwrap();
        writeln!(&mut output).unwrap();
        output.flush().unwrap();
    }
    runtime.block_on(service.shutdown()).unwrap();
}
