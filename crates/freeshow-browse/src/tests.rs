use super::*;
use serde_json::json;
use std::sync::{Arc, Barrier};

type Queued = std::pin::Pin<Box<dyn std::future::Future<Output = Result<BrowseReply>> + Send>>;
fn queued(service: &BrowseService, path: &str, cancel: CancellationToken) -> Queued {
    let service = service.clone();
    let payload = json!(path);
    Box::pin(async move {
        service
            .dispatch_queued_with_cancel("FILE_INFO", &payload, cancel)
            .await
    })
}
fn pending(future: &mut Queued) {
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(future.as_mut().poll(&mut context).is_pending());
}
struct WorkerGates {
    entered: tokio::sync::mpsc::Receiver<(String, Instant)>,
    releases: std::collections::HashMap<String, std::sync::mpsc::SyncSender<()>>,
}
impl WorkerGates {
    fn install(service: &BrowseService, paths: &[String]) -> Self {
        let (entered, receive) = tokio::sync::mpsc::channel(16);
        let mut releases = std::collections::HashMap::new();
        let mut gates = std::collections::HashMap::new();
        for path in paths {
            let (release, gate) = std::sync::mpsc::sync_channel(1);
            releases.insert(path.clone(), release);
            gates.insert(path.clone(), gate);
        }
        let gates = std::sync::Mutex::new(gates);
        *service.inner.worker_hooks.entered.lock().unwrap() =
            Some(Arc::new(move |_, request, started| {
                let Request::FileInfo(path) = request else {
                    panic!("Expected the real FILE_INFO worker boundary")
                };
                let gate = gates.lock().unwrap().remove(path);
                let _ = entered.try_send((path.clone(), started));
                if let Some(gate) = gate {
                    let _ = gate.recv();
                }
            }));
        Self {
            entered: receive,
            releases,
        }
    }
    async fn expect_entry(&mut self, path: &str) {
        let entered = tokio::time::timeout(Duration::from_secs(5), self.entered.recv())
            .await
            .expect("Actual browse worker did not enter")
            .unwrap();
        assert_eq!(entered.0, path);
    }
    async fn expect_pending_entry(&mut self, path: &str, work: &mut Queued) -> Instant {
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = work => panic!("Held worker replied before its gate: {result:?}"),
                entered = self.entered.recv() => {
                    let (entered, started) = entered.unwrap();
                    assert_eq!(entered, path);
                    started
                },
            }
        })
        .await
        .expect("Actual browse worker did not enter the admitted FIFO work")
    }
    fn release(&mut self, path: &str) {
        self.releases.remove(path).unwrap().send(()).unwrap();
    }
}
async fn worker_permits(service: &BrowseService, expected: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.inner.permits.available_permits() != expected {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("Actual browse work did not return its execution permit");
}
fn gate_paths(root: &std::path::Path, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            root.join(format!("missing-{index}.mp3"))
                .to_str()
                .unwrap()
                .into()
        })
        .collect()
}

#[test]
fn queued_without_runtime_rejects_before_retaining_or_dispatching_work() {
    use std::task::Poll;
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    let mut work = queued(&service, "missing", CancellationToken::new());
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    let Poll::Ready(result) = work.as_mut().poll(&mut context) else {
        panic!("Queued admission without a runtime remained pending")
    };
    assert_eq!(result.unwrap_err().code, "runtime_required");
    assert_eq!(service.waiting_count(), 0);
    assert_eq!(service.inner.permits.available_permits(), 2);
    drop(work);
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(service.shutdown())
        .unwrap();
    assert_eq!(service.retained_worker_count(), 0);
}

