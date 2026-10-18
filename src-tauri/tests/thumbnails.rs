#[allow(dead_code)]
#[path = "../src/assets.rs"]
mod assets;
#[allow(dead_code)]
#[path = "../src/browse.rs"]
mod browse;
#[allow(dead_code)]
#[path = "../src/thumbnails.rs"]
mod thumbnails;

#[cfg(target_os = "linux")]
use freeshow_core::protocol::Event;
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use serde_json::{json, Value};
#[cfg(target_os = "linux")]
use std::{fs, time::Duration};
#[cfg(target_os = "linux")]
use tokio::sync::mpsc;

fn request(command: &str, data: Value, context: &RuntimeContext) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":command,"data":data},"context":context,"listenerId":"native-thumbnail-test"})).unwrap()
}
struct Fixture {
    dir: tempfile::TempDir,
    library: LibraryService,
    assets: assets::NativeAssets,
    native: thumbnails::NativeThumbnails,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("app");
        let library =
            LibraryService::start(Runtime::new(app.clone(), dir.path().join("data")).unwrap())
                .unwrap();
        let assets = assets::NativeAssets::start(&library.context().unwrap(), &app)
            .await
            .unwrap();
        let native = thumbnails::NativeThumbnails::new(app.join("media-cache")).unwrap();
        Self {
            dir,
            library,
            assets,
            native,
        }
    }
    async fn close(&self) {
        self.library.finish_writes().await.unwrap();
        self.native.shutdown().await.unwrap();
        self.assets.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn thumbnail_roles_and_terminal_shutdown_are_explicit_on_every_platform() {
    let f = Fixture::new().await;
    assert!(f.dir.path().is_dir());
    let current = f.library.context().unwrap();
    let emit = |_| panic!("unauthorized role must not capture");
    let error = f
        .native
        .dispatch(
            &f.library,
            &f.assets,
            request("GET_THUMBNAIL", json!({"input":"missing"}), &current),
            "output",
            &emit,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "thumbnail_role");
    assert!(thumbnails::COMMANDS.contains(&"DOES_MEDIA_EXIST"));
    assert!(browse::COMMANDS.contains(&"READ_FOLDER"));
    f.native.shutdown().await.unwrap();
    let error = f
        .native
        .dispatch(
            &f.library,
            &f.assets,
            request("GET_THUMBNAIL", json!({"input":"missing"}), &current),
            "main",
            &emit,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "thumbnail_shutdown");
    f.close().await;
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use tokio::{sync::oneshot, time::timeout};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![];
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&vec![122; width as usize * height as usize * 4])
                .unwrap();
        }
        bytes
    }
    fn start(
        f: &Fixture,
        input: &std::path::Path,
        size: u32,
    ) -> (
        tokio::task::JoinHandle<Result<Vec<Event>, freeshow_core::WireError>>,
        mpsc::Receiver<Event>,
    ) {
        let (send, receive) = mpsc::channel(4);
        let native = f.native.clone();
        let library = f.library.clone();
        let assets = f.assets.clone();
        let request = request(
            "GET_THUMBNAIL",
            json!({"input":input,"size":size}),
            &library.context().unwrap(),
        );
        let job = tokio::spawn(async move {
            let emit = |event| send.try_send(event).map_err(|error| error.to_string());
            native
                .dispatch(&library, &assets, request, "main", &emit)
                .await
        });
        (job, receive)
    }
    async fn capture(receive: &mut mpsc::Receiver<Event>) -> Event {
        timeout(Duration::from_secs(2), receive.recv())
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn actual_png_installs_before_reply_and_cache_hit_reuses_its_grant_without_recapture() {
        let f = Fixture::new().await;
        let input = f.dir.path().join("日本語 literal%20.png");
        let bytes = png(4, 2);
        fs::write(&input, &bytes).unwrap();
        let (job, mut pending) = start(&f, &input, 4);
        let event = capture(&mut pending).await;
        let id = event.native_capture.as_ref().unwrap()["id"]
            .as_str()
            .unwrap();
        assert_eq!(event.channel, "MAIN");
        assert_eq!(event.message.channel, "CAPTURE_CANVAS");
        assert_eq!(event.message.data.as_ref().unwrap()["id"], id);
        assert_eq!(
            event.native_assets.as_ref().unwrap()[0]["sourcePath"],
            json!(input)
        );
        assert!(!f.native.cache_root().exists());
        f.native
            .complete(&f.library, "main", id, &bytes, false)
            .unwrap();
        assert!(f
            .native
            .complete(&f.library, "main", id, &bytes, false)
            .is_err());
        let reply = timeout(Duration::from_secs(2), job)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .remove(0);
        assert_eq!(reply.listener_id.as_deref(), Some("native-thumbnail-test"));
        let output = reply.message.data.as_ref().unwrap()["output"]
            .as_str()
            .unwrap();
        assert_eq!(fs::read(output).unwrap(), bytes);
        assert_eq!(
            std::path::Path::new(output).parent().unwrap(),
            f.native.cache_root()
        );
        let grants = reply.native_assets.as_ref().unwrap();
        assert_eq!(grants[0]["sourcePath"], output);
        let emit = |_| panic!("cache hit must not recapture");
        let again = f
            .native
            .dispatch(
                &f.library,
                &f.assets,
                request(
                    "GET_THUMBNAIL",
                    json!({"input":input,"size":4}),
                    &f.library.context().unwrap(),
                ),
                "main",
                &emit,
            )
            .await
            .unwrap();
        assert_eq!(again[0].message.data, reply.message.data);
        assert_eq!(
            again[0].native_assets.as_ref().unwrap()[0]["url"],
            grants[0]["url"]
        );
        assert_eq!(f.native.status()["requests"], 0);
        f.close().await;
    }

    #[tokio::test]
    async fn source_capture_failure_returns_empty_output_and_never_installs_a_prediction() {
        let f = Fixture::new().await;
        let input = f.dir.path().join("invalid.png");
        fs::write(&input, b"invalid image").unwrap();
        let (job, mut pending) = start(&f, &input, 4);
        let event = capture(&mut pending).await;
        let id = event.native_capture.as_ref().unwrap()["id"]
            .as_str()
            .unwrap();
        f.native
            .complete(&f.library, "main", id, &[], true)
            .unwrap();
        let reply = job.await.unwrap().unwrap();
        assert_eq!(reply[0].message.data.as_ref().unwrap()["output"], "");
        assert!(reply[0]
            .native_assets
            .as_ref()
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        assert!(!f.native.cache_root().exists());
        f.close().await;
    }

    #[tokio::test]
    async fn simultaneous_same_cache_requests_capture_once_and_keep_each_callers_payload() {
        let f = Fixture::new().await;
        let input = f.dir.path().join("shared.png");
        let bytes = png(4, 2);
        fs::write(&input, &bytes).unwrap();
        let (first, mut pending) = start(&f, &input, 4);
        let event = capture(&mut pending).await;
        let id = event.native_capture.as_ref().unwrap()["id"]
            .as_str()
            .unwrap();
        let native = f.native.clone();
        let library = f.library.clone();
        let assets = f.assets.clone();
        let mut second_request = request(
            "GET_THUMBNAIL",
            json!({"input":input,"size":4,"caller":{"keep":true}}),
            &library.context().unwrap(),
        );
        second_request.listener_id = Some("second-caller".into());
        let second = tokio::spawn(async move {
            native
                .dispatch(&library, &assets, second_request, "main", &|_| {
                    panic!("the waiting request must use the installed cache")
                })
                .await
        });
        timeout(Duration::from_secs(2), async {
            while f.native.status()["requests"] != 2 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(f.native.status()["captures"], 1);
        assert!(!second.is_finished());
        f.native
            .complete(&f.library, "main", id, &bytes, false)
            .unwrap();
        let first_reply = first.await.unwrap().unwrap().remove(0);
        let second_reply = timeout(Duration::from_secs(2), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .remove(0);
        assert_eq!(
            first_reply.listener_id.as_deref(),
            Some("native-thumbnail-test")
        );
        assert_eq!(second_reply.listener_id.as_deref(), Some("second-caller"));
        assert!(first_reply
            .message
            .data
            .as_ref()
            .unwrap()
            .get("caller")
            .is_none());
        assert_eq!(
            second_reply.message.data.as_ref().unwrap()["caller"],
            json!({"keep":true})
        );
        assert_eq!(
            first_reply.message.data.as_ref().unwrap()["output"],
            second_reply.message.data.as_ref().unwrap()["output"]
        );
        assert_eq!(
            fs::read(
                second_reply.message.data.as_ref().unwrap()["output"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
            bytes
        );
        assert_eq!(first_reply.native_assets, second_reply.native_assets);
        assert_eq!(f.native.status()["requests"], 0);
        f.close().await;
    }

    #[tokio::test]
    async fn queued_stale_cache_invalidation_does_not_consume_the_canvas_capture_deadline() {
        let f = Fixture::new().await;
        let input = f.dir.path().join("stale-before-backup.png");
        let bytes = png(4, 2);
        fs::write(&input, &bytes).unwrap();
        fs::create_dir(f.native.cache_root()).unwrap();
        let output = f.native.cache_root().join(format!(
            "{}-4.png",
            freeshow_thumbnails::file_path_hash_code(input.to_str().unwrap())
        ));
        fs::write(&output, png(4, 1)).unwrap();
        fs::File::options()
            .write(true)
            .open(&output)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        let (entered, started) = oneshot::channel();
        let (release, held) = std::sync::mpsc::sync_channel(1);
        let library = f.library.clone();
        let hold = request("TEST_HOLD", Value::Null, &library.context().unwrap());
        let owner = tokio::spawn(async move {
            library
                .commit(hold, "main", move |_| {
                    entered.send(()).unwrap();
                    held.recv().unwrap();
                    Ok(vec![])
                })
                .await
        });
        started.await.unwrap();
        let (job, mut pending) = start(&f, &input, 4);
        timeout(Duration::from_secs(2), async {
            while f.library.status().unwrap().queued_jobs == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(f.native.status()["captures"], 0);
        assert!(pending.try_recv().is_err());
        // Exercise the real owner queue, exceeding the production 15s canvas
        // deadline before the capture is published. No relaxed test clock.
        tokio::time::sleep(Duration::from_secs(16)).await;
        release.send(()).unwrap();
        owner.await.unwrap().unwrap();
        let event = capture(&mut pending).await;
        assert!(
            !output.exists(),
            "stale entry is invalidated before publication"
        );
        let id = event.native_capture.as_ref().unwrap()["id"]
            .as_str()
            .unwrap();
        f.native
            .complete(&f.library, "main", id, &bytes, false)
            .unwrap();
        let reply = timeout(Duration::from_secs(2), job)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            reply[0].message.data.as_ref().unwrap()["output"],
            json!(output)
        );
        assert_eq!(fs::read(&output).unwrap(), bytes);
        assert_eq!(f.native.status()["requests"], 0);
        f.close().await;
    }

    #[tokio::test]
    async fn capture_responds_while_owner_is_blocked_and_accepted_install_survives_caller_drop() {
        let f = Fixture::new().await;
        let input = f.dir.path().join("queued.png");
        let bytes = png(4, 2);
        fs::write(&input, &bytes).unwrap();
        let (entered, started) = oneshot::channel();
        let (release, held) = std::sync::mpsc::sync_channel(1);
        let library = f.library.clone();
        let hold = request("TEST_HOLD", Value::Null, &library.context().unwrap());
        let owner = tokio::spawn(async move {
            library
                .commit(hold, "main", move |_| {
                    entered.send(()).unwrap();
                    held.recv().unwrap();
                    Ok(vec![])
                })
                .await
        });
        started.await.unwrap();
        let (job, mut pending) = start(&f, &input, 4);
        let event = capture(&mut pending).await;
        let id = event.native_capture.as_ref().unwrap()["id"]
            .as_str()
            .unwrap();
        f.native
            .complete(&f.library, "main", id, &bytes, false)
            .unwrap();
        timeout(Duration::from_secs(2), async {
            while f.library.status().unwrap().queued_jobs == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(!job.is_finished());
        job.abort();
        assert!(job.await.unwrap_err().is_cancelled());
        // Accepted effects own the generation lock even after the caller drops.
        // A concurrent follower must wait for that install, then reuse it.
        let (follower, mut recapture) = start(&f, &input, 4);
        timeout(Duration::from_secs(2), async {
            while f.native.status()["requests"] != 2 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(!follower.is_finished());
        assert!(recapture.try_recv().is_err());
        release.send(()).unwrap();
        owner.await.unwrap().unwrap();
        timeout(Duration::from_secs(2), follower)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(recapture.try_recv().is_err());
        f.library.finish_writes().await.unwrap();
        let output = f.native.cache_root().join(format!(
            "{}-4.png",
            freeshow_thumbnails::file_path_hash_code(input.to_str().unwrap())
        ));
        assert_eq!(fs::read(output).unwrap(), bytes);
        f.native.shutdown().await.unwrap();
        assert_eq!(f.native.status()["requests"], 0);
        f.assets.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn role_unknown_overlimit_and_stale_uploads_cannot_consume_or_write_another_capture() {
        let f = Fixture::new().await;
        let input = f.dir.path().join("stale.png");
        let bytes = png(4, 2);
        fs::write(&input, &bytes).unwrap();
        let (job, mut pending) = start(&f, &input, 4);
        let event = capture(&mut pending).await;
        let id = event.native_capture.as_ref().unwrap()["id"]
            .as_str()
            .unwrap();
        assert!(f
            .native
            .complete(&f.library, "output", id, &bytes, false)
            .is_err());
        assert!(f
            .native
            .complete(
                &f.library,
                "main",
                &format!("native-thumbnail-{}", "f".repeat(32)),
                &bytes,
                false
            )
            .is_err());
        assert!(f
            .native
            .complete(&f.library, "main", id, &vec![0; 8 * 1024 * 1024 + 1], false)
            .is_err());
        assert_eq!(f.native.status()["captures"], 1);
        let current = f.library.context().unwrap();
        let other = f.dir.path().join("other");
        fs::create_dir(&other).unwrap();
        f.library
            .dispatch(
                request(
                    "UPDATE_DATA_PATH",
                    json!({"oldPath":current.data_path,"newPath":other}),
                    &current,
                ),
                "main",
            )
            .await
            .unwrap();
        f.native.context_changed(&f.library.context().unwrap());
        f.assets
            .context_changed(&f.library.context().unwrap())
            .unwrap();
        assert!(job.await.unwrap().is_err());
        assert!(f
            .native
            .complete(&f.library, "main", id, &bytes, false)
            .is_err());
        assert!(!f.native.cache_root().exists());
        assert_eq!(f.native.status()["requests"], 0);
        f.close().await;
    }

    #[tokio::test]
    async fn four_whole_capture_leases_bound_admission_and_shutdown_releases_all_waiters() {
        let f = Fixture::new().await;
        let mut jobs = vec![];
        for index in 0..4 {
            let input = f.dir.path().join(format!("{index}.png"));
            fs::write(&input, png(4, 2)).unwrap();
            let (job, mut pending) = start(&f, &input, 4);
            capture(&mut pending).await;
            jobs.push(job);
        }
        assert_eq!(f.native.status()["captures"], 4);
        let error = f
            .native
            .dispatch(
                &f.library,
                &f.assets,
                request(
                    "GET_THUMBNAIL",
                    json!({"input":"missing"}),
                    &f.library.context().unwrap(),
                ),
                "main",
                &|_| panic!("busy request cannot capture"),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "thumbnail_busy");
        timeout(Duration::from_secs(2), f.native.shutdown())
            .await
            .unwrap()
            .unwrap();
        for job in jobs {
            assert!(job.await.unwrap().is_err());
        }
        assert_eq!(f.native.status()["requests"], 0);
        f.close().await;
    }

    #[tokio::test]
    async fn folder_listing_finishes_original_capture_slots_then_publishes_existing_cache_grants() {
        let f = Fixture::new().await;
        let folder = f.dir.path().join("media/folder");
        fs::create_dir_all(&folder).unwrap();
        for index in 0..2 {
            fs::write(folder.join(format!("{index}.png")), png(4, 2)).unwrap();
        }
        fs::write(folder.join("song.mp3"), b"audio").unwrap();
        let browser = browse::NativeBrowse::new(
            freeshow_browse::BrowseService::new(freeshow_browse::BrowseConfig::default()).unwrap(),
        );
        let current = f.library.context().unwrap();
        let input = request(
            "READ_FOLDER",
            json!({"path":folder.parent().unwrap(),"depth":0,"captureFolderContent":true}),
            &current,
        );
        let lease = f.assets.begin(&f.library, &input).unwrap().unwrap();
        let (send, mut receive) = mpsc::channel(4);
        let library = f.library.clone();
        let assets = f.assets.clone();
        let native = f.native.clone();
        let browse = browser.clone();
        let job = tokio::spawn(async move {
            let emit = |event| send.try_send(event).map_err(|error| error.to_string());
            browse
                .dispatch_with_thumbnails(&library, input, "main", &native, &assets, &emit)
                .await
        });
        for _ in 0..2 {
            let event = capture(&mut receive).await;
            let data = event.message.data.as_ref().unwrap();
            assert_eq!(data["size"]["width"], 250);
            assert_eq!(
                event.native_assets.as_ref().unwrap()[0]["sourcePath"],
                data["input"]
            );
            f.native
                .complete(
                    &f.library,
                    "main",
                    data["id"].as_str().unwrap(),
                    &png(250, 125),
                    false,
                )
                .unwrap();
        }
        let mut replies = timeout(Duration::from_secs(2), job)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        f.assets
            .complete(&f.library, lease, &mut replies)
            .await
            .unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].message.channel, "READ_FOLDER");
        let data = replies[0].message.data.as_ref().unwrap();
        for index in 0..2 {
            let original = folder.join(format!("{index}.png"));
            let output = data[original.to_str().unwrap()]["thumbnailPath"]
                .as_str()
                .unwrap();
            assert_eq!(fs::read(output).unwrap(), png(250, 125));
            assert!(replies[0]
                .native_assets
                .as_ref()
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|grant| grant["sourcePath"] == output));
        }
        assert!(data
            .get(folder.join("song.mp3").to_str().unwrap())
            .is_none());
        assert!(data[folder.to_str().unwrap()]
            .as_object()
            .unwrap()
            .contains_key("noMedia"));
        assert!(data[folder.to_str().unwrap()]["noMedia"].is_null());
        browser.shutdown().await.unwrap();
        f.close().await;
    }
}
