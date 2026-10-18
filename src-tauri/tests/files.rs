#[path = "../src/files.rs"]
mod files;

use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_files::{paths, FilesService, Limits};
use serde_json::{json, Value};
use std::{fs, time::Duration};
use tokio::{sync::oneshot, time::timeout};

fn request(command: &str, data: Value, context: &RuntimeContext) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":command,"data":data},"listenerId":"lookup","context":context})).unwrap()
}

fn service() -> (tempfile::TempDir, LibraryService, files::NativeFiles) {
    let temp = tempfile::tempdir().unwrap();
    let runtime = Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap();
    (
        temp,
        LibraryService::start(runtime).unwrap(),
        files::NativeFiles::new(FilesService::start(Limits::default()).unwrap()),
    )
}

#[tokio::test]
async fn existing_file_reply_retains_context_without_creating_media_or_sync_cache_entries() {
    let (temp, library, native) = service();
    assert!(files::COMMANDS.contains(&"LOCATE_MEDIA_FILE"));
    let context = library.context().unwrap();
    let existing = temp.path().join("existing 敬拜.mp4");
    fs::write(&existing, b"media").unwrap();
    let events = files::dispatch(
        &library,
        &native,
        request(
            "LOCATE_MEDIA_FILE",
            json!({"filePath":existing,"folders":[]}),
            &context,
        ),
        "main",
    )
    .await
    .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].message.data.as_ref().unwrap(),
        &json!({"path":existing,"hasChanged":false})
    );
    assert_eq!(events[0].context.as_ref(), Some(&context));
    assert_eq!(events[0].listener_id.as_deref(), Some("lookup"));
    assert!(!context.data_path.join("Media").exists());
    assert!(!temp.path().join("app/cache_sync.json").exists());
    native.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_lookup_effects_merge_cache_keys_and_invalidate_only_fork_thumbnail_names() {
    let temp = tempfile::tempdir().unwrap();
    let mut runtime = Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap();
    runtime
        .stores
        .set(
            "CACHE_SYNC",
            json!({"unrelated":{"keep":true},"replacedPaths":{"prior":"retained"}}),
        )
        .unwrap();
    let library = LibraryService::start(runtime).unwrap();
    let native = files::NativeFiles::new(FilesService::start(Limits::default()).unwrap());
    let context = library.context().unwrap();
    let root = temp.path().join("search/subfolder");
    fs::create_dir_all(&root).unwrap();
    let thumbnail_root = temp.path().join("app/Thumbnails");
    fs::create_dir(&thumbnail_root).unwrap();
    fs::write(thumbnail_root.join("unrelated.png"), b"retain").unwrap();
    let originals = [
        temp.path().join("missing/Worship E\u{301}.mp4"),
        temp.path().join("missing/Audio 敬拜.wav"),
    ];
    let found = [root.join("WORSHIP É.MP4"), root.join("Audio 敬拜.wav")];
    for (original, actual) in originals.iter().zip(&found) {
        fs::write(actual, b"media").unwrap();
        for size in [900, 500, 250, 100] {
            fs::write(
                thumbnail_root.join(format!(
                    "{}-{size}.png",
                    paths::file_path_hash(original.to_str().unwrap())
                )),
                b"stale",
            )
            .unwrap();
        }
    }
    let first = files::dispatch(
        &library,
        &native,
        request(
            "LOCATE_MEDIA_FILE",
            json!({"filePath":originals[0],"folders":[temp.path().join("search")]}),
            &context,
        ),
        "main",
    );
    let second = files::dispatch(
        &library,
        &native,
        request(
            "LOCATE_MEDIA_FILE",
            json!({"filePath":originals[1],"folders":[temp.path().join("search")]}),
            &context,
        ),
        "main",
    );
    let (first, second) = tokio::join!(first, second);
    for (events, expected) in [first.unwrap(), second.unwrap()].iter().zip(&found) {
        assert_eq!(
            events[0].message.data.as_ref().unwrap(),
            &json!({"path":expected,"hasChanged":true})
        );
    }
    let cache: Value =
        serde_json::from_slice(&fs::read(temp.path().join("app/cache_sync.json")).unwrap())
            .unwrap();
    assert_eq!(cache["unrelated"], json!({"keep":true}));
    assert_eq!(cache["replacedPaths"]["prior"], "retained");
    assert_eq!(cache["replacedPaths"].as_object().unwrap().len(), 3);
    for actual in found {
        assert!(cache["replacedPaths"]
            .as_object()
            .unwrap()
            .values()
            .any(|path| path.as_str() == actual.to_str()));
    }
    assert_eq!(fs::read_dir(thumbnail_root).unwrap().count(), 1);
    native.shutdown().await.unwrap();
}

