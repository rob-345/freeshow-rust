use crate::*;
use flate2::{write::GzEncoder, Compression};
use std::io::Write;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(bytes).unwrap();
    gzip.finish().unwrap()
}
struct Server {
    url: String,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn close(self) {
        self.stop.cancel();
        self.task.await.unwrap();
    }
}
async fn server(bytes: Vec<u8>, status: u16, length: bool, hold: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/fixture.gz", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let cancel = stop.clone();
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {biased;_=cancel.cancelled()=>break,accepted=listener.accept()=>{let (mut stream,_)=accepted.unwrap();let bytes=bytes.clone();let cancel=cancel.clone();assert!(connections.len()<16);connections.spawn(async move{
                let mut request=Vec::new();let mut byte=[0u8;1];while !request.ends_with(b"\r\n\r\n"){if request.len()==8192{return;}
            if tokio::select!{_=cancel.cancelled()=>return,result=stream.read(&mut byte)=>result}.unwrap_or(0)==0{return;}request.push(byte[0]);}
                let mut headers=format!("HTTP/1.1 {status} Fixture\r\nConnection: close\r\n");if length{headers.push_str(&format!("Content-Length: {}\r\n",bytes.len()));}headers.push_str("\r\n");if stream.write_all(headers.as_bytes()).await.is_err(){return;}
                if hold{let first=bytes.len().min(12);if stream.write_all(&bytes[..first]).await.is_err(){return;}cancel.cancelled().await;return;}
                for chunk in bytes.chunks(7){if tokio::select!{_=cancel.cancelled()=>return,result=stream.write_all(chunk)=>result}.is_err(){return;}tokio::task::yield_now().await;}
            });},Some(_)=connections.join_next(),if !connections.is_empty()=>{}}
        }
        while connections.join_next().await.is_some() {}
    });
    Server { url, stop, task }
}
fn owner() -> DownloadOwner {
    DownloadOwner::new("runtime", 1, "/captured/library").unwrap()
}
fn plan(root: &Path) -> HostDownloadPlan {
    plan_for_host("linux", "x64", root, owner()).unwrap()
}
fn config(url: &str) -> Config {
    Config {
        controlled_test: Some(ControlledTestConfig::loopback(url).unwrap()),
        ..Config::default()
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(
            Instant::now() < end,
            "controlled ownership barrier timed out"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn scheduled_until(
    state: &mut tokio::sync::watch::Receiver<service::TestScheduling>,
    condition: impl Fn(&service::TestScheduling) -> bool,
) -> service::TestScheduling {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = state.borrow_and_update().clone();
            if condition(&snapshot) {
                return snapshot;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .expect("fixed network owners did not reach the observed wait state")
}

async fn cleaned(mut state: tokio::sync::watch::Receiver<Option<Result<()>>>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = state.borrow_and_update().clone();
            if let Some(result) = snapshot {
                result.unwrap();
                return;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .expect("actual filesystem cleanup did not complete");
}

#[tokio::test(start_paused = true)]
async fn genuinely_idle_owners_do_not_poll() {
    let service = DownloadService::start(Config::default()).unwrap();
    let mut scheduling = service.scheduling();
    let idle = scheduled_until(&mut scheduling, |state| {
        state.cycles.iter().all(|cycles| *cycles > 0)
    })
    .await;
    assert_eq!(idle.prepared, [0, 0]);
    // Advance a controlled runtime clock across fifty old 20ms poll periods.
    // Compare real owner loop counters rather than inferring idleness by sleep.
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(scheduling.borrow().cycles, idle.cycles);
    tokio::time::resume();
    service.shutdown().await.unwrap();
    assert_eq!(service.status().workers, 0);
}

#[tokio::test]
async fn first_job_after_witnessed_idle_wakes_real_transfer() {
    // Keep real job/deadline checks on the ordinary std/Tokio clock. The paused
    // clock test above admits no jobs and makes no claim about std deadline TTL.
    let root = tempfile::tempdir().unwrap();
    let bytes = b"first real job after idle";
    let server = server(gzip(bytes), 200, true, false).await;
    let service = DownloadService::start(config(&server.url)).unwrap();
    let mut scheduling = service.scheduling();
    scheduled_until(&mut scheduling, |state| state.cycles.iter().all(|n| *n > 0)).await;
    let prepared = service
        .begin(
            plan(root.path()),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
    let accepted = prepared.accept(&owner()).unwrap();
    assert_eq!(
        std::fs::read(accepted.verify_for_install(&owner()).unwrap().staged_path).unwrap(),
        bytes
    );
    scheduled_until(&mut scheduling, |state| state.prepared == [0, 0]).await;
    accepted.finish().await.unwrap();
    assert_eq!(service.status().admitted, 0);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    service.shutdown().await.unwrap();
    assert_eq!(service.status().workers, 0);
    server.close().await;
}

#[tokio::test]
async fn shutdown_timeout_and_abandoned_retry_retain_the_real_remaining_join_and_sticky_fault() {
    use std::future::Future;

    for panic in [false, true] {
        let service = DownloadService::start(Config {
            shutdown_timeout: Duration::from_millis(20),
            ..Config::default()
        })
        .unwrap();
        let (entered, release) = service.hold_network_exit(1);
        let shutdown = service.shutdown();
        tokio::pin!(shutdown);
        tokio::select! {
            result = &mut shutdown => panic!("shutdown completed before actual owner exit: {result:?}"),
            result = entered => result.unwrap(),
        }
        assert_eq!(
            shutdown.await.err().unwrap().code,
            "download_shutdown_timeout"
        );
        assert_eq!(
            service.retained_network_joins().await,
            1,
            "the first joined owner must be removed, the held owner retained"
        );
        {
            let retry = service.shutdown();
            tokio::pin!(retry);
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(retry.as_mut().poll(&mut context).is_pending());
            // Dropping this observer must not drop the actual pending JoinHandle.
        }
        assert_eq!(service.retained_network_joins().await, 1);
        assert_eq!(
            service.status().workers,
            2,
            "FS owners stay alive until network drain"
        );
        release.send(panic).unwrap();
        let result = service.shutdown().await;
        if panic {
            let error = result.unwrap_err();
            assert_eq!(error.code, "download_worker");
            assert_eq!(service.status().failure.unwrap().message, error.message);
            assert_eq!(service.shutdown().await.unwrap_err().message, error.message);
        } else {
            result.unwrap();
            service.shutdown().await.unwrap();
        }
        assert_eq!(service.retained_network_joins().await, 0);
        assert_eq!(service.status().workers, 0);
    }
}

#[tokio::test]
async fn held_prepared_output_expires_and_cleans_without_a_second_request() {
    let root = tempfile::tempdir().unwrap();
    let server = server(gzip(b"held until real deadline"), 200, true, false).await;
    let service = DownloadService::start(config(&server.url)).unwrap();
    let prepared = service
        .begin(
            plan(root.path()),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
    let private = prepared.job.private.clone();
    let cleanup = prepared.job.cleanup_observer();
    assert!(private.join("artifact").is_file());
    let mut scheduling = service.scheduling();
    scheduled_until(&mut scheduling, |state| state.prepared == [1, 1]).await;
    cleaned(cleanup).await;
    assert!(!private.exists());
    assert_eq!(service.status().admitted, 0);
    assert_eq!(
        prepared.accept(&owner()).err().unwrap().code,
        "download_deadline"
    );
    scheduled_until(&mut scheduling, |state| state.prepared == [0, 0]).await;
    service.shutdown().await.unwrap();
    assert_eq!(service.status().workers, 0);
    server.close().await;
}

#[tokio::test]
async fn prepared_cancellation_and_abandonment_wake_cleanup_without_another_job() {
    for variant in 0..3 {
        let root = tempfile::tempdir().unwrap();
        let server = server(gzip(b"cancelled or abandoned"), 200, true, false).await;
        let service = DownloadService::start(config(&server.url)).unwrap();
        let caller = CancellationToken::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        let ticket = service
            .begin(plan(root.path()), caller.clone(), deadline)
            .unwrap();
        let private = ticket.job.private.clone();
        let cleanup = ticket.job.cleanup_observer();
        let mut scheduling = service.scheduling();
        let mut retained = None;
        if variant == 1 {
            // The network owner already accepted and prepared the real transfer;
            // its successful result is still inside the uncollected ticket.
            scheduled_until(&mut scheduling, |state| state.prepared == [1, 1]).await;
            drop(ticket);
        } else {
            let prepared = ticket.finish().await.unwrap();
            scheduled_until(&mut scheduling, |state| state.prepared == [1, 1]).await;
            if variant == 0 {
                caller.cancel();
                retained = Some(prepared);
            } else {
                // Neither idle owner's wait snapshot may retain this Job Arc.
                drop(prepared);
            }
        }
        cleaned(cleanup).await;
        assert!(Instant::now() < deadline, "cleanup must not rely on expiry");
        assert!(!private.exists());
        assert_eq!(service.status().admitted, 0);
        if let Some(prepared) = retained {
            assert_eq!(
                prepared.accept(&owner()).err().unwrap().code,
                "download_cancelled"
            );
        }
        scheduled_until(&mut scheduling, |state| state.prepared == [0, 0]).await;
        service.shutdown().await.unwrap();
        assert_eq!(service.status().workers, 0);
        server.close().await;
    }
}
#[tokio::test]
async fn actual_http_gzip_prepares_exact_bytes_without_touching_final_destination() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("ffmpeg"), b"untouched host binary").unwrap();
    let bytes = b"inert fixture UTF-8 \xe6\x95\xac\xe6\x8b\x9c\0\xff";
    let compressed = gzip(bytes);
    let server = server(compressed.clone(), 200, true, false).await;
    let service = DownloadService::start(config(&server.url)).unwrap();
    let ticket = service
        .begin(
            plan(root.path()),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    let progress = ticket.progress();
    let prepared = ticket.finish().await.unwrap();
    assert_eq!(prepared.stats.compressed_bytes, compressed.len() as u64);
    assert_eq!(prepared.stats.uncompressed_bytes, bytes.len() as u64);
    assert!(prepared.stats.progress_updates <= 256);
    assert_eq!(progress.borrow().as_ref().unwrap().progress, 100.0);
    let accepted = prepared.accept(&owner()).unwrap();
    let view = accepted.verify_for_install(&owner()).unwrap();
    assert_eq!(std::fs::read(view.staged_path).unwrap(), bytes);
    assert_eq!(view.destination, root.path().join("ffmpeg"));
    assert_eq!(view.mode, Some(0o755));
    assert!(view.post_install_commands().is_empty());
    assert_eq!(
        std::fs::read(root.path().join("ffmpeg")).unwrap(),
        b"untouched host binary"
    );
    // Simulate only the trusted host's install, with inert bytes in this
    // temporary directory. The service never executes or renames a binary.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(view.staged_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::rename(view.staged_path, &view.destination).unwrap();
    accepted.finish().await.unwrap();
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    assert_eq!(std::fs::read(root.path().join("ffmpeg")).unwrap(), bytes);
    service.shutdown().await.unwrap();
    assert_eq!(service.status().workers, 0);
    server.close().await;
}
#[tokio::test]
async fn source_gzip_members_padding_crc_and_truncation_have_real_cleanup() {
    let a = gzip(b"A");
    let b = gzip(b"B");
    let mut crc = a.clone();
    let n = crc.len();
    crc[n - 8] ^= 1;
    for (wire, expected, error) in [
        (a.clone(), Some(b"A".as_slice()), None),
        (
            [a.clone(), b.clone()].concat(),
            Some(b"AB".as_slice()),
            None,
        ),
        (
            [a.clone(), vec![0], b.clone()].concat(),
            None,
            Some("Nonzero data after gzip zero padding is unsupported"),
        ),
        (crc, None, Some("incorrect data check")),
        (
            a[..a.len() - 1].to_vec(),
            None,
            Some("unexpected end of file"),
        ),
        (vec![], None, Some("unexpected end of file")),
    ] {
        let root = tempfile::tempdir().unwrap();
        let server = server(wire, 200, true, false).await;
        let service = DownloadService::start(config(&server.url)).unwrap();
        let result = service
            .begin(
                plan(root.path()),
                CancellationToken::new(),
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap()
            .finish()
            .await;
        match result {
            Ok(prepared) => {
                let accepted = prepared.accept(&owner()).unwrap();
                assert_eq!(
                    std::fs::read(accepted.verify_for_install(&owner()).unwrap().staged_path)
                        .unwrap(),
                    expected.unwrap()
                );
                accepted.finish().await.unwrap();
            }
            Err(actual) => assert_eq!(actual.message, error.unwrap()),
        };
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        service.shutdown().await.unwrap();
        server.close().await;
    }
}
#[tokio::test]
async fn unknown_content_length_has_no_downloading_progress_and_http_errors_never_stage() {
    for status in [200, 204, 205, 404, 503] {
        let root = tempfile::tempdir().unwrap();
        let server = server(gzip(b"inert"), status, false, false).await;
        let service = DownloadService::start(config(&server.url)).unwrap();
        let ticket = service
            .begin(
                plan(root.path()),
                CancellationToken::new(),
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
        let progress = ticket.progress();
        let result = ticket.finish().await;
        if status == 200 {
            let prepared = result.unwrap();
            assert!(progress.borrow().is_none());
            prepared.discard().await.unwrap();
        } else {
            assert_eq!(
                result.err().unwrap().message,
                format!("Download failed: {status}")
            );
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        service.shutdown().await.unwrap();
        server.close().await;
    }
}
#[tokio::test]
async fn bounded_admission_dropped_ticket_and_cancellation_drain_slow_archive_work() {
    let root = tempfile::tempdir().unwrap();
    let server = server(gzip(&vec![7; 65536]), 200, true, true).await;
    let service = DownloadService::start(config(&server.url)).unwrap();
    let first = service
        .begin(
            plan(root.path()),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    let second = service
        .begin(
            plan(root.path()),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(
        service
            .begin(
                plan(root.path()),
                CancellationToken::new(),
                Instant::now() + Duration::from_secs(5)
            )
            .err()
            .unwrap()
            .code,
        "download_busy"
    );
    until(|| service.status().active == 2).await;
    until(|| {
        std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().join("artifact").is_file())
            .count()
            == 2
    })
    .await;
    drop(first);
    second.cancel();
    assert_eq!(
        second.finish().await.err().unwrap().code,
        "download_cancelled"
    );
    until(|| service.status().admitted == 0).await;
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    service.shutdown().await.unwrap();
    server.close().await;
}
#[tokio::test]
async fn prepared_context_rejection_and_accepted_install_survive_cancellation_until_owner_releases()
{
    let root = tempfile::tempdir().unwrap();
    let server = server(gzip(b"inert"), 200, true, false).await;
    let service = DownloadService::start(Config {
        shutdown_timeout: Duration::from_millis(20),
        ..config(&server.url)
    })
    .unwrap();
    let prepared = service
        .begin(
            plan(root.path()),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
    let wrong = DownloadOwner::new("runtime", 2, "/other").unwrap();
    assert_eq!(
        prepared.accept(&wrong).err().unwrap().code,
        "stale_generation"
    );
    until(|| service.status().admitted == 0).await;
    let caller = CancellationToken::new();
    let prepared = service
        .begin(
            plan(root.path()),
            caller.clone(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
    let accepted = prepared.accept(&owner()).unwrap();
    let path = accepted
        .verify_for_install(&owner())
        .unwrap()
        .staged_path
        .to_path_buf();
    caller.cancel();
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "download_shutdown_timeout"
    );
    assert!(
        path.exists(),
        "accepted install cannot be cleaned while the IO owner holds its guard"
    );
    accepted.verify_for_install(&owner()).unwrap();
    accepted.finish().await.unwrap();
    service.shutdown().await.unwrap();
    assert!(!path.exists());
    assert_eq!(service.status().workers, 0);
    server.close().await;
}
#[tokio::test]
async fn byte_header_member_work_deadline_and_staging_identity_limits_are_explicit() {
    let root = tempfile::tempdir().unwrap();
    let wire = gzip(&vec![b'x'; 4096]);
    let server = server(wire.clone(), 200, true, false).await;
    let mut configs = Vec::new();
    let mut c = config(&server.url);
    c.compressed_bytes = 1;
    configs.push(c);
    let mut c = config(&server.url);
    c.uncompressed_bytes = 32;
    configs.push(c);
    let mut c = config(&server.url);
    c.gzip_steps = 1;
    configs.push(c);
    for c in configs {
        let service = DownloadService::start(c).unwrap();
        let error = service
            .begin(
                plan(root.path()),
                CancellationToken::new(),
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap()
            .finish()
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, "download_limit");
        service.shutdown().await.unwrap();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
    let service = DownloadService::start(config(&server.url)).unwrap();
    assert_eq!(
        service
            .begin(plan(root.path()), CancellationToken::new(), Instant::now())
            .err()
            .unwrap()
            .code,
        "download_deadline"
    );
    let prepared = service
        .begin(
            plan(root.path()),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
    let accepted = prepared.accept(&owner()).unwrap();
    let path = accepted
        .verify_for_install(&owner())
        .unwrap()
        .staged_path
        .to_path_buf();
    std::fs::write(&path, b"changed").unwrap();
    assert_eq!(
        accepted.verify_for_install(&owner()).err().unwrap().code,
        "download_identity"
    );
    accepted.finish().await.unwrap();
    service.shutdown().await.unwrap();
    server.close().await;
}
#[test]
fn table_and_private_configuration_and_progress_are_bounded() {
    assert!(download_url("linux", "x64").unwrap().contains("b6.1.1"));
    assert!(download_url("win32", "ia32").unwrap().contains("b6.0/"));
    assert!(download_url("linux", "riscv64").is_none());
    assert!(ControlledTestConfig::loopback("https://127.0.0.1/a").is_err());
    assert!(ControlledTestConfig::loopback("http://example.com/a").is_err());
    assert!(ControlledTestConfig::loopback("http://user@127.0.0.1/a").is_err());
    assert!(ControlledTestConfig::loopback(&"x".repeat(8193)).is_err());
    assert!(plan_for_host(&"x".repeat(33), "x64", "/", owner()).is_err());
    assert!(progress_for_bytes(1, 0.0).is_none());
    assert!(progress_for_bytes(1, f64::NAN).is_none());
    assert_eq!(progress_for_bytes(1, 8.0).unwrap().progress, 13.0);
    assert_eq!(progress_for_bytes(1, -8.0).unwrap().progress, -12.0);
    assert_eq!(progress_for_bytes(9, 8.0).unwrap().progress, 113.0);
    let bounded = source_completion(false, Some(&"界".repeat(1000)));
    assert!(bounded.reply["error"].as_str().unwrap().len() <= 1024);
    assert_eq!(
        source_completion(false, None).reply,
        serde_json::json!({"success":false,"error":"Unknown download error"})
    );
}

#[tokio::test]
async fn live_body_deadline_idle_timeout_and_dropped_prepared_artifact_clean_before_releasing_ownership(
) {
    let root = tempfile::tempdir().unwrap();
    let held = server(gzip(b"inert fixture"), 200, true, true).await;
    for idle in [false, true] {
        let mut c = config(&held.url);
        if idle {
            c.idle_timeout = Duration::from_millis(20);
        } else {
            c.timeout = Duration::from_millis(20);
        }
        let service = DownloadService::start(c).unwrap();
        let ticket = service
            .begin(
                plan(root.path()),
                CancellationToken::new(),
                Instant::now() + Duration::from_secs(2),
            )
            .unwrap();
        until(|| {
            std::fs::read_dir(root.path())
                .unwrap()
                .any(|entry| entry.unwrap().path().join("artifact").exists())
        })
        .await;
        assert_eq!(
            ticket.finish().await.err().unwrap().code,
            if idle {
                "download_idle_timeout"
            } else {
                "download_deadline"
            }
        );
        assert_eq!(service.status().admitted, 0);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        service.shutdown().await.unwrap();
    }
    held.close().await;
    let ready = server(gzip(b"inert"), 200, true, false).await;
    let service = DownloadService::start(config(&ready.url)).unwrap();
    let prepared = service
        .begin(
            plan(root.path()),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
    drop(prepared);
    until(|| service.status().admitted == 0).await;
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    service.shutdown().await.unwrap();
    ready.close().await;
}

#[tokio::test]
async fn replaced_root_and_unowned_private_contents_fault_without_deleting_other_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("bin");
    std::fs::create_dir(&root).unwrap();
    let server = server(gzip(b"inert"), 200, true, false).await;
    let service = DownloadService::start(config(&server.url)).unwrap();
    let accepted = service
        .begin(
            plan(&root),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap()
        .finish()
        .await
        .unwrap()
        .accept(&owner())
        .unwrap();
    let old = temp.path().join("moved-bin");
    std::fs::rename(&root, &old).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("foreign"), b"untouched").unwrap();
    assert_eq!(
        accepted.verify_for_install(&owner()).err().unwrap().code,
        "download_identity"
    );
    assert_eq!(
        accepted.finish().await.unwrap_err().code,
        "download_identity"
    );
    assert!(service.status().closing);
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "download_identity"
    );
    assert_eq!(std::fs::read(root.join("foreign")).unwrap(), b"untouched");
    assert_eq!(
        std::fs::read_dir(&old).unwrap().count(),
        1,
        "lost-identity private artifact is reported, not silently deleted or called cleaned"
    );
    let service = DownloadService::start(config(&server.url)).unwrap();
    let accepted = service
        .begin(
            plan(&root),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap()
        .finish()
        .await
        .unwrap()
        .accept(&owner())
        .unwrap();
    let private = accepted
        .verify_for_install(&owner())
        .unwrap()
        .staged_path
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::write(private.join("foreign"), b"must remain").unwrap();
    assert_eq!(
        accepted.finish().await.unwrap_err().code,
        "download_filesystem"
    );
    assert!(service.shutdown().await.is_err());
    assert_eq!(
        std::fs::read(private.join("foreign")).unwrap(),
        b"must remain",
        "cleanup never recursively deletes unknown contents"
    );
    server.close().await;
}

#[tokio::test]
async fn terminal_and_library_cancellation_revoke_unaccepted_artifacts_even_when_callers_retain_them(
) {
    let root = tempfile::tempdir().unwrap();
    let server = server(gzip(b"inert"), 200, true, false).await;
    let service = DownloadService::start(config(&server.url)).unwrap();
    let queued = service
        .begin(
            plan(root.path()),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(service.status().admitted, 0);
    assert_eq!(
        queued.finish().await.err().unwrap().code,
        "download_shutdown"
    );
    let service = DownloadService::start(config(&server.url)).unwrap();
    let completed_but_uncollected = service
        .begin(
            plan(root.path()),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    until(|| *locked(&completed_but_uncollected.job.phase) == service::Phase::Prepared).await;
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    service.shutdown().await.unwrap();
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    assert_eq!(
        completed_but_uncollected.finish().await.err().unwrap().code,
        "download_shutdown",
        "an uncollected successful reply cannot retain its private artifact during shutdown"
    );
    let service = DownloadService::start(config(&server.url)).unwrap();
    let cancel = CancellationToken::new();
    let prepared = service
        .begin(
            plan(root.path()),
            cancel.clone(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
    cancel.cancel();
    until(|| service.status().admitted == 0).await;
    assert_eq!(
        std::fs::read_dir(root.path()).unwrap().count(),
        0,
        "retaining a revoked prepared value cannot retain a private artifact"
    );
    assert_eq!(
        prepared.accept(&owner()).err().unwrap().code,
        "download_cancelled"
    );
    service.shutdown().await.unwrap();
    server.close().await;
}

#[tokio::test]
async fn rejected_acceptance_awaits_actual_cleanup_and_retains_both_acceptance_and_cleanup_faults()
{
    let root = tempfile::tempdir().unwrap();
    let server = server(gzip(b"inert prepared bytes"), 200, true, false).await;
    for foreign in [false, true] {
        let service = DownloadService::start(config(&server.url)).unwrap();
        let prepared = service
            .begin(
                plan(root.path()),
                CancellationToken::new(),
                Instant::now() + Duration::from_secs(2),
            )
            .unwrap()
            .finish()
            .await
            .unwrap();
        let unexpected = prepared.job.private.join("foreign");
        if foreign {
            std::fs::write(&unexpected, b"must remain").unwrap();
        }
        let wrong = DownloadOwner::new("runtime", 2, "/another/library").unwrap();
        let error = prepared.accept_or_discard(&wrong).await.err().unwrap();
        assert_eq!(error.code, "stale_generation");
        assert!(error.message.contains("obsolete owner"));
        assert_eq!(
            service.status().admitted,
            0,
            "acceptance error is returned only after actual cleanup receipt"
        );
        if foreign {
            assert!(error
                .message
                .contains("cleanup failed [download_filesystem]"));
            assert_eq!(
                service.status().failure.unwrap().code,
                "download_filesystem"
            );
            assert_eq!(
                service.shutdown().await.err().unwrap().code,
                "download_filesystem"
            );
            assert_eq!(std::fs::read(unexpected).unwrap(), b"must remain");
        } else {
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
            service.shutdown().await.unwrap();
        }
    }
    server.close().await;
}