#[tokio::test]
async fn queued_public_test_harness_observes_actual_owners_and_drop_releases_gates() {
    let root = tempfile::tempdir().unwrap();
    let paths = gate_paths(root.path(), 10);
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    let (mut entered, releases) = service.hold_workers_for_test().unwrap();
    assert_eq!(
        service.hold_workers_for_test().unwrap_err().code,
        "browse_busy"
    );
    let mut held = vec![];
    for path in &paths[..2] {
        let mut work = queued(&service, path, CancellationToken::new());
        pending(&mut work);
        held.push(work);
    }
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..2 {
        let index = tokio::time::timeout(Duration::from_secs(5), entered.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(seen.insert(index));
    }
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), [0, 1]);
    assert_eq!(service.inner.permits.available_permits(), 0);
    let mut waiters = vec![];
    for path in &paths[2..] {
        let mut work = queued(&service, path, CancellationToken::new());
        pending(&mut work);
        waiters.push(work);
    }
    assert_eq!(service.queued_waiters_for_test(), 8);
    drop(waiters);
    drop(held);
    assert_eq!(service.queued_waiters_for_test(), 0);
    assert_eq!(
        service.inner.permits.available_permits(),
        0,
        "The actual native gates remain held after caller Drop"
    );
    drop(releases);
    worker_permits(&service, 2).await;
    service.shutdown().await.unwrap();
    assert_eq!(service.retained_worker_count(), 0);
    assert!(
        entered.try_recv().is_err(),
        "The one-shot owner gates multiplied"
    );
}

#[tokio::test]
async fn queued_two_real_workers_and_eight_waiters_are_bounded_and_fifo() {
    let root = tempfile::tempdir().unwrap();
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    let paths = gate_paths(root.path(), 10);
    let mut gates = WorkerGates::install(&service, &paths);
    let mut first = queued(&service, &paths[0], CancellationToken::new());
    pending(&mut first);
    gates.expect_entry(&paths[0]).await;
    let mut second = queued(&service, &paths[1], CancellationToken::new());
    pending(&mut second);
    gates.expect_entry(&paths[1]).await;
    assert_eq!(service.inner.permits.available_permits(), 0);
    assert_eq!(service.waiting_count(), 0);
    let mut waiters = std::collections::VecDeque::new();
    let mut registered = vec![];
    for path in &paths[2..] {
        let mut waiter = queued(&service, path, CancellationToken::new());
        let before = Instant::now();
        pending(&mut waiter); // Registers the actual FIFO semaphore waiter now.
        registered.push((before, Instant::now()));
        waiters.push_back(waiter);
        assert_eq!(service.waiting_count(), waiters.len());
    }
    assert_eq!(service.retained_worker_count(), 2);
    assert_eq!(
        service
            .dispatch("FILE_INFO", &json!("missing"))
            .await
            .unwrap_err()
            .code,
        "browse_busy"
    );
    assert_eq!(
        service
            .dispatch_queued("FILE_INFO", &json!(null))
            .await
            .unwrap_err()
            .code,
        "browse_busy",
        "The ninth waiter is rejected before retaining/parsing malformed work"
    );
    gates.release(&paths[0]);
    assert!(first.await.unwrap().data.is_null());
    for (index, path) in paths[2..].iter().enumerate() {
        if waiters.len() > 1 {
            pending(waiters.back_mut().unwrap());
            assert!(
                gates.entered.try_recv().is_err(),
                "A later FIFO waiter overtook the head"
            );
        }
        let mut head = waiters.pop_front().unwrap();
        let started = gates.expect_pending_entry(path, &mut head).await;
        let (before, after) = registered[index];
        assert!(
            started >= before && started <= after,
            "The total work budget was restarted after the real admission wait"
        );
        assert_eq!(service.waiting_count(), 7 - index);
        gates.release(path);
        assert!(head.await.unwrap().data.is_null());
    }
    gates.release(&paths[1]);
    second.await.unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(service.retained_worker_count(), 0);
}

#[tokio::test]
async fn queued_cancellation_and_caller_drop_remove_waiters_without_native_entry() {
    let root = tempfile::tempdir().unwrap();
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    let paths = gate_paths(root.path(), 4);
    let mut gates = WorkerGates::install(&service, &paths);
    let mut held = vec![];
    for path in &paths[..2] {
        let mut work = queued(&service, path, CancellationToken::new());
        pending(&mut work);
        gates.expect_entry(path).await;
        held.push(work);
    }
    let cancel = CancellationToken::new();
    let mut cancelled = queued(&service, &paths[2], cancel.clone());
    let mut abandoned = queued(&service, &paths[3], CancellationToken::new());
    pending(&mut cancelled);
    pending(&mut abandoned);
    assert_eq!(service.waiting_count(), 2);
    cancel.cancel();
    assert_eq!(cancelled.await.unwrap_err().code, "cancelled");
    assert_eq!(service.waiting_count(), 1);
    drop(abandoned);
    assert_eq!(service.waiting_count(), 0);
    for path in &paths[..2] {
        gates.release(path);
    }
    for work in held {
        work.await.unwrap();
    }
    service.shutdown().await.unwrap();
    assert!(
        gates.entered.try_recv().is_err(),
        "Canceled waiters entered a native worker"
    );
    assert_eq!(service.retained_worker_count(), 0);
}

