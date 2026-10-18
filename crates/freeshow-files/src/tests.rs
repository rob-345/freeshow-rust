use super::*;
use std::{fs, sync::mpsc as sync, time::Duration};

fn pool() -> FilesService {
    FilesService::start(Limits {
        workers: 1,
        queue_capacity: 1,
        ..Limits::default()
    })
    .unwrap()
}
fn request() -> LocateRequest {
    LocateRequest {
        file_path: "/missing/locate-unit-test.png".into(),
        folders: Vec::new(),
    }
}
fn root() -> MediaRoot {
    MediaRoot {
        path: "/missing/media-locate-unit-test".into(),
        create_if_missing: false,
    }
}
fn pause(service: &FilesService) -> sync::Sender<()> {
    let (entered, wait) = sync::channel();
    let (release, resumed) = sync::channel();
    let _answer = service
        .admit(
            Operation::Pause {
                entered,
                release: resumed,
            },
            JobControl::default(),
        )
        .unwrap();
    wait.recv_timeout(Duration::from_secs(2)).unwrap();
    release
}

#[tokio::test]
async fn bounded_admission_cancelled_queue_and_runtime_responsiveness() {
    let service = pool();
    let release = pause(&service);
    let cancel = CancelToken::default();
    let queued = service
        .admit(
            Operation::Locate(request(), root()),
            JobControl {
                cancel: cancel.clone(),
                ..JobControl::default()
            },
        )
        .unwrap();
    assert_eq!(
        service
            .locate(request(), root(), JobControl::default())
            .await
            .unwrap_err()
            .code,
        "files_busy"
    );
    // A Tokio timer progresses while the independent filesystem worker waits.
    tokio::time::timeout(
        Duration::from_secs(1),
        tokio::time::sleep(Duration::from_millis(5)),
    )
    .await
    .unwrap();
    assert_eq!(service.status().active_jobs, 1);
    assert_eq!(service.status().queued_jobs, 1);
    cancel.cancel();
    release.send(()).unwrap();
    assert_eq!(queued.await.unwrap().err().unwrap().code, "files_cancelled");
    service.shutdown().await.unwrap();
    assert_eq!(service.status().workers, 0);
}

