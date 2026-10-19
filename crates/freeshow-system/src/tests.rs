use super::*;
use std::sync::mpsc as sync;

fn service() -> SystemService {
    SystemService::start(Limits {
        queue_capacity: 1,
        ..Limits::default()
    })
    .unwrap()
}
fn pause(service: &SystemService) -> (sync::Sender<()>, oneshot::Receiver<Result<Answer, Error>>) {
    let (entered, wait) = sync::channel();
    let (release, resumed) = sync::channel();
    let answer = service
        .admit(
            Operation::Pause {
                entered,
                release: resumed,
            },
            JobControl::default(),
        )
        .unwrap();
    wait.recv_timeout(Duration::from_secs(2)).unwrap();
    (release, answer)
}

#[tokio::test]
async fn bounded_admission_and_runtime_progress_while_os_worker_is_active() {
    let service = service();
    let (release, active) = pause(&service);
    let queued = service.admit(Operation::Os, JobControl::default()).unwrap();
    assert_eq!(
        service
            .get_os(JobControl::default())
            .await
            .unwrap_err()
            .code,
        "system_busy"
    );
    assert_eq!(service.status().active_jobs, 1);
    assert_eq!(service.status().queued_jobs, 1);
    tokio::time::timeout(
        Duration::from_secs(1),
        tokio::time::sleep(Duration::from_millis(5)),
    )
    .await
    .unwrap();
    release.send(()).unwrap();
    active.await.unwrap().unwrap();
    queued.await.unwrap().unwrap();
    service.shutdown().await.unwrap();
    assert!(!service.status().worker_running);
}

