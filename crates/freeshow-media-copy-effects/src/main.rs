//! Bounded differential harness. This executable is never a runtime sidecar.
use freeshow_media_copy::*;
use freeshow_media_copy_effects::{apply, TrustedCopyRoots};
use serde_json::{json, Value};
use std::{
    io::{BufRead, Read, Write},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;
fn main() {
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
        let stores = Arc::new(StoreSnapshot::new(
            request["stores"]["projects"].clone(),
            request["stores"]["overlays"].clone(),
            request["stores"]["templates"].clone(),
        ));
        let context = CopyContext::new(
            request["dataRoot"].as_str().unwrap_or(""),
            request["syncRoot"]
                .as_str()
                .map(|v| MediaSyncRoot::Configured(v.into()))
                .unwrap_or(MediaSyncRoot::Default),
            request["approvedRoot"].as_str().map(Into::into),
            stores,
        );
        let value = match context.and_then(|context| {
            rt.block_on(service.prepare(
                request["command"].as_str().unwrap_or(""),
                &request["data"],
                &context,
            ))
        }) {
            Ok(plan) => match plan.pin_for_commit() {
                Ok(pin) => {
                    let result = apply(
                        TrustedCopyRoots {
                            data_root: plan.captured_data_root(),
                            media_root: plan.destination_root(),
                        },
                        &plan,
                        &pin,
                        &CancellationToken::new(),
                    );
                    match result {
                        Ok(receipt) => json!({"receipt":receipt,"plan":plan.manifest()}),
                        Err(error) => json!({"error":error}),
                    }
                }
                Err(e) => json!({"error":{"code":e.code,"message":e.message}}),
            },
            Err(e) => json!({"error":{"code":e.code,"message":e.message}}),
        };
        serde_json::to_writer(&mut output, &value).unwrap();
        writeln!(&mut output).unwrap();
        output.flush().unwrap();
        rt.block_on(service.flush_released()).unwrap();
    }
    rt.block_on(service.shutdown()).unwrap();
}
