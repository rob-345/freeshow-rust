#[path = "../src/media_copy.rs"]
mod media_copy;
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
fn request(command: &str, data: Value, context: &RuntimeContext) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":command,"data":data},"context":context,"listenerId":"media-copy-listener"})).unwrap()
}
struct Fixture {
    _dir: tempfile::TempDir,
    library: LibraryService,
    native: media_copy::NativeMediaCopy,
    published: Arc<Mutex<Vec<Event>>>,
    opened: Arc<Mutex<Vec<PathBuf>>>,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(dir.path().join("app"), dir.path().join("data")).unwrap(),
        )
        .unwrap();
        let published = Arc::new(Mutex::new(vec![]));
        let opened = Arc::new(Mutex::new(vec![]));
        let send = published.clone();
        let open = opened.clone();
        let native = media_copy::NativeMediaCopy::new(
            library.clone(),
            Arc::new(move |event| {
                send.lock().unwrap().push(event);
                Ok(())
            }),
            Arc::new(move |path| {
                assert!(path.exists());
                open.lock().unwrap().push(path.to_path_buf());
                Ok(())
            }),
        )
        .unwrap();
        Self {
            _dir: dir,
            library,
            native,
            published,
            opened,
        }
    }
    async fn close(&self) {
        self.library.finish_writes().await.unwrap();
        self.native.shutdown().await.unwrap();
        assert_eq!(self.native.status()["requests"], 0);
    }
}
#[tokio::test]
async fn media_copy_admission_context_caps_and_terminal_close_are_explicit() {
    let f = Fixture::new();
    let context = f.library.context().unwrap();
    assert_eq!(
        media_copy::COMMANDS,
        ["BUNDLE_MEDIA_FILES", "MEDIA_FOLDER_COPY"]
    );
    assert_eq!(
        f.native
            .dispatch(request("WRONG", Value::Null, &context))
            .await
            .unwrap_err()
            .code,
        "unsupported_command"
    );
    assert_eq!(
        f.native
            .dispatch(request(
                "MEDIA_FOLDER_COPY",
                json!({"paths":["x".repeat(1024*1024)]}),
                &context
            ))
            .await
            .unwrap_err()
            .code,
        "copy_resource_limit"
    );
    let mut stale = context.clone();
    stale.library_generation += 1;
    f.native.context_changed(&context);
    assert_eq!(
        f.native
            .dispatch(request("MEDIA_FOLDER_COPY", json!({"paths":[]}), &stale))
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    let reply = f
        .native
        .dispatch(request("MEDIA_FOLDER_COPY", json!({"paths":[]}), &context))
        .await;
    if cfg!(target_os = "linux") {
        let replies = reply.unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].message.data, Some(json!(false)));
        assert_eq!(
            replies[0].listener_id.as_deref(),
            Some("media-copy-listener")
        );
        assert_eq!(replies[0].context.as_ref(), Some(&context));
    } else {
        assert_eq!(reply.unwrap_err().code, "unsupported_platform");
    }
    f.native.shutdown().await.unwrap();
    assert_eq!(
        f.native
            .dispatch(request("MEDIA_FOLDER_COPY", json!({"paths":[]}), &context))
            .await
            .unwrap_err()
            .code,
        "media_copy_shutdown"
    );
    assert!(f.published.lock().unwrap().is_empty());
    assert!(f.opened.lock().unwrap().is_empty());
    f.close().await;
}
#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{fs, time::Duration};
    use tokio::{sync::oneshot, time::timeout};

    struct Hold {
        release: Option<std::sync::mpsc::SyncSender<()>>,
        started: oneshot::Receiver<()>,
        owner: tokio::task::JoinHandle<freeshow_core::Result<Vec<Event>>>,
    }
    impl Hold {
        async fn started(&mut self) {
            timeout(Duration::from_secs(3), &mut self.started)
                .await
                .unwrap()
                .unwrap();
        }
        async fn release(mut self) {
            self.release.take().unwrap().send(()).unwrap();
            timeout(Duration::from_secs(3), &mut self.owner)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }
    impl Drop for Hold {
        fn drop(&mut self) {
            if let Some(send) = self.release.take() {
                let _ = send.send(());
            }
        }
    }
    fn hold(
        f: &Fixture,
        after: impl FnOnce(&mut Runtime) -> freeshow_core::Result<Vec<Event>> + Send + 'static,
    ) -> Hold {
        let (entered, started) = oneshot::channel();
        let (release, held) = std::sync::mpsc::sync_channel(1);
        let library = f.library.clone();
        let context = library.context().unwrap();
        let owner = tokio::spawn(async move {
            library
                .commit(
                    request("MEDIA_FOLDER_COPY", Value::Null, &context),
                    "main",
                    move |runtime| {
                        let _ = entered.send(());
                        held.recv_timeout(Duration::from_secs(10)).unwrap();
                        after(runtime)
                    },
                )
                .await
        });
        Hold {
            release: Some(release),
            started,
            owner,
        }
    }
    async fn queued(f: &Fixture, count: usize) {
        timeout(Duration::from_secs(3), async {
            while f.library.status().unwrap().queued_jobs != count {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }
    fn source(f: &Fixture, name: &str, bytes: &[u8]) -> PathBuf {
        let dir = f._dir.path().join("Original media 敬拜");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
    fn destination(context: &RuntimeContext, source: &std::path::Path) -> PathBuf {
        context
            .data_path
            .join("Media")
            .join(freeshow_media_copy::parent_folder_id(
                source.to_str().unwrap(),
            ))
            .join(source.file_name().unwrap())
    }
    #[tokio::test]
    async fn exact_opaque_copy_bool_cache_skip_and_listener_context_reach_native_owner() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let bytes = b"\0\xff\x80native media\r\n";
        let path = source(&f, "video 日本語.bin", bytes);
        let payload = json!({"paths":[path],"dataPath":"/ignored","outputPath":"/ignored"});
        let first = f
            .native
            .dispatch(request("MEDIA_FOLDER_COPY", payload.clone(), &context))
            .await
            .unwrap();
        assert_eq!(first[0].message.data, Some(json!(true)));
        assert_eq!(first[0].context.as_ref(), Some(&context));
        assert_eq!(fs::read(destination(&context, &path)).unwrap(), bytes);
        fs::write(
            &path,
            b"different source bytes do not replace existing destination",
        )
        .unwrap();
        let second = f
            .native
            .dispatch(request("MEDIA_FOLDER_COPY", payload, &context))
            .await
            .unwrap();
        assert_eq!(second[0].message.data, Some(json!(false)));
        assert_eq!(fs::read(destination(&context, &path)).unwrap(), bytes);
        assert!(f.published.lock().unwrap().is_empty());
        f.close().await;
    }
    #[tokio::test]
    async fn bundle_frozen_show_project_overlay_template_references_have_no_fake_success_callback()
    {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let paths: Vec<_> = (0..4)
            .map(|i| source(&f, &format!("bundle-{i}.bin"), &[0, 255, i]))
            .collect();
        fs::write(
            context.data_path.join("Shows/Native.show"),
            serde_json::to_vec(
                &json!(["show",{"media":{"one":{"path":paths[0]}},"slides":{},"unknown":true}]),
            )
            .unwrap(),
        )
        .unwrap();
        for (file, key, value) in [
            (
                "PROJECTS",
                "projects",
                json!({"native":{"shows":[{"type":"image","id":paths[1]}]}}),
            ),
            (
                "OVERLAYS",
                "native",
                json!({"items":[{"type":"media","src":paths[2]}]}),
            ),
            (
                "TEMPLATES",
                "native",
                json!({"items":[{"type":"media","src":paths[3]}]}),
            ),
        ] {
            f.library
                .dispatch(
                    request(
                        "SET_STORE_VALUE",
                        json!({"file":file,"key":key,"value":value}),
                        &context,
                    ),
                    "main",
                )
                .await
                .unwrap();
        }
        let replies = f
            .native
            .dispatch(request(
                "BUNDLE_MEDIA_FILES",
                json!({"openFolder":true}),
                &context,
            ))
            .await
            .unwrap();
        assert!(
            replies.is_empty(),
            "the source BUNDLE handler has no successful callback"
        );
        for (i, path) in paths.iter().enumerate() {
            assert_eq!(
                fs::read(destination(&context, path)).unwrap(),
                [0, 255, i as u8]
            );
        }
        assert_eq!(*f.opened.lock().unwrap(), [context.data_path.join("Media")]);
        assert!(f.published.lock().unwrap().is_empty());
        f.close().await;
    }

    #[tokio::test]
    async fn accepted_pins_and_two_host_leases_survive_caller_drop_and_terminal_drain() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let paths: Vec<_> = (0..2)
            .map(|i| source(&f, &format!("held-{i}.bin"), &[i, 255, 0]))
            .collect();
        let mut final_owner = hold(&f, |_| Ok(vec![]));
        final_owner.started().await;
        let native = f.native.clone();
        let input = request("MEDIA_FOLDER_COPY", json!({"paths":[paths[0]]}), &context);
        let caller = tokio::spawn(async move { native.dispatch(input).await });
        queued(&f, 1).await;
        // Preparation already read/staged bytes outside the held IO owner.
        // This final closure is accepted and retains its opaque commit pin.
        let native = f.native.clone();
        let input = request("MEDIA_FOLDER_COPY", json!({"paths":[paths[1]]}), &context);
        let waiting = tokio::spawn(async move { native.dispatch(input).await });
        timeout(Duration::from_secs(3), async {
            while f.native.status()["requests"] != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert_eq!(f.native.status()["requests"], 2);
        assert_eq!(
            f.native
                .dispatch(request("MEDIA_FOLDER_COPY", json!({"paths":[]}), &context))
                .await
                .unwrap_err()
                .code,
            "media_copy_busy"
        );
        let native = f.native.clone();
        let shutdown = tokio::spawn(async move { native.shutdown().await });
        timeout(Duration::from_secs(3), async {
            while f.native.status()["accepting"] == true {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!shutdown.is_finished());
        assert_eq!(f.native.status()["requests"], 2);
        for path in &paths {
            assert!(!destination(&context, path).exists());
        }
        final_owner.release().await;
        assert_eq!(waiting.await.unwrap().unwrap_err().code, "copy_cancelled");
        timeout(Duration::from_secs(3), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            fs::read(destination(&context, &paths[0])).unwrap(),
            [0, 255, 0]
        );
        assert!(!destination(&context, &paths[1]).exists());
        assert_eq!(f.native.status()["requests"], 0);
        assert!(f.published.lock().unwrap().is_empty());
        f.close().await;
    }

    #[tokio::test]
    async fn concurrent_batches_to_one_absent_tree_both_copy_exact_bytes() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let paths: Vec<_> = (0..2)
            .map(|i| source(&f, &format!("shared-{i}.bin"), &[255, i]))
            .collect();
        let mut first = hold(&f, |_| Ok(vec![]));
        first.started().await;
        let mut jobs = vec![];
        for path in &paths {
            let native = f.native.clone();
            let input = request("MEDIA_FOLDER_COPY", json!({"paths":[path]}), &context);
            jobs.push(tokio::spawn(async move { native.dispatch(input).await }));
        }
        queued(&f, 1).await;
        timeout(Duration::from_secs(3), async {
            while f.native.status()["requests"] != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        first.release().await;
        for job in jobs {
            assert_eq!(
                job.await.unwrap().unwrap()[0].message.data,
                Some(json!(true))
            );
        }
        for (i, path) in paths.iter().enumerate() {
            assert_eq!(
                fs::read(destination(&context, path)).unwrap(),
                [255, i as u8]
            );
        }
        assert_eq!(f.native.status()["requests"], 0);
        f.close().await;
    }

    #[tokio::test]
    async fn changed_media_setting_rejects_prepared_old_destination_without_writes() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let path = source(&f, "setting.bin", b"unchanged media");
        let next = f._dir.path().join("changed media root");
        fs::create_dir(&next).unwrap();
        let changed = next.clone();
        let mut final_owner = hold(&f, move |runtime| {
            runtime
                .stores
                .set_key("config", "mediaFolderPath", json!(changed))?;
            Ok(vec![])
        });
        final_owner.started().await;
        let native = f.native.clone();
        let input = request("MEDIA_FOLDER_COPY", json!({"paths":[path]}), &context);
        let job = tokio::spawn(async move { native.dispatch(input).await });
        queued(&f, 1).await;
        final_owner.release().await;
        assert_eq!(job.await.unwrap().unwrap_err().code, "copy_context_changed");
        assert!(!destination(&context, &path).exists());
        assert_eq!(fs::read_dir(next).unwrap().count(), 0);
        assert!(f.published.lock().unwrap().is_empty());
        f.close().await;
    }

    #[tokio::test]
    async fn changed_bundle_store_rejects_frozen_references_before_final_effects() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let path = source(&f, "store.bin", b"bundle source");
        fs::write(
            context.data_path.join("Shows/Guard.show"),
            serde_json::to_vec(&json!(["guard",{"media":{"one":{"path":path}},"slides":{}}]))
                .unwrap(),
        )
        .unwrap();
        let mut first = hold(&f, |_| Ok(vec![]));
        first.started().await;
        let native = f.native.clone();
        let input = request("BUNDLE_MEDIA_FILES", json!({"openFolder":true}), &context);
        let job = tokio::spawn(async move { native.dispatch(input).await });
        queued(&f, 1).await;
        let mut final_owner = hold(&f, |runtime| {
            runtime
                .stores
                .set_key("PROJECTS", "future", json!({"changed":true}))?;
            Ok(vec![])
        });
        queued(&f, 2).await;
        first.release().await;
        final_owner.started().await;
        queued(&f, 1).await;
        final_owner.release().await;
        assert_eq!(job.await.unwrap().unwrap_err().code, "copy_input_changed");
        assert!(!destination(&context, &path).exists());
        assert!(f.opened.lock().unwrap().is_empty());
        assert!(f.published.lock().unwrap().is_empty());
        f.close().await;
    }

    #[tokio::test]
    async fn changed_library_rejects_accepted_old_copy_before_installation() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let path = source(&f, "root.bin", b"old library media");
        let next = f._dir.path().join("next library");
        fs::create_dir(&next).unwrap();
        let update = request(
            "UPDATE_DATA_PATH",
            json!({"oldPath":context.data_path,"newPath":next}),
            &context,
        );
        let mut final_owner = hold(&f, move |runtime| runtime.dispatch(update, "main"));
        final_owner.started().await;
        let native = f.native.clone();
        let input = request("MEDIA_FOLDER_COPY", json!({"paths":[path]}), &context);
        let job = tokio::spawn(async move { native.dispatch(input).await });
        queued(&f, 1).await;
        final_owner.release().await;
        let current = f.library.context().unwrap();
        assert_ne!(current, context);
        f.native.context_changed(&current);
        assert_eq!(job.await.unwrap().unwrap_err().code, "stale_library");
        assert!(!destination(&context, &path).exists());
        assert!(!destination(&current, &path).exists());
        assert!(f.published.lock().unwrap().is_empty());
        f.close().await;
    }

    #[tokio::test]
    async fn copy_preparation_reads_outside_blocked_io_and_final_guard_rejects_later_source_mutation(
    ) {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let path = source(&f, "outside-owner.bin", b"captured before owner release");
        let mut owner = hold(&f, |_| Ok(vec![]));
        owner.started().await;
        let native = f.native.clone();
        let input = request("MEDIA_FOLDER_COPY", json!({"paths":[path]}), &context);
        let job = tokio::spawn(async move { native.dispatch(input).await });
        queued(&f, 1).await;
        fs::write(&path, b"changed after independent preparation").unwrap();
        owner.release().await;
        assert_eq!(
            job.await.unwrap().unwrap_err().code,
            "copy_effect_source_changed"
        );
        assert!(!destination(&context, &path).exists());
        assert_eq!(f.native.status()["requests"], 0);
        f.close().await;
    }

    #[tokio::test]
    async fn custom_bundle_preserves_one_early_missing_folder_alert_before_copy_creates_root() {
        let f = Fixture::new();
        let context = f.library.context().unwrap();
        let path = source(&f, "missing-open.bin", b"real opaque bytes");
        let custom = f._dir.path().join("not created yet 敬拜");
        f.library
            .dispatch(
                request(
                    "SET_STORE_VALUE",
                    json!({"file":"config","key":"mediaFolderPath","value":custom}),
                    &context,
                ),
                "main",
            )
            .await
            .unwrap();
        fs::write(
            context.data_path.join("Shows/Opening.show"),
            serde_json::to_vec(&json!(["opening",{"media":{"one":{"path":path}},"slides":{}}]))
                .unwrap(),
        )
        .unwrap();
        let result = f
            .native
            .dispatch(request(
                "BUNDLE_MEDIA_FILES",
                json!({"openFolder":true,"outputPath":custom}),
                &context,
            ))
            .await
            .unwrap();
        assert!(result.is_empty());
        let events = f.published.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message.channel, "ALERT");
        assert_eq!(events[0].message.data, Some(json!("This does not exist!")));
        assert_eq!(events[0].context.as_ref(), Some(&context));
        assert_eq!(events[0].listener_id, None);
        assert!(f.opened.lock().unwrap().is_empty());
        assert_eq!(
            fs::read(
                custom
                    .join(freeshow_media_copy::parent_folder_id(
                        path.to_str().unwrap()
                    ))
                    .join(path.file_name().unwrap())
            )
            .unwrap(),
            b"real opaque bytes"
        );
        f.close().await;
    }
}
