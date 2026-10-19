use super::*;
use std::io::Write;
use std::sync::{mpsc, Arc};
use tokio::sync::oneshot;
use zip::{write::SimpleFileOptions, ZipWriter};

struct Pause {
    reached: Option<oneshot::Receiver<()>>,
    release: Option<mpsc::SyncSender<()>>,
}
impl Pause {
    fn new() -> (Self, Box<dyn FnOnce() + Send>) {
        let (entered, reached) = oneshot::channel();
        let (release, wait) = mpsc::sync_channel(1);
        (
            Self {
                reached: Some(reached),
                release: Some(release),
            },
            Box::new(move || {
                let _ = entered.send(());
                let _ = wait.recv();
            }),
        )
    }
    async fn entered(&mut self) {
        tokio::time::timeout(Duration::from_secs(3), self.reached.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
    fn release(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.try_send(());
        }
    }
}
impl Drop for Pause {
    fn drop(&mut self) {
        self.release();
    }
}
async fn staged(service: &ImportService, dir: &TempDir) -> PreparedImport {
    let path = dir.path().join("media.project");
    let mut archive = ZipWriter::new(std::fs::File::create(&path).unwrap());
    for (name, bytes) in [
        (
            "data.json",
            b"{\"files\":[\"/original/cover.png\"]}".as_slice(),
        ),
        ("cover.png", b"actual staged bytes".as_slice()),
    ] {
        archive
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(bytes).unwrap();
    }
    archive.finish().unwrap();
    service
        .prepare(
            "IMPORT_FILES",
            &json!({"id":"freeshow_project","paths":[path]}),
            ImportRoots {
                imports_root: dir.path().join("Imports"),
            },
        )
        .await
        .unwrap()
}
async fn wait_for(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn terminal_waits_live_stage_and_real_removal_across_abandoned_bounded_observers() {
    let dir = tempfile::tempdir().unwrap();
    let service = Arc::new(
        ImportService::new(ImportConfig {
            max_prepared: 1,
            ..ImportConfig::default()
        })
        .unwrap(),
    );
    let prepared = staged(&service, &dir).await;
    let stage = prepared.plan.staging_directory().unwrap().to_owned();
    service.close_preparation().await.unwrap();
    service.finish_cleanup().await.unwrap();
    assert!(stage.is_dir());
    assert_eq!(service.retained_available(), 0);
    assert_eq!(
        prepared.plan.ensure_current().unwrap_err().code,
        "cancelled"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), service.shutdown())
            .await
            .is_err()
    );
    assert!(!service.inner.cleanup.joined());
    assert_eq!(service.inner.cleanup.live(), 1);
    assert_eq!(
        service
            .prepare(
                "IMPORT_FILES",
                &json!({"id":"txt"}),
                ImportRoots {
                    imports_root: dir.path().join("Imports")
                }
            )
            .await
            .err()
            .unwrap()
            .code,
        "shutdown"
    );

