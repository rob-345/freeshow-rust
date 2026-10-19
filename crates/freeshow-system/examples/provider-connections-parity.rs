//! Test-only bounded projection driver; no runtime sidecar or real stores.
use freeshow_system::provider_connections::{project, MAX_SNAPSHOT_BYTES};
use serde_json::{json, Value};
use std::io::{self, Read};

fn main() {
    // This transport limit is separate from the per-snapshot production bound.
    let mut bytes = Vec::new();
    io::stdin()
        .take((MAX_SNAPSHOT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .unwrap();
    let reply = if bytes.len() > MAX_SNAPSHOT_BYTES {
        json!({"ok":false,"error":{"code":"provider_connections_transport","message":"Test input exceeds transport limit"}})
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Err(_) => {
                json!({"ok":false,"error":{"code":"provider_connections_parse","message":"Test input is not JSON"}})
            }
            Ok(snapshot) => match project(&snapshot) {
                Ok(value) => json!({"ok":true,"value":value}),
                Err(error) => {
                    json!({"ok":false,"error":{"code":error.code(),"message":error.message()}})
                }
            },
        }
    };
    println!("{reply}");
}
