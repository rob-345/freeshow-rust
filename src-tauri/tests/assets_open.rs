#[path = "../src/assets.rs"]
#[allow(dead_code)]
mod assets;

use freeshow_assets::Control;
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
};

fn request(command: &str, data: Value, current: &RuntimeContext) -> Request {
    serde_json::from_value(
        json!({"channel":"MAIN","data":{"channel":command,"data":data},"context":current}),
    )
    .unwrap()
}
async fn fixture() -> (tempfile::TempDir, LibraryService, assets::NativeAssets) {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = Runtime::new(
        temporary.path().join("app"),
        temporary.path().join("library"),
    )
    .unwrap();
    let library = LibraryService::start(runtime).unwrap();
    let native =
        assets::NativeAssets::start(&library.context().unwrap(), &temporary.path().join("app"))
            .await
            .unwrap();
    (temporary, library, native)
}

#[tokio::test]
async fn now_playing_renderer_text_cannot_create_native_file_authority() {
    let (temporary, library, native) = fixture().await;
    let current = library.context().unwrap();
    let path = temporary.path().join("unregistered 日本語 literal%.wav");
    fs::write(&path, b"actual registered media").unwrap();
    let key = path.to_str().unwrap();
    assert!(native
        .begin(
            &library,
            &request("NOW_PLAYING", json!({"filePath":path}), &current)
        )
        .unwrap()
        .is_none());
    assert_eq!(
        native
            .open_registered_media(&library, &current, key, Control::default())
            .await
            .unwrap_err()
            .code,
        "assets_unregistered"
    );
    assert_eq!(native.status()["grants"], 0);
    let grants = native
        .import_grants(&library, &current, std::slice::from_ref(&path))
        .await
        .unwrap();
    assert!(grants
        .as_array()
        .unwrap()
        .iter()
        .any(|grant| grant["sourcePath"] == key));
    let read = native
        .open_registered_media(&library, &current, key, Control::default())
        .await
        .unwrap();
    assert_eq!(read.authority().source_path(), key);
    assert_eq!(
        read.authority().context(),
        &assets::context(&current).unwrap()
    );
    let (mut file, authority) = read.into_parts();
    let before = authority.revalidate(Control::default()).await.unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"actual registered media");
    file.seek(SeekFrom::Start(0)).unwrap();
    let after = authority.revalidate(Control::default()).await.unwrap();
    assert!(before.same_version(&after));
    for key in [
        grants[0]["url"].as_str().unwrap(),
        "file:///unregistered.wav",
        "https://example.invalid/audio.wav",
    ] {
        assert_eq!(
            native
                .open_registered_media(&library, &current, key, Control::default())
                .await
                .unwrap_err()
                .code,
            "assets_unregistered"
        );
    }
    drop((file, authority, before, after));
    native.shutdown().await.unwrap();
    assert_eq!(native.status()["fileWorkersJoined"], true);
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn full_library_context_retires_existing_authority_and_new_context_requires_a_fresh_trusted_grant(
) {
    let (temporary, library, native) = fixture().await;
    let current = library.context().unwrap();
    let path = temporary.path().join("source.wav");
    fs::write(&path, b"native bytes").unwrap();
    let key = path.to_str().unwrap();
    native
        .import_grants(&library, &current, std::slice::from_ref(&path))
        .await
        .unwrap();
    let read = native
        .open_registered_media(&library, &current, key, Control::default())
        .await
        .unwrap();
    for foreign in [
        RuntimeContext {
            runtime_id: "foreign".into(),
            ..current.clone()
        },
        RuntimeContext {
            data_path: temporary.path().join("foreign"),
            ..current.clone()
        },
        RuntimeContext {
            library_generation: current.library_generation + 1,
            ..current.clone()
        },
    ] {
        assert_eq!(
            native
                .open_registered_media(&library, &foreign, key, Control::default())
                .await
                .unwrap_err()
                .code,
            "stale_library"
        );
    }
    let next_path = temporary.path().join("next");
    fs::create_dir(&next_path).unwrap();
    library
        .dispatch(
            request(
                "UPDATE_DATA_PATH",
                json!({"oldPath":current.data_path,"newPath":next_path}),
                &current,
            ),
            "main",
        )
        .await
        .unwrap();
    let next = library.context().unwrap();
    native.context_changed(&next).unwrap();
    assert_eq!(
        read.authority()
            .revalidate(Control::default())
            .await
            .unwrap_err()
            .code,
        "assets_stale_context"
    );
    assert_eq!(
        native
            .open_registered_media(&library, &current, key, Control::default())
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    assert_eq!(
        native
            .open_registered_media(&library, &next, key, Control::default())
            .await
            .unwrap_err()
            .code,
        "assets_unregistered"
    );
    native
        .import_grants(&library, &next, std::slice::from_ref(&path))
        .await
        .unwrap();
    let next_read = native
        .open_registered_media(&library, &next, key, Control::default())
        .await
        .unwrap();
    assert_eq!(
        next_read.authority().context(),
        &assets::context(&next).unwrap()
    );
    drop((read, next_read));
    native.shutdown().await.unwrap();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn protected_cache_selection_and_canceled_native_requests_never_create_a_descriptor() {
    let (temporary, library, native) = fixture().await;
    let current = library.context().unwrap();
    let protected = temporary.path().join("app/protected-cache");
    fs::create_dir(&protected).unwrap();
    let path = protected.join("secret.wav");
    fs::write(&path, b"protected ciphertext").unwrap();
    let grants = native
        .import_grants(&library, &current, std::slice::from_ref(&path))
        .await
        .unwrap();
    assert_eq!(grants, json!([]));
    assert_eq!(
        native
            .open_registered_media(
                &library,
                &current,
                path.to_str().unwrap(),
                Control::default()
            )
            .await
            .unwrap_err()
            .code,
        "assets_unregistered"
    );
    let allowed = temporary.path().join("allowed.wav");
    fs::write(&allowed, b"plain media").unwrap();
    native
        .import_grants(&library, &current, std::slice::from_ref(&allowed))
        .await
        .unwrap();
    let control = Control::default();
    control.cancel.cancel();
    assert_eq!(
        native
            .open_registered_media(&library, &current, allowed.to_str().unwrap(), control)
            .await
            .unwrap_err()
            .code,
        "assets_cancelled"
    );
    assert_eq!(native.status()["nativeReads"], 0);
    native.shutdown().await.unwrap();
    assert_eq!(native.status()["fileWorkersJoined"], true);
    library.finish_writes().await.unwrap();
}
