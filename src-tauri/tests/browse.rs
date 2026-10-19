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
    if cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    )) {
        let events = reply.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message.channel, "FILE_INFO");
        assert_eq!(events[0].message.data, Some(Value::Null));
        assert_eq!(events[0].listener_id.as_deref(), Some("native-browse"));
        assert_eq!(events[0].context.as_ref(), Some(&context));
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

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod queued {
    use super::*;
    use freeshow_core::{protocol::Event, WireError};
    use std::{fs, time::Duration};
    use tokio::{task::JoinHandle, time::timeout};
    type Job = JoinHandle<Result<Vec<Event>, WireError>>;
    type Gates = Vec<std::sync::mpsc::SyncSender<()>>;

    struct Fixture {
        temp: tempfile::TempDir,
        library: LibraryService,
        service: BrowseService,
        native: browse::NativeBrowse,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let library = LibraryService::start(
                Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
            )
            .unwrap();
            let service = BrowseService::new(BrowseConfig::default()).unwrap();
            let native = browse::NativeBrowse::new(service.clone());
            fs::write(temp.path().join("source.wav"), b"actual native bytes").unwrap();
            Self {
                temp,
                library,
                service,
                native,
            }
        }
        fn spawn(&self, index: usize) -> Job {
            let native = self.native.clone();
            let library = self.library.clone();
            let mut captured = request(
                "FILE_INFO",
                json!(self.temp.path().join("source.wav")),
                &library.context().unwrap(),
            );
            captured.listener_id = Some(format!("queued-{index}"));
            tokio::spawn(async move { native.dispatch(&library, captured, "main").await })
        }
    }
    async fn waiting(service: &BrowseService, count: usize) {
        timeout(Duration::from_secs(5), async {
            while service.queued_waiters_for_test() != count {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("actual bounded waiting slots never reached expected count");
    }
    async fn occupied(f: &Fixture, queued: usize) -> (Vec<Job>, Gates) {
        let (mut entered, releases) = f.service.hold_workers_for_test().unwrap();
        let mut jobs = vec![f.spawn(0), f.spawn(1)];
        let mut entries = Vec::new();
        for _ in 0..2 {
            entries.push(
                timeout(Duration::from_secs(5), entered.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        entries.sort();
        assert_eq!(entries, [0, 1], "two actual filesystem owners are held");
        for index in 2..queued + 2 {
            jobs.push(f.spawn(index));
        }
        waiting(&f.service, queued).await;
        assert!(jobs.iter().all(|job| !job.is_finished()));
        (jobs, releases)
    }
    fn reply(events: &[Event], index: usize, context: &RuntimeContext) {
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].channel, "MAIN");
        assert_eq!(events[0].message.channel, "FILE_INFO");
        let listener = format!("queued-{index}");
        assert_eq!(events[0].listener_id.as_deref(), Some(listener.as_str()));
        assert_eq!(events[0].context.as_ref(), Some(context));
        let data = events[0].message.data.as_ref().unwrap();
        assert_eq!(data["extension"], "wav");
        assert_eq!(data["stat"]["size"].as_f64(), Some(19.0));
        assert_eq!(data["stat"].as_object().unwrap().len(), 14);
    }

    #[tokio::test]
    async fn two_entered_plus_eight_waiting_keep_each_original_reply_and_a_finite_host_bound() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let (jobs, releases) = occupied(&f, 8).await;
        assert_eq!(f.spawn(10).await.unwrap().unwrap_err().code, "browse_busy");
        assert_eq!(f.service.queued_waiters_for_test(), 8);
        for release in releases {
            release.send(()).unwrap();
        }
        for (index, job) in jobs.into_iter().enumerate() {
            let events = timeout(Duration::from_secs(5), job)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            reply(&events, index, &context);
        }
        waiting(&f.service, 0).await;
        timeout(Duration::from_secs(5), f.native.shutdown())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn actual_library_retirement_cancels_queued_replies_but_new_context_waits_for_real_workers(
    ) {
        let f = Fixture::new();
        let (jobs, releases) = occupied(&f, 3).await;
        let old = f.library.context().unwrap();
        let next = f.temp.path().join("other-library");
        fs::create_dir(&next).unwrap();
        f.library
            .dispatch(
                request(
                    "UPDATE_DATA_PATH",
                    json!({"oldPath":old.data_path,"newPath":next}),
                    &old,
                ),
                "main",
            )
            .await
            .unwrap();
        let current = f.library.context().unwrap();
        assert_ne!(old, current);
        f.native.context_changed(&current);
        for job in jobs {
            assert_eq!(
                timeout(Duration::from_secs(5), job)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap_err()
                    .code,
                "stale_library"
            );
        }
        waiting(&f.service, 0).await;
        let fresh = f.spawn(100);
        waiting(&f.service, 1).await;
        assert!(
            !fresh.is_finished(),
            "cancelled old observers do not release actual entered workers"
        );
        for release in releases {
            release.send(()).unwrap();
        }
        let events = timeout(Duration::from_secs(5), fresh)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        reply(&events, 100, &current);
        timeout(Duration::from_secs(5), f.native.shutdown())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn queued_caller_abort_recovers_capacity_and_shutdown_still_waits_for_physical_return() {
        let f = Fixture::new();
        let (mut jobs, releases) = occupied(&f, 8).await;
        let abandoned = jobs.pop().unwrap();
        abandoned.abort();
        assert!(abandoned.await.unwrap_err().is_cancelled());
        waiting(&f.service, 7).await;
        jobs.push(f.spawn(99));
        waiting(&f.service, 8).await;
        assert_eq!(f.spawn(100).await.unwrap().unwrap_err().code, "browse_busy");
        {
            let shutdown = f.native.shutdown();
            tokio::pin!(shutdown);
            assert!(timeout(Duration::from_millis(25), shutdown.as_mut())
                .await
                .is_err());
            for job in jobs {
                assert_eq!(
                    timeout(Duration::from_secs(5), job)
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap_err()
                        .code,
                    "cancelled"
                );
            }
            waiting(&f.service, 0).await;
            assert!(
                timeout(Duration::from_millis(25), shutdown.as_mut())
                    .await
                    .is_err(),
                "cancelled observers are not native thread joins"
            );
        }
        assert_eq!(
            f.spawn(101).await.unwrap().unwrap_err().code,
            "browse_shutdown"
        );
        for release in releases {
            release.send(()).unwrap();
        }
        timeout(Duration::from_secs(5), f.native.shutdown())
            .await
            .unwrap()
            .unwrap();
        f.native.shutdown().await.unwrap();
    }
}
