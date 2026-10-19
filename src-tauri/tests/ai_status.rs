#[path = "../src/ai_status.rs"]
mod ai_status;
use ai_status::NativeStatus;
use freeshow_core::{
    protocol::{Event, Request},
    runtime::Runtime,
    service::LibraryService,
};
use serde_json::json;
use std::{
    fs,
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};
use tokio::time::timeout;
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const WAIT: Duration = Duration::from_secs(10);
fn fixture() -> (tempfile::TempDir, LibraryService, NativeStatus) {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    let status = NativeStatus::new(temp.path().join("app"));
    (temp, library, status)
}
fn request(library: &LibraryService) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"AI_GET_STATUS","data":{"engineId":"nemotron","customPath":"renderer decoy ignored"}},"context":library.context().unwrap(),"listenerId":"native-ai-status-敬拜"})).unwrap()
}
async fn entered(receiver: mpsc::Receiver<()>) {
    timeout(WAIT, async {
        loop {
            match receiver.try_recv() {
                Ok(()) => break,
                Err(mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(1)).await
                }
                Err(e) => panic!("Native gate failed: {e}"),
            }
        }
    })
    .await
    .unwrap();
}
async fn until(mut check: impl FnMut() -> bool) {
    timeout(WAIT, async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
async fn query(status: &NativeStatus, library: &LibraryService) -> Vec<Event> {
    timeout(WAIT, status.dispatch(library, request(library), "main"))
        .await
        .unwrap()
        .unwrap()
}
async fn select(temp: &Path, library: &LibraryService, status: &NativeStatus) {
    let old = library.context().unwrap();
    let next = temp.join("selected next");
    fs::create_dir(&next).unwrap();
    let change=serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":old.data_path,"newPath":next}},"context":old})).unwrap();
    library.dispatch(change, "main").await.unwrap();
    status.context_changed(&library.context().unwrap());
}
fn fast_stamp(user_data: &Path) {
    let code = r#"import fs from 'node:fs';import path from 'node:path';import {files,makeStamp} from './tests/parity/ai-status-oracle.mjs';const models=path.join(process.argv[1],'bin/nemotron/models');fs.mkdirSync(models,{recursive:true});for(const[name]of files)fs.writeFileSync(path.join(models,name),name);fs.writeFileSync(path.join(models,'model.json'),JSON.stringify(makeStamp(models),null,4));"#;
    let node = std::env::var_os("FREESHOW_NATIVE_AUDIO_NODE").unwrap_or_else(|| "node".into());
    let child = std::process::Command::new(node)
        .args(["--input-type=module", "-e", code])
        .arg(user_data)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
}
#[tokio::test]
async fn actual_missing_and_source_stamp_shortcut_preserve_native_root_envelope_and_library_independence(
) {
    let _serial = SERIAL.lock().await;
    let (temp, library, status) = fixture();
    assert_eq!(status.test_snapshot(), (0, 0, false, false));
    let events = query(&status, &library).await;
    let event = &events[0];
    assert_eq!(event.channel, "MAIN");
    assert_eq!(event.message.channel, "AI_GET_STATUS");
    assert_eq!(event.listener_id.as_deref(), Some("native-ai-status-敬拜"));
    assert_eq!(event.context.as_ref(), Some(&library.context().unwrap()));
    let intel_mac = cfg!(all(target_os = "macos", target_arch = "x86_64"));
    let message = event.message.data.as_ref().unwrap();
    assert_eq!(message["nemotron"]["ready"], false);
    assert!(message["nemotron"]["error"].is_string());
    assert!(event.native_lifetime.is_some());
    assert!(!temp
        .path()
        .join("app")
        .join("bin")
        .join("nemotron")
        .exists());
    assert_eq!(status.test_snapshot().0, 1);
    let clone = event.clone();
    drop(events);
    assert_eq!(status.test_snapshot().0, 1);
    let encoded = serde_json::to_value(&clone).unwrap();
    assert!(encoded.get("nativeLifetime").is_none());
    drop(clone);
    assert_eq!(status.test_snapshot().0, 0);
    fast_stamp(&temp.path().join("app"));
    let stamp = temp
        .path()
        .join("app")
        .join("bin")
        .join("nemotron")
        .join("models")
        .join("model.json");
    let before = fs::read(&stamp).unwrap();
    for switched in [false, true] {
        if switched {
            select(temp.path(), &library, &status).await;
        }
        let events = query(&status, &library).await;
        let data = events[0].message.data.as_ref().unwrap();
        if !intel_mac {
            assert_eq!(
                data["nemotron"],
                json!({"ready":true,"localPath":temp.path().join("app").join("bin").join("nemotron").join("models")})
            );
        } else {
            assert_eq!(data["nemotron"]["ready"], false);
        }
        assert_eq!(
            events[0].context.as_ref(),
            Some(&library.context().unwrap())
        );
        assert_eq!(fs::read(&stamp).unwrap(), before);
        drop(events);
    }
    status.shutdown().await.unwrap();
    assert_eq!(status.test_snapshot(), (0, 0, true, true));
    library.finish_writes().await.unwrap();
}
#[tokio::test]
async fn invalid_selection_role_context_and_listener_do_not_start_a_thread() {
    let _serial = SERIAL.lock().await;
    let (_temp, library, status) = fixture();
    assert_eq!(
        status
            .dispatch(&library, request(&library), "output")
            .await
            .unwrap_err()
            .code,
        "ai_status_role"
    );
    let mut stale = request(&library);
    stale.context.as_mut().unwrap().library_generation += 1;
    assert_eq!(
        status
            .dispatch(&library, stale, "main")
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    for payload in [
        json!({}),
        json!({"engineId":"cloud"}),
        json!({"engineId":"nemotron","modelId":"model"}),
        json!({"engineId":"nemotron","modelId":[]}),
    ] {
        let mut r = request(&library);
        r.data["data"] = payload;
        assert_eq!(
            status.dispatch(&library, r, "main").await.unwrap_err().code,
            "ai_status_unsupported"
        );
    }
    let mut r = request(&library);
    r.listener_id = Some("x".repeat(8193));
    assert_eq!(
        status.dispatch(&library, r, "main").await.unwrap_err().code,
        "ai_status_limit"
    );
    assert_eq!(status.test_snapshot(), (0, 0, false, false));
    status.shutdown().await.unwrap();
    assert_eq!(
        status
            .dispatch(&library, request(&library), "main")
            .await
            .unwrap_err()
            .code,
        "ai_status_shutdown"
    );
    library.finish_writes().await.unwrap();
}
#[tokio::test]
async fn accepted_caller_drop_and_four_ready_clones_retain_actual_work_through_real_join() {
    let _serial = SERIAL.lock().await;
    let (_temp, library, status) = fixture();
    let (observed, release) = status.test_hold("processing");
    let host = status.clone();
    let source = library.clone();
    let caller =
        tokio::spawn(async move { host.dispatch(&source, request(&source), "main").await });
    entered(observed).await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert_eq!(status.test_snapshot().0, 1);
    assert_eq!(status.test_snapshot().1, 0);
    let (exit_observed, exit_release) = status.test_hold("exit");
    assert!(timeout(Duration::from_millis(30), status.shutdown())
        .await
        .is_err());
    assert_eq!(status.test_snapshot().0, 1);
    assert!(!status.test_snapshot().3);
    release.send(()).unwrap();
    entered(exit_observed).await;
    until(|| status.test_snapshot().0 == 0).await;
    assert!(timeout(Duration::from_millis(30), status.shutdown())
        .await
        .is_err());
    assert!(!status.test_snapshot().3);
    exit_release.send(()).unwrap();
    status.shutdown().await.unwrap();
    assert!(status.test_snapshot().3);
    drop(status);
    let host = NativeStatus::new(
        library
            .context()
            .unwrap()
            .data_path
            .parent()
            .unwrap()
            .join("app"),
    );
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(query(&host, &library).await);
    }
    assert_eq!(host.test_snapshot().0, 4);
    assert_eq!(
        host.dispatch(&library, request(&library), "main")
            .await
            .unwrap_err()
            .code,
        "ai_status_busy"
    );
    let clones: Vec<_> = held.iter().map(|events| events[0].clone()).collect();
    drop(held);
    assert_eq!(host.test_snapshot().0, 4);
    for event in &clones {
        let encoded = serde_json::to_vec(event).unwrap();
        assert!(!String::from_utf8_lossy(&encoded).contains("nativeLifetime"));
    }
    host.shutdown().await.unwrap();
    assert_eq!(host.test_snapshot().0, 4);
    assert!(host.test_snapshot().3);
    drop(clones);
    assert_eq!(host.test_snapshot().0, 0);
    library.finish_writes().await.unwrap();
}
#[tokio::test]
async fn actual_selection_cancels_old_observer_while_pending_native_frame_stays_owned() {
    let _serial = SERIAL.lock().await;
    let (temp, library, status) = fixture();
    let (observed, release) = status.test_hold("processing");
    let host = status.clone();
    let source = library.clone();
    let old = request(&library);
    let caller = tokio::spawn(async move { host.dispatch(&source, old, "main").await });
    entered(observed).await;
    select(temp.path(), &library, &status).await;
    assert_eq!(
        timeout(WAIT, caller)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code,
        "ai_status_cancelled"
    );
    assert_eq!(status.test_snapshot().0, 1);
    release.send(()).unwrap();
    until(|| status.test_snapshot().0 == 0).await;
    let events = query(&status, &library).await;
    assert_eq!(
        events[0].context.as_ref(),
        Some(&library.context().unwrap())
    );
    drop(events);
    status.shutdown().await.unwrap();
    assert!(status.test_snapshot().3);
    library.finish_writes().await.unwrap();
}
#[tokio::test]
async fn status_preparation_finishes_during_an_actually_held_library_commit() {
    let _serial = SERIAL.lock().await;
    let (_temp, library, status) = fixture();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let held_library = library.clone();
    let hold = request(&library);
    let commit = tokio::spawn(async move {
        held_library
            .commit_without_media_refresh(hold, "main", move |_| {
                entered_tx.send(()).unwrap();
                let _ = release_rx.recv();
                Ok(vec![])
            })
            .await
    });
    entered(entered_rx).await;
    let start = Instant::now();
    let events = timeout(Duration::from_millis(500), query(&status, &library))
        .await
        .unwrap();
    assert!(start.elapsed() < Duration::from_millis(500));
    assert!(!commit.is_finished());
    assert_eq!(
        events[0].context.as_ref(),
        Some(&library.context().unwrap())
    );
    drop(events);
    drop(release_tx);
    timeout(WAIT, commit).await.unwrap().unwrap().unwrap();
    status.shutdown().await.unwrap();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn genuine_worker_panic_keeps_cleanup_frame_owned_and_shutdown_retries_actual_join() {
    let _serial = SERIAL.lock().await;
    let (_temp, library, status) = fixture();
    status.test_panic_next();
    let (observed, release) = status.test_hold("result");
    let host = status.clone();
    let source = library.clone();
    let caller =
        tokio::spawn(async move { host.dispatch(&source, request(&source), "main").await });
    entered(observed).await;
    let error = timeout(WAIT, caller).await.unwrap().unwrap().unwrap_err();
    assert!(["ai_status_worker", "ai_status_cancelled"].contains(&error.code));
    assert_eq!(status.test_snapshot().0, 1);
    assert!(timeout(Duration::from_millis(30), status.shutdown())
        .await
        .is_err());
    assert!(!status.test_snapshot().3);
    release.send(()).unwrap();
    let error = status.shutdown().await.unwrap_err();
    assert_eq!(error.code, "ai_status_worker");
    assert_eq!(status.test_snapshot(), (0, 0, true, true));
    library.finish_writes().await.unwrap();
}

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
fn fixed_tiny(user_data: &Path) -> std::path::PathBuf {
    let models = user_data.join("bin").join("nemotron").join("models");
    fs::create_dir_all(&models).unwrap();
    for (name, _) in freeshow_ai_status::FILES {
        fs::write(models.join(name), name).unwrap();
    }
    models.join("model.json")
}
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
async fn held_library(
    library: &LibraryService,
) -> (
    mpsc::Sender<()>,
    tokio::task::JoinHandle<freeshow_core::Result<Vec<Event>>>,
) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let owner = library.clone();
    let r = request(library);
    let commit = tokio::spawn(async move {
        owner
            .commit_without_media_refresh(r, "main", move |_| {
                entered_tx.send(()).unwrap();
                let _ = release_rx.recv();
                Ok(vec![])
            })
            .await
    });
    entered(entered_rx).await;
    (release_tx, commit)
}
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
#[tokio::test]
async fn real_fixed_fixture_preparation_waits_for_ordered_stamp_attempt_and_preserves_source_write_failure_ready(
) {
    let _serial = SERIAL.lock().await;
    for fail_write in [false, true] {
        let (temp, library, status) = fixture();
        let stamp = fixed_tiny(&temp.path().join("app"));
        status.test_fixed_tiny();
        if fail_write {
            fs::create_dir(&stamp).unwrap();
        }
        let (release, commit) = held_library(&library).await;
        let host = status.clone();
        let source = library.clone();
        let caller =
            tokio::spawn(async move { host.dispatch(&source, request(&source), "main").await });
        until(|| library.status().unwrap().queued_jobs == 1).await;
        assert!(!caller.is_finished());
        assert_eq!(status.test_snapshot().0, 1);
        assert_eq!(status.test_stamp_observation(), (0, false));
        assert_eq!(
            stamp.exists(),
            fail_write,
            "Filesystem preparation cannot install the ordered stamp"
        );
        drop(release);
        timeout(WAIT, commit).await.unwrap().unwrap().unwrap();
        let events = timeout(WAIT, caller).await.unwrap().unwrap().unwrap();
        assert_eq!(status.test_stamp_observation(), (1, fail_write));
        assert_eq!(
            events[0].message.data.as_ref().unwrap()["nemotron"],
            json!({"ready":true,"localPath":temp.path().join("app").join("bin").join("nemotron").join("models")})
        );
        assert_eq!(
            events[0].context.as_ref(),
            Some(&library.context().unwrap())
        );
        assert!(events[0].native_lifetime.is_some());
        assert_eq!(status.test_snapshot().0, 1);
        if fail_write {
            assert!(stamp.is_dir());
        } else {
            let body: serde_json::Value =
                serde_json::from_slice(&fs::read(&stamp).unwrap()).unwrap();
            assert_eq!(body["revision"], freeshow_ai_status::REVISION);
            for (name, digest) in freeshow_ai_status::FILES {
                assert_eq!(body["files"][name]["sha256"], digest);
                assert_eq!(body["files"][name]["size"], name.len());
            }
        }
        drop(events);
        assert_eq!(status.test_snapshot().0, 0);
        status.shutdown().await.unwrap();
        library.finish_writes().await.unwrap();
    }
}
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
#[tokio::test]
async fn original_deadline_and_caller_abort_leave_queued_stamp_owned_until_the_real_library_fence()
{
    let _serial = SERIAL.lock().await;
    for expire in [false, true] {
        let (temp, library, status) = fixture();
        let stamp = fixed_tiny(&temp.path().join("app"));
        status.test_fixed_tiny();
        if expire {
            status.test_deadline(Duration::from_millis(100));
        }
        let (release, commit) = held_library(&library).await;
        let host = status.clone();
        let source = library.clone();
        let caller =
            tokio::spawn(async move { host.dispatch(&source, request(&source), "main").await });
        until(|| library.status().unwrap().queued_jobs == 1).await;
        if expire {
            assert_eq!(
                timeout(WAIT, caller)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap_err()
                    .code,
                "ai_status_deadline"
            );
        } else {
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
        }
        assert!(!stamp.exists());
        assert_eq!(status.test_snapshot().0, 1);
        assert_eq!(status.test_stamp_observation(), (0, false));
        drop(release);
        timeout(WAIT, commit).await.unwrap().unwrap().unwrap();
        library.finish_writes().await.unwrap();
        assert!(!stamp.exists());
        assert_eq!(status.test_snapshot().0, 0);
        assert_eq!(status.test_stamp_observation(), (0, false));
        status.shutdown().await.unwrap();
    }
}
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
#[tokio::test]
async fn actual_queued_library_replacement_rejects_old_stamp_before_io_and_new_context_can_apply_it(
) {
    let _serial = SERIAL.lock().await;
    let (temp, library, status) = fixture();
    let stamp = fixed_tiny(&temp.path().join("app"));
    status.test_fixed_tiny();
    let (release, commit) = held_library(&library).await;
    let old = library.context().unwrap();
    let next = temp.path().join("next model library");
    fs::create_dir(&next).unwrap();
    let change=serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":old.data_path,"newPath":next}},"context":old})).unwrap();
    let change_owner = library.clone();
    let change = tokio::spawn(async move { change_owner.dispatch(change, "main").await });
    until(|| library.status().unwrap().queued_jobs == 1).await;
    let host = status.clone();
    let source = library.clone();
    let caller =
        tokio::spawn(async move { host.dispatch(&source, request(&source), "main").await });
    until(|| library.status().unwrap().queued_jobs == 2).await;
    assert!(!stamp.exists());
    drop(release);
    timeout(WAIT, commit).await.unwrap().unwrap().unwrap();
    timeout(WAIT, change).await.unwrap().unwrap().unwrap();
    status.context_changed(&library.context().unwrap());
    let error = timeout(WAIT, caller).await.unwrap().unwrap().unwrap_err();
    assert!(["stale_library", "ai_status_cancelled"].contains(&error.code));
    until(|| status.test_snapshot().0 == 0).await;
    assert!(!stamp.exists());
    assert_eq!(status.test_stamp_observation(), (0, false));
    let events = query(&status, &library).await;
    assert_eq!(
        events[0].context.as_ref(),
        Some(&library.context().unwrap())
    );
    assert!(stamp.is_file());
    assert_eq!(status.test_stamp_observation(), (1, false));
    drop(events);
    status.shutdown().await.unwrap();
    library.finish_writes().await.unwrap();
}
#[tokio::test]
async fn internal_shutdown_timeout_and_four_observers_retain_then_retry_the_actual_native_join() {
    let _serial = SERIAL.lock().await;
    let (_temp, library, status) = fixture();
    drop(query(&status, &library).await);
    let (observed, release) = status.test_hold("exit");
    status.seal();
    entered(observed).await;
    assert_eq!(
        status.shutdown().await.unwrap_err().code,
        "ai_status_shutdown_timeout"
    );
    assert!(!status.test_snapshot().3);
    let mut observers = Vec::new();
    for _ in 0..4 {
        let owner = status.clone();
        observers.push(tokio::spawn(async move { owner.shutdown().await }));
    }
    until(|| status.test_shutdown_observers() == 4).await;
    assert_eq!(
        status.shutdown().await.unwrap_err().code,
        "ai_status_observer_busy"
    );
    let dropped = observers.pop().unwrap();
    dropped.abort();
    assert!(dropped.await.unwrap_err().is_cancelled());
    until(|| status.test_shutdown_observers() == 3).await;
    let owner = status.clone();
    observers.push(tokio::spawn(async move { owner.shutdown().await }));
    until(|| status.test_shutdown_observers() == 4).await;
    assert!(!status.test_snapshot().3);
    release.send(()).unwrap();
    for observer in observers {
        timeout(WAIT, observer).await.unwrap().unwrap().unwrap();
    }
    assert_eq!(status.test_shutdown_observers(), 0);
    assert!(status.test_snapshot().3);
    status.shutdown().await.unwrap();
    library.finish_writes().await.unwrap();
}
