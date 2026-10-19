#[path = "../src/ai_local.rs"]
mod ai_local;
use ai_local::NativeInventory;
use freeshow_core::{
    protocol::{Event, Request},
    runtime::Runtime,
    service::LibraryService,
    WireError,
};
use futures_util::FutureExt;
use serde_json::json;
use std::{fs, panic::AssertUnwindSafe, time::Duration};
use tokio::{task::JoinHandle, time::timeout};
const DEADLINE: Duration = Duration::from_secs(10);
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
    let engine = temp.path().join("app").join("bin").join("nemotron");
    fs::create_dir_all(engine.join("模型")).unwrap();
    fs::write(engine.join("README"), b"123").unwrap();
    fs::write(engine.join("模型").join("weights"), b"45678").unwrap();
    fs::create_dir_all(temp.path().join("library").join("bin").join("nemotron")).unwrap();
    fs::write(
        temp.path()
            .join("library")
            .join("bin")
            .join("nemotron")
            .join("fake"),
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

fn query(
    library: &LibraryService,
    inventory: &NativeInventory,
) -> JoinHandle<Result<Vec<Event>, WireError>> {
    let request = request(library);
    let library = library.clone();
    let inventory = inventory.clone();
    tokio::spawn(async move { inventory.dispatch(&library, request, "main").await })
}

async fn drain(library: &LibraryService, inventory: &NativeInventory) {
    // Always attempt both independent owners, including assertion-failure paths.
    // Library finish_writes is its real ordered terminal fence, not a thread join.
    let (inventory_result, library_result) = tokio::join!(
        timeout(DEADLINE, inventory.shutdown()),
        timeout(DEADLINE, library.finish_writes())
    );
    inventory_result.unwrap().unwrap();
    library_result.unwrap().unwrap();
    let status = inventory.status().unwrap();
    assert!(status.worker_joined);
    assert_eq!(status.retained, 0);
}

async fn join_observer<T>(
    mut observer: JoinHandle<T>,
) -> Result<Result<T, tokio::task::JoinError>, tokio::time::error::Elapsed> {
    let result = timeout(DEADLINE, &mut observer).await;
    if result.is_err() {
        observer.abort();
        // A timed-out observation is never treated as an actual task join.
        let _ = observer.await;
    }
    result
}

#[tokio::test]
async fn abort_after_real_admission_cancels_registered_parent_and_drains_actual_native_capacity() {
    let (_temp, library, inventory) = fixture();
    let (entered, release) = inventory.test_hold_after_begin();
    let mut observer = Some(query(&library, &inventory));
    let assertions = AssertUnwindSafe(async {
        let parent = timeout(DEADLINE, entered).await.unwrap().unwrap();
        assert!(!parent.is_cancelled());
        assert_eq!(inventory.test_registered(), 1);
        assert_eq!(inventory.status().unwrap().retained, 1);
        observer.as_ref().unwrap().abort();
        let cancelled = join_observer(observer.take().unwrap()).await.unwrap();
        assert!(cancelled.unwrap_err().is_cancelled());
        assert!(
            parent.is_cancelled(),
            "Caller Drop must cancel the registered parent, not only its child Ticket"
        );
        assert_eq!(inventory.test_registered(), 0);
    })
    .catch_unwind()
    .await;
    // Releasing/closing this test gate is independent of the actual FS lease.
    drop(release);
    if let Some(observer) = observer {
        observer.abort();
        let _ = join_observer(observer).await;
    }
    drain(&library, &inventory).await;
    if let Err(panic) = assertions {
        std::panic::resume_unwind(panic)
    }
}

#[tokio::test]
async fn actual_library_switch_cancels_admitted_old_observer_without_releasing_its_whole_ticket() {
    let (temp, library, inventory) = fixture();
    let engine = temp.path().join("app").join("bin").join("nemotron");
    fs::create_dir_all(&engine).unwrap();
    fs::write(engine.join("actual-inventory"), b"12345").unwrap();
    let (entered, release) = inventory.test_hold_after_begin();
    let mut release = Some(release);
    let mut observer = Some(query(&library, &inventory));
    let assertions = AssertUnwindSafe(async {
        let parent = timeout(DEADLINE, entered).await.unwrap().unwrap();
        let previous = library.context().unwrap();
        inventory.context_changed(&previous);
        assert!(!parent.is_cancelled(), "Current context must remain live");
        let next = temp.path().join("new-library");
        fs::create_dir(&next).unwrap();
        let change = serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":previous.data_path,"newPath":next}},"context":previous})).unwrap();
        timeout(DEADLINE, library.dispatch(change, "main")).await.unwrap().unwrap();
        let current = library.context().unwrap();
        assert_ne!(current, previous);
        inventory.context_changed(&current);
        assert!(parent.is_cancelled());
        assert_eq!(inventory.test_registered(), 1);
        assert_eq!(inventory.status().unwrap().retained, 1, "Cancellation does not dispose the held accepted Ticket");
        assert!(!observer.as_ref().unwrap().is_finished(), "The held old observer must not publish a reply");

        let events = timeout(DEADLINE, inventory.dispatch(&library, request(&library), "main")).await.unwrap().unwrap();
        assert_eq!(events[0].context.as_ref(), Some(&current));
        assert_eq!(events[0].message.data, Some(json!([{"path":engine,"name":"nemotron","size":5}])));
        assert_eq!(inventory.status().unwrap().retained, 2, "New-context result and old accepted observer share the same native owner budget");
        drop(events);
        assert_eq!(inventory.status().unwrap().retained, 1);
        release.take().unwrap().send(()).unwrap();
        let result = join_observer(observer.take().unwrap()).await.unwrap().unwrap();
        assert_eq!(result.unwrap_err().code, "ai_local_cancelled");
        assert_eq!(inventory.test_registered(), 0);
    })
    .catch_unwind()
    .await;
    drop(release);
    if let Some(observer) = observer {
        observer.abort();
        let _ = join_observer(observer).await;
    }
    drain(&library, &inventory).await;
    if let Err(panic) = assertions {
        std::panic::resume_unwind(panic)
    }
}

#[tokio::test]
async fn real_inventory_returns_while_the_actual_ordered_library_commit_is_still_blocked() {
    let (temp, library, inventory) = fixture();
    let engine = temp.path().join("app").join("bin").join("nemotron");
    fs::create_dir_all(&engine).unwrap();
    fs::write(engine.join("actual-inventory"), b"1234567").unwrap();
    let (entered, entered_owner) = tokio::sync::oneshot::channel();
    let (release, held) = std::sync::mpsc::sync_channel::<()>(1);
    let commit_request = serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"AI_TEST_HELD_COMMIT"},"context":library.context().unwrap()})).unwrap();
    let commit_library = library.clone();
    let commit = tokio::spawn(async move {
        commit_library
            .commit(commit_request, "main", move |_| {
                let name = std::thread::current().name().unwrap_or("").to_string();
                let _ = entered.send(name);
                // Closing the sender also releases the real owner after a test fault.
                let _ = held.recv();
                Ok(vec![])
            })
            .await
    });
    let assertions = AssertUnwindSafe(async {
        assert_eq!(
            timeout(DEADLINE, entered_owner).await.unwrap().unwrap(),
            "freeshow-library"
        );
        assert_eq!(
            library.status().unwrap().active_job.unwrap().command,
            "AI_TEST_HELD_COMMIT"
        );
        assert!(!commit.is_finished());
        let events = timeout(
            DEADLINE,
            inventory.dispatch(&library, request(&library), "main"),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            events[0].message.data,
            Some(json!([{"path":engine,"name":"nemotron","size":7}]))
        );
        assert!(events[0].native_lifetime.is_some());
        assert_eq!(inventory.status().unwrap().retained, 1);
        assert!(
            !commit.is_finished(),
            "AI completion cannot release or depend on the held ordered commit"
        );
        assert_eq!(
            library.status().unwrap().active_job.unwrap().command,
            "AI_TEST_HELD_COMMIT"
        );
        drop(events);
    })
    .catch_unwind()
    .await;
    drop(release); // Always release the actual ordered IO owner, even after panic.
    let committed = join_observer(commit).await;
    drain(&library, &inventory).await;
    committed.unwrap().unwrap().unwrap();
    if let Err(panic) = assertions {
        std::panic::resume_unwind(panic)
    }
}
