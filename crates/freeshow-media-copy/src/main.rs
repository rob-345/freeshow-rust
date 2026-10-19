//! Differential test harness only; never used as a runtime sidecar.
use freeshow_media_copy::*;
use serde_json::{json, Value};
use std::{
    io::{BufRead, Read, Write},
    sync::Arc,
};
fn main() {
    let pure = std::env::args().any(|a| a == "--paths");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let service = MediaCopyService::new(CopyConfig::default()).unwrap();
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    loop {
        let mut line = String::new();
        if (&mut input)
            .take(8 * 1024 * 1024 + 1)
            .read_line(&mut line)
            .unwrap()
            == 0
            || line.len() > 8 * 1024 * 1024
        {
            break;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        let reply = if pure {
            json!({"parentId":parent_folder_id(request["path"].as_str().unwrap()),"hash":file_path_hash(request["path"].as_str().unwrap())})
        } else {
            let stores = Arc::new(StoreSnapshot::new(
                request["stores"]["projects"].clone(),
                request["stores"]["overlays"].clone(),
                request["stores"]["templates"].clone(),
            ));
            let sync = request["syncRoot"]
                .as_str()
                .map(|p| MediaSyncRoot::Configured(p.into()))
                .unwrap_or(MediaSyncRoot::Default);
            let context = CopyContext::new(
                request["dataRoot"].as_str().unwrap_or(""),
                sync,
                request["approvedRoot"].as_str().map(Into::into),
                stores,
            );
            match context.and_then(|context| {
                rt.block_on(service.prepare(
                    request["command"].as_str().unwrap_or(""),
                    &request["data"],
                    &context,
                ))
            }) {
                Ok(plan) => {
                    let bytes:Vec<_>=plan.effects().iter().filter_map(|e|if let CopyEffect::Install{file}=e{Some(json!({"destination":file.destination(),"hex":std::fs::read(file.staged_path()).unwrap().iter().map(|b|format!("{b:02x}")).collect::<String>()}))}else{None}).collect();
                    let value = json!({"plan":plan.manifest(),"staged":bytes});
                    drop(plan);
                    value
                }
                Err(e) => {
                    json!({"error":{"code":e.code,"message":e.message,"diagnostics":e.diagnostics,"omittedDiagnostics":e.omitted_diagnostics}})
                }
            }
        };
        serde_json::to_writer(&mut output, &reply).unwrap();
        writeln!(&mut output).unwrap();
        output.flush().unwrap();
        rt.block_on(service.flush_released()).unwrap();
    }
    rt.block_on(service.shutdown()).unwrap();
}
