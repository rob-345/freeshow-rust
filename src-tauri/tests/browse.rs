#[path = "../src/browse.rs"]
// The cross-adapter capture entrypoint is exercised by tests/thumbnails.rs.
// Production is checked without this test-module-only allowance.
#[allow(dead_code)]
mod browse;

use freeshow_browse::{BrowseConfig, BrowseService};
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use serde_json::{json, Value};
#[cfg(target_os = "linux")]
use std::{fs, time::Duration};
#[cfg(target_os = "linux")]
use tokio::{sync::oneshot, time::timeout};

fn request(command: &str, data: Value, context: &RuntimeContext) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":command,"data":data},"context":context,"listenerId":"native-browse"})).unwrap()
}

#[tokio::test]
async fn role_context_platform_limits_and_terminal_shutdown_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    let context = library.context().unwrap();
    let native = browse::NativeBrowse::new(BrowseService::new(BrowseConfig::default()).unwrap());
    assert_eq!(
        browse::COMMANDS,
        &["READ_FOLDER", "FILE_INFO", "GET_SIMILAR"]
    );
    let mut stale = context.clone();
    stale.library_generation += 1;
    assert_eq!(
        native
            .dispatch(
                &library,
                request("FILE_INFO", json!("missing"), &stale),
                "main"
            )
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    assert_eq!(
        native
            .dispatch(
                &library,
                request("FILE_INFO", json!("missing"), &context),
                "output-1"
            )
            .await
            .unwrap_err()
            .code,
        "browse_role"
    );
    let mut missing = request("FILE_INFO", json!("missing"), &context);
    missing.context = None;
    assert_eq!(
        native
            .dispatch(&library, missing, "main")
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    assert_eq!(
        native
            .dispatch(
                &library,
                request("FILE_INFO", json!("x".repeat(32769)), &context),
                "main"
            )
            .await
            .unwrap_err()
            .code,
        "browse_resource_limit"
    );
    let reply = native
        .dispatch(
            &library,
            request("FILE_INFO", json!(temp.path().join("missing")), &context),
            "main",
        )
        .await;
    if cfg!(target_os = "linux") {
        assert_eq!(reply.unwrap()[0].message.data, Some(Value::Null));
    } else {
        assert_eq!(reply.unwrap_err().code, "unsupported_platform");
    }
    let mut unchanged = context.clone();
    native.context_changed(&unchanged);
    unchanged.library_generation += 1;
    native.context_changed(&unchanged);
    native.shutdown().await.unwrap();
    native.shutdown().await.unwrap();
    assert_eq!(
        native
            .dispatch(
                &library,
                request("FILE_INFO", json!("missing"), &context),
                "main"
            )
            .await
            .unwrap_err()
            .code,
        "browse_shutdown"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn real_file_info_folder_and_similarity_replies_do_not_wait_for_the_library_owner() {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    let context = library.context().unwrap();
    let native = browse::NativeBrowse::new(BrowseService::new(BrowseConfig::default()).unwrap());
    let folder = temp.path().join("Media 敬拜");
    fs::create_dir(&folder).unwrap();
    let file = folder.join("worship.png");
    fs::write(&file, b"native bytes").unwrap();
    let similar = folder.join("worship-copy.png");
    fs::write(&similar, b"other bytes").unwrap();
    let subfolder = folder.join("Nested");
    fs::create_dir(&subfolder).unwrap();
    fs::write(subfolder.join("child.txt"), b"child").unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, paused) = std::sync::mpsc::channel();
    let ordered = library.clone();
    let frozen = context.clone();
    let owner = tokio::spawn(async move {
        ordered
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
    timeout(Duration::from_secs(5), ready)
        .await
        .unwrap()
        .unwrap();
    for (command, data) in [
        ("FILE_INFO", json!(file)),
        ("READ_FOLDER", json!({"path":folder,"depth":0})),
        ("GET_SIMILAR", json!({"paths":[file]})),
    ] {
        let events = timeout(
            Duration::from_secs(3),
            native.dispatch(&library, request(command, data, &context), "main"),
        )
        .await
        .expect("filesystem work waited for library IO")
        .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].context.as_ref(), Some(&context));
        assert_eq!(events[0].listener_id.as_deref(), Some("native-browse"));
        assert_eq!(events[0].message.channel, command);
        let reply = events[0].message.data.as_ref().unwrap();
        match command {
            "FILE_INFO" => {
                assert_eq!(reply["stat"]["size"].as_f64(), Some(12.0));
                assert_eq!(reply["stat"].as_object().unwrap().len(), 14);
                assert_eq!(reply["extension"], "png");
            }
            "READ_FOLDER" => {
                assert_eq!(reply[file.to_str().unwrap()]["isFolder"], false);
                assert_eq!(reply[subfolder.to_str().unwrap()]["isFolder"], true);
                assert!(reply[subfolder.to_str().unwrap()]
                    .as_object()
                    .unwrap()
                    .contains_key("noMedia"));
            }
            "GET_SIMILAR" => {
                assert!(reply
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["path"] == json!(similar)));
            }
            _ => unreachable!(),
        }
        assert!(!owner.is_finished());
    }
    release.send(()).unwrap();
    owner.await.unwrap().unwrap();
    native.shutdown().await.unwrap();
}
#[path = "../src/assets.rs"]
#[allow(dead_code)]
mod assets;
#[path = "../src/thumbnails.rs"]
#[allow(dead_code)]
mod thumbnails;