#[tokio::test]
async fn queued_total_deadline_expires_wait_and_result_without_releasing_held_workers() {
    let root = tempfile::tempdir().unwrap();
    let service = BrowseService::new(BrowseConfig {
        timeout: Duration::from_millis(100),
        ..BrowseConfig::default()
    })
    .unwrap();
    let paths = gate_paths(root.path(), 4);
    let mut gates = WorkerGates::install(&service, &paths[..3]);
    let mut held = vec![];
    for path in &paths[..2] {
        let mut work = queued(&service, path, CancellationToken::new());
        pending(&mut work);
        gates.expect_entry(path).await;
        held.push(work);
    }
    let mut expired = queued(&service, &paths[2], CancellationToken::new());
    pending(&mut expired);
    assert_eq!(service.waiting_count(), 1);
    assert_eq!(expired.await.unwrap_err().code, "browse_resource_limit");
    assert_eq!(service.waiting_count(), 0);
    for work in held {
        assert_eq!(work.await.unwrap_err().code, "browse_resource_limit");
    }
    assert_eq!(
        service.inner.permits.available_permits(),
        0,
        "Timeout did not stop the actual held native work"
    );
    for path in &paths[..2] {
        gates.release(path);
    }
    worker_permits(&service, 2).await;
    let reply = service
        .dispatch_queued("FILE_INFO", &json!(paths[3]))
        .await
        .unwrap();
    assert!(reply.data.is_null());
    gates.expect_entry(&paths[3]).await;
    service.shutdown().await.unwrap();
    assert!(
        gates.entered.try_recv().is_err(),
        "An expired waiter was dispatched later"
    );
}

#[tokio::test]
async fn queued_shutdown_cancels_waiters_and_retains_real_workers_after_observer_drop() {
    let root = tempfile::tempdir().unwrap();
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    let paths = gate_paths(root.path(), 10);
    let mut gates = WorkerGates::install(&service, &paths);
    let mut held = vec![];
    for path in &paths[..2] {
        let mut work = queued(&service, path, CancellationToken::new());
        pending(&mut work);
        gates.expect_entry(path).await;
        held.push(work);
    }
    let mut waiters = vec![];
    for path in &paths[2..] {
        let mut work = queued(&service, path, CancellationToken::new());
        pending(&mut work);
        waiters.push(work);
    }
    {
        use std::future::Future;
        let mut shutdown = Box::pin(service.shutdown());
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(shutdown.as_mut().poll(&mut context).is_pending());
    }
    for work in waiters {
        assert_eq!(work.await.unwrap_err().code, "browse_shutdown");
    }
    for work in held {
        assert_eq!(work.await.unwrap_err().code, "browse_shutdown");
    }
    assert_eq!(service.waiting_count(), 0);
    assert_eq!(service.inner.permits.available_permits(), 0);
    assert_eq!(service.retained_worker_count(), 2);
    assert_eq!(
        service
            .dispatch_queued("FILE_INFO", &json!(paths[2]))
            .await
            .unwrap_err()
            .code,
        "browse_shutdown"
    );
    for path in &paths[..2] {
        gates.release(path);
    }
    service.shutdown().await.unwrap();
    assert!(gates.entered.try_recv().is_err());
    assert_eq!(service.retained_worker_count(), 0);
}

