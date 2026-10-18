use super::*;
use serde_json::json;
use std::sync::{Arc, Barrier};
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