#[tokio::test]
async fn media_folder_getter_preserves_lazy_default_and_configured_path_behavior() {
    let (temp, library, native) = service();
    let context = library.context().unwrap();
    let configured = temp.path().join("configured but missing");
    let replies = files::dispatch(
        &library,
        &native,
        request("SET_MEDIA_FOLDER_PATH", json!(configured), &context),
        "main",
    )
    .await
    .unwrap();
    assert!(replies.is_empty());
    let replies = files::dispatch(
        &library,
        &native,
        request("GET_MEDIA_FOLDER_PATH", Value::Null, &context),
        "main",
    )
    .await
    .unwrap();
    assert_eq!(
        replies[0].message.data.as_ref().unwrap(),
        &json!(configured)
    );
    assert!(!configured.exists());
    assert!(!context.data_path.join("Media").exists());
    files::dispatch(
        &library,
        &native,
        request("SET_MEDIA_FOLDER_PATH", json!(""), &context),
        "main",
    )
    .await
    .unwrap();
    let replies = files::dispatch(
        &library,
        &native,
        request("GET_MEDIA_FOLDER_PATH", Value::Null, &context),
        "main",
    )
    .await
    .unwrap();
    assert_eq!(
        replies[0].message.data.as_ref().unwrap(),
        &json!(context.data_path.join("Media"))
    );
    assert!(context.data_path.join("Media").is_dir());
    native.shutdown().await.unwrap();
}

#[tokio::test]
async fn resource_failure_retains_completed_missing_file_invalidations_and_returns_an_error() {
    let (temp, library, previous_native) = service();
    previous_native.shutdown().await.unwrap();
    let native = files::NativeFiles::new(
        FilesService::start(Limits {
            max_entries: 0,
            ..Limits::default()
        })
        .unwrap(),
    );
    let context = library.context().unwrap();
    let original = temp.path().join("missing/media.mp4");
    let root = temp.path().join("search");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("entry.mp4"), b"media").unwrap();
    let cache = temp.path().join("app/Thumbnails");
    fs::create_dir(&cache).unwrap();
    for size in [900, 500, 250, 100] {
        fs::write(
            cache.join(format!(
                "{}-{size}.png",
                paths::file_path_hash(original.to_str().unwrap())
            )),
            b"stale",
        )
        .unwrap();
    }
    let error = files::dispatch(
        &library,
        &native,
        request(
            "LOCATE_MEDIA_FILE",
            json!({"filePath":original,"folders":[root]}),
            &context,
        ),
        "main",
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "files_limit");
    assert_eq!(fs::read_dir(cache).unwrap().count(), 0);
    assert!(!temp.path().join("app/cache_sync.json").exists());
    native.shutdown().await.unwrap();
}

#[tokio::test]
async fn old_lookup_cannot_apply_queued_cache_effects_after_library_selection() {
    let (temp, library, native) = service();
    let context = library.context().unwrap();
    let target = temp.path().join("search");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("media.mp4"), b"media").unwrap();
    let destination = temp.path().join("next-library");
    fs::create_dir(&destination).unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, paused) = std::sync::mpsc::channel();
    let owner = library.clone();
    let frozen = context.clone();
    let blocking = tokio::spawn(async move {
        owner
            .commit(
                request("TEST_HOLD", Value::Null, &frozen),
                "main",
                move |_| {
                    let _ = entered.send(());
                    let _ = paused.recv();
                    Ok(vec![])
                },
            )
            .await
    });
    timeout(Duration::from_secs(3), ready)
        .await
        .unwrap()
        .unwrap();
    let owner = library.clone();
    let frozen = context.clone();
    let switching = tokio::spawn(async move {
        owner
            .dispatch(
                request(
                    "UPDATE_DATA_PATH",
                    json!({"oldPath":frozen.data_path,"newPath":destination}),
                    &frozen,
                ),
                "main",
            )
            .await
    });
    timeout(Duration::from_secs(3), async {
        while library.status().unwrap().queued_jobs != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let owner = library.clone();
    let worker = native.clone();
    let payload = request(
        "LOCATE_MEDIA_FILE",
        json!({"filePath":temp.path().join("missing/media.mp4"),"folders":[target]}),
        &context,
    );
    let lookup =
        tokio::spawn(async move { files::dispatch(&owner, &worker, payload, "main").await });
    timeout(Duration::from_secs(3), async {
        while library.status().unwrap().queued_jobs != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!lookup.is_finished());
    release.send(()).unwrap();
    blocking.await.unwrap().unwrap();
    switching.await.unwrap().unwrap();
    let error = timeout(Duration::from_secs(3), lookup)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "stale_library");
    assert!(!temp.path().join("app/cache_sync.json").exists());
    assert_eq!(
        library.context().unwrap().library_generation,
        context.library_generation + 1
    );
    native.shutdown().await.unwrap();
}
