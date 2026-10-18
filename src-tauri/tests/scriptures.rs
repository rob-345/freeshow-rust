#[path = "../src/scriptures.rs"]
mod scriptures;
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn request(data: Value, context: &RuntimeContext) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"BIBLE","data":data},"context":context,"listenerId":"scripture-listener"})).unwrap()
}
struct Fixture {
    dir: tempfile::TempDir,
    library: LibraryService,
    native: scriptures::NativeScriptures,
    published: Arc<Mutex<Vec<Event>>>,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(dir.path().join("app"), dir.path().join("data")).unwrap(),
        )
        .unwrap();
        let published = Arc::new(Mutex::new(vec![]));
        let send = published.clone();
        let native = scriptures::NativeScriptures::new(
            library.clone(),
            dir.path().join("Documents"),
            Arc::new(move |event| {
                send.lock().unwrap().push(event);
                Ok(())
            }),
        )
        .unwrap();
        Self {
            dir,
            library,
            native,
            published,
        }
    }
    async fn close(&self) {
        self.library.finish_writes().await.unwrap();
        self.native.shutdown().await.unwrap();
        assert_eq!(self.native.status()["requests"], 0);
    }
}
#[tokio::test]
async fn admission_context_input_limits_and_terminal_shutdown_are_explicit() {
    let f = Fixture::new();
    assert_eq!(scriptures::COMMANDS, ["BIBLE"]);
    let old = f.library.context().unwrap();
    let mut wrong = request(Value::Null, &old);
    wrong.channel = "AUDIO".into();
    assert_eq!(
        f.native.dispatch(wrong).await.unwrap_err().code,
        "unsupported_command"
    );
    assert_eq!(
        f.native
            .dispatch(request(json!({"name":"a".repeat(65536)}), &old))
            .await
            .unwrap_err()
            .code,
        "scripture_input_limit"
    );
    let mut long_id = request(Value::Null, &old);
    long_id.listener_id = Some("a".repeat(257));
    assert_eq!(
        f.native.dispatch(long_id).await.unwrap_err().code,
        "scripture_input_limit"
    );
    let other = f.dir.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let update = serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":old.data_path,"newPath":other}},"context":old})).unwrap();
    f.library.dispatch(update, "main").await.unwrap();
    f.native.context_changed(&f.library.context().unwrap());
    assert_eq!(
        f.native
            .dispatch(request(json!({"name":"missing"}), &old))
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    f.native.shutdown().await.unwrap();
    assert_eq!(
        f.native
            .dispatch(request(
                json!({"name":"missing"}),
                &f.library.context().unwrap()
            ))
            .await
            .unwrap_err()
            .code,
        "scriptures_shutdown"
    );
    assert!(f.published.lock().unwrap().is_empty());
    f.close().await;
}

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
mod files {
    use super::*;
    use std::{fs, time::Duration};
    use tokio::{sync::oneshot, time::timeout};

    async fn block_owner(
        f: &Fixture,
    ) -> (std::sync::mpsc::SyncSender<()>, tokio::task::JoinHandle<()>) {
        let (entered, started) = oneshot::channel();
        let (release, held) = std::sync::mpsc::sync_channel(1);
        let library = f.library.clone();
        let context = library.context().unwrap();
        let owner = tokio::spawn(async move {
            library
                .commit(request(Value::Null, &context), "main", move |_| {
                    entered.send(()).unwrap();
                    held.recv().unwrap();
                    Ok(vec![])
                })
                .await
                .unwrap();
        });
        started.await.unwrap();
        (release, owner)
    }
    fn write_bible(root: &std::path::Path, name: &str, value: &Value) {
        let parent = root.join("Bibles");
        fs::create_dir_all(&parent).unwrap();
        fs::write(
            parent.join(format!("{name}.fsb")),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
    }
    async fn wait_queued(f: &Fixture, count: usize) {
        timeout(Duration::from_secs(3), async {
            while f.library.status().unwrap().queued_jobs != count {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn primary_migration_and_missing_callbacks_run_independently_of_ordered_writes() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let original = json!(["stored", {"books":[{"chapters":[{"verses":[{"number":1.0,"value":"First 敬拜","future":[true,null]},{"number":2.0,"text":"kept","value":"old"},{"number":3.0,"value":false}]}]}],"unknown":{"retained":true}}]);
        write_bible(&context.data_path, "Native 敬拜", &original);
        write_bible(
            &f.dir.path().join("Documents"),
            "Native 敬拜",
            &json!(["legacy", {}]),
        );
        let (release, owner) = block_owner(&f).await;
        let event = timeout(Duration::from_secs(3), f.native.dispatch(request(json!({"name":"Native 敬拜","id":"requested","path":"/ignored","dataPath":"/ignored"}), &context))).await.unwrap().unwrap().remove(0);
        let mut expected = original;
        expected[0] = json!("requested");
        let verses = expected[1]["books"][0]["chapters"][0]["verses"]
            .as_array_mut()
            .unwrap();
        verses[0].as_object_mut().unwrap().remove("value");
        verses[0]["text"] = json!("First 敬拜");
        verses[2].as_object_mut().unwrap().remove("value");
        verses[2]["text"] = json!("");
        assert_eq!(event.message.channel, "BIBLE");
        assert_eq!(
            event.message.data,
            Some(json!({"id":"requested","content":expected}))
        );
        assert_eq!(event.listener_id.as_deref(), Some("scripture-listener"));
        assert_eq!(event.context, Some(context.clone()));
        let missing = timeout(
            Duration::from_secs(3),
            f.native.dispatch(request(
                json!({"name":"missing","id":{"unknown":[false,null]}}),
                &context,
            )),
        )
        .await
        .unwrap()
        .unwrap()
        .remove(0);
        assert_eq!(
            missing.message.data,
            Some(json!({"id":{"unknown":[false,null]},"error":"not_found"}))
        );
        release.send(()).unwrap();
        owner.await.unwrap();
        assert!(f.published.lock().unwrap().is_empty());
        f.close().await;
    }
    #[tokio::test]
    async fn absent_directory_is_created_by_owner_then_genuine_legacy_fallback_runs() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let root = context.data_path.join("Bibles");
        fs::remove_dir(&root).unwrap();
        let value = json!(["legacy", {"text":"Legacy content","unknown":[null,2.0]}]);
        write_bible(&f.dir.path().join("Documents"), "Legacy", &value);
        let event = f
            .native
            .dispatch(request(json!({"name":"Legacy","id":false}), &context))
            .await
            .unwrap()
            .remove(0);
        assert!(root.is_dir());
        assert_eq!(
            event.message.data,
            Some(json!({"id":false,"content":value}))
        );
        assert!(f.published.lock().unwrap().is_empty());
        f.close().await;
    }
    // Existing non-directory roots require the same mkdir attempt as absent
    // roots in the original getDataFolderPath/createFolder implementation.
    #[tokio::test]
    async fn failed_directory_attempt_alert_and_legacy_reply_preserve_source_contract() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let root = context.data_path.join("Bibles");
        fs::remove_dir(&root).unwrap();
        fs::write(&root, b"preserve obstruction").unwrap();
        let value = json!(["loaded", {"books":[]}]);
        write_bible(&f.dir.path().join("Documents"), "Legacy", &value);
        let event = f
            .native
            .dispatch(request(json!({"name":"Legacy","id":"loaded"}), &context))
            .await
            .unwrap()
            .remove(0);
        assert_eq!(
            event.message.data,
            Some(json!({"id":"loaded","content":value}))
        );
        let published = f.published.lock().unwrap().clone();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].message.channel, "ALERT");
        assert_eq!(
            published[0].message.data,
            Some(json!(format!(
                "Error: Could not create folder at: {}!",
                root.display()
            )))
        );
        assert_eq!(published[0].context.as_ref(), Some(&context));
        assert!(published[0].listener_id.is_none());
        assert_eq!(fs::read(&root).unwrap(), b"preserve obstruction");
        f.close().await;
    }
    #[tokio::test]
    async fn detached_accepted_directory_attempt_still_publishes_its_independent_alert() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let root = context.data_path.join("Bibles");
        fs::remove_dir(&root).unwrap();
        let (release, owner) = block_owner(&f).await;
        let native = f.native.clone();
        let captured = context.clone();
        let job = tokio::spawn(async move {
            native
                .dispatch(request(json!({"name":"missing","id":"dropped"}), &captured))
                .await
        });
        wait_queued(&f, 1).await;
        fs::write(&root, b"created after preparation").unwrap();
        job.abort();
        assert!(job.await.unwrap_err().is_cancelled());
        release.send(()).unwrap();
        owner.await.unwrap();
        f.library.finish_writes().await.unwrap();
        f.native.shutdown().await.unwrap();
        assert_eq!(f.published.lock().unwrap().len(), 1);
        assert_eq!(f.published.lock().unwrap()[0].message.channel, "ALERT");
        assert_eq!(f.native.status()["requests"], 0);
        assert_eq!(fs::read(&root).unwrap(), b"created after preparation");
    }
    #[tokio::test]
    async fn two_whole_leases_survive_caller_drop_and_terminal_shutdown_drains_queued_effects() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let root = context.data_path.join("Bibles");
        fs::remove_dir(&root).unwrap();
        let (release, owner) = block_owner(&f).await;
        let mut jobs = vec![];
        for _ in 0..2 {
            let native = f.native.clone();
            let captured = context.clone();
            jobs.push(tokio::spawn(async move {
                native
                    .dispatch(request(json!({"name":"missing"}), &captured))
                    .await
            }));
        }
        wait_queued(&f, 2).await;
        let dropped = jobs.remove(0);
        dropped.abort();
        assert!(dropped.await.unwrap_err().is_cancelled());
        assert_eq!(f.native.status()["requests"], 2);
        assert_eq!(
            f.native
                .dispatch(request(json!({"name":"missing"}), &context))
                .await
                .unwrap_err()
                .code,
            "scriptures_busy"
        );
        let native = f.native.clone();
        let shutdown = tokio::spawn(async move { native.shutdown().await });
        timeout(Duration::from_secs(2), async {
            while f.native.status()["accepting"] == true {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!shutdown.is_finished());
        assert!(!root.exists());
        release.send(()).unwrap();
        owner.await.unwrap();
        let error = jobs.remove(0).await.unwrap().unwrap_err();
        assert!(matches!(
            error.code,
            "scripture_cancelled" | "scripture_shutdown"
        ));
        shutdown.await.unwrap().unwrap();
        assert!(root.is_dir());
        assert_eq!(f.native.status()["requests"], 0);
        f.close().await;
    }
    #[tokio::test]
    async fn root_change_ahead_of_old_directory_intent_rejects_its_effect_and_reply() {
        let f = Fixture::new();
        let old = f.library.context().unwrap();
        let old_bibles = old.data_path.join("Bibles");
        fs::remove_dir(&old_bibles).unwrap();
        let next = f.dir.path().join("next");
        fs::create_dir(&next).unwrap();
        let (release, owner) = block_owner(&f).await;
        let library = f.library.clone();
        let update: Request = serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":old.data_path,"newPath":next}},"context":old})).unwrap();
        let change = tokio::spawn(async move { library.dispatch(update, "main").await });
        wait_queued(&f, 1).await;
        let native = f.native.clone();
        let captured = old.clone();
        let job = tokio::spawn(async move {
            native
                .dispatch(request(json!({"name":"missing"}), &captured))
                .await
        });
        wait_queued(&f, 2).await;
        release.send(()).unwrap();
        owner.await.unwrap();
        change.await.unwrap().unwrap();
        f.native.context_changed(&f.library.context().unwrap());
        assert_eq!(job.await.unwrap().unwrap_err().code, "stale_library");
        assert!(!old_bibles.exists());
        assert!(f.published.lock().unwrap().is_empty());
        f.close().await;
    }
}
