#[path = "../src/ffmpeg_download.rs"]
mod ffmpeg_download;

use ffmpeg_download::{Config, ControlledTest, NativeFfmpegDownloads};
use flate2::{write::GzEncoder, Compression};
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_media::{MediaConfig, MediaService};
use serde_json::json;
#[cfg(unix)]
use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{oneshot, Semaphore},
    task::JoinHandle,
    time::timeout,
};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap()
}
fn request(context: &RuntimeContext) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"FFMPEG_DOWNLOAD","data":{"url":"https://untrusted.invalid/override","path":"/not-authorized","payload":"ignored"}},"listenerId":"download-listener","context":context})).unwrap()
}
async fn wait(mut predicate: impl FnMut() -> bool) {
    timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
struct Server {
    url: String,
    received: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn new(bytes: &[u8], status: u16, held: bool) -> Self {
        let mut gzip = GzEncoder::new(vec![], Compression::default());
        gzip.write_all(bytes).unwrap();
        let bytes = Arc::new(gzip.finish().unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/inert.gz", listener.local_addr().unwrap());
        let received = Arc::new(AtomicUsize::new(0));
        let count = received.clone();
        let release = Arc::new(Semaphore::new(if held { 0 } else { 16 }));
        let gate = release.clone();
        let task = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            for _ in 0..8 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let bytes = bytes.clone();
                let gate = gate.clone();
                let count = count.clone();
                tasks.spawn(async move {
                let mut request = [0;4096]; let _ = stream.read(&mut request).await;
                let header = format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
                if stream.write_all(header.as_bytes()).await.is_err() { return; }
                count.fetch_add(1, Ordering::AcqRel);
                let _permit = gate.acquire().await.unwrap(); let _ = stream.write_all(&bytes).await; let _ = stream.shutdown().await;
            });
            }
            while tasks.join_next().await.is_some() {}
        });
        Self {
            url,
            received,
            release,
            task,
        }
    }
    async fn requested(&self, count: usize) {
        wait(|| self.received.load(Ordering::Acquire) >= count).await;
    }
    fn send(&self) {
        self.release.add_permits(8);
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    app: PathBuf,
    library: LibraryService,
    media: MediaService,
    native: NativeFfmpegDownloads,
    events: Arc<Mutex<Vec<Event>>>,
}
impl Fixture {
    fn new(
        server: &Server,
        modify: impl FnOnce(&mut Config),
        media: impl FnOnce(&Path) -> MediaConfig,
    ) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let app = root.join("app 敬拜");
        let library = LibraryService::start(
            Runtime::new(app.clone(), root.join("selected-library")).unwrap(),
        )
        .unwrap();
        let media = MediaService::new(media(&app)).unwrap();
        let events = Arc::new(Mutex::new(vec![]));
        let observed = events.clone();
        let mut config = Config {
            controlled_test: Some(
                ControlledTest::loopback(
                    if cfg!(windows) { "win32" } else { "linux" },
                    "x64",
                    &server.url,
                )
                .unwrap(),
            ),
            ..Config::default()
        };
        modify(&mut config);
        let native = NativeFfmpegDownloads::new(
            library.clone(),
            media.clone(),
            app.clone(),
            config,
            Arc::new(move |event| {
                lock(&observed).push(event);
                Ok(())
            }),
        )
        .unwrap();
        Self {
            _temp: temp,
            root,
            app,
            library,
            media,
            native,
            events,
        }
    }
    fn normal(server: &Server) -> Self {
        Self::new(
            server,
            |_| {},
            |_| MediaConfig {
                discover_system: false,
                ..MediaConfig::default()
            },
        )
    }
    fn request(&self) -> Request {
        request(&self.library.context().unwrap())
    }
    fn destination(&self) -> PathBuf {
        self.app.join("bin").join(if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        })
    }
    fn private_count(&self) -> usize {
        fs::read_dir(self.app.join("bin"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .starts_with(".freeshow-ffmpeg-")
                    })
                    .count()
            })
            .unwrap_or(0)
    }
    async fn close(&self) {
        self.native.shutdown().await.unwrap();
        self.media.shutdown().await;
        self.library.finish_writes().await.unwrap();
    }
    fn success_count(&self) -> usize {
        lock(&self.events)
            .iter()
            .filter(|e| {
                e.message
                    .data
                    .as_ref()
                    .is_some_and(|v| v["status"] == "complete")
            })
            .count()
    }
}
async fn pause(
    library: &LibraryService,
) -> (
    std::sync::mpsc::Sender<()>,
    JoinHandle<freeshow_core::Result<Vec<Event>>>,
) {
    let context = library.context().unwrap();
    let owner = library.clone();
    let (entered, ready) = oneshot::channel();
    let (release, paused) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        owner
            .commit(request(&context), "main", move |_| {
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
async fn actual_gzip_installs_original_bytes_mode_replacement_and_source_events_in_fixed_app_root()
{
    let server = Server::new(
        b"inert executable fixture \xe6\x95\xac\xe6\x8b\x9c\0\xff",
        200,
        false,
    )
    .await;
    let f = Fixture::normal(&server);
    assert_eq!(ffmpeg_download::COMMANDS, &["FFMPEG_DOWNLOAD"]);
    fs::create_dir(f.app.join("bin")).unwrap();
    fs::write(f.destination(), b"old installation").unwrap();
    let result = f
        .native
        .begin(f.request(), "main")
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert!(result.diagnostics.is_empty());
    assert_eq!(
        fs::read(f.destination()).unwrap(),
        b"inert executable fixture \xe6\x95\xac\xe6\x8b\x9c\0\xff"
    );
    assert_eq!(f.private_count(), 0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(f.destination()).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    assert!(!f.library.context().unwrap().data_path.join("bin").exists());
    let events = result.into_events().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].message.channel, "FFMPEG_DOWNLOAD");
    assert_eq!(events[0].message.data, Some(json!({"success":true})));
    assert_eq!(events[0].listener_id.as_deref(), Some("download-listener"));
    assert_eq!(
        events[0].context.as_ref(),
        Some(&f.library.context().unwrap())
    );
    {
        let progress = lock(&f.events);
        assert!(!progress.is_empty());
        assert!(progress.len() <= 257);
        assert!(progress.iter().all(|e| e.channel == "MAIN"
            && e.message.channel == "MEDIA_DOWNLOAD_PROGRESS"
            && e.listener_id.is_none()
            && e.context.is_some()));
        assert_eq!(
            progress.last().unwrap().message.data,
            Some(
                json!({"url":"ffmpeg","name":"FFmpeg","progress":100.0,"total":100,"status":"complete"})
            )
        );
    }
    f.close().await;
}

#[tokio::test]
async fn ready_uncollected_results_hold_two_whole_host_leases_and_close_revokes_them() {
    let server = Server::new(b"inert", 200, false).await;
    let f = Fixture::normal(&server);
    let first = f.native.begin(f.request(), "main").unwrap();
    let second = f.native.begin(f.request(), "main").unwrap();
    wait(|| f.native.status()["readyUncollected"] == 2).await;
    assert!(matches!(f.native.begin(f.request(),"main"),Err(e) if e.code=="download_busy"));
    let collected = first.finish().await.unwrap();
    assert_eq!(f.native.status()["admittedRequests"], 2);
    collected.into_events().unwrap();
    assert_eq!(f.native.status()["admittedRequests"], 1);
    let third = f.native.begin(f.request(), "main").unwrap();
    drop(third);
    f.native.shutdown().await.unwrap();
    assert_eq!(f.native.status()["admittedRequests"], 0);
    assert!(second.finish().await.unwrap().into_events().is_err());
    assert_eq!(f.private_count(), 0);
    assert!(matches!(f.native.begin(f.request(),"main"),Err(e) if e.code=="download_shutdown"));
    f.close().await;
}

#[tokio::test]
async fn caller_drop_before_accept_cancels_real_http_and_cleans_private_artifacts() {
    let server = Server::new(b"inert", 200, true).await;
    let f = Fixture::normal(&server);
    let ticket = f.native.begin(f.request(), "main").unwrap();
    server.requested(1).await;
    assert_eq!(f.native.status()["admittedRequests"], 1);
    drop(ticket);
    wait(|| f.native.status()["admittedRequests"] == 0).await;
    assert!(!f.destination().exists());
    assert_eq!(f.private_count(), 0);
    assert_eq!(f.success_count(), 0);
    f.close().await;
}

#[tokio::test]
async fn ordered_root_preparation_waits_for_owner_and_close_timeout_retains_then_retries_real_work()
{
    let server = Server::new(b"inert", 200, false).await;
    let f = Fixture::new(
        &server,
        |c| c.shutdown_timeout = Duration::from_millis(30),
        |_| MediaConfig {
            discover_system: false,
            ..MediaConfig::default()
        },
    );
    let (release, barrier) = pause(&f.library).await;
    let first = f.native.begin(f.request(), "main").unwrap();
    let second = f.native.begin(f.request(), "main").unwrap();
    wait(|| f.library.status().unwrap().queued_jobs == 2).await;
    assert!(!f.app.join("bin").exists());
    assert_eq!(server.received.load(Ordering::Acquire), 0);
    assert!(matches!(f.native.begin(f.request(),"main"),Err(e) if e.code=="download_busy"));
    assert!(matches!(f.native.shutdown().await,Err(e) if e.code=="download_shutdown_timeout"));
    assert_eq!(f.native.status()["admittedRequests"], 2);
    assert!(f.library.status().unwrap().accepting_requests);
    release.send(()).unwrap();
    barrier.await.unwrap().unwrap();
    // Releasing the library barrier does not bound OS cleanup/join scheduling to
    // this fixture's 30ms observer deadline. Drive only its documented timeout
    // retry until actual owners join; every other failure remains an error.
    timeout(Duration::from_secs(5), async {
        loop {
            match f.native.shutdown().await {
                Ok(()) => break,
                Err(error) => {
                    assert_eq!(error.code, "download_shutdown_timeout", "{error:?}");
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        }
    })
    .await
    .expect("actual download cleanup and joins must settle after barrier release");
    let actual = f.native.status();
    assert_eq!(actual["foundation"]["admitted"], 0);
    assert_eq!(actual["foundation"]["active"], 0);
    assert_eq!(actual["foundation"]["workers"], 0);
    assert!(first.finish().await.is_err());
    assert!(second.finish().await.is_err());
    assert_eq!(f.native.status()["admittedRequests"], 0);
    assert!(!f.app.join("bin").exists());
    f.close().await;
}

#[tokio::test]
async fn accepted_install_survives_caller_drop_and_close_drains_before_write_admission_closes() {
    let server = Server::new(b"accepted inert bytes", 200, true).await;
    let f = Fixture::new(
        &server,
        |c| c.shutdown_timeout = Duration::from_millis(30),
        |_| MediaConfig {
            discover_system: false,
            ..MediaConfig::default()
        },
    );
    let ticket = f.native.begin(f.request(), "main").unwrap();
    server.requested(1).await;
    let (release, barrier) = pause(&f.library).await;
    server.send();
    wait(|| f.library.status().unwrap().queued_jobs == 1).await;
    drop(ticket);
    assert!(matches!(f.native.shutdown().await,Err(e) if e.code=="download_shutdown_timeout"));
    assert_eq!(f.native.status()["admittedRequests"], 1);
    assert_eq!(f.private_count(), 1);
    release.send(()).unwrap();
    barrier.await.unwrap().unwrap();
    f.native.shutdown().await.unwrap();
    assert_eq!(fs::read(f.destination()).unwrap(), b"accepted inert bytes");
    assert_eq!(f.private_count(), 0);
    assert_eq!(f.success_count(), 0);
    assert_eq!(f.native.status()["admittedRequests"], 0);
    f.close().await;
}

#[tokio::test]
async fn context_switch_cancels_preparation_and_rejects_ready_old_generation() {
    let server = Server::new(b"inert", 200, true).await;
    let f = Fixture::normal(&server);
    let old = f.library.context().unwrap();
    let ticket = f.native.begin(f.request(), "main").unwrap();
    server.requested(1).await;
    fs::create_dir(f.root.join("switched")).unwrap();
    f.library.dispatch(serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"newPath":f.root.join("switched"),"oldPath":old.data_path}},"context":old})).unwrap(),"main").await.unwrap();
    let current = f.library.context().unwrap();
    assert_ne!(old, current);
    f.native.context_changed(&current);
    assert!(ticket.finish().await.is_err());
    assert!(!f.destination().exists());
    assert_eq!(f.private_count(), 0);
    assert_eq!(f.success_count(), 0);
    assert!(matches!(f.native.begin(request(&old),"main"),Err(e) if e.code=="stale_library"));
    server.send();
    let ready = f
        .native
        .begin(f.request(), "main")
        .unwrap()
        .finish()
        .await
        .unwrap();
    let previous = f.library.context().unwrap();
    fs::create_dir(f.root.join("switched-again")).unwrap();
    f.library.dispatch(serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"newPath":f.root.join("switched-again"),"oldPath":previous.data_path}},"context":previous})).unwrap(),"main").await.unwrap();
    let current = f.library.context().unwrap();
    f.native.context_changed(&current);
    assert_eq!(f.native.status()["admittedRequests"], 0);
    assert!(ready.into_events().is_err());
    f.close().await;
}