#[tokio::test]
async fn queued_canceled_or_dropped_result_keeps_permit_until_actual_worker_return() {
    for cancel_waiter in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let service = BrowseService::new(BrowseConfig::default()).unwrap();
        let (entered, reached) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::sync_channel(1);
        let boundary = std::sync::Mutex::new(Some((entered, gate)));
        *service.inner.worker_hooks.returned.lock().unwrap() = Some(Arc::new(move || {
            let boundary = boundary.lock().unwrap().take();
            if let Some((entered, gate)) = boundary {
                let _ = entered.send(());
                let _ = gate.recv();
            }
        }));
        let cancel = CancellationToken::new();
        let paths = gate_paths(root.path(), 2);
        let mut work = queued(&service, &paths[0], cancel.clone());
        pending(&mut work);
        tokio::time::timeout(Duration::from_secs(5), reached)
            .await
            .unwrap()
            .unwrap();
        if cancel_waiter {
            cancel.cancel();
            assert_eq!(work.await.unwrap_err().code, "cancelled");
        } else {
            drop(work);
        }
        assert_eq!(
            service.inner.permits.available_permits(),
            1,
            "Engine result/caller Drop is not actual worker return"
        );
        assert!(service
            .dispatch_queued("FILE_INFO", &json!(paths[1]))
            .await
            .unwrap()
            .data
            .is_null());
        worker_permits(&service, 1).await;
        assert_eq!(service.inner.permits.available_permits(), 1);
        release.send(()).unwrap();
        worker_permits(&service, 2).await;
        service.shutdown().await.unwrap();
        assert_eq!(service.retained_worker_count(), 0);
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn queued_prepare_retains_source_thumbnail_slots_and_dispatch_rejection() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("picture.png"), b"source thumbnail intent").unwrap();
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    let payload = json!({"path":root.path(),"generateThumbnails":true});
    let original = service
        .prepare_listing_with_cancel(&payload, CancellationToken::new())
        .await
        .unwrap();
    let queued = service
        .prepare_listing_queued_with_cancel(&payload, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(queued.data, original.data);
    assert_eq!(queued.thumbnail_intents, original.thumbnail_intents);
    assert_eq!(queued.thumbnail_intents.len(), 1);
    assert_eq!(queued.diagnostics.len(), original.diagnostics.len());
    let error = service
        .dispatch_queued("READ_FOLDER", &payload)
        .await
        .unwrap_err();
    assert_eq!(error.code, "thumbnail_unavailable");
    assert_eq!(error.thumbnail_intents, original.thumbnail_intents);
    service.shutdown().await.unwrap();
}