    let (mut remove, hook) = Pause::new();
    service.inner.cleanup.before_remove(hook);
    drop(prepared);
    remove.entered().await;
    assert!(stage.is_dir());
    assert_eq!(service.retained_available(), 0);
    let mut observers = vec![];
    for _ in 0..4 {
        let service = service.clone();
        observers.push(tokio::spawn(async move { service.shutdown().await }));
    }
    wait_for(|| service.observers_available() == 0).await;
    assert_eq!(service.shutdown().await.unwrap_err().code, "lifecycle_busy");
    for observer in observers {
        observer.abort();
        let _ = observer.await;
    }
    assert_eq!(service.observers_available(), 4);
    assert!(!service.inner.cleanup.joined());
    assert!(stage.exists());
    assert_eq!(service.retained_available(), 0);
    remove.release();
    tokio::time::timeout(Duration::from_secs(3), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(service.inner.cleanup.joined());
    assert!(!stage.exists());
    assert_eq!(service.inner.cleanup.live(), 0);
    assert_eq!(service.retained_available(), 1);
}

#[tokio::test]
async fn abandoned_barriers_do_not_fill_queue_and_disposal_is_not_native_thread_exit() {
    let dir = tempfile::tempdir().unwrap();
    let service = Arc::new(
        ImportService::new(ImportConfig {
            max_prepared: 1,
            ..ImportConfig::default()
        })
        .unwrap(),
    );
    let prepared = staged(&service, &dir).await;
    let stage = prepared.plan.staging_directory().unwrap().to_owned();
    let (mut remove, hook) = Pause::new();
    service.inner.cleanup.before_remove(hook);
    drop(prepared);
    remove.entered().await;
    // The actual native remove is held throughout. Each canceled observer must
    // leave no queue message that can steal a later plan's removal slot.
    for _ in 0..8 {
        assert!(
            tokio::time::timeout(Duration::from_millis(5), service.finish_cleanup())
                .await
                .is_err()
        );
    }
    assert!(!service.cleanup_failed());
    assert!(stage.is_dir());
    assert_eq!(service.retained_available(), 0);
    remove.release();
    service.finish_cleanup().await.unwrap();
    assert!(!stage.exists());
    assert_eq!(service.retained_available(), 1);

    let (mut exit, hook) = Pause::new();
    service.inner.cleanup.before_exit(hook);
    let owner = service.clone();
    let observer = tokio::spawn(async move { owner.shutdown().await });
    exit.entered().await;
    observer.abort();
    let _ = observer.await;
    // Real negative control: no queued work/live stage and a successful ordinary
    // fence still do not prove this deliberately held native thread has exited.
    service.finish_cleanup().await.unwrap();
    assert_eq!(service.inner.cleanup.live(), 0);
    assert!(!service.inner.cleanup.thread_finished());
    assert!(!service.inner.cleanup.joined());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), service.shutdown())
            .await
            .is_err()
    );
    assert!(!service.inner.cleanup.joined());
    exit.release();
    tokio::time::timeout(Duration::from_secs(3), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(service.inner.cleanup.joined());
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn actual_cleanup_worker_panic_is_sticky_after_disposal_and_real_join() {
    let dir = tempfile::tempdir().unwrap();
    let service = ImportService::new(ImportConfig {
        max_prepared: 1,
        ..ImportConfig::default()
    })
    .unwrap();
    let prepared = staged(&service, &dir).await;
    let stage = prepared.plan.staging_directory().unwrap().to_owned();
    service
        .inner
        .cleanup
        .before_remove(Box::new(|| panic!("controlled native cleanup panic")));
    drop(prepared);
    // A first error is not drain proof: the panic can become visible before the
    // frame has disposed its owned TempDir. Observe actual disposal, then retry.
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "cleanup_failure"
    );
    wait_for(|| service.inner.cleanup.live() == 0).await;
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "cleanup_failure"
    );
    assert!(service.inner.cleanup.joined());
    assert!(!stage.exists());
    assert_eq!(service.retained_available(), 1);
    assert_eq!(
        service.close_preparation().await.unwrap_err().code,
        "cleanup_failure"
    );
}

#[tokio::test]
async fn failed_cleanup_barrier_still_waits_for_its_other_accepted_native_removals() {
    let dir = tempfile::tempdir().unwrap();
    let service = ImportService::new(ImportConfig::default()).unwrap();
    let failed = staged(&service, &dir).await;
    let other = staged(&service, &dir).await;
    let failed_path = failed.plan.staging_directory().unwrap().to_owned();
    let other_path = other.plan.staging_directory().unwrap().to_owned();
    std::fs::rename(&failed_path, dir.path().join("externally-moved-stage")).unwrap();
    std::fs::write(&failed_path, b"obstruct actual directory cleanup").unwrap();
    drop(failed);
    wait_for(|| service.inner.cleanup.live() == 1).await;
    assert!(service.cleanup_failed());
    let (mut remove, hook) = Pause::new();
    service.inner.cleanup.before_remove(hook);
    drop(other);
    remove.entered().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), service.finish_cleanup())
            .await
            .is_err()
    );
    assert!(other_path.is_dir());
    assert_eq!(service.retained_available(), 1);
    remove.release();
    assert_eq!(
        service.finish_cleanup().await.unwrap_err().code,
        "cleanup_failure"
    );
    assert!(!other_path.exists());
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "cleanup_failure"
    );
    assert!(service.inner.cleanup.joined());
}