#[tokio::test]
async fn source_error_reply_and_untrusted_role_or_app_root_cannot_install() {
    let server = Server::new(b"not downloaded", 404, false).await;
    let f = Fixture::normal(&server);
    assert!(matches!(f.native.begin(f.request(),"output-1"),Err(e) if e.code=="download_role"));
    let result = f
        .native
        .begin(f.request(), "main")
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert!(!result.diagnostics.is_empty());
    let events = result.into_events().unwrap();
    assert_eq!(events[0].message.data.as_ref().unwrap()["success"], false);
    assert!(events[0].message.data.as_ref().unwrap()["error"].is_string());
    assert_eq!(
        lock(&f.events)
            .last()
            .unwrap()
            .message
            .data
            .as_ref()
            .unwrap()["status"],
        "error"
    );
    assert!(!f.destination().exists());
    assert_eq!(f.private_count(), 0);
    let wrong = f.root.join("renderer-selected-app");
    fs::create_dir(&wrong).unwrap();
    let native = NativeFfmpegDownloads::new(
        f.library.clone(),
        f.media.clone(),
        wrong.clone(),
        Config {
            controlled_test: Some(ControlledTest::loopback("linux", "x64", &server.url).unwrap()),
            ..Config::default()
        },
        Arc::new(|_| Ok(())),
    )
    .unwrap();
    let result = native
        .begin(f.request(), "main")
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert!(result
        .diagnostics
        .iter()
        .any(|e| e.code == "download_root_identity"));
    assert!(!wrong.join("bin").exists());
    native.shutdown().await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn unsupported_platform_fails_before_directory_or_http_effects() {
    let server = Server::new(b"inert", 200, false).await;
    let f = Fixture::new(
        &server,
        |c| {
            c.controlled_test =
                Some(ControlledTest::loopback("unsupported", "x64", &server.url).unwrap())
        },
        |_| MediaConfig {
            discover_system: false,
            ..MediaConfig::default()
        },
    );
    let result = f
        .native
        .begin(f.request(), "main")
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(
        result.into_events().unwrap()[0]
            .message
            .data
            .as_ref()
            .unwrap()["success"],
        false
    );
    assert!(!f.app.join("bin").exists());
    assert_eq!(server.received.load(Ordering::Acquire), 0);
    f.close().await;
}

#[cfg(unix)]
fn script(path: &Path, bytes: &[u8]) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn actual_cached_resolver_is_invalidated_before_source_success() {
    // These are generated harmless -version fixtures, never downloaded FFmpeg.
    let server = Server::new(b"#!/bin/sh\nexit 0\n", 200, false).await;
    let f = Fixture::new(
        &server,
        |_| {},
        |app| {
            let old = app.join("old fixture 敬拜");
            script(&old, b"#!/bin/sh\nexit 0\n");
            MediaConfig {
                ffmpeg_candidates: vec![old, app.join("bin/ffmpeg")],
                discover_system: false,
                ..MediaConfig::default()
            }
        },
    );
    let old = f.app.join("old fixture 敬拜");
    assert_eq!(
        f.media
            .dispatch("FFMPEG_CHECK", Value::Null)
            .await
            .unwrap()
            .data["path"],
        json!(old)
    );
    fs::remove_file(&old).unwrap();
    assert_eq!(
        f.media
            .dispatch("FFMPEG_CHECK", Value::Null)
            .await
            .unwrap()
            .data["path"],
        json!(old)
    );
    let ticket = f.native.begin(f.request(), "main").unwrap();
    wait(|| f.success_count() == 1).await;
    assert_eq!(
        f.media
            .dispatch("FFMPEG_CHECK", Value::Null)
            .await
            .unwrap()
            .data["path"],
        json!(f.destination())
    );
    ticket.finish().await.unwrap().into_events().unwrap();
    f.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn mac_warning_matches_source_and_timeout_kills_reaps_without_success_after_rename() {
    for hangs in [false, true] {
        let server = Server::new(b"#!/bin/sh\nexit 0\n", 200, false).await;
        let toolroot = tempfile::tempdir().unwrap();
        let xattr = toolroot.path().join("xattr");
        let codesign = toolroot.path().join("codesign");
        let pid = toolroot.path().join("pid");
        script(&xattr, b"#!/bin/sh\nexit 1\n");
        if hangs {
            script(
                &codesign,
                format!(
                    "#!/bin/sh\necho $$ > '{}'\nexec /bin/sleep 10\n",
                    pid.display()
                )
                .as_bytes(),
            );
        } else {
            script(&codesign, b"#!/bin/sh\nexit 1\n");
        }
        let f = Fixture::new(
            &server,
            |c| {
                c.tool_timeout = Duration::from_millis(40);
                c.controlled_test = Some(
                    ControlledTest::loopback("darwin", "x64", &server.url)
                        .unwrap()
                        .mac_tools(xattr, codesign)
                        .unwrap(),
                );
            },
            |_| MediaConfig {
                discover_system: false,
                ..MediaConfig::default()
            },
        );
        let result = f
            .native
            .begin(f.request(), "main")
            .unwrap()
            .finish()
            .await
            .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].code,
            if hangs {
                "download_mac_timeout"
            } else {
                "download_mac_tool"
            }
        );
        assert_eq!(
            result.into_events().unwrap()[0]
                .message
                .data
                .as_ref()
                .unwrap()["success"],
            !hangs
        );
        assert!(f.destination().exists());
        assert_eq!(f.private_count(), 0);
        #[cfg(target_os = "linux")]
        if hangs {
            let id = fs::read_to_string(&pid).unwrap();
            assert!(!PathBuf::from(format!("/proc/{}", id.trim())).exists());
        }
        f.close().await;
    }
}