#[tokio::test]
async fn deadlines_include_queue_wait_and_cancelled_jobs_never_sample_os() {
    let service = service();
    let (release, _) = pause(&service);
    let queued = service
        .admit(
            Operation::Memory,
            JobControl {
                deadline: Instant::now() + Duration::from_millis(5),
                ..JobControl::default()
            },
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(15)).await;
    release.send(()).unwrap();
    assert_eq!(queued.await.unwrap().unwrap_err().code, "system_deadline");
    let (release, _) = pause(&service);
    let cancel = CancelToken::default();
    let queued = service
        .admit(
            Operation::Memory,
            JobControl {
                cancel: cancel.clone(),
                ..JobControl::default()
            },
        )
        .unwrap();
    cancel.cancel();
    release.send(()).unwrap();
    assert_eq!(queued.await.unwrap().unwrap_err().code, "system_cancelled");
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_rejects_new_work_discards_active_result_and_settles_queue() {
    let service = service();
    let (release, active) = pause(&service);
    let queued = service.admit(Operation::Os, JobControl::default()).unwrap();
    let closing = service.clone();
    let shutdown = tokio::spawn(async move { closing.shutdown().await });
    tokio::task::yield_now().await;
    assert!(service.status().closing);
    assert_eq!(
        service
            .get_device_name(JobControl::default())
            .await
            .unwrap_err()
            .code,
        "system_shutdown"
    );
    release.send(()).unwrap();
    assert_eq!(active.await.unwrap().unwrap_err().code, "system_shutdown");
    assert_eq!(queued.await.unwrap().unwrap_err().code, "system_shutdown");
    tokio::time::timeout(Duration::from_secs(1), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(service.status().active_jobs, 0);
    assert_eq!(service.status().queued_jobs, 0);
    assert!(!service.status().worker_running);
}

#[tokio::test]
async fn dropped_future_cancels_only_its_admitted_token_and_normal_completion_keeps_token() {
    let service = service();
    let (release, _) = pause(&service);
    let token = CancelToken::default();
    let worker = service.clone();
    let control = JobControl {
        cancel: token.clone(),
        ..JobControl::default()
    };
    let pending = tokio::spawn(async move { worker.get_os(control).await });
    tokio::task::yield_now().await;
    assert_eq!(service.status().queued_jobs, 1);
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    assert!(token.is_cancelled());
    release.send(()).unwrap();
    // Drain the cancelled queue slot before checking a successful request.
    tokio::time::timeout(Duration::from_secs(1), async {
        while service.status().queued_jobs > 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let token = CancelToken::default();
    service
        .get_os(JobControl {
            cancel: token.clone(),
            ..JobControl::default()
        })
        .await
        .unwrap();
    assert!(!token.is_cancelled());
    service.shutdown().await.unwrap();
}

#[test]
fn hard_pool_limits_reject_unbounded_configuration() {
    for capacity in [0, 33, usize::MAX] {
        assert!(
            matches!(SystemService::start(Limits { queue_capacity: capacity, ..Limits::default() }), Err(Error { code, .. }) if code == "system_invalid")
        );
    }
    assert!(SystemService::start(Limits {
        max_job_duration: Duration::ZERO,
        ..Limits::default()
    })
    .is_err());
    assert!(SystemService::start(Limits {
        max_job_duration: Duration::from_secs(11),
        ..Limits::default()
    })
    .is_err());
}

#[test]
fn linux_proc_semantics_use_memavailable_and_bounded_unsigned_kib_fields() {
    let sample = b"MemTotal: 16777216 kB\nMemFree: 1024 kB\nMemAvailable: 9000000 kB\n";
    assert_eq!(meminfo_field(sample, b"MemAvailable:"), 9_000_000 * 1024);
    assert_ne!(
        meminfo_field(sample, b"MemAvailable:"),
        meminfo_field(sample, b"MemFree:")
    );
    assert_eq!(meminfo_field(sample, b"MemTotal:"), 16 * 1024 * 1024 * 1024);
    assert_eq!(meminfo_field(b"MemAvailable: 0 kB", b"MemAvailable:"), 0);
    assert_eq!(meminfo_field(b"MemFree: 123 kB", b"MemAvailable:"), 0);
    assert_eq!(
        meminfo_field(b"MemAvailable: malformed kB", b"MemAvailable:"),
        0
    );
    assert_eq!(
        meminfo_field(b"MemAvailable: +123 kB", b"MemAvailable:"),
        123 * 1024
    );
    assert_eq!(
        meminfo_field(b"MemAvailable:\x0b123 kB", b"MemAvailable:"),
        123 * 1024
    );
    assert_eq!(
        meminfo_field(b"MemAvailable: -1 kB", b"MemAvailable:"),
        u64::MAX.wrapping_mul(1024)
    );
    assert_eq!(
        meminfo_field(b"MemAvailable: 18446744073709551616 kB", b"MemAvailable:"),
        u64::MAX.wrapping_mul(1024)
    );
    assert_eq!(
        meminfo_field(b"MemAvailable: 123 other-unit", b"MemAvailable:"),
        123 * 1024
    );
    assert_eq!(
        meminfo_field(b"\0MemAvailable: 123 kB", b"MemAvailable:"),
        0
    );
    let mut oversized = vec![b' '; 4095];
    oversized.extend_from_slice(b"MemAvailable: 123 kB");
    assert_eq!(meminfo_field(&oversized, b"MemAvailable:"), 0);
}

#[tokio::test]
async fn native_memory_has_provenance_and_original_reply_shape() {
    let service = service();
    let sample = service.memory_sample(JobControl::default()).await.unwrap();
    assert!(sample.total > 0);
    assert!(!sample.total_source.is_empty());
    assert!(!sample.free_source.is_empty());
    let usage = sample.usage();
    assert_eq!(usage.total, sample.total as f64);
    assert_eq!(usage.free, sample.free as f64);
    assert_eq!(
        usage.performance_mode,
        performance_mode(usage.total, usage.free)
    );
    #[cfg(target_os = "linux")]
    {
        assert!(["/proc/meminfo:MemTotal", "sysinfo.totalram*mem_unit"]
            .contains(&sample.total_source.as_str()));
        assert!(["/proc/meminfo:MemAvailable", "sysinfo.freeram*mem_unit"]
            .contains(&sample.free_source.as_str()));
    }
    service.shutdown().await.unwrap();
}

fn exit_service(panic_at_exit: bool) -> (SystemService, sync::Receiver<()>, sync::SyncSender<()>) {
    let (entered, waiting) = sync::sync_channel(1);
    let (release, resumed) = sync::sync_channel(1);
    let service = SystemService::start_inner(
        Limits::default(),
        WorkerHooks {
            exit: Some((entered, resumed)),
            panic_at_exit,
        },
    )
    .unwrap();
    (service, waiting, release)
}
async fn observed(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Actual native ownership observation did not become ready");
}

#[tokio::test]
async fn logical_completion_timeout_and_abandoned_waiter_retain_native_exit_then_real_join() {
    let (service, entered, release) = exit_service(false);
    service.0.close();
    observed(|| entered.try_recv().is_ok()).await;
    assert!(*service.0.completion.borrow());
    assert_eq!(service.status().active_jobs, 0);
    assert_eq!(service.status().queued_jobs, 0);
    assert!(service.status().worker_running);
    assert!(!service.status().worker_joined);
    let error = service
        .shutdown_until(Instant::now() + Duration::from_millis(10))
        .await
        .unwrap_err();
    assert_eq!(error.code, "system_shutdown_timeout");
    assert!(service.0.worker.lock().unwrap().is_some());
    assert_eq!(service.status().shutdown_observers, 0);
    let copy = service.clone();
    let waiter = tokio::spawn(async move { copy.shutdown().await });
    observed(|| service.status().shutdown_observers == 1).await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(service.status().shutdown_observers, 0);
    assert!(service.status().worker_running);
    assert!(!service.status().worker_joined);
    release.send(()).unwrap();
    observed(|| !service.status().worker_running).await;
    // Exit observation never performs the actual join or discards the handle.
    assert!(!service.status().worker_joined);
    service.shutdown().await.unwrap();
    assert!(service.status().worker_joined);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_observers_are_try_bounded_and_drop_does_not_release_the_native_owner() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let (service, entered, release) = exit_service(false);
    service.0.close();
    observed(|| entered.try_recv().is_ok()).await;
    let mut observers = (0..SHUTDOWN_OBSERVERS)
        .map(|_| Box::pin(service.shutdown()))
        .collect::<Vec<_>>();
    let mut context = Context::from_waker(Waker::noop());
    for observer in &mut observers {
        assert!(matches!(
            observer.as_mut().poll(&mut context),
            Poll::Pending
        ));
    }
    assert_eq!(service.status().shutdown_observers, SHUTDOWN_OBSERVERS);
    assert_eq!(service.shutdown().await.unwrap_err().code, "system_busy");
    observers.pop();
    assert_eq!(service.status().shutdown_observers, SHUTDOWN_OBSERVERS - 1);
    let mut replacement = Box::pin(service.shutdown());
    assert!(matches!(
        replacement.as_mut().poll(&mut context),
        Poll::Pending
    ));
    assert_eq!(service.status().shutdown_observers, SHUTDOWN_OBSERVERS);
    drop(replacement);
    drop(observers);
    assert_eq!(service.status().shutdown_observers, 0);
    assert!(service.status().worker_running);
    assert!(!service.status().worker_joined);
    release.send(()).unwrap();
    let one = service.clone();
    let two = service.clone();
    let (one, two) = tokio::join!(one.shutdown(), two.shutdown());
    one.unwrap();
    two.unwrap();
    assert!(service.status().worker_joined);
}

#[tokio::test]
async fn caught_job_panic_is_sticky_before_reply_and_still_requires_native_join() {
    let (service, entered, release) = exit_service(false);
    let answer = service
        .admit(Operation::Panic, JobControl::default())
        .unwrap();
    let error = answer.await.unwrap().unwrap_err();
    assert_eq!(error.code, "system_worker");
    assert_eq!(service.0.state.failure(), Some(error.clone()));
    service.0.close();
    observed(|| entered.try_recv().is_ok()).await;
    assert!(*service.0.completion.borrow());
    assert!(!service.status().worker_joined);
    assert_eq!(
        service
            .shutdown_until(Instant::now() + Duration::from_millis(10))
            .await
            .unwrap_err()
            .code,
        "system_shutdown_timeout"
    );
    release.send(()).unwrap();
    observed(|| !service.status().worker_running).await;
    assert_eq!(service.shutdown().await.unwrap_err(), error);
    assert!(service.status().worker_joined);
    assert_eq!(service.shutdown().await.unwrap_err(), error);
    assert_eq!(
        service
            .get_os(JobControl::default())
            .await
            .unwrap_err()
            .code,
        "system_shutdown"
    );
}

#[tokio::test]
async fn actual_native_join_panic_stays_failed_after_handle_is_joined() {
    let (service, entered, release) = exit_service(true);
    service.0.close();
    observed(|| entered.try_recv().is_ok()).await;
    assert!(*service.0.completion.borrow());
    assert_eq!(service.0.state.failure(), None);
    release.send(()).unwrap();
    observed(|| !service.status().worker_running).await;
    assert!(!service.status().worker_joined);
    let error = service.shutdown().await.unwrap_err();
    assert_eq!(error.code, "system_worker");
    assert!(service.status().worker_joined);
    assert_eq!(service.shutdown().await.unwrap_err(), error);
}

#[test]
fn no_runtime_pending_shutdown_retains_owner_and_finished_shutdown_joins_without_runtime() {
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    let (service, entered, release) = exit_service(false);
    service.0.close();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut context = Context::from_waker(Waker::noop());
    let result = pin!(service.shutdown()).as_mut().poll(&mut context);
    assert!(matches!(result, Poll::Ready(Err(Error { code, .. })) if code == "system_runtime"));
    assert!(!service.status().worker_joined);
    assert!(service.status().worker_running);
    assert_eq!(service.status().shutdown_observers, 0);
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while service.status().worker_running {
        assert!(
            Instant::now() < deadline,
            "Actual native owner did not exit"
        );
        std::thread::yield_now();
    }
    assert!(!service.status().worker_joined);
    assert!(matches!(
        pin!(service.shutdown()).as_mut().poll(&mut context),
        Poll::Ready(Ok(()))
    ));
    assert!(service.status().worker_joined);
}