#[tokio::test]
async fn receiver_panic_retains_later_stage_and_original_permit_until_final_disposal() {
    let dir = tempfile::tempdir().unwrap();
    let service = ImportService::new(ImportConfig::default()).unwrap();
    let first = staged(&service, &dir).await;
    let later = staged(&service, &dir).await;
    let stage = later.plan.staging_directory().unwrap().to_owned();
    let slots = later.plan.lease.as_ref().unwrap().semaphore().clone();
    service
        .inner
        .cleanup
        .before_remove(Box::new(|| panic!("controlled receiver failure")));
    drop(first);
    wait_for(|| service.inner.cleanup.thread_finished()).await;
    assert_eq!(slots.available_permits(), 1);
    // This release reaches the actual disconnected receiver. It must retain
    // both directory and permit, rather than keep(path) and release quota or
    // perform filesystem cleanup on this plan's releasing thread.
    drop(later);
    assert!(stage.is_dir());
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "cleanup_failure"
    );
    assert!(service.inner.cleanup.joined());
    assert_eq!(service.inner.cleanup.live(), 1);
    assert!(stage.is_dir());
    assert_eq!(slots.available_permits(), 1);
    drop(service);
    assert!(!stage.exists());
    assert_eq!(slots.available_permits(), 2);
}

#[tokio::test]
async fn final_owner_drop_retains_actual_join_instead_of_detaching_cleanup() {
    let service = ImportService::new(ImportConfig::default()).unwrap();
    service.close_preparation().await.unwrap();
    let (mut exit, hook) = Pause::new();
    service.inner.cleanup.before_exit(hook);
    let (done, mut observe) = oneshot::channel();
    let owner = std::thread::spawn(move || {
        drop(service);
        let _ = done.send(());
    });
    exit.entered().await;
    // This is an actual native-exit gate, not a scheduling delay. Final Drop
    // cannot have returned merely because the receiver was closed.
    let detached = tokio::time::timeout(Duration::from_millis(20), &mut observe)
        .await
        .is_ok();
    exit.release();
    if !detached {
        tokio::time::timeout(Duration::from_secs(3), observe)
            .await
            .unwrap()
            .unwrap();
    }
    wait_for(|| owner.is_finished()).await;
    owner.join().unwrap();
    assert!(!detached, "Final owner detached the cleanup thread");
}

#[test]
fn pending_no_runtime_shutdown_retains_handle_and_finished_retry_joins_without_runtime() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let service = ImportService::new(ImportConfig::default()).unwrap();
    runtime.block_on(service.close_preparation()).unwrap();
    drop(runtime);
    let (mut exit, hook) = Pause::new();
    service.inner.cleanup.before_exit(hook);
    let mut context = Context::from_waker(Waker::noop());
    let mut pending = Box::pin(service.shutdown());
    assert!(
        matches!(pending.as_mut().poll(&mut context), Poll::Ready(Err(error)) if error.code == "runtime_required")
    );
    drop(pending);
    let mut entered = exit.reached.take().unwrap();
    let started = Instant::now();
    while entered.try_recv().is_err() {
        assert!(started.elapsed() < Duration::from_secs(3));
        std::thread::yield_now();
    }
    assert!(!service.inner.cleanup.thread_finished());
    assert!(!service.inner.cleanup.joined());
    exit.release();
    let started = Instant::now();
    while !service.inner.cleanup.thread_finished() {
        assert!(started.elapsed() < Duration::from_secs(3));
        std::thread::yield_now();
    }
    assert!(matches!(
        Box::pin(service.shutdown()).as_mut().poll(&mut context),
        Poll::Ready(Ok(()))
    ));
    assert!(service.inner.cleanup.joined());
}