#[tokio::test]
async fn explicit_cancel_drains_and_does_not_make_a_terminal_service() {
    let server = Server::new(b"inert", 200, true).await;
    let f = Fixture::normal(&server);
    let ticket = f.native.begin(f.request(), "main").unwrap();
    server.requested(1).await;
    ticket.cancel();
    assert!(ticket.finish().await.is_err());
    assert_eq!(f.native.status()["admittedRequests"], 0);
    assert_eq!(f.private_count(), 0);
    server.send();
    f.native
        .begin(f.request(), "main")
        .unwrap()
        .finish()
        .await
        .unwrap()
        .into_events()
        .unwrap();
    f.close().await;
}

#[tokio::test]
async fn one_cleanup_fault_keeps_diagnosis_sticky_but_drains_the_other_owner() {
    let server = Server::new(b"inert", 200, true).await;
    let f = Fixture::normal(&server);
    let first = f.native.begin(f.request(), "main").unwrap();
    let second = f.native.begin(f.request(), "main").unwrap();
    server.requested(2).await;
    wait(|| f.private_count() == 2).await;
    let private = fs::read_dir(f.app.join("bin"))
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".freeshow-ffmpeg-")
        })
        .unwrap()
        .path();
    let foreign = private.join("foreign-keep");
    fs::write(&foreign, b"preserve unknown contents").unwrap();
    server.send();
    let first = first.finish().await.unwrap();
    let second = second.finish().await.unwrap();
    let outcomes = [
        first.into_events().unwrap()[0]
            .message
            .data
            .as_ref()
            .unwrap()["success"]
            .clone(),
        second.into_events().unwrap()[0]
            .message
            .data
            .as_ref()
            .unwrap()["success"]
            .clone(),
    ];
    // A foundation cleanup fault cancels its other preparations too. Both
    // owners must still drain; neither may turn a failure into success.
    assert!(outcomes.contains(&json!(false)));
    assert_eq!(f.private_count(), 1);
    assert_eq!(fs::read(&foreign).unwrap(), b"preserve unknown contents");
    assert!(f.native.shutdown().await.is_err());
    let status = f.native.status();
    assert_eq!(status["admittedRequests"], 0);
    assert_eq!(status["foundation"]["admitted"], 0);
    assert_eq!(status["foundation"]["workers"], 0);
    assert!(!status["failures"].as_array().unwrap().is_empty());
    assert!(f.native.shutdown().await.is_err());
    f.media.shutdown().await;
    f.library.finish_writes().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn installation_retains_guard_until_real_inflight_resolver_releases_cache_lock() {
    let server = Server::new(b"#!/bin/sh\nexit 0\n", 200, false).await;
    let f = Fixture::new(
        &server,
        |_| {},
        |app| {
            let old = app.join("blocked resolver");
            let started = app.join("started");
            let release = app.join("release");
            script(
                &old,
                format!(
                    "#!/bin/sh\necho ready > '{}'\nwhile [ ! -f '{}' ]; do :; done\nexit 0\n",
                    started.display(),
                    release.display()
                )
                .as_bytes(),
            );
            MediaConfig {
                ffmpeg_candidates: vec![old, app.join("bin/ffmpeg")],
                discover_system: false,
                ..MediaConfig::default()
            }
        },
    );
    let media = f.media.clone();
    let resolving = tokio::spawn(async move { media.dispatch("FFMPEG_CHECK", Value::Null).await });
    wait(|| f.app.join("started").exists()).await;
    let ticket = f.native.begin(f.request(), "main").unwrap();
    wait(|| f.destination().exists()).await;
    assert_eq!(f.success_count(), 0);
    assert_eq!(f.native.status()["admittedRequests"], 1);
    assert_eq!(f.private_count(), 1);
    fs::remove_file(f.app.join("blocked resolver")).unwrap();
    fs::write(f.app.join("release"), b"release").unwrap();
    assert_eq!(
        resolving.await.unwrap().unwrap().data["path"],
        json!(f.app.join("blocked resolver"))
    );
    ticket.finish().await.unwrap().into_events().unwrap();
    assert_eq!(f.private_count(), 0);
    assert_eq!(
        f.media
            .dispatch("FFMPEG_CHECK", Value::Null)
            .await
            .unwrap()
            .data["path"],
        json!(f.destination())
    );
    f.close().await;
}

