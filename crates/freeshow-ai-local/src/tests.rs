use crate::*;
use std::{
    fs,
    sync::{atomic::Ordering, mpsc},
};
fn fixture(limits: Limits) -> (tempfile::TempDir, InventoryService) {
    let dir = tempfile::tempdir().unwrap();
    let service =
        InventoryService::start(TrustedUserData::from_host(dir.path()).unwrap(), limits).unwrap();
    (dir, service)
}
async fn observed(mut f: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !f() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn gate(service: &InventoryService, exit: bool) -> (mpsc::Receiver<()>, mpsc::SyncSender<()>) {
    let (entered, wait) = mpsc::sync_channel(1);
    let (release, resume) = mpsc::sync_channel(1);
    let hooks = service.test_hooks();
    let slot = if exit { &hooks.exit } else { &hooks.processing };
    *slot.lock().unwrap() = Some((entered, resume));
    (wait, release)
}
async fn query(service: &InventoryService) -> Result<Vec<BinFile>> {
    service
        .try_submit(Control::default())?
        .finish()
        .await?
        .into_result()
}

#[tokio::test]
async fn real_empty_missing_nested_files_and_engine_file_match_source_shape() {
    let (dir, service) = fixture(Limits::default());
    assert!(query(&service).await.unwrap().is_empty());
    let engine = dir.path().join("bin/nemotron");
    fs::create_dir_all(engine.join("models/nested")).unwrap();
    fs::write(engine.join("zero"), []).unwrap();
    assert!(query(&service).await.unwrap().is_empty());
    fs::write(engine.join("models/nested/音.bin"), [1, 2, 3, 4]).unwrap();
    let rows = query(&service).await.unwrap();
    assert_eq!(
        rows,
        vec![BinFile {
            path: engine.to_str().unwrap().into(),
            name: "nemotron",
            size: 4
        }]
    );
    fs::remove_dir_all(&engine).unwrap();
    fs::write(&engine, [1, 2, 3]).unwrap();
    assert!(query(&service).await.unwrap().is_empty());
    service.shutdown().await.unwrap();
    assert!(service.status().worker_joined);
}
#[tokio::test]
async fn actual_budget_failures_are_explicit_and_reclaim_whole_slots() {
    for limits in [
        Limits {
            entries: 1,
            ..Limits::default()
        },
        Limits {
            directories: 1,
            ..Limits::default()
        },
        Limits {
            depth: 0,
            ..Limits::default()
        },
        Limits {
            steps: 1,
            ..Limits::default()
        },
        Limits {
            path_bytes: 1,
            ..Limits::default()
        },
    ] {
        let (dir, service) = fixture(limits);
        let engine = dir.path().join("bin/nemotron");
        fs::create_dir_all(engine.join("child")).unwrap();
        fs::write(engine.join("child/a"), [1]).unwrap();
        fs::write(engine.join("b"), [2]).unwrap();
        assert_eq!(query(&service).await.err().unwrap().code, "ai_local_limit");
        assert_eq!(service.status().retained, 0);
        service.shutdown().await.unwrap();
    }
}
#[test]
fn exact_number_sum_and_native_root_limits_never_round_or_guess() {
    assert_eq!(
        engine::sum_for_test(MAX_EXACT_SIZE - 1, 1).unwrap(),
        MAX_EXACT_SIZE
    );
    for (a, b) in [(MAX_EXACT_SIZE, 1), (0, MAX_EXACT_SIZE + 1), (u64::MAX, 1)] {
        assert_eq!(
            engine::sum_for_test(a, b).unwrap_err().code,
            "ai_local_unsupported"
        );
    }
    for root in [
        PathBuf::from("relative"),
        PathBuf::from("/x/../y"),
        PathBuf::from("/x/./y"),
        PathBuf::from("/x//y"),
    ] {
        assert!(TrustedUserData::from_host(root).is_err());
    }
    assert!(InventoryService::start(
        TrustedUserData::from_host(std::env::temp_dir()).unwrap(),
        Limits {
            capacity: 5,
            ..Limits::default()
        }
    )
    .is_err());
}
#[tokio::test]
async fn four_ready_uncollected_and_borrowed_results_retain_whole_capacity_until_actual_drop() {
    let (dir, service) = fixture(Limits::default());
    fs::create_dir_all(dir.path().join("bin/nemotron")).unwrap();
    fs::write(dir.path().join("bin/nemotron/a"), [1]).unwrap();
    let mut tickets = Vec::new();
    for _ in 0..4 {
        tickets.push(service.try_submit(Control::default()).unwrap());
    }
    observed(|| tickets.iter().all(Ticket::ready_for_test)).await;
    assert_eq!(service.status().retained, 4);
    assert_eq!(
        service.try_submit(Control::default()).err().unwrap().code,
        "ai_local_busy"
    );
    let read = tickets.pop().unwrap().finish().await.unwrap();
    assert_eq!(read.result().unwrap()[0].size, 1);
    assert_eq!(service.status().retained, 4);
    drop(read);
    assert_eq!(service.status().retained, 3);
    drop(tickets);
    observed(|| service.status().retained == 0).await;
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn dropped_cancelled_and_expired_queued_requests_cannot_enter_late_or_free_active_owner() {
    let (_dir, service) = fixture(Limits::default());
    let (entered, release) = gate(&service, false);
    let active = service.try_submit(Control::default()).unwrap();
    observed(|| entered.try_recv().is_ok()).await;
    let dropped = service.try_submit(Control::default()).unwrap();
    drop(dropped.finish()); // Accepted Ticket captured by an unpolled dropped future.
    assert_eq!(service.status().retained, 2);
    let cancel = CancelToken::new();
    let cancelled = service
        .try_submit(Control {
            cancel: cancel.clone(),
            ..Control::default()
        })
        .unwrap();
    cancel.cancel();
    assert_eq!(
        cancelled.finish().await.err().unwrap().code,
        "ai_local_cancelled"
    );
    let timed = service
        .try_submit(Control {
            deadline: Instant::now() + Duration::from_millis(10),
            ..Control::default()
        })
        .unwrap();
    assert_eq!(
        timed.finish().await.err().unwrap().code,
        "ai_local_deadline"
    );
    assert_eq!(service.status().retained, 4);
    release.send(()).unwrap();
    active.finish().await.unwrap().into_result().unwrap();
    observed(|| service.status().retained == 0).await;
    assert_eq!(
        service.test_hooks().native_entries.load(Ordering::Acquire),
        1
    );
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn logical_done_and_timeout_do_not_join_a_held_native_exit_and_ready_results_also_block_drain(
) {
    let (_dir, service) = fixture(Limits {
        shutdown_timeout: Duration::from_millis(10),
        ..Limits::default()
    });
    let read = service
        .try_submit(Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    let (entered, release) = gate(&service, true);
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "ai_local_shutdown_timeout"
    );
    observed(|| entered.try_recv().is_ok()).await;
    assert!(service.test_hooks().logical_done.load(Ordering::Acquire));
    assert!(service.status().worker_running);
    assert!(!service.status().worker_joined);
    release.send(()).unwrap();
    observed(|| !service.status().worker_running).await;
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "ai_local_shutdown_timeout"
    );
    assert!(service.status().worker_joined);
    assert_eq!(service.status().retained, 1);
    drop(read);
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn aborting_shutdown_observer_preserves_actual_exit_owner_and_four_observers_are_bounded() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let (_dir, service) = fixture(Limits::default());
    let (entered, release) = gate(&service, true);
    let copy = service.clone();
    let waiter = tokio::spawn(async move { copy.shutdown().await });
    observed(|| entered.try_recv().is_ok()).await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(service.status().shutdown_observers, 0);
    assert!(!service.status().worker_joined);
    let mut observers = (0..4)
        .map(|_| Box::pin(service.shutdown()))
        .collect::<Vec<_>>();
    let mut context = Context::from_waker(Waker::noop());
    for f in &mut observers {
        assert!(matches!(f.as_mut().poll(&mut context), Poll::Pending));
    }
    assert_eq!(service.shutdown().await.unwrap_err().code, "ai_local_busy");
    drop(observers);
    release.send(()).unwrap();
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn real_worker_panic_is_sticky_and_does_not_fake_native_join() {
    let (_dir, service) = fixture(Limits::default());
    service.test_hooks().panic.store(true, Ordering::Release);
    let ticket = service.try_submit(Control::default()).unwrap();
    let error = match ticket.finish().await {
        Err(e) => e,
        Ok(read) => read.into_result().unwrap_err(),
    };
    assert_eq!(error.code, "ai_local_worker");
    let first = service.shutdown().await.unwrap_err();
    assert_eq!(first.code, "ai_local_worker");
    assert!(service.status().worker_joined);
    assert_eq!(service.shutdown().await.unwrap_err(), first);
}
#[test]
fn no_runtime_pending_teardown_keeps_owner_and_witnessed_exit_can_join_without_runtime() {
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    let (_dir, service) = fixture(Limits::default());
    let (entered, release) = gate(&service, true);
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(
        pin!(service.shutdown()).as_mut().poll(&mut context),
        Poll::Ready(Err(Error {
            code: "ai_local_runtime",
            ..
        }))
    ));
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(!service.status().worker_joined);
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while service.status().worker_running {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(matches!(
        pin!(service.shutdown()).as_mut().poll(&mut context),
        Poll::Ready(Ok(()))
    ));
    assert!(service.status().worker_joined);
}

#[tokio::test]
async fn genuine_thread_teardown_panic_is_joined_and_failure_stays_sticky() {
    let (_dir, service) = fixture(Limits::default());
    service
        .test_hooks()
        .panic_exit
        .store(true, Ordering::Release);
    let error = service.shutdown().await.unwrap_err();
    assert_eq!(error.code, "ai_local_worker");
    assert!(service.status().worker_joined);
    assert_eq!(service.shutdown().await.unwrap_err(), error);
}

#[tokio::test]
async fn logical_done_notification_does_not_complete_a_real_held_native_exit() {
    let (_dir, service) = fixture(Limits {
        shutdown_timeout: Duration::from_millis(10),
        ..Limits::default()
    });
    let (entered, release) = gate(&service, true);
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "ai_local_shutdown_timeout"
    );
    observed(|| entered.try_recv().is_ok()).await;
    assert!(service.test_hooks().logical_done.load(Ordering::Acquire));
    assert_eq!(service.status().retained, 0);
    assert!(service.status().worker_running);
    assert!(!service.status().worker_joined);
    release.send(()).unwrap();
    observed(|| !service.status().worker_running).await;
    service.shutdown().await.unwrap();
    assert!(service.status().worker_joined);
}
#[tokio::test]
async fn active_ticket_drop_keeps_native_frame_and_failed_send_disposal_before_slot_reuse() {
    let (_dir, service) = fixture(Limits {
        capacity: 1,
        shutdown_timeout: Duration::from_millis(10),
        ..Limits::default()
    });
    let (entered, release) = gate(&service, false);
    let ticket = service.try_submit(Control::default()).unwrap();
    observed(|| entered.try_recv().is_ok()).await;
    drop(ticket);
    assert_eq!(service.status().retained, 1);
    assert_eq!(
        service.try_submit(Control::default()).err().unwrap().code,
        "ai_local_busy"
    );
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "ai_local_shutdown_timeout"
    );
    release.send(()).unwrap();
    observed(|| !service.status().worker_running && service.status().retained == 0).await;
    assert_eq!(
        service.test_hooks().native_entries.load(Ordering::Acquire),
        0
    );
    service.shutdown().await.unwrap();
    assert!(service.status().worker_joined);
}

#[tokio::test]
async fn a_ready_reply_does_not_override_actual_cancel_or_the_total_observer_deadline() {
    let (_dir, service) = fixture(Limits::default());
    let cancel = CancelToken::new();
    let ticket = service
        .try_submit(Control {
            cancel: cancel.clone(),
            ..Control::default()
        })
        .unwrap();
    observed(|| ticket.ready_for_test()).await;
    cancel.cancel();
    assert_eq!(
        ticket.finish().await.err().unwrap().code,
        "ai_local_cancelled"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let ticket = service
        .try_submit(Control {
            deadline,
            ..Control::default()
        })
        .unwrap();
    observed(|| ticket.ready_for_test()).await;
    tokio::time::sleep_until(deadline.into()).await;
    assert_eq!(
        ticket.finish().await.err().unwrap().code,
        "ai_local_deadline"
    );
    observed(|| service.status().retained == 0).await;
    service.shutdown().await.unwrap();
}
