//! Test driver only. Native argv root is not product IPC or renderer authority.
use freeshow_ai_local::{Control, InventoryService, Limits, TrustedUserData};
fn main() {
    let root = std::env::args_os().nth(1).expect("Fixture user-data root");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let service =
        InventoryService::start(TrustedUserData::from_host(root).unwrap(), Limits::default())
            .unwrap();
    runtime.block_on(async {
        let observed = match service.try_submit(Control::default()) {
            Ok(ticket) => ticket.finish().await,
            Err(e) => Err(e),
        };
        let reply = match &observed {
            Ok(read) => match read.result() {
                Ok(value) => serde_json::json!({"ok":true,"value":value}),
                Err(e) => {
                    serde_json::json!({"ok":false,"error":{"code":e.code,"message":e.message}})
                }
            },
            Err(e) => serde_json::json!({"ok":false,"error":{"code":e.code,"message":e.message}}),
        };
        println!("{reply}");
        drop(observed);
        service.shutdown().await.unwrap();
    });
}