fn budget(config: BrowseConfig) -> Budget {
    Budget {
        config,
        cancel: CancellationToken::new(),
        request_cancel: CancellationToken::new(),
        started: Instant::now(),
        operations: 0,
        entries: 0,
        strings: 0,
        cells: 0,
        substrings: 0,
    }
}
#[test]
fn source_path_shapes_and_utf16_scores() {
    let fixtures: Value = serde_json::from_str(include_str!("path-fixtures.json")).unwrap();
    for item in fixtures.as_array().unwrap() {
        let path = item["path"].as_str().unwrap();
        assert_eq!(paths::basename(path), item["base"]);
        assert_eq!(paths::parent(path), item["dir"]);
        assert_eq!(paths::extension(path), item["ext"]);
        assert_eq!(paths::stem(path), item["name"]);
        assert_eq!(paths::join(path, "next"), item["joined"]);
    }
    let mut limits = budget(BrowseConfig::default());
    assert_eq!(engine::score("🙌", "😀", &mut limits).unwrap(), 0.5);
    assert_eq!(engine::score("Song", "Song", &mut limits).unwrap(), 1.0);
    assert_eq!(engine::score("abc", "xyz", &mut limits).unwrap(), 0.0);
    assert!(engine::score("", "", &mut limits).unwrap().is_nan());
}
#[test]
fn operation_cell_string_and_cancel_bounds() {
    let mut limits = budget(BrowseConfig {
        levenshtein_cells: 3,
        ..BrowseConfig::default()
    });
    assert_eq!(
        engine::score("abcd", "abce", &mut limits).unwrap_err().code,
        "browse_resource_limit"
    );
    let mut limits = budget(BrowseConfig {
        name_units: 1,
        ..BrowseConfig::default()
    });
    assert!(engine::score("🙌", "😀", &mut limits).is_err());
    let mut limits = budget(BrowseConfig {
        operations: 1,
        ..BrowseConfig::default()
    });
    limits.check().unwrap();
    assert!(limits.check().is_err());
    let mut limits = budget(BrowseConfig::default());
    limits.cancel.cancel();
    assert_eq!(limits.check().unwrap_err().code, "cancelled");
}
#[tokio::test]
async fn invalid_request_unknown_and_shutdown_are_explicit() {
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    for (command, payload, code) in [
        ("UNKNOWN", json!(null), "unsupported_command"),
        ("FILE_INFO", json!("bad\0path"), "browse_resource_limit"),
        (
            "READ_FOLDER",
            json!({"path":[],"depth":17}),
            "browse_resource_limit",
        ),
        (
            "READ_FOLDER",
            json!({"path":[],"generateThumbnails":1}),
            "invalid_request",
        ),
        ("GET_SIMILAR", json!({"paths":[1]}), "invalid_request"),
    ] {
        assert_eq!(
            service.dispatch(command, &payload).await.unwrap_err().code,
            code
        );
    }
    let held = service
        .inner
        .permits
        .clone()
        .try_acquire_many_owned(2)
        .unwrap();
    assert_eq!(
        service
            .dispatch("FILE_INFO", &json!("missing"))
            .await
            .unwrap_err()
            .code,
        "browse_busy"
    );
    drop(held);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        service
            .dispatch_with_cancel("FILE_INFO", &json!("missing"), cancel)
            .await
            .unwrap_err()
            .code,
        "cancelled"
    );
    service.shutdown().await.unwrap();
    assert_eq!(
        service
            .dispatch("FILE_INFO", &json!("missing"))
            .await
            .unwrap_err()
            .code,
        "browse_shutdown"
    );
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn filesystem_limits_defaults_and_thumbnail_intents() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("file.mp3"), b"audio").unwrap();
    std::fs::write(dir.path().join("picture.jpg"), b"image").unwrap();
    std::fs::write(dir.path().join("file1.mp3"), b"audio").unwrap();
    std::fs::write(dir.path().join("file2.mp3"), b"audio").unwrap();
    for config in [
        BrowseConfig {
            entries: 1,
            ..BrowseConfig::default()
        },
        BrowseConfig {
            output_bytes: 10,
            ..BrowseConfig::default()
        },
        BrowseConfig {
            timeout: Duration::from_nanos(1),
            ..BrowseConfig::default()
        },
    ] {
        let service = BrowseService::new(config).unwrap();
        assert_eq!(
            service
                .dispatch("READ_FOLDER", &json!({"path":dir.path()}))
                .await
                .unwrap_err()
                .code,
            "browse_resource_limit"
        );
        service.shutdown().await.unwrap();
    }
    for config in [
        BrowseConfig {
            substring_units: 1,
            ..BrowseConfig::default()
        },
        BrowseConfig {
            levenshtein_cells: 1,
            ..BrowseConfig::default()
        },
        BrowseConfig {
            name_units: 1,
            ..BrowseConfig::default()
        },
    ] {
        let service = BrowseService::new(config).unwrap();
        assert_eq!(
            service
                .dispatch(
                    "GET_SIMILAR",
                    &json!({"paths":[dir.path().join("file.mp3")]})
                )
                .await
                .unwrap_err()
                .code,
            "browse_resource_limit"
        );
        service.shutdown().await.unwrap();
    }
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    assert_eq!(
        service
            .dispatch("FILE_INFO", &json!(dir.path().join("absent")))
            .await
            .unwrap()
            .data,
        Value::Null
    );
    let error = service
        .dispatch(
            "READ_FOLDER",
            &json!({"path":dir.path(),"generateThumbnails":true}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "thumbnail_unavailable");
    assert_eq!(error.thumbnail_intents.len(), 1);
    assert_eq!(error.thumbnail_intents[0].size, 250);
    assert_eq!(
        error.thumbnail_intents[0].reason,
        ThumbnailReason::RootMedia
    );
    service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn prepared_listing_keeps_capture_traversal_and_pending_slots_without_retrying_options() {
    let root = tempfile::tempdir().unwrap();
    let child = root.path().join("folder");
    std::fs::create_dir_all(child.join("nested/deeper")).unwrap();
    for index in 0..5 {
        std::fs::write(child.join(format!("{index}.png")), b"image").unwrap();
    }
    std::fs::write(child.join("song.mp3"), b"audio").unwrap();
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    let payload = json!({"path":root.path(),"depth":0,"captureFolderContent":true});
    let prepared = service
        .prepare_listing_with_cancel(&payload, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(prepared.thumbnail_intents.len(), 4);
    for (index, intent) in prepared.thumbnail_intents.iter().enumerate() {
        let path = child.join(format!("{index}.png"));
        assert_eq!(intent.input, path.to_str().unwrap());
        assert_eq!(intent.reason, ThumbnailReason::FolderCapture);
        assert_eq!(prepared.data[path.to_str().unwrap()]["thumbnailPath"], "");
    }
    assert!(prepared
        .data
        .get(child.join("4.png").to_str().unwrap())
        .is_none());
    assert!(prepared
        .data
        .get(child.join("song.mp3").to_str().unwrap())
        .is_none());
    assert_eq!(
        prepared.data[child.to_str().unwrap()]["noMedia"],
        Value::Null
    );
    assert!(prepared.data[child.to_str().unwrap()]
        .as_object()
        .unwrap()
        .contains_key("noMedia"));
    assert_eq!(
        prepared.data[child.to_str().unwrap()]["files"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    let unresolved = service.dispatch("READ_FOLDER", &payload).await.unwrap_err();
    assert_eq!(unresolved.code, "thumbnail_unavailable");
    assert_eq!(unresolved.thumbnail_intents, prepared.thumbnail_intents);
    let without_capture = service
        .dispatch("READ_FOLDER", &json!({"path":root.path(),"depth":0}))
        .await
        .unwrap();
    assert!(without_capture
        .data
        .get(child.join("0.png").to_str().unwrap())
        .is_none());
    let deeper = service
        .prepare_listing_with_cancel(
            &json!({"path":root.path(),"depth":1,"captureFolderContent":true}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let stub = &deeper.data[child.join("nested").to_str().unwrap()];
    assert_eq!(stub["files"], json!([]));
    assert!(!stub.as_object().unwrap().contains_key("noMedia"));
    assert!(deeper
        .data
        .get(child.join("song.mp3").to_str().unwrap())
        .is_some());
    service.shutdown().await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn source_depth_sentinels_links_and_capture_audio_only() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("one/two/three")).unwrap();
    std::fs::write(dir.path().join("one/song.mp3"), b"audio").unwrap();
    std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    let reply = service
        .dispatch(
            "READ_FOLDER",
            &json!({"path":dir.path(),"depth":1,"captureFolderContent":true}),
        )
        .await
        .unwrap();
    let stub = &reply.data[dir.path().join("one/two").to_str().unwrap()];
    assert_eq!(stub["files"], json!([]));
    assert!(!stub.as_object().unwrap().contains_key("noMedia"));
    let ordinary = &reply.data[dir.path().join("one").to_str().unwrap()];
    assert_eq!(ordinary["noMedia"], Value::Null);
    assert!(ordinary.as_object().unwrap().contains_key("noMedia"));
    assert!(reply.data.as_object().unwrap().len() < 30);
    service.shutdown().await.unwrap();
}
#[test]
fn configuration_is_bounded() {
    for config in [
        BrowseConfig {
            workers: 0,
            ..BrowseConfig::default()
        },
        BrowseConfig {
            depth: 100,
            ..BrowseConfig::default()
        },
        BrowseConfig {
            levenshtein_cells: u64::MAX,
            ..BrowseConfig::default()
        },
    ] {
        assert!(BrowseService::new(config).is_err());
    }
}
#[tokio::test]
async fn shutdown_both_admission_orderings() {
    for after in [false, true] {
        let service = BrowseService::new(BrowseConfig::default()).unwrap();
        let reached = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let a = reached.clone();
        let b = release.clone();
        let hook: Box<dyn FnOnce() + Send> = Box::new(move || {
            a.wait();
            b.wait();
        });
        if after {
            *service.inner.after.lock().unwrap() = Some(hook);
        } else {
            *service.inner.before.lock().unwrap() = Some(hook);
        }
        let worker = service.clone();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(worker.dispatch("FILE_INFO", &json!("")))
                .unwrap_err()
                .code
        });
        reached.wait();
        service.inner.cancel.cancel();
        service.inner.sender.lock().unwrap().take();
        if after {
            assert_eq!(service.inner.permits.available_permits(), 1);
        } else {
            assert_eq!(service.inner.permits.available_permits(), 2);
        }
        release.wait();
        assert_eq!(thread.join().unwrap(), "browse_shutdown");
        service.shutdown().await.unwrap();
    }
}
#[test]
fn shutdown_without_runtime_returns_without_panic() {
    use std::{
        future::Future,
        task::{Context, Poll, Wake, Waker},
    };
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    let mut future = Box::pin(service.shutdown());
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    for _ in 0..1000 {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(result) => {
                if let Err(error) = result {
                    assert_eq!(error.code, "runtime_required");
                }
                return;
            }
            Poll::Pending => std::thread::park_timeout(Duration::from_millis(1)),
        }
    }
    panic!("Shutdown failed to finish");
}
