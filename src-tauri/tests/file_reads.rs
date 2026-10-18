#[path = "../src/file_reads.rs"]
mod file_reads;
use freeshow_core::protocol::Event;
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn request(command: &str, data: Value, context: &RuntimeContext) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":command,"data":data},"context":context,"listenerId":"file-read-test"})).unwrap()
}
struct Fixture {
    dir: tempfile::TempDir,
    library: LibraryService,
    native: file_reads::NativeFileReads,
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
        Self {
            dir,
            library,
            native: file_reads::NativeFileReads::new(Arc::new(move |event| {
                send.lock().unwrap().push(event);
                Ok(())
            }))
            .unwrap(),
            published,
        }
    }
    async fn close(&self) {
        self.library.finish_writes().await.unwrap();
        self.native.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn file_read_admission_context_and_terminal_shutdown_are_explicit_on_every_platform() {
    let f = Fixture::new();
    assert!(f.dir.path().is_dir());
    assert_eq!(
        file_reads::COMMANDS,
        ["READ_FILE", "READ_BIBLES_FOLDER", "READ_EXIF"]
    );
    let old = f.library.context().unwrap();
    let other = f.dir.path().join("other");
    std::fs::create_dir(&other).unwrap();
    f.library
        .dispatch(
            request(
                "UPDATE_DATA_PATH",
                json!({"oldPath":old.data_path,"newPath":other}),
                &old,
            ),
            "main",
        )
        .await
        .unwrap();
    f.native.context_changed(&f.library.context().unwrap());
    let stale = f
        .native
        .dispatch(
            &f.library,
            request("READ_FILE", json!({"path":"missing"}), &old),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code, "stale_library");
    f.native.shutdown().await.unwrap();
    let closed = f
        .native
        .dispatch(
            &f.library,
            request(
                "READ_EXIF",
                json!({"id":"missing.jpg"}),
                &f.library.context().unwrap(),
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(closed.code, "file_reads_shutdown");
    assert_eq!(f.native.status()["requests"], 0);
    assert!(f.published.lock().unwrap().is_empty());
    f.close().await;
}

#[cfg(target_os = "linux")]
mod linux {
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
                .commit(
                    request("TEST_HOLD", Value::Null, &context),
                    "main",
                    move |_| {
                        entered.send(()).unwrap();
                        held.recv().unwrap();
                        Ok(vec![])
                    },
                )
                .await
                .unwrap();
        });
        started.await.unwrap();
        (release, owner)
    }
    #[tokio::test]
    async fn actual_document_and_exif_reads_preserve_contract_and_do_not_wait_for_library_owner() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let path = f.dir.path().join("日本語 literal%20.txt");
        let bytes = b"\xef\xbb\xbfNative\0\n\xf0\x9f\x80 invalid\xff";
        fs::write(&path, bytes).unwrap();
        let image = f.dir.path().join("日本語 literal%20.jpg");
        fs::write(
            &image,
            include_bytes!("../../crates/freeshow-exif/fixtures/full-II.jpg"),
        )
        .unwrap();
        let (release, owner) = block_owner(&f).await;
        let file = timeout(
            Duration::from_secs(2),
            f.native.dispatch(
                &f.library,
                request("READ_FILE", json!({"path":path}), &context),
            ),
        )
        .await
        .unwrap()
        .unwrap()
        .remove(0);
        assert_eq!(
            file.message.data,
            Some(json!({"content":String::from_utf8_lossy(bytes)}))
        );
        assert_eq!(file.listener_id.as_deref(), Some("file-read-test"));
        assert_eq!(file.context, Some(context.clone()));
        assert!(file.native_assets.is_none());
        let exif = timeout(
            Duration::from_secs(2),
            f.native.dispatch(
                &f.library,
                request("READ_EXIF", json!({"id":image}), &context),
            ),
        )
        .await
        .unwrap()
        .unwrap()
        .remove(0);
        let data = exif.message.data.as_ref().unwrap();
        assert_eq!(data["id"], json!(image));
        assert_eq!(data["exif"]["image"]["Make"], "Écamera\0");
        assert_eq!(
            data["exif"]["exif"]["DateTimeOriginal"],
            "2001:02:03 04:05:06"
        );
        assert_eq!(data["exif"]["exif"]["ExifVersion"], json!([48, 50, 51, 49]));
        let hydration = exif.native_exif.as_ref().unwrap();
        assert!(hydration["byteBuffers"]
            .as_array()
            .unwrap()
            .contains(&json!("/exif/exif/ExifVersion")));
        assert!(hydration["specialNumbers"]
            .as_array()
            .unwrap()
            .contains(&json!({"path":"/exif/image/YResolution/3","kind":"NegativeZero"})));
        assert_eq!(exif.context, Some(context.clone()));
        assert_eq!(exif.listener_id, file.listener_id);
        let missing = f
            .native
            .dispatch(
                &f.library,
                request(
                    "READ_EXIF",
                    json!({"id":f.dir.path().join("missing.jpg")}),
                    &context,
                ),
            )
            .await
            .unwrap()
            .remove(0);
        assert!(missing.message.data.as_ref().unwrap().get("exif").is_none());
        assert_eq!(
            missing.native_exif.as_ref().unwrap()["undefinedPaths"],
            json!(["/exif"])
        );
        assert_eq!(f.native.status()["requests"], 0);
        release.send(()).unwrap();
        owner.await.unwrap();
        f.close().await;
    }

    #[tokio::test]
    async fn bible_root_is_frozen_and_failed_creation_emits_the_exact_source_alert_then_empty_reply(
    ) {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let root = context.data_path.join("Bibles");
        fs::remove_dir(&root).unwrap();
        let replies = f
            .native
            .dispatch(
                &f.library,
                request(
                    "READ_BIBLES_FOLDER",
                    json!({"dataPath":"/ignored","path":"/ignored"}),
                    &context,
                ),
            )
            .await
            .unwrap();
        assert_eq!(replies.len(), 1);
        assert!(root.is_dir());
        assert_eq!(replies[0].message.data, Some(json!([])));
        fs::write(root.join("01.FSB"), b"not parsed").unwrap();
        fs::write(root.join("01.fsb"), b"also not parsed").unwrap();
        fs::create_dir(root.join("folder.fsb")).unwrap();
        let listed = f
            .native
            .dispatch(
                &f.library,
                request("READ_BIBLES_FOLDER", Value::Null, &context),
            )
            .await
            .unwrap()
            .remove(0);
        assert_eq!(
            listed.message.data,
            Some(json!([
                {"path":root.join("01.FSB"),"name":"01"},
                {"path":root.join("01.fsb"),"name":"01"},
                {"path":root.join("folder.fsb"),"name":"folder"}
            ]))
        );
        fs::remove_dir_all(&root).unwrap();
        fs::write(&root, b"keep original obstruction").unwrap();
        let failed = f
            .native
            .dispatch(
                &f.library,
                request("READ_BIBLES_FOLDER", Value::Null, &context),
            )
            .await
            .unwrap();
        assert_eq!(failed.len(), 1);
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
        assert!(published[0].listener_id.is_none());
        assert_eq!(published[0].context.as_ref(), Some(&context));
        assert_eq!(failed[0].message.channel, "READ_BIBLES_FOLDER");
        assert_eq!(failed[0].message.data, Some(json!([])));
        assert!(failed
            .iter()
            .all(|event| event.context.as_ref() == Some(&context)));
        assert_eq!(fs::read(&root).unwrap(), b"keep original obstruction");
        f.close().await;
    }

    #[tokio::test]
    async fn failed_bible_creation_publishes_one_source_alert_even_when_its_caller_drops() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let root = context.data_path.join("Bibles");
        fs::remove_dir(&root).unwrap();
        fs::write(&root, b"keep obstruction").unwrap();
        let (release, owner) = block_owner(&f).await;
        let native = f.native.clone();
        let library = f.library.clone();
        let captured = context.clone();
        let job = tokio::spawn(async move {
            native
                .dispatch(
                    &library,
                    request("READ_BIBLES_FOLDER", Value::Null, &captured),
                )
                .await
        });
        timeout(Duration::from_secs(2), async {
            while f.library.status().unwrap().queued_jobs != 1 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        job.abort();
        assert!(job.await.unwrap_err().is_cancelled());
        assert!(f.published.lock().unwrap().is_empty());
        release.send(()).unwrap();
        owner.await.unwrap();
        f.library.finish_writes().await.unwrap();
        f.native.shutdown().await.unwrap();
        let published = f.published.lock().unwrap();
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
        assert_eq!(fs::read(&root).unwrap(), b"keep obstruction");
        assert_eq!(f.native.status()["requests"], 0);
    }

    #[tokio::test]
    async fn queued_bible_creation_keeps_its_whole_lease_after_caller_abort_and_shutdown_drains_it()
    {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let root = context.data_path.join("Bibles");
        fs::remove_dir(&root).unwrap();
        assert!(!root.exists());
        let (release, owner) = block_owner(&f).await;
        let mut jobs = vec![];
        for _ in 0..4 {
            let native = f.native.clone();
            let library = f.library.clone();
            let context = context.clone();
            jobs.push(tokio::spawn(async move {
                native
                    .dispatch(
                        &library,
                        request("READ_BIBLES_FOLDER", Value::Null, &context),
                    )
                    .await
            }));
        }
        timeout(Duration::from_secs(2), async {
            while f.library.status().unwrap().queued_jobs != 4 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        let dropped = jobs.remove(0);
        dropped.abort();
        assert!(dropped.await.unwrap_err().is_cancelled());
        assert_eq!(f.native.status()["requests"], 4);
        let error = f
            .native
            .dispatch(
                &f.library,
                request("READ_FILE", json!({"path":"missing"}), &context),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "file_reads_busy");
        assert!(!root.exists());
        release.send(()).unwrap();
        owner.await.unwrap();
        for job in jobs {
            assert_eq!(job.await.unwrap().unwrap().len(), 1);
        }
        f.library.finish_writes().await.unwrap();
        f.native.shutdown().await.unwrap();
        assert!(root.is_dir());
        assert_eq!(f.native.status()["requests"], 0);
    }
}
