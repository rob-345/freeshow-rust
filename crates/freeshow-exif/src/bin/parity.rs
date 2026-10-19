use freeshow_exif::{Context, Control, ExifService, Limits};
use serde_json::{json, Value};
use std::io::{self, BufRead};
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let service = ExifService::start(Limits::default()).unwrap();
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let output = runtime.block_on(async {
            let value: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            let id = value["id"].as_str().ok_or("id is required")?.to_owned();
            let context = Context {
                runtime_id: "parity".into(),
                library_generation: 1,
                data_path: "/parity".into(),
            };
            Ok::<_, String>(match service.read(id, context, Control::default()).await {
                Ok(outcome) => serde_json::to_value(outcome).unwrap(),
                Err(error) => json!({"error":error}),
            })
        });
        println!("{}", output.unwrap_or_else(|error| json!({"error":error})));
    }
    runtime.block_on(service.shutdown()).unwrap();
}
