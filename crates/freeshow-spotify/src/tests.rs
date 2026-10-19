#![allow(
    clippy::await_holding_lock,
    reason = "Serializes the mandatory process-global native owner tests"
)]
use super::*;
use std::{sync::Mutex, time::Instant};
static TEST_OWNER: Mutex<()> = Mutex::new(());
fn owner() -> Owner {
    Owner::new("native-test", 1, Path::new("/fixture")).unwrap()
}
fn unavailable() -> Backend {
    Backend {
        executable: "/no-test-binary".into(),
        env: vec![],
        unavailable: Some(Error::new(
            "spotify_test_unavailable",
            "Controlled unavailable capability",
        )),
    }
}

#[test]
fn exact_parser_quirks_and_safe_wire_restore_native_values() {
    let Outcome::State(state)=parse_linux("string \"xesam:title\" variant string \"Playing title\"\nstring \"mpris:length\" variant int64 120000000\nvariant int64 -3\nvariant double -0.5").unwrap() else {panic!("expected state")};
    assert!(state.is_playing);
    assert_eq!(state.position_sec, 120.0);
    assert_eq!(state.duration_sec, 0.0);
    assert_eq!(state.volume, 1.0);
    assert_eq!(state.artist, "Unknown");
    assert_eq!(state.wire().undefined_fields, ["albumArt"]);
    let Outcome::State(state) = parse_linux(&format!(
        "string \"xesam:title\" variant string \"x\" int64 {} double .",
        "9".repeat(512)
    ))
    .unwrap() else {
        panic!()
    };
    assert!(state.position_sec.is_infinite());
    assert!(state.volume.is_nan());
    assert_eq!(state.wire().special_numbers.len(), 2);
    assert!(matches!(
        parse_linux("empty").unwrap(),
        Outcome::SourceNull(NullReason::MissingTitle)
    ));
}
#[test]
fn bounds_and_native_configuration_never_echo_session_or_metadata() {
    assert!(parse_linux(&"x".repeat(1024 * 1024 + 1)).is_err());
    assert!(Backend::capture(Path::new("relative")).is_err());
    assert!(Owner::new("", 1, Path::new("/fixture")).is_err());
    assert!(Limits {
        stdout_bytes: usize::MAX,
        ..Limits::default()
    }
    .validate()
    .is_err());
    assert!(!format!("{:?}", unavailable()).contains("no-test-binary"));
}
#[tokio::test(flavor = "current_thread")]
async fn actual_idle_owner_wakes_and_all_ready_results_keep_global_quota_across_join() {
    let _serial = TEST_OWNER.lock().unwrap();
    let own = owner();
    let service = SpotifyService::start(own.clone(), unavailable(), Limits::default()).unwrap();
    assert!(matches!(
        service.ready().await.unwrap(),
        Capability::Unavailable { .. }
    ));
    let foreign = owner();
    assert_eq!(
        service
            .begin_state(&foreign, Instant::now())
            .err()
            .unwrap()
            .code,
        "spotify_stale"
    );
    assert_eq!(
        service.retire(&foreign).await.unwrap_err().code,
        "spotify_stale"
    );
    assert!(!service.status().closing);
    let mut results = vec![];
    for _ in 0..JOB_CAPACITY {
        results.push(
            service
                .begin_state(&own, Instant::now() + Duration::from_secs(1))
                .unwrap()
                .finish()
                .await
                .unwrap(),
        );
    }
    assert_eq!(service.status().completed, 4);
    assert_eq!(service.status().retained, 4);
    assert_eq!(service.status().spawned, 0);
    assert_eq!(
        service
            .begin_state(&own, Instant::now())
            .err()
            .unwrap()
            .code,
        "spotify_busy"
    );
    service.retire(&own).await.unwrap();
    assert!(service.status().joined);
    let newer = owner();
    let next = SpotifyService::start(newer.clone(), unavailable(), Limits::default()).unwrap();
    next.ready().await.unwrap();
    assert_eq!(
        next.begin_state(&newer, Instant::now()).err().unwrap().code,
        "spotify_busy"
    );
    assert_eq!(
        results.pop().unwrap().into_reply(&own).err().unwrap().code,
        "spotify_shutdown"
    );
    let receipt = next
        .begin_state(&newer, Instant::now() + Duration::from_secs(1))
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(
        receipt.into_reply(&newer).err().unwrap().code,
        "spotify_test_unavailable"
    );
    drop(results);
    next.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn actual_lost_result_sender_keeps_whole_ticket_quota_until_collected_after_join() {
    let _serial = TEST_OWNER.lock().unwrap();
    let own = owner();
    let s = SpotifyService::start(own.clone(), unavailable(), Limits::default()).unwrap();
    s.ready().await.unwrap();
    s.0.shared
        .abandon_reply
        .store(true, std::sync::atomic::Ordering::Release);
    let held = s
        .begin_state(&own, Instant::now() + Duration::from_secs(1))
        .unwrap();
    // Wait for the real owner failure/exit notification, retaining the ticket.
    loop {
        let wake = s.0.shared.notify.notified();
        tokio::pin!(wake);
        wake.as_mut().enable();
        if s.0.shared.done.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }
        wake.await;
    }
    assert_eq!(s.shutdown().await.unwrap_err().code, "spotify_owner");
    assert!(s.status().drained);
    assert_eq!(s.status().retained, 1);
    let next_owner = owner();
    let next = SpotifyService::start(next_owner.clone(), unavailable(), Limits::default()).unwrap();
    next.ready().await.unwrap();
    let mut ready = vec![];
    for _ in 0..3 {
        ready.push(
            next.begin_state(&next_owner, Instant::now() + Duration::from_secs(1))
                .unwrap()
                .finish()
                .await
                .unwrap(),
        );
    }
    assert_eq!(
        next.begin_state(&next_owner, Instant::now())
            .err()
            .unwrap()
            .code,
        "spotify_busy"
    );
    assert_eq!(held.finish().await.err().unwrap().code, "spotify_owner");
    assert_eq!(next.status().retained, 3);
    drop(ready);
    next.shutdown().await.unwrap();
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::service::Gate;
    use std::sync::{atomic::Ordering, mpsc};
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new(name: &str) -> Self {
            Self(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures")
                    .join(name),
            )
        }
        fn backend(&self) -> Backend {
            Backend {
                executable: self.0.clone(),
                env: vec![],
                unavailable: None,
            }
        }
    }
    fn gate(slot: &Mutex<Option<Gate>>) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered, observed) = mpsc::channel();
        let (release, held) = mpsc::channel();
        *slot.lock().unwrap() = Some(Gate {
            entered,
            release: held,
        });
        (observed, release)
    }
    #[tokio::test(flavor = "current_thread")]
    async fn observed_real_process_panic_cleanup_retains_lease_then_sticky_shutdown_joins() {
        let _serial = TEST_OWNER.lock().unwrap();
        let fixture = Fixture::new("sleep.sh");
        let own = owner();
        let s = SpotifyService::start(own.clone(), fixture.backend(), Limits::default()).unwrap();
        assert_eq!(s.ready().await.unwrap(), Capability::Available);
        let (entered, release) = gate(&s.0.shared.reap_gate);
        s.0.shared.panic_after_spawn.store(true, Ordering::Release);
        let ticket = s
            .begin_state(&own, Instant::now() + Duration::from_secs(1))
            .unwrap();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let status = s.status();
        assert_ne!(status.active_pid, 0);
        assert_eq!(status.retained, 1);
        assert_eq!(status.spawned, 2);
        assert_eq!(status.reaped, 1);
        assert!(Path::new(&format!("/proc/{}", status.active_pid)).exists());
        drop(ticket);
        assert_eq!(
            s.status().retained,
            1,
            "drop cannot certify an unwaited child"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), s.shutdown())
                .await
                .is_err()
        );
        assert!(!s.status().joined);
        assert_eq!(s.status().retained, 1);
        release.send(()).unwrap();
        assert_eq!(
            s.shutdown().await.unwrap_err().code,
            "spotify_owner_panicked"
        );
        let done = s.status();
        assert!(done.joined);
        assert_eq!(done.spawned, done.reaped);
        assert_eq!(done.active_pid, 0);
        assert_eq!(done.retained, 0);
        assert!(!Path::new(&format!("/proc/{}", status.active_pid)).exists());
        assert_eq!(
            s.shutdown().await.unwrap_err().code,
            "spotify_owner_panicked"
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn bounded_observers_abort_timeout_and_retry_keep_same_real_exit_join() {
        let _serial = TEST_OWNER.lock().unwrap();
        let own = owner();
        let s = SpotifyService::start(own, unavailable(), Limits::default()).unwrap();
        s.ready().await.unwrap();
        let (entered, release) = gate(&s.0.shared.exit_gate);
        s.close();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut waits = vec![];
        for _ in 0..OBSERVER_CAPACITY {
            let copy = s.clone();
            waits.push(tokio::spawn(async move { copy.shutdown().await }));
        }
        for _ in 0..100 {
            if s.status().observers == OBSERVER_CAPACITY {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(s.status().observers, OBSERVER_CAPACITY);
        assert_eq!(
            s.shutdown().await.unwrap_err().code,
            "spotify_observer_busy"
        );
        waits.pop().unwrap().abort();
        tokio::task::yield_now().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(20), s.shutdown())
                .await
                .is_err()
        );
        assert!(!s.status().joined);
        assert!(SpotifyService::start(owner(), unavailable(), Limits::default()).is_err());
        release.send(()).unwrap();
        for waiter in waits {
            waiter.await.unwrap().unwrap();
        }
        s.shutdown().await.unwrap();
        assert!(s.status().joined);
        assert_eq!(s.status().observers, 0);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn process_resource_timeout_and_caller_cancel_have_actual_wait_receipts() {
        let _serial = TEST_OWNER.lock().unwrap();
        for fixture_name in ["stdout.sh", "stderr.sh"] {
            let fixture = Fixture::new(fixture_name);
            let own = owner();
            let s = SpotifyService::start(
                own.clone(),
                fixture.backend(),
                Limits {
                    stdout_bytes: 1,
                    stderr_bytes: 1,
                    ..Limits::default()
                },
            )
            .unwrap();
            s.ready().await.unwrap();
            let reply = s
                .begin_state(&own, Instant::now() + Duration::from_secs(1))
                .unwrap()
                .finish()
                .await
                .unwrap();
            assert_eq!(
                reply.into_reply(&own).err().unwrap().code,
                "spotify_output_limit"
            );
            assert_eq!(s.status().spawned, s.status().reaped);
            s.shutdown().await.unwrap();
        }
        let fixture = Fixture::new("sleep.sh");
        let own = owner();
        let s = SpotifyService::start(own.clone(), fixture.backend(), Limits::default()).unwrap();
        s.ready().await.unwrap();
        let reply = s
            .begin_state(&own, Instant::now() + Duration::from_millis(30))
            .unwrap()
            .finish()
            .await
            .unwrap();
        assert_eq!(
            reply.into_reply(&own).err().unwrap().code,
            "spotify_timeout"
        );
        assert_eq!(s.status().spawned, s.status().reaped);
        let ticket = s
            .begin_state(&own, Instant::now() + Duration::from_secs(1))
            .unwrap();
        ticket.cancel();
        let reply = ticket.finish().await.unwrap();
        assert_eq!(
            reply.into_reply(&own).err().unwrap().code,
            "spotify_cancelled"
        );
        s.shutdown().await.unwrap();
        assert!(s.status().joined);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn capability_probe_caller_abort_and_shutdown_retain_actual_child_wait() {
        let _serial = TEST_OWNER.lock().unwrap();
        let fixture = Fixture::new("probe-hold.sh");
        let own = owner();
        let s = SpotifyService::start(own, fixture.backend(), Limits::default()).unwrap();
        loop {
            let wake = s.0.shared.notify.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            if s.status().active_pid != 0 {
                break;
            }
            wake.await;
        }
        let (entered, release) = gate(&s.0.shared.reap_gate);
        let copy = s.clone();
        let observer = tokio::spawn(async move { copy.ready().await });
        tokio::task::yield_now().await;
        observer.abort();
        tokio::task::yield_now().await;
        s.close();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let active = s.status();
        assert_eq!(active.spawned, 1);
        assert_eq!(active.reaped, 0);
        assert_ne!(active.active_pid, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), s.shutdown())
                .await
                .is_err()
        );
        assert!(!s.status().joined);
        release.send(()).unwrap();
        s.shutdown().await.unwrap();
        let done = s.status();
        assert!(done.drained);
        assert_eq!(done.spawned, 1);
        assert_eq!(done.reaped, 1);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn unpolled_old_lifecycle_observers_keep_process_global_capacity_after_real_join() {
        let _serial = TEST_OWNER.lock().unwrap();
        let s = SpotifyService::start(owner(), unavailable(), Limits::default()).unwrap();
        s.ready().await.unwrap();
        let (entered, release) = gate(&s.0.shared.exit_gate);
        s.close();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut held = Vec::new();
        for _ in 0..OBSERVER_CAPACITY {
            let mut future = Box::pin(s.shutdown());
            assert!(futures_util::poll!(future.as_mut()).is_pending());
            held.push(future);
        }
        release.send(()).unwrap();
        held.remove(0).await.unwrap();
        assert!(s.status().joined);
        assert_eq!(s.status().observers, 3);
        let next = SpotifyService::start(owner(), unavailable(), Limits::default()).unwrap();
        next.ready().await.unwrap();
        assert_eq!(next.status().observers, 3);
        let (entered, release) = gate(&next.0.shared.exit_gate);
        next.close();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut fourth = Box::pin(next.shutdown());
        assert!(futures_util::poll!(fourth.as_mut()).is_pending());
        assert_eq!(next.status().observers, 4);
        assert_eq!(
            next.shutdown().await.unwrap_err().code,
            "spotify_observer_busy"
        );
        drop(held);
        assert_eq!(next.status().observers, 1);
        release.send(()).unwrap();
        fourth.await.unwrap();
        next.shutdown().await.unwrap();
        assert!(next.status().drained);
        assert_eq!(next.status().observers, 0);
    }
}
