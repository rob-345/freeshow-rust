#[path = "../src/ai_local.rs"]
mod ai_local;
use ai_local::NativeInventory;
use freeshow_core::{protocol::Request, runtime::Runtime, service::LibraryService};
use serde_json::json;
use std::fs;
fn fixture() -> (tempfile::TempDir, LibraryService, NativeInventory) {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    let inventory = NativeInventory::new(temp.path().join("app"));
    (temp, library, inventory)
}
fn request(library: &LibraryService) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"AI_GET_BIN","data":{"path":"untrusted renderer root","engine":"ignored"}},"context":library.context().unwrap(),"listenerId":"ai-native-context"})).unwrap()
}
#[tokio::test]
async fn native_global_root_reply_and_original_order_survive_selected_library_changes() {
    let (temp, library, inventory) = fixture();
    assert!(inventory.status().is_none());
    let engine = temp.path().join("app/bin/nemotron");
    fs::create_dir_all(engine.join("模型")).unwrap();
    fs::write(engine.join("README"), b"123").unwrap();
    fs::write(engine.join("模型/weights"), b"45678").unwrap();
    fs::create_dir_all(temp.path().join("library/bin/nemotron")).unwrap();
    fs::write(
        temp.path().join("library/bin/nemotron/fake"),
        b"untrusted selected library",
    )
    .unwrap();
    for selected in [false, true] {
        if selected {
            let old = library.context().unwrap();
            let next = temp.path().join("next");
            fs::create_dir(&next).unwrap();
            let change=serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":old.data_path,"newPath":next}},"context":old})).unwrap();
            library.dispatch(change, "main").await.unwrap();
            inventory.context_changed(&library.context().unwrap());
        }
        let events = inventory
            .dispatch(&library, request(&library), "main")
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.channel, "MAIN");
        assert_eq!(event.message.channel, "AI_GET_BIN");
        assert_eq!(event.listener_id.as_deref(), Some("ai-native-context"));
        assert_eq!(event.context.as_ref(), Some(&library.context().unwrap()));
        assert_eq!(
            event.message.data,
            Some(json!([{"path":engine,"name":"nemotron","size":8}]))
        );
        let row = &event.message.data.as_ref().unwrap()[0];
        assert_eq!(
            row.as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["path", "name", "size"]
        );
        assert!(event.native_lifetime.is_some());
        assert_eq!(inventory.status().unwrap().retained, 1);
        drop(events);
        assert_eq!(inventory.status().unwrap().retained, 0);
    }
    inventory.shutdown().await.unwrap();
    assert!(inventory.status().unwrap().worker_joined);
    library.finish_writes().await.unwrap();
}
#[tokio::test]
async fn invalid_role_context_listener_and_capability_do_not_start_an_owner() {
    let (_temp, library, inventory) = fixture();
    assert_eq!(
        inventory
            .dispatch(&library, request(&library), "output-1")
            .await
            .unwrap_err()
            .code,
        "ai_local_role"
    );
    let mut stale = request(&library);
    stale.context.as_mut().unwrap().library_generation += 1;
    assert_eq!(
        inventory
            .dispatch(&library, stale, "main")
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    let mut long = request(&library);
    long.listener_id = Some("x".repeat(8193));
    assert_eq!(
        inventory
            .dispatch(&library, long, "main")
            .await
            .unwrap_err()
            .code,
        "ai_local_limit"
    );
    assert!(inventory.status().is_none());
    inventory.seal();
    assert_eq!(
        inventory
            .dispatch(&library, request(&library), "main")
            .await
            .unwrap_err()
            .code,
        "ai_local_shutdown"
    );
    inventory.shutdown().await.unwrap();
    assert!(inventory.status().is_none());
    let unavailable = NativeInventory::new("relative-untrusted".into());
    assert_eq!(
        unavailable
            .dispatch(&library, request(&library), "main")
            .await
            .unwrap_err()
            .code,
        "ai_local_unsupported"
    );
    assert!(unavailable.status().is_none());
    unavailable.shutdown().await.unwrap();
    library.finish_writes().await.unwrap();
}
#[tokio::test]
async fn actual_ready_events_and_clones_hold_four_native_slots_through_encoding_and_close_retry() {
    let (_temp, library, inventory) = fixture();
    let mut events = Vec::new();
    for _ in 0..4 {
        events.push(
            inventory
                .dispatch(&library, request(&library), "main")
                .await
                .unwrap(),
        );
    }
    assert_eq!(inventory.status().unwrap().retained, 4);
    assert_eq!(
        inventory
            .dispatch(&library, request(&library), "main")
            .await
            .unwrap_err()
            .code,
        "ai_local_busy"
    );
    let copied = events[0].clone();
    let encoded = serde_json::to_value(&events).unwrap();
    assert!(encoded
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v[0].get("nativeLifetime").is_none()));
    events.remove(0);
    assert_eq!(inventory.status().unwrap().retained, 4);
    drop(copied);
    assert_eq!(inventory.status().unwrap().retained, 3);
    let last = inventory
        .dispatch(&library, request(&library), "main")
        .await
        .unwrap();
    assert_eq!(inventory.status().unwrap().retained, 4);
    inventory.seal();
    assert_eq!(
        inventory
            .dispatch(&library, request(&library), "main")
            .await
            .unwrap_err()
            .code,
        "ai_local_shutdown"
    );
    assert_eq!(
        inventory.shutdown().await.unwrap_err().code,
        "ai_local_shutdown_timeout"
    );
    let status = inventory.status().unwrap();
    assert!(status.worker_joined);
    assert_eq!(status.retained, 4);
    drop(events);
    drop(last);
    drop(encoded);
    inventory.shutdown().await.unwrap();
    let status = inventory.status().unwrap();
    assert!(status.worker_joined);
    assert_eq!(status.retained, 0);
    library.finish_writes().await.unwrap();
}
