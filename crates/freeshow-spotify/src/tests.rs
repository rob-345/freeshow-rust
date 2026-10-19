#![allow(
    clippy::await_holding_lock,
    reason = "Serializes the mandatory process-global native owner tests"
)]
use super::*;
use std::{
    sync::{mpsc, Mutex},
    time::Instant,
};
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
fn new_gate() -> (Gate, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (entered, observed) = mpsc::channel();
    let (release, held) = mpsc::channel();
    (
        Gate {
            entered,
            release: held,
        },
        observed,
        release,
    )
}
fn gate(slot: &Mutex<Option<Gate>>) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (native, observed, release) = new_gate();
    *slot.lock().unwrap() = Some(native);
    (observed, release)
}
// An independent, owned bounded test releaser prevents the deliberately broken
// synchronous-join control from hanging the runtime that observes its timeout.
fn watchdog(release: mpsc::Sender<()>) -> (mpsc::Sender<()>, std::thread::JoinHandle<bool>) {
    let (now, wait) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let expired = matches!(
            wait.recv_timeout(Duration::from_secs(2)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        let _ = release.send(());
        expired
    });
    (now, thread)
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

#[tokio::test(flavor = "current_thread")]
async fn post_done_native_frame_preserves_timeout_observer_drop_and_sticky_actual_join_failure() {
    let _serial = TEST_OWNER.lock().unwrap();
    let s = SpotifyService::start(owner(), unavailable(), Limits::default()).unwrap();
    s.ready().await.unwrap();
    let (entered, release) = gate(&s.0.shared.post_done_gate);
    s.0.shared.panic_after_done.store(true, Ordering::Release);
    s.close();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let (release_now, releaser) = watchdog(release);
    assert!(s.0.shared.done.load(Ordering::Acquire));
    assert!(!s.status().running);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), s.shutdown())
            .await
            .is_err(),
        "done cannot make a synchronous native join defeat the observer timeout"
    );
    assert!(!s.status().joined);
    assert!(!s.status().drained);
    assert_eq!(s.status().observers, 0);
    assert_eq!(
        SpotifyService::start(owner(), unavailable(), Limits::default())
            .err()
            .unwrap()
            .code,
        "spotify_owner_busy"
    );
    let copy = s.clone();
    let observer = tokio::spawn(async move { copy.shutdown().await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while s.status().observers == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert_eq!(s.status().observers, 0);
    assert!(!s.status().joined);
    release_now.send(()).unwrap();
    assert!(
        !releaser.join().unwrap(),
        "positive case released explicitly"
    );
    let error = s.shutdown().await.unwrap_err();
    assert_eq!(error.code, "spotify_owner_panicked");
    assert!(s.status().drained);
    assert_eq!(s.status().terminal_error, Some(error.clone()));
    assert_eq!(s.shutdown().await.unwrap_err(), error);
    let next = SpotifyService::start(owner(), unavailable(), Limits::default()).unwrap();
    next.shutdown().await.unwrap();
}

#[test]
fn no_runtime_pending_post_done_exit_retains_handle_and_finished_exit_can_join() {
    use std::{
        future::Future,
        task::{Context as TaskContext, Poll, Waker},
    };
    let _serial = TEST_OWNER.lock().unwrap();
    let s = SpotifyService::start(owner(), unavailable(), Limits::default()).unwrap();
    let (entered, release) = gate(&s.0.shared.post_done_gate);
    s.close();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let (release_now, releaser) = watchdog(release);
    let mut context = TaskContext::from_waker(Waker::noop());
    assert!(matches!(
        std::pin::pin!(s.shutdown()).as_mut().poll(&mut context),
        Poll::Ready(Err(Error {
            code: "spotify_runtime",
            ..
        }))
    ));
    assert!(!s.status().joined);
    assert_eq!(s.status().observers, 0);
    release_now.send(()).unwrap();
    assert!(!releaser.join().unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while s
        .0
        .thread
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|thread| !thread.is_finished())
    {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(matches!(
        std::pin::pin!(s.shutdown()).as_mut().poll(&mut context),
        Poll::Ready(Ok(()))
    ));
    assert!(s.status().drained);
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::sync::atomic::Ordering;
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
    #[tokio::test(flavor = "current_thread")]
    async fn startup_help_panic_keeps_accepted_queued_lease_until_actual_child_wait() {
        let _serial = TEST_OWNER.lock().unwrap();
        let own = owner();
        let (after_spawn, entered, continue_probe) = new_gate();
        let (reap, reaping, release_reap) = new_gate();
        let s = SpotifyService::start_configured(
            own.clone(),
            Fixture::new("probe-hold.sh").backend(),
            Limits::default(),
            StartHooks {
                after_spawn: Some(after_spawn),
                reap: Some(reap),
                panic_after_spawn: true,
            },
        )
        .unwrap();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let ticket = s
            .begin_state(&own, Instant::now() + Duration::from_secs(2))
            .unwrap();
        assert_eq!(s.status().retained, 1);
        continue_probe.send(()).unwrap();
        reaping.recv_timeout(Duration::from_secs(5)).unwrap();
        let held = s.status();
        assert_eq!(held.spawned, 1);
        assert_eq!(held.reaped, 0);
        assert_eq!(held.retained, 1);
        assert_ne!(held.active_pid, 0);
        assert!(Path::new(&format!("/proc/{}", held.active_pid)).exists());
        assert_eq!(s.ready().await.unwrap_err().code, "spotify_owner_panicked");
        drop(ticket);
        assert_eq!(
            s.status().retained,
            1,
            "queued work owns the failed-send lease"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), s.shutdown())
                .await
                .is_err()
        );
        assert!(!s.status().joined);
        release_reap.send(()).unwrap();
        let error = s.shutdown().await.unwrap_err();
        assert_eq!(error.code, "spotify_owner_panicked");
        let finished = s.status();
        assert!(finished.drained);
        assert_eq!((finished.spawned, finished.reaped), (1, 1));
        assert_eq!(
            (finished.queries, finished.completed, finished.retained),
            (1, 1, 0)
        );
        assert!(!Path::new(&format!("/proc/{}", held.active_pid)).exists());
        assert_eq!(s.shutdown().await.unwrap_err(), error);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_reprobe_help_panic_keeps_active_lease_until_actual_child_wait() {
        let _serial = TEST_OWNER.lock().unwrap();
        let own = owner();
        let s = SpotifyService::start(
            own.clone(),
            Fixture::new("probe-hold.sh").backend(),
            Limits {
                query_timeout: Duration::from_millis(50),
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(
            s.ready().await.unwrap(),
            Capability::Unavailable {
                code: "spotify_timeout"
            }
        );
        assert_eq!((s.status().spawned, s.status().reaped), (1, 1));
        let (reaping, release_reap) = gate(&s.0.shared.reap_gate);
        s.0.shared.panic_after_spawn.store(true, Ordering::Release);
        let ticket = s
            .begin_state(&own, Instant::now() + Duration::from_secs(2))
            .unwrap();
        reaping.recv_timeout(Duration::from_secs(5)).unwrap();
        let held = s.status();
        assert_eq!((held.spawned, held.reaped, held.retained), (2, 1, 1));
        assert_ne!(held.active_pid, 0);
        assert!(Path::new(&format!("/proc/{}", held.active_pid)).exists());
        drop(ticket);
        assert_eq!(
            s.status().retained,
            1,
            "active HELP continuation still owns its lease"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), s.shutdown())
                .await
                .is_err()
        );
        assert!(!s.status().joined);
        release_reap.send(()).unwrap();
        let error = s.shutdown().await.unwrap_err();
        assert_eq!(error.code, "spotify_owner_panicked");
        let finished = s.status();
        assert!(finished.drained);
        assert_eq!((finished.spawned, finished.reaped), (2, 2));
        assert_eq!(
            (finished.queries, finished.completed, finished.retained),
            (1, 1, 0)
        );
        assert!(!Path::new(&format!("/proc/{}", held.active_pid)).exists());
        assert_eq!(s.shutdown().await.unwrap_err(), error);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn borrowed_reply_keeps_actual_result_quota_and_checks_foreign_and_retired_owner() {
        let _serial = TEST_OWNER.lock().unwrap();
        let own = owner();
        let s = SpotifyService::start(
            own.clone(),
            Fixture::new("stdout.sh").backend(),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(s.ready().await.unwrap(), Capability::Available);
        let mut receipts = Vec::new();
        for _ in 0..JOB_CAPACITY {
            receipts.push(
                s.begin_state(&own, Instant::now() + Duration::from_secs(2))
                    .unwrap()
                    .finish()
                    .await
                    .unwrap(),
            );
        }
        for receipt in &receipts {
            assert_eq!(
                receipt.reply(&own).unwrap().wire().data,
                serde_json::Value::Null
            );
            assert_eq!(s.status().retained, JOB_CAPACITY);
            assert_eq!(receipt.reply(&owner()).err().unwrap().code, "spotify_stale");
            assert_eq!(s.status().retained, JOB_CAPACITY);
        }
        assert_eq!(
            s.begin_state(&own, Instant::now()).err().unwrap().code,
            "spotify_busy"
        );
        s.shutdown().await.unwrap();
        assert!(s.status().drained);
        assert_eq!(s.status().retained, JOB_CAPACITY);
        let borrowed = receipts[0].reply(&own).err().unwrap();
        assert_eq!(borrowed.code, "spotify_shutdown");
        assert_eq!(
            receipts.pop().unwrap().into_reply(&own).err().unwrap(),
            borrowed
        );
        assert_eq!(s.status().retained, JOB_CAPACITY - 1);
        drop(receipts);
        assert_eq!(s.status().retained, 0);
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

#[test]
fn native_command_numeric_and_track_id_subset_is_explicit() {
    use crate::commands::{track_id, Command};
    for (value, expected) in [
        (0.0, "0"),
        (-0.0, "0"),
        (1e-7, "1e-7"),
        (1e21, "1e+21"),
        (-1.25, "-1.25"),
    ] {
        let Command::SetVolume(text) = PreparedCommand::new("setVolume", Some(value)).unwrap().0
        else {
            panic!()
        };
        assert_eq!(text, expected);
    }
    for (value, expected) in [
        (-0.0000005, "0"),
        (0.0000005, "1"),
        (-0.0000015, "-1"),
        (0.0000015, "2"),
    ] {
        let Command::Seek(text) = PreparedCommand::new("seek", Some(value)).unwrap().0 else {
            panic!()
        };
        assert_eq!(text, expected);
    }
    for value in [None, Some(f64::NAN), Some(f64::INFINITY)] {
        assert_eq!(
            PreparedCommand::new("setVolume", value).err().unwrap().code,
            "spotify_command_unsupported"
        );
    }
    assert!(PreparedCommand::new("seek", Some(1e20)).is_err());
    assert!(PreparedCommand::new("play", None).is_err());
    assert!(
        PreparedCommand::new("pause", Some(f64::NAN)).is_ok(),
        "source ignores unrelated value"
    );
    assert_eq!(
        track_id("string \"mpris:trackid\" variant string \"/track/01\"").unwrap(),
        Some("/track/01")
    );
    assert_eq!(
        track_id("string \"mpris:trackid\" variant object path \"/track/01\"").unwrap(),
        None
    );
    assert_eq!(
        track_id("string\u{feff}\"mpris:trackid\" variant string \"/\"").unwrap(),
        Some("/")
    );
    assert_eq!(
        track_id("string\u{85}\"mpris:trackid\" variant string \"/ok\"").unwrap(),
        None
    );
    for path in ["/x y", "/x;touch", "/x//y", "/x/", "/é", "relative"] {
        assert_eq!(
            track_id(&format!(
                "string \"mpris:trackid\" variant string \"{path}\""
            ))
            .unwrap_err()
            .code,
            "spotify_command_unsupported"
        );
    }
}
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "current_thread")]
async fn command_ready_results_keep_global_quota_and_native_seals_after_actual_join() {
    let _serial = TEST_OWNER.lock().unwrap();
    let own = owner();
    let service = SpotifyService::start(
        own.clone(),
        Backend {
            executable: Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stdout.sh"),
            env: vec![],
            unavailable: None,
        },
        Limits::default(),
    )
    .unwrap();
    service.ready().await.unwrap();
    let mut completed = Vec::new();
    for _ in 0..JOB_CAPACITY {
        let accepted = service
            .begin_command(
                &own,
                PreparedCommand::new("pause", None).unwrap(),
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        assert!(accepted.reply(&own).unwrap());
        assert_eq!(accepted.reply(&owner()).unwrap_err().code, "spotify_stale");
        let finished = accepted.finish().await.unwrap();
        assert_eq!(finished.result(&own).unwrap(), &CommandOutcome::Sent);
        assert_eq!(finished.result(&owner()).unwrap_err().code, "spotify_stale");
        completed.push(finished);
    }
    assert_eq!(service.status().retained, 4);
    assert_eq!(
        service
            .begin_state(&own, Instant::now())
            .err()
            .unwrap()
            .code,
        "spotify_busy"
    );
    assert_eq!(
        service
            .begin_command(
                &own,
                PreparedCommand::new("pause", None).unwrap(),
                Instant::now()
            )
            .err()
            .unwrap()
            .code,
        "spotify_busy"
    );
    service.shutdown().await.unwrap();
    assert!(service.status().drained);
    let next_owner = owner();
    let next = SpotifyService::start(
        next_owner.clone(),
        Backend {
            executable: Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stdout.sh"),
            env: vec![],
            unavailable: None,
        },
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        next.begin_command(
            &next_owner,
            PreparedCommand::new("pause", None).unwrap(),
            Instant::now()
        )
        .err()
        .unwrap()
        .code,
        "spotify_busy"
    );
    drop(completed);
    tokio::time::timeout(Duration::from_secs(5), async {
        while next.status().retained != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    next.shutdown().await.unwrap();
}

#[cfg(target_os = "linux")]
mod command_lifecycle {
    use super::*;
    fn backend() -> Backend {
        Backend {
            executable: Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sleep.sh"),
            env: vec![],
            unavailable: None,
        }
    }
    async fn observed(service: &SpotifyService, condition: impl Fn(&Status) -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !condition(&service.status()) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test(flavor = "current_thread")]
    async fn independent_query_and_commands_hold_real_children_and_drop_is_not_cancellation() {
        let _serial = TEST_OWNER.lock().unwrap();
        let own = owner();
        let s = SpotifyService::start(
            own.clone(),
            backend(),
            Limits {
                query_timeout: Duration::from_secs(5),
                ..Limits::default()
            },
        )
        .unwrap();
        s.ready().await.unwrap();
        let state = s
            .begin_state(&own, Instant::now() + Duration::from_secs(5))
            .unwrap();
        let a = s
            .begin_command(
                &own,
                PreparedCommand::new("pause", None).unwrap(),
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
        let b = s
            .begin_command(
                &own,
                PreparedCommand::new("next", None).unwrap(),
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
        observed(&s, |v| v.active_pid != 0 && v.command_pids.len() == 2).await;
        let before = s.status();
        for pid in std::iter::once(before.active_pid).chain(before.command_pids.clone()) {
            assert!(Path::new(&format!("/proc/{pid}")).exists());
        }
        let a_control = a.control();
        let b_control = b.control();
        assert!(!a_control.settled());
        assert!(!b_control.settled());
        assert!(a.reply(&own).unwrap());
        drop(a);
        drop(b.finish());
        assert_eq!(
            s.status().command_pids.len(),
            2,
            "dropping early receipt or unpolled completion never cancels actual accepted commands"
        );
        assert_eq!(s.status().retained, 3);
        let (entered, release) = gate(&s.0.shared.reap_gate);
        s.close();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), s.shutdown())
                .await
                .is_err()
        );
        assert!(!s.status().drained);
        assert!(!a_control.settled());
        assert!(
            !b_control.settled(),
            "held actual reap is not a settlement witness"
        );
        assert_eq!(
            s.status().retained,
            3,
            "actual held reap still owns all effects"
        );
        release.send(()).unwrap();
        s.shutdown().await.unwrap();
        assert!(s.status().drained);
        assert!(a_control.settled());
        assert!(b_control.settled());
        assert_eq!(s.status().commands_completed, 2);
        for pid in std::iter::once(before.active_pid).chain(before.command_pids) {
            assert!(!Path::new(&format!("/proc/{pid}")).exists());
        }
        assert_eq!(
            s.status().retained,
            1,
            "uncollected state result still owns its lease"
        );
        drop(state);
        assert_eq!(
            s.status().retained,
            0,
            "live monitoring handles do not retain job permits"
        );
        let next_owner = owner();
        let next = SpotifyService::start(
            next_owner.clone(),
            backend(),
            Limits {
                query_timeout: Duration::from_secs(5),
                ..Limits::default()
            },
        )
        .unwrap();
        next.ready().await.unwrap();
        let accepted = next
            .begin_command(
                &next_owner,
                PreparedCommand::new("pause", None).unwrap(),
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
        let next_control = accepted.control();
        observed(&next, |status| status.command_pids.len() == 1).await;
        a_control.cancel();
        b_control.cancel();
        assert!(
            !next_control.token.is_cancelled(),
            "old sealed controls cannot cancel a replacement run"
        );
        assert!(!next_control.settled());
        next_control.cancel();
        let finished = accepted.finish().await.unwrap();
        assert_eq!(
            finished.result(&next_owner).unwrap_err().code,
            "spotify_cancelled"
        );
        observed(&next, |_| next_control.settled()).await;
        drop(finished);
        next.shutdown().await.unwrap();
        assert!(next.status().drained);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_command_panic_keeps_all_child_anchors_until_kill_wait_and_sticky_join() {
        let _serial = TEST_OWNER.lock().unwrap();
        let own = owner();
        let s = SpotifyService::start(
            own.clone(),
            backend(),
            Limits {
                query_timeout: Duration::from_secs(5),
                ..Limits::default()
            },
        )
        .unwrap();
        s.ready().await.unwrap();
        for _ in 0..2 {
            drop(
                s.begin_command(
                    &own,
                    PreparedCommand::new("pause", None).unwrap(),
                    Instant::now() + Duration::from_secs(5),
                )
                .unwrap(),
            );
        }
        observed(&s, |v| v.command_pids.len() == 2).await;
        let (entered, release) = gate(&s.0.shared.reap_gate);
        s.0.shared.panic_after_spawn.store(true, Ordering::Release);
        let third = s
            .begin_command(
                &own,
                PreparedCommand::new("next", None).unwrap(),
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(third);
        let held = s.status();
        assert_eq!(held.command_pids.len(), 3);
        assert_eq!(held.retained, 3);
        assert!(!held.drained);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), s.shutdown())
                .await
                .is_err()
        );
        assert!(!s.status().joined);
        release.send(()).unwrap();
        let error = s.shutdown().await.unwrap_err();
        assert_eq!(error.code, "spotify_owner_panicked");
        let done = s.status();
        assert!(done.drained);
        assert_eq!(done.retained, 0);
        assert_eq!(done.commands_completed, 3);
        assert_eq!(done.spawned, 4);
        assert_eq!(done.reaped, 4);
        for pid in held.command_pids {
            assert!(!Path::new(&format!("/proc/{pid}")).exists());
        }
        assert_eq!(s.shutdown().await.unwrap_err(), error);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn queued_control_survives_early_reply_drop_and_actual_settlement_is_not_thread_join() {
        let _serial = TEST_OWNER.lock().unwrap();
        let own = owner();
        let (after_spawn, entered, release) = new_gate();
        let s = SpotifyService::start_configured(
            own.clone(),
            backend(),
            Limits::default(),
            StartHooks {
                after_spawn: Some(after_spawn),
                ..StartHooks::default()
            },
        )
        .unwrap();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let receipt = s
            .begin_command(
                &own,
                PreparedCommand::new("pause", None).unwrap(),
                Instant::now() + Duration::from_secs(2),
            )
            .unwrap();
        assert!(receipt.reply(&own).unwrap());
        let control = receipt.control();
        drop(receipt);
        assert!(!control.settled());
        assert_eq!(s.status().retained, 1);
        control.cancel();
        assert!(
            !control.settled(),
            "a queued cancellation signal is not actual disposal/settlement"
        );
        let (exiting, release_exit) = gate(&s.0.shared.exit_gate);
        s.close();
        assert!(!control.settled());
        release.send(()).unwrap();
        exiting.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(control.settled());
        assert_eq!(s.status().commands_completed, 1);
        assert_eq!(
            s.status().spawned,
            1,
            "cancelled queued work never entered a command child"
        );
        assert_eq!(s.status().reaped, 1);
        assert_eq!(s.status().retained, 0, "monitoring never owns job quota");
        assert!(!s.status().joined);
        assert!(!s.status().drained);
        release_exit.send(()).unwrap();
        s.shutdown().await.unwrap();
        assert!(s.status().drained);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn aggregate_scheduler_unwind_keeps_actual_child_leases_outside_dropped_futures() {
        let _serial = TEST_OWNER.lock().unwrap();
        let own = owner();
        let s = SpotifyService::start(
            own.clone(),
            backend(),
            Limits {
                query_timeout: Duration::from_secs(5),
                ..Limits::default()
            },
        )
        .unwrap();
        s.ready().await.unwrap();
        let mut controls = Vec::new();
        for _ in 0..2 {
            let accepted = s
                .begin_command(
                    &own,
                    PreparedCommand::new("pause", None).unwrap(),
                    Instant::now() + Duration::from_secs(5),
                )
                .unwrap();
            controls.push(accepted.control());
            drop(accepted);
        }
        observed(&s, |v| v.command_pids.len() == 2).await;
        let (entered, release) = gate(&s.0.shared.reap_gate);
        s.0.shared.panic_scheduler.store(true, Ordering::Release);
        drop(
            s.begin_command(
                &own,
                PreparedCommand::new("next", None).unwrap(),
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap(),
        );
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let held = s.status();
        assert_eq!(held.command_pids.len(), 2);
        assert_eq!(
            held.retained, 2,
            "dropped futures cannot release the only actual-child quota anchors"
        );
        assert!(!held.drained);
        assert!(
            controls.iter().all(|control| !control.settled()),
            "future Drop is not actual child settlement"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), s.shutdown())
                .await
                .is_err()
        );
        release.send(()).unwrap();
        assert_eq!(
            s.shutdown().await.unwrap_err().code,
            "spotify_owner_panicked"
        );
        assert!(s.status().drained);
        assert!(controls.iter().all(CommandControl::settled));
        assert_eq!(s.status().retained, 0);
        assert_eq!(s.status().spawned, 3);
        assert_eq!(s.status().reaped, 3);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn known_backend_capability_absence_never_gets_an_early_command_true() {
    let _serial = TEST_OWNER.lock().unwrap();
    let own = owner();
    let service = SpotifyService::start(own.clone(), unavailable(), Limits::default()).unwrap();
    service.ready().await.unwrap();
    assert_eq!(
        service
            .begin_command(
                &own,
                PreparedCommand::new("pause", None).unwrap(),
                Instant::now() + Duration::from_secs(1)
            )
            .err()
            .unwrap()
            .code,
        "spotify_test_unavailable"
    );
    assert_eq!(service.status().commands, 0);
    assert_eq!(service.status().retained, 0);
    service.shutdown().await.unwrap();
}