#[tokio::test]
async fn accepted_install_queued_behind_library_switch_is_rejected_and_receipted() {
    let server = Server::new(b"stale inert", 200, true).await;
    let f = Fixture::normal(&server);
    let old = f.library.context().unwrap();
    let ticket = f.native.begin(f.request(), "main").unwrap();
    server.requested(1).await;
    let (release, barrier) = pause(&f.library).await;
    let destination = f.root.join("new selected library");
    fs::create_dir(&destination).unwrap();
    let owner = f.library.clone();
    let changing = tokio::spawn(async move {
        owner.dispatch(serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"newPath":destination,"oldPath":old.data_path}},"context":old})).unwrap(), "main").await
    });
    wait(|| f.library.status().unwrap().queued_jobs == 1).await;
    server.send();
    wait(|| f.library.status().unwrap().queued_jobs == 2).await;
    release.send(()).unwrap();
    barrier.await.unwrap().unwrap();
    changing.await.unwrap().unwrap();
    assert!(matches!(ticket.finish().await,Err(e) if e.code=="stale_library"));
    assert!(!f.destination().exists());
    assert_eq!(f.private_count(), 0);
    assert_eq!(f.native.status()["admittedRequests"], 0);
    assert_eq!(f.success_count(), 0);
    f.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn close_kills_reaps_live_mac_child_and_invalidates_actual_cache_after_rename() {
    let server = Server::new(b"#!/bin/sh\nexit 0\n", 200, false).await;
    let tools = tempfile::tempdir().unwrap();
    let xattr = tools.path().join("xattr");
    let codesign = tools.path().join("codesign");
    let pid = tools.path().join("pid");
    script(&xattr, b"#!/bin/sh\nexit 0\n");
    script(
        &codesign,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec /bin/sleep 10\n",
            pid.display()
        )
        .as_bytes(),
    );
    let f = Fixture::new(
        &server,
        |c| {
            c.controlled_test = Some(
                ControlledTest::loopback("darwin", "x64", &server.url)
                    .unwrap()
                    .mac_tools(xattr, codesign)
                    .unwrap(),
            )
        },
        |app| {
            let old = app.join("old cached executable");
            script(&old, b"#!/bin/sh\nexit 0\n");
            MediaConfig {
                ffmpeg_candidates: vec![old, app.join("bin/ffmpeg")],
                discover_system: false,
                ..MediaConfig::default()
            }
        },
    );
    let old = f.app.join("old cached executable");
    assert_eq!(
        f.media
            .dispatch("FFMPEG_CHECK", Value::Null)
            .await
            .unwrap()
            .data["path"],
        json!(old)
    );
    fs::remove_file(old).unwrap();
    let ticket = f.native.begin(f.request(), "main").unwrap();
    wait(|| pid.exists()).await;
    assert!(f.destination().exists());
    assert_eq!(f.private_count(), 1);
    f.native.shutdown().await.unwrap();
    assert!(ticket.finish().await.is_err());
    assert_eq!(f.private_count(), 0);
    assert_eq!(f.success_count(), 0);
    assert_eq!(f.native.status()["admittedRequests"], 0);
    #[cfg(target_os = "linux")]
    {
        let id = fs::read_to_string(pid).unwrap();
        assert!(!PathBuf::from(format!("/proc/{}", id.trim())).exists());
    }
    assert_eq!(
        f.media
            .dispatch("FFMPEG_CHECK", Value::Null)
            .await
            .unwrap()
            .data["path"],
        json!(f.destination())
    );
    f.close().await;
}
