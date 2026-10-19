//! Test-only bounded projection driver; no runtime sidecar or real stores.
use freeshow_system::provider_connections::{catalogue, project, MAX_SNAPSHOT_BYTES};
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
    } else if std::env::args().nth(1).as_deref() == Some("--catalogue") {
        // Only this private fixture selector changes mode. Renderer payloads
        // cannot select production catalogue/providers or native IO authority.
        json!({"ok":true,"value":catalogue()})
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