#[tokio::test]
async fn deadline_includes_queue_wait_and_shutdown_closes_admission_and_settles_jobs() {
    let service = pool();
    let release = pause(&service);
    let queued = service
        .admit(
            Operation::Locate(request(), root()),
            JobControl {
                deadline: Instant::now() + Duration::from_millis(5),
                ..JobControl::default()
            },
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(15)).await;
    release.send(()).unwrap();
    assert_eq!(queued.await.unwrap().err().unwrap().code, "files_deadline");
    let release = pause(&service);
    let queued = service
        .admit(Operation::Locate(request(), root()), JobControl::default())
        .unwrap();
    let shutdown = service.shutdown();
    tokio::pin!(shutdown);
    tokio::select! { result = &mut shutdown => panic!("Worker ended before release: {result:?}"), _ = tokio::time::sleep(Duration::from_millis(5)) => {} }
    assert_eq!(
        service
            .locate(request(), root(), JobControl::default())
            .await
            .unwrap_err()
            .code,
        "files_shutdown"
    );
    release.send(()).unwrap();
    shutdown.await.unwrap();
    assert_eq!(queued.await.unwrap().err().unwrap().code, "files_shutdown");
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropping_request_future_cancels_queued_work_without_store_or_file_writes() {
    let service = pool();
    let release = pause(&service);
    let worker = service.clone();
    let job = tokio::spawn(async move {
        worker
            .locate(request(), root(), JobControl::default())
            .await
    });
    tokio::task::yield_now().await;
    assert_eq!(service.status().queued_jobs, 1);
    job.abort();
    assert!(job.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(service.status().active_jobs, 0);
    assert_eq!(service.status().queued_jobs, 0);
}

#[tokio::test]
async fn budgets_bound_real_trees_and_directory_symlinks_do_not_recurse() {
    let temp = tempfile::tempdir().unwrap();
    let search = temp.path().join("search");
    fs::create_dir(&search).unwrap();
    for index in 0..5 {
        fs::write(search.join(format!("{index}.png")), "content").unwrap();
    }
    let root = MediaRoot {
        path: search.to_str().unwrap().into(),
        create_if_missing: false,
    };
    let service = FilesService::start(Limits {
        max_entries: 1,
        ..Limits::default()
    })
    .unwrap();
    let error = service
        .locate(request(), root.clone(), JobControl::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, "files_limit");
    assert_eq!(error.thumbnail_invalidations.len(), 2);
    service.shutdown().await.unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&search, search.join("loop")).unwrap();
        let service = pool();
        let result = service
            .locate(request(), root, JobControl::default())
            .await
            .unwrap();
        assert!(result.reply.is_none());
        assert_eq!(result.work.directories, 1);
        service.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn fallback_directory_creation_is_lazy_and_configured_roots_are_not_created() {
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("original.png");
    fs::write(&original, "media").unwrap();
    let service = pool();
    let root = MediaRoot::from_host(None, temp.path().to_str().unwrap());
    let result = service
        .locate(
            LocateRequest {
                file_path: original.to_str().unwrap().into(),
                folders: Vec::new(),
            },
            root.clone(),
            JobControl::default(),
        )
        .await
        .unwrap();
    assert!(!result.reply.unwrap().has_changed);
    assert!(!temp.path().join("Media").exists());
    let folder = service
        .media_folder_path(root, JobControl::default())
        .await
        .unwrap();
    assert!(Path::new(&folder.reply).is_dir());
    let configured = temp.path().join("not-created");
    service
        .media_folder_path(
            MediaRoot::from_host(Some(configured.to_str().unwrap()), "unused"),
            JobControl::default(),
        )
        .await
        .unwrap();
    assert!(!configured.exists());
    service.shutdown().await.unwrap();
}

use std::path::Path;

#[tokio::test]
async fn completed_or_rejected_requests_do_not_cancel_the_callers_token() {
    let service = pool();
    let release = pause(&service);
    let _queued = service
        .admit(Operation::Locate(request(), root()), JobControl::default())
        .unwrap();
    let token = CancelToken::default();
    let control = JobControl {
        cancel: token.clone(),
        ..JobControl::default()
    };
    assert_eq!(
        service
            .locate(request(), root(), control.clone())
            .await
            .unwrap_err()
            .code,
        "files_busy"
    );
    assert!(!token.is_cancelled());
    release.send(()).unwrap();
    _queued.await.unwrap().unwrap();
    service.media_folder_path(root(), control).await.unwrap();
    assert!(!token.is_cancelled());
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_worker_completion_and_repeated_shutdown_settle_every_worker() {
    let service = FilesService::start(Limits {
        workers: 4,
        queue_capacity: 8,
        ..Limits::default()
    })
    .unwrap();
    let mut releases = Vec::new();
    for _ in 0..4 {
        releases.push(pause(&service));
    }
    assert_eq!(service.status().active_jobs, 4);
    let closing = service.clone();
    let task = tokio::spawn(async move { closing.shutdown().await });
    tokio::task::yield_now().await;
    assert!(service.status().closing);
    for release in releases {
        release.send(()).unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(service.status().workers, 0);
    assert_eq!(service.status().active_jobs, 0);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn oversize_payload_is_rejected_before_retaining_a_queue_slot() {
    let service = FilesService::start(Limits {
        workers: 1,
        queue_capacity: 1,
        max_path_bytes: 16,
        ..Limits::default()
    })
    .unwrap();
    let release = pause(&service);
    let error = service
        .locate(
            LocateRequest {
                file_path: "x".repeat(17),
                folders: Vec::new(),
            },
            MediaRoot {
                path: "root".into(),
                create_if_missing: false,
            },
            JobControl::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "files_limit");
    assert_eq!(service.status().queued_jobs, 0);
    release.send(()).unwrap();
    service.shutdown().await.unwrap();
}
