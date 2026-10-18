#[path = "../src/network_queries.rs"]
mod network_queries;
#[path = "../src/network_snapshot.rs"]
mod network_snapshot;
#[path = "support/network_oracle.rs"]
mod oracle;
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use network_queries::NativeNetworkQueries;
use network_snapshot::{NetworkSnapshots, SnapshotControl, SnapshotLimits};
use serde_json::{json, Value};
use std::{sync::mpsc, time::Duration};
fn setup() -> (
    tempfile::TempDir,
    LibraryService,
    NetworkSnapshots,
    NativeNetworkQueries,
) {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    let snapshots = NetworkSnapshots::start(SnapshotLimits::default()).unwrap();
    let native = NativeNetworkQueries::new(library.clone(), snapshots.clone());
    (temp, library, snapshots, native)
}
fn request(context: Option<&RuntimeContext>, payload: Value) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"IP","data":payload},"listenerId":"native-ip-敬拜","context":context})).unwrap()
}
async fn wait_for(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
async fn switch(library: &LibraryService, old: &RuntimeContext, path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap();
    let request:Request=serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"newPath":path,"oldPath":old.data_path}},"context":old})).unwrap();
    library.dispatch(request, "main").await.unwrap();
    assert_ne!(library.context().unwrap(), *old);
}
#[tokio::test]
async fn actual_main_ip_replies_match_fresh_native_source_and_ignore_payload_authority() {
    let (_temp, library, snapshots, native) = setup();
    let context = library.context().unwrap();
    let payloads = vec![
        Value::Null,
        json!(false),
        json!(17),
        json!("ignored"),
        json!([]),
        json!({}),
        json!({"interfaces":{"invented":[{"address":"203.0.113.99","family":"IPv4"}]},"hostname":"spoofed","port":1234,"path":"/not/read"}),
    ];
    let mut stable = false;
    for _ in 0..3 {
        let before = oracle::capture(&payloads);
        let mut results = Vec::new();
        for payload in &payloads {
            let events = native
                .dispatch(request(Some(&context), payload.clone()), "main")
                .await
                .unwrap();
            assert_eq!(events.len(), 1);
            let event = &events[0];
            assert_eq!(event.channel, "MAIN");
            assert_eq!(event.message.channel, "IP");
            assert_eq!(event.listener_id.as_deref(), Some("native-ip-敬拜"));
            assert_eq!(event.context.as_ref(), Some(&context));
            results.push(event.message.data.clone().unwrap());
        }
        let after = oracle::capture(&payloads);
        if before != after {
            continue;
        }
        assert_eq!(json!(results), before["replies"]);
        eprintln!("Actual network source runtime: {}", before["runtime"]);
        stable = true;
        break;
    }
    assert!(
        stable,
        "native interfaces changed across three bounded comparisons"
    );
    assert!(
        snapshots.status()["nativeCaptures"].as_u64().unwrap() >= payloads.len() as u64,
        "live IP must invoke native enumeration on every request"
    );
    // Stopping a query consumer does not close the shared Bonjour snapshot owner.
    native.shutdown().await.unwrap();
    let other = snapshots
        .snapshot(SnapshotControl::default())
        .await
        .unwrap();
    assert!(!other.value().hostname.is_empty());
    drop(other);
    assert_eq!(snapshots.status()["workerRunning"], true);
    snapshots.shutdown().await.unwrap();
}
#[tokio::test]
async fn obsolete_and_missing_context_windows_and_oversized_inputs_fail_before_os_work() {
    let (_temp, library, snapshots, native) = setup();
    let current = library.context().unwrap();
    let mut old = current.clone();
    old.runtime_id.push('x');
    for context in [None, Some(&old)] {
        assert_eq!(
            native
                .dispatch(request(context, Value::Null), "main")
                .await
                .err()
                .unwrap()
                .code,
            "stale_library"
        );
    }
    assert_eq!(
        native
            .dispatch(request(Some(&current), Value::Null), "output")
            .await
            .err()
            .unwrap()
            .code,
        "window_error"
    );
    assert_eq!(
        native
            .dispatch(
                request(Some(&current), json!("x".repeat(65 * 1024))),
                "main"
            )
            .await
            .err()
            .unwrap()
            .code,
        "network_query_limit"
    );
    let mut deep = Value::Null;
    for _ in 0..18 {
        deep = json!([deep]);
    }
    assert_eq!(
        native
            .dispatch(request(Some(&current), deep), "main")
            .await
            .err()
            .unwrap()
            .code,
        "network_query_limit"
    );
    assert_eq!(snapshots.status()["nativeCaptures"], 0);
    native.shutdown().await.unwrap();
    snapshots.shutdown().await.unwrap();
}
#[tokio::test]
async fn accepted_caller_drop_and_timeouts_keep_real_host_leases_until_worker_finishes() {
    let (_temp, library, snapshots, _native) = setup();
    let context = library.context().unwrap();
    let native = NativeNetworkQueries::test_limits(
        library,
        snapshots.clone(),
        128 * 1024,
        Duration::from_millis(20),
    );
    let (entered, ready) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    snapshots.pause_next(entered, gate);
    let caller = native.clone();
    let task = tokio::spawn(async move {
        caller
            .dispatch(request(Some(&context), Value::Null), "main")
            .await
    });
    wait_for(|| ready.try_recv().is_ok()).await;
    task.abort();
    let _ = task.await;
    assert_eq!(native.status()["requests"], 1);
    assert_eq!(snapshots.status()["admitted"], 1);
    assert_eq!(
        native.shutdown().await.err().unwrap().code,
        "network_query_shutdown_timeout"
    );
    assert_eq!(native.status()["requests"], 1);
    release.send(()).unwrap();
    native.shutdown().await.unwrap();
    assert_eq!(native.status()["requests"], 0);
    // Closing this consumer cannot multiply/terminate the shared worker.
    let other = snapshots
        .snapshot(SnapshotControl::default())
        .await
        .unwrap();
    drop(other);
    snapshots.shutdown().await.unwrap();
}
#[tokio::test]
async fn library_change_cancels_old_capture_and_final_checks_never_retag_it() {
    let (temp, library, snapshots, native) = setup();
    let old = library.context().unwrap();
    let (entered, ready) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    snapshots.pause_next(entered, gate);
    let caller = native.clone();
    let context = old.clone();
    let task = tokio::spawn(async move {
        caller
            .dispatch(request(Some(&context), Value::Null), "main")
            .await
    });
    wait_for(|| ready.try_recv().is_ok()).await;
    switch(&library, &old, &temp.path().join("new-library")).await;
    native.context_changed(&library.context().unwrap());
    assert_eq!(task.await.unwrap().err().unwrap().code, "stale_library");
    assert_eq!(native.status()["requests"], 1);
    release.send(()).unwrap();
    wait_for(|| native.status()["requests"] == 0).await;
    let current = library.context().unwrap();
    let event = native
        .dispatch(request(Some(&current), Value::Null), "main")
        .await
        .unwrap()
        .remove(0);
    assert_eq!(event.context.as_ref(), Some(&current));
    native.shutdown().await.unwrap();
    snapshots.shutdown().await.unwrap();
}
#[tokio::test]
async fn real_os_failures_output_limits_and_worker_panic_never_become_localhost_success() {
    let (_temp, library, snapshots, native) = setup();
    let context = library.context().unwrap();
    snapshots.fail_hostname(true);
    assert!(native
        .dispatch(request(Some(&context), Value::Null), "main")
        .await
        .unwrap()[0]
        .message
        .data
        .as_ref()
        .unwrap()
        .is_array());
    snapshots.fail_interfaces(true);
    assert_eq!(
        native
            .dispatch(request(Some(&context), Value::Null), "main")
            .await
            .err()
            .unwrap()
            .code,
        "network_snapshot_os"
    );
    snapshots.fail_interfaces(false);
    snapshots.fail_hostname(false);
    let tiny = NativeNetworkQueries::test_limits(
        library.clone(),
        snapshots.clone(),
        2,
        Duration::from_secs(1),
    );
    assert_eq!(
        tiny.dispatch(request(Some(&context), Value::Null), "main")
            .await
            .err()
            .unwrap()
            .code,
        "network_query_limit"
    );
    tiny.shutdown().await.unwrap();
    assert!(snapshots.panic_worker().await.is_err());
    let first = native.shutdown().await.err().unwrap();
    assert_eq!(first.code, "network_snapshot_worker");
    assert_eq!(
        native.shutdown().await.err().unwrap().message,
        first.message
    );
    assert_eq!(
        snapshots.shutdown().await.err().unwrap().message,
        first.message
    );
}
