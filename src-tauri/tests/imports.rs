#[path = "../src/imports.rs"]
mod imports;

use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_imports::{ImportConfig, ImportService};
use imports::NativeImports;
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::oneshot, time::timeout};
use zip::{write::SimpleFileOptions, ZipWriter};

fn request(id: &str, paths: &[PathBuf], context: &RuntimeContext) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"IMPORT_FILES","data":{"id":id,"paths":paths}},"listenerId":"must-not-echo","context":context})).unwrap()
}

fn setup() -> (tempfile::TempDir, LibraryService, NativeImports, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let staging = root.join("staging");
    fs::create_dir(&staging).unwrap();
    let runtime = Runtime::new(root.join("app"), root.join("library")).unwrap();
    let library = LibraryService::start(runtime).unwrap();
    let native = NativeImports::new(
        ImportService::new(ImportConfig {
            staging_root: Some(staging.clone()),
            ..ImportConfig::default()
        })
        .unwrap(),
    );
    (temp, library, native, staging)
}

fn bundle(root: &Path, filename: &str) -> PathBuf {
    let source = root.join(filename);
    let mut zip = ZipWriter::new(fs::File::create(&source).unwrap());
    zip.start_file("data.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(br#"{"files":["/original/native-media.png"],"project":{"id":"native-import","name":"Native import"},"unknown":42}"#).unwrap();
    zip.start_file("native-media.png", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"unaltered native media bytes").unwrap();
    zip.finish().unwrap();
    source
}

async fn queued(library: &LibraryService, count: usize) {
    timeout(Duration::from_secs(5), async {
        while library.status().unwrap().queued_jobs != count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn pause(
    library: &LibraryService,
) -> (
    std::sync::mpsc::Sender<()>,
    tokio::task::JoinHandle<freeshow_core::Result<Vec<Event>>>,
) {
    let context = library.context().unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, paused) = std::sync::mpsc::channel();
    let owner = library.clone();
    let task = tokio::spawn(async move {
        owner
            .commit(request("txt", &[], &context), "main", move |_| {
                let _ = entered.send(());
                let _ = paused.recv();
                Ok(vec![])
            })
            .await
    });
    timeout(Duration::from_secs(5), ready)
        .await
        .unwrap()
        .unwrap();
    (release, task)
}

#[tokio::test]
async fn text_transport_preserves_raw_unicode_context_and_no_ack_or_directory_effect() {
    let (temp, library, native, staging) = setup();
    assert_eq!(imports::COMMANDS, &["IMPORT_FILES"]);
    let context = library.context().unwrap();
    let source = temp.path().join("敬拜.txt");
    fs::write(&source, "[Verse]\r\n[C]Hello 敬拜 🙌\r\n").unwrap();
    let events = native
        .dispatch(
            &library,
            request("txt", std::slice::from_ref(&source), &context),
            "main",
            |_| panic!("no side events"),
        )
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].message.channel, "IMPORT2");
    let payload = events[0].message.data.as_ref().unwrap();
    assert_eq!(payload["channel"], "txt");
    assert_eq!(
        payload["data"][0]["content"],
        "[Verse]\r\n[C]Hello 敬拜 🙌\r\n"
    );
    assert_eq!(payload["data"][0]["name"], "敬拜");
    assert_eq!(events[0].context.as_ref(), Some(&context));
    assert!(events[0].listener_id.is_none());
    assert!(!context.data_path.join("Imports").exists());
    assert_eq!(
        native
            .dispatch(&library, request("txt", &[], &context), "main", |_| Ok(()))
            .await
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        native
            .dispatch(&library, request("txt", &[], &context), "output-1", |_| Ok(
                ()
            ))
            .await
            .unwrap_err()
            .code,
        "import_role"
    );
    assert_eq!(
        native
            .dispatch(&library, request("pdf", &[], &context), "main", |_| Ok(()))
            .await
            .unwrap_err()
            .code,
        "unsupported_format"
    );
    native.shutdown().await.unwrap();
    assert_eq!(
        native.status(),
        json!({"closing":true,"admittedRequests":0})
    );
    assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
    assert_eq!(
        native
            .dispatch(&library, request("txt", &[], &context), "main", |_| Ok(()))
            .await
            .unwrap_err()
            .code,
        "import_shutdown"
    );
}

#[tokio::test]
async fn invalid_bundle_keeps_source_alert_but_never_emits_success() {
    let (temp, library, native, _) = setup();
    let context = library.context().unwrap();
    let source = temp.path().join("invalid.project");
    let mut zip = ZipWriter::new(fs::File::create(&source).unwrap());
    zip.start_file("data.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"{\"files\":{}}").unwrap();
    zip.finish().unwrap();
    let observed = Mutex::new(vec![]);
    let error = native
        .dispatch(
            &library,
            request("freeshow_project", &[source], &context),
            "main",
            |event| {
                observed.lock().unwrap().push(event);
                Ok(())
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "import_invalid_bundle");
    let observed = observed.into_inner().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].message.channel, "ALERT");
    assert_eq!(observed[0].message.data, Some(json!("popup.importing")));
    assert!(observed[0].listener_id.is_none());
    assert_eq!(observed[0].context.as_ref(), Some(&context));
    assert!(!context.data_path.join("Imports").exists());
    native.shutdown().await.unwrap();
}

#[tokio::test]
async fn admitted_install_survives_dropped_ipc_wait_and_drains_private_staging() {
    let (temp, library, native, staging) = setup();
    let context = library.context().unwrap();
    let source = bundle(temp.path(), "accepted.project");
    let (release, owner) = pause(&library).await;
    let importer = native.clone();
    let ordered = library.clone();
    let frozen = context.clone();
    let waiting = tokio::spawn(async move {
        importer
            .dispatch(
                &ordered,
                request("freeshow_project", &[source], &frozen),
                "main",
                |_| Ok(()),
            )
            .await
    });
    queued(&library, 1).await;
    assert_eq!(native.status()["admittedRequests"], 1);
    assert_eq!(fs::read_dir(&staging).unwrap().count(), 1);
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    assert_eq!(
        native.status()["admittedRequests"],
        1,
        "accepted closure owns the lease"
    );
    release.send(()).unwrap();
    owner.await.unwrap().unwrap();
    library.finish_writes().await.unwrap();
    let files: Vec<_> = fs::read_dir(context.data_path.join("Imports/Projects"))
        .unwrap()
        .map(|item| item.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(
        fs::read(&files[0]).unwrap(),
        b"unaltered native media bytes"
    );
    native.shutdown().await.unwrap();
    native.shutdown().await.unwrap();
    assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
    assert_eq!(native.status()["admittedRequests"], 0);
}

#[tokio::test]
async fn stale_queued_import_cannot_install_or_publish_into_the_next_library() {
    let (temp, library, native, staging) = setup();
    let context = library.context().unwrap();
    let source = bundle(temp.path(), "stale.project");
    let next = temp.path().canonicalize().unwrap().join("next");
    fs::create_dir(&next).unwrap();
    let (release, owner) = pause(&library).await;
    let ordered = library.clone();
    let frozen = context.clone();
    let destination = next.clone();
    let switch = tokio::spawn(async move {
        ordered.dispatch(serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":frozen.data_path,"newPath":destination}},"context":frozen})).unwrap(), "main").await
    });
    queued(&library, 1).await;
    let importer = native.clone();
    let ordered = library.clone();
    let frozen = context.clone();
    let importing = tokio::spawn(async move {
        importer
            .dispatch(
                &ordered,
                request("freeshow_project", &[source], &frozen),
                "main",
                |_| panic!("stale alert must not publish"),
            )
            .await
    });
    queued(&library, 2).await;
    release.send(()).unwrap();
    owner.await.unwrap().unwrap();
    switch.await.unwrap().unwrap();
    native.context_changed(&library.context().unwrap());
    assert_eq!(importing.await.unwrap().unwrap_err().code, "stale_library");
    assert!(!context.data_path.join("Imports").exists());
    assert!(!next.join("Imports").exists());
    native.shutdown().await.unwrap();
    assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
}

#[tokio::test]
async fn pipeline_capacity_includes_prepared_waiters_and_shutdown_cancels_then_joins_them() {
    let (temp, library, native, staging) = setup();
    let context = library.context().unwrap();
    let source = bundle(temp.path(), "bounded.project");
    let (release, owner) = pause(&library).await;
    let mut tasks = vec![];
    for count in 1..=2 {
        let importer = native.clone();
        let ordered = library.clone();
        let frozen = context.clone();
        let source = source.clone();
        tasks.push(tokio::spawn(async move {
            importer
                .dispatch(
                    &ordered,
                    request("freeshow_project", &[source], &frozen),
                    "main",
                    |_| Ok(()),
                )
                .await
        }));
        queued(&library, count).await;
    }
    assert_eq!(
        native
            .dispatch(&library, request("txt", &[], &context), "main", |_| Ok(()))
            .await
            .unwrap_err()
            .code,
        "import_busy"
    );
    let importer = native.clone();
    let shutting_down = tokio::spawn(async move { importer.shutdown().await });
    timeout(Duration::from_secs(5), async {
        while native.status()["closing"] != true {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !shutting_down.is_finished(),
        "shutdown must own retained cleanup"
    );
    release.send(()).unwrap();
    owner.await.unwrap().unwrap();
    for task in tasks {
        assert_eq!(task.await.unwrap().unwrap_err().code, "cancelled");
    }
    timeout(Duration::from_secs(5), shutting_down)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!context.data_path.join("Imports").exists());
    assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
}

#[tokio::test]
async fn tampered_private_stage_rejects_commit_and_retains_only_failure_alerts() {
    let (temp, library, native, staging) = setup();
    let context = library.context().unwrap();
    let source = bundle(temp.path(), "tampered.project");
    let (release, owner) = pause(&library).await;
    let observed = Arc::new(Mutex::new(vec![]));
    let collector = observed.clone();
    let importer = native.clone();
    let ordered = library.clone();
    let frozen = context.clone();
    let importing = tokio::spawn(async move {
        importer
            .dispatch(
                &ordered,
                request("freeshow_project", &[source], &frozen),
                "main",
                |event| {
                    collector.lock().unwrap().push(event);
                    Ok(())
                },
            )
            .await
    });
    queued(&library, 1).await;
    let stage = fs::read_dir(&staging)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let staged_file = fs::read_dir(stage).unwrap().next().unwrap().unwrap().path();
    fs::write(staged_file, b"untrusted replacement").unwrap();
    release.send(()).unwrap();
    owner.await.unwrap().unwrap();
    let error = importing.await.unwrap().unwrap_err();
    assert!(error.code.starts_with("import_"), "{error:?}");
    assert!(observed
        .lock()
        .unwrap()
        .iter()
        .all(|event| event.message.channel == "ALERT" && event.listener_id.is_none()));
    assert!(!context.data_path.join("Imports").exists());
    native.shutdown().await.unwrap();
    assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
}
