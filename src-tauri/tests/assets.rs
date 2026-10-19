#[path = "../src/assets.rs"]
// Thumbnail grants are exercised by the separate native thumbnail host tests.
#[allow(dead_code)]
mod assets;

use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

fn request(command: &str, data: Value, current: &RuntimeContext) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":command,"data":data},"context":current,"listenerId":"native-assets"})).unwrap()
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
async fn get(url: &str) -> Vec<u8> {
    let (authority, path) = url
        .strip_prefix("http://")
        .unwrap()
        .split_once('/')
        .unwrap();
    let mut socket = TcpStream::connect(authority).await.unwrap();
    socket.write_all(format!("GET /{path} HTTP/1.1\r\nHost: {authority}\r\nOrigin: tauri://localhost\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), socket.read_to_end(&mut reply))
        .await
        .unwrap()
        .unwrap();
    let boundary = reply
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap();
    assert!(reply.starts_with(b"HTTP/1.1 200"));
    assert!(String::from_utf8_lossy(&reply[..boundary])
        .to_lowercase()
        .contains("access-control-allow-origin: tauri://localhost"));
    reply[boundary + 4..].to_vec()
}
fn media_file(base: &std::path::Path) -> PathBuf {
    let path = base.join("日本語 literal% media.png");
    fs::write(&path, b"actual source bytes").unwrap();
    path
}

#[tokio::test]
async fn committed_show_grants_only_typed_media_and_preserves_full_source_payload() {
    let (temporary, library, native) = fixture().await;
    let current = library.context().unwrap();
    let path = media_file(temporary.path());
    let private = temporary.path().join("private.json");
    fs::write(&private, b"unreferenced private bytes").unwrap();
    let data = json!({"id":"show","content":["show",{"media":{"m":{"path":path}},"slides":{"s":{"items":[{"type":"text","src":private}]}},"unknown":{"path":private}}]});
    let lease = native
        .begin(&library, &request("SHOW", json!("show"), &current))
        .unwrap()
        .unwrap();
    assert_eq!(native.status()["grants"], 0);
    let mut event = Event::reply("MAIN", "SHOW", data.clone(), Some("native-assets".into()));
    event.context = Some(current.clone());
    let mut events = vec![event];
    native.complete(&library, lease, &mut events).await.unwrap();
    assert_eq!(events[0].message.data, Some(data));
    assert_eq!(events[0].listener_id.as_deref(), Some("native-assets"));
    let grants = events[0]
        .native_assets
        .as_ref()
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0]["sourcePath"], json!(path));
    assert_eq!(
        get(grants[0]["url"].as_str().unwrap()).await,
        b"actual source bytes"
    );
    assert_eq!(native.replay(&current).unwrap(), json!(grants));
    let (revision, replay) = native.replay_since(&current, None).unwrap();
    assert_eq!(replay.unwrap(), json!(grants));
    assert!(native
        .replay_since(&current, Some(&revision))
        .unwrap()
        .1
        .is_none());
    native.shutdown().await.unwrap();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn save_publication_follows_successful_commit_and_does_not_block_saving_at_asset_capacity() {
    let (temporary, library, native) = fixture().await;
    let current = library.context().unwrap();
    let path = media_file(temporary.path());
    let input = request(
        "SAVE",
        json!({"showsCache":{"show":{"name":"Asset save","media":{"m":{"path":path}}}}}),
        &current,
    );
    let lease = native.begin(&library, &input).unwrap().unwrap();
    assert_eq!(native.status()["grants"], 0);
    let mut events = library.dispatch(input, "main").await.unwrap();
    native.complete(&library, lease, &mut events).await.unwrap();
    let saved = events
        .iter()
        .find(|event| event.message.channel == "SAVE2")
        .unwrap();
    assert_eq!(
        saved
            .native_assets
            .as_ref()
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let slots = (0..4)
        .map(|_| {
            native
                .begin(&library, &request("SHOW", json!("show"), &current))
                .unwrap()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        native
            .begin(&library, &request("SHOW", json!("show"), &current))
            .err()
            .unwrap()
            .code,
        "assets_busy"
    );
    let input = request(
        "SAVE",
        json!({"PROJECTS":{"savedAtCapacity":true}}),
        &current,
    );
    assert!(native.begin(&library, &input).unwrap().is_none());
    assert!(library
        .dispatch(input, "main")
        .await
        .unwrap()
        .iter()
        .any(|event| event.message.channel == "SAVE2"));
    drop(slots);
    native.shutdown().await.unwrap();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn native_drop_and_listing_use_accepted_files_and_file_info_never_authorizes_request_paths() {
    let (temporary, library, native) = fixture().await;
    let current = library.context().unwrap();
    let path = temporary.path().join("日本語 literal%20 media.png");
    fs::write(&path, b"actual source bytes").unwrap();
    let grants = native
        .drop_grants(
            &library,
            &current,
            &json!([{"path":path,"directory":false},{"path":temporary.path(),"directory":true}]),
        )
        .await
        .unwrap();
    assert_eq!(grants.as_array().unwrap().len(), 1);
    assert_eq!(
        get(grants[0]["url"].as_str().unwrap()).await,
        b"actual source bytes"
    );
    let another = temporary.path().join("accepted.wav");
    fs::write(&another, b"accepted listing bytes").unwrap();
    assert!(native
        .begin(&library, &request("FILE_INFO", json!(another), &current))
        .unwrap()
        .is_none());
    assert_eq!(
        native.replay(&current).unwrap().as_array().unwrap().len(),
        1
    );
    let lease = native
        .begin(
            &library,
            &request("READ_FOLDER", json!({"path":temporary.path()}), &current),
        )
        .unwrap()
        .unwrap();
    let mut event = Event::reply(
        "MAIN",
        "READ_FOLDER",
        json!({"folder":{"isFolder":true,"path":temporary.path(),"files":["not-authorized"]},"file":{"isFolder":false,"path":another,"thumbnailPath":""}}),
        None,
    );
    event.context = Some(current.clone());
    let mut events = vec![event];
    native.complete(&library, lease, &mut events).await.unwrap();
    assert_eq!(
        events[0]
            .native_assets
            .as_ref()
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        native.replay(&current).unwrap().as_array().unwrap().len(),
        2
    );
    let imported = temporary.path().join("committed import.png");
    fs::write(&imported, b"final imported bytes").unwrap();
    let published = native
        .import_grants(&library, &current, std::slice::from_ref(&imported))
        .await
        .unwrap();
    assert!(!published.as_array().unwrap().is_empty());
    for grant in published.as_array().unwrap() {
        assert_eq!(
            get(grant["url"].as_str().unwrap()).await,
            b"final imported bytes"
        );
    }
    native.shutdown().await.unwrap();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn publication_is_independent_of_the_library_owner_and_context_switch_revokes_all_capabilities(
) {
    let (temporary, library, native) = fixture().await;
    let current = library.context().unwrap();
    let path = media_file(temporary.path());
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, held) = std::sync::mpsc::sync_channel(1);
    let owner = {
        let library = library.clone();
        let current = current.clone();
        tokio::spawn(async move {
            library
                .commit(
                    request("TEST_HOLD", Value::Null, &current),
                    "main",
                    move |_| {
                        let _ = entered.send(());
                        let _ = held.recv();
                        Ok(vec![])
                    },
                )
                .await
        })
    };
    started.await.unwrap();
    let grants = tokio::time::timeout(
        Duration::from_secs(2),
        native.drop_grants(
            &library,
            &current,
            &json!([{"path":path,"directory":false}]),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        get(grants[0]["url"].as_str().unwrap()).await,
        b"actual source bytes"
    );
    release.send(()).unwrap();
    owner.await.unwrap().unwrap();
    let stale = native
        .begin(&library, &request("SHOW", json!("show"), &current))
        .unwrap()
        .unwrap();
    fs::create_dir(temporary.path().join("other")).unwrap();
    library
        .dispatch(
            request(
                "UPDATE_DATA_PATH",
                json!({"oldPath":current.data_path,"newPath":temporary.path().join("other")}),
                &current,
            ),
            "main",
        )
        .await
        .unwrap();
    let next = library.context().unwrap();
    assert_eq!(next.library_generation, current.library_generation + 1);
    native.context_changed(&next).unwrap();
    assert!(native.replay(&next).unwrap().as_array().unwrap().is_empty());
    let mut event = Event::reply(
        "MAIN",
        "SHOW",
        json!({"id":"show","content":["show",{"media":{"m":{"path":path}}}]}),
        None,
    );
    event.context = Some(current.clone());
    assert_eq!(
        native
            .complete(&library, stale, &mut [event])
            .await
            .err()
            .unwrap()
            .code,
        "stale_library"
    );
    assert!(native.replay(&next).unwrap().as_array().unwrap().is_empty());
    native.shutdown().await.unwrap();
    assert_eq!(
        native
            .begin(&library, &request("SHOW", json!("show"), &next))
            .err()
            .unwrap()
            .code,
        "assets_shutdown"
    );
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn backup_only_snapshots_never_authorize_uncommitted_shows_and_reset_saves_do_publish() {
    let (temporary, library, native) = fixture().await;
    let current = library.context().unwrap();
    let path = media_file(temporary.path());
    let mut snapshot = json!({"showsCache":{"show":{"name":"Reset media","media":{"m":{"path":path}}}},"customTriggers":{"backup":true,"isAutoBackup":true}});
    let backup = request("SAVE", snapshot.clone(), &current);
    assert!(native.begin(&library, &backup).unwrap().is_none());
    library.dispatch(backup, "main").await.unwrap();
    assert_eq!(native.status()["grants"], 0);
    snapshot["customTriggers"] = json!({"reset":true});
    let reset = request("SAVE", snapshot, &current);
    let lease = native.begin(&library, &reset).unwrap().unwrap();
    let mut events = library.dispatch(reset, "main").await.unwrap();
    assert!(!events.iter().any(|event| event.message.channel == "SAVE2"));
    native.complete(&library, lease, &mut events).await.unwrap();
    assert_eq!(
        events[0]
            .native_assets
            .as_ref()
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    native.shutdown().await.unwrap();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn stale_save_reaches_library_owner_recovery_without_authorizing_any_media() {
    let (temporary, library, native) = fixture().await;
    let previous = library.context().unwrap();
    let path = media_file(temporary.path());
    let next_root = temporary.path().join("next");
    fs::create_dir(&next_root).unwrap();
    library
        .dispatch(
            request(
                "UPDATE_DATA_PATH",
                json!({"oldPath":previous.data_path,"newPath":next_root}),
                &previous,
            ),
            "main",
        )
        .await
        .unwrap();
    native.context_changed(&library.context().unwrap()).unwrap();
    let stale = request(
        "SAVE",
        json!({"runtimeContext":previous,"PROJECTS":{"rejected":true},"showsCache":{"old":{"name":"Recovery media","media":{"m":{"path":path}}}}}),
        &previous,
    );
    assert!(native.begin(&library, &stale).unwrap().is_none());
    let expected = json!(stale);
    let error = library.dispatch(stale, "main").await.unwrap_err();
    let freeshow_core::Error::StaleLibrary {
        recovery_path: Some(recovery),
        recovery_error: None,
    } = error
    else {
        panic!("stale save must retain a recovery file: {error:?}");
    };
    assert_eq!(
        recovery.parent().unwrap(),
        temporary.path().join("app/Recovery")
    );
    let saved: Value = serde_json::from_slice(&fs::read(recovery).unwrap()).unwrap();
    assert_eq!(saved["request"], expected);
    assert!(!next_root.join("Shows/Recovery media.show").exists());
    assert_eq!(native.status()["grants"], 0);
    native.shutdown().await.unwrap();
    library.finish_writes().await.unwrap();
}
