use super::*;

fn context() -> Context {
    Context {
        runtime_id: "test".into(),
        library_generation: 1,
        data_path: "/library".into(),
    }
}
fn grant(path: &std::path::Path) -> NativeGrant {
    NativeGrant {
        path: path.to_owned(),
        source_path: path.to_str().unwrap().into(),
        provenance: Provenance::NativeSelection,
        protection: Protection::Plain,
    }
}
async fn server() -> AssetServer {
    AssetServer::start(
        Config {
            file_workers: 1,
            file_queue_capacity: 1,
            shutdown_timeout: Duration::from_millis(100),
            ..Config::default()
        },
        context(),
    )
    .await
    .unwrap()
}
async fn settle(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn file(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, b"real bytes").unwrap();
    path
}

#[tokio::test(flavor = "current_thread")]
async fn bounded_queue_rejects_excess_and_context_cancels_queued_work_before_open() {
    let directory = tempfile::tempdir().unwrap();
    let held = file(&directory, "held.png");
    let queued = file(&directory, "queued.png");
    let service = server().await;
    let (entered, release) = service.0.shared.pool.pause_for(held.clone());
    let first = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .authorize(&context(), grant(&held), Control::default())
                .await
        })
    };
    entered.await.unwrap();
    let second = {
        let service = service.clone();
        let queued = queued.clone();
        tokio::spawn(async move {
            service
                .authorize(&context(), grant(&queued), Control::default())
                .await
        })
    };
    settle(|| service.status().queued_io == 1).await;
    assert_eq!(
        service
            .authorize(&context(), grant(&queued), Control::default())
            .await
            .unwrap_err()
            .code,
        "assets_busy"
    );
    service
        .activate_context(Context {
            library_generation: 2,
            ..context()
        })
        .unwrap();
    assert_eq!(
        second.await.unwrap().unwrap_err().code,
        "assets_stale_context"
    );
    assert_eq!(
        first.await.unwrap().unwrap_err().code,
        "assets_stale_context"
    );
    assert_eq!(service.status().open_files, 0);
    release.send(()).unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(service.status().grants, 0);
    assert_eq!(service.status().queued_io, 0);
    assert_eq!(service.status().active_io, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_cancel_and_deadline_reply_while_the_native_worker_is_held() {
    for deadline in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = file(&directory, "held.png");
        let service = server().await;
        let (entered, release) = service.0.shared.pool.pause_for(path.clone());
        let control = Control::new(
            Instant::now() + Duration::from_millis(if deadline { 20 } else { 2000 }),
            CancellationToken::new(),
        );
        let cancel = control.cancel.clone();
        let task = {
            let service = service.clone();
            tokio::spawn(async move { service.authorize(&context(), grant(&path), control).await })
        };
        entered.await.unwrap();
        if !deadline {
            cancel.cancel();
        }
        assert_eq!(
            task.await.unwrap().unwrap_err().code,
            if deadline {
                "assets_deadline"
            } else {
                "assets_cancelled"
            }
        );
        assert_eq!(service.status().active_io, 1);
        assert_eq!(service.status().grants, 0);
        release.send(()).unwrap();
        service.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn aborting_batch_rolls_back_new_grants_without_revoking_deduplicated_existing_grants() {
    let directory = tempfile::tempdir().unwrap();
    let existing = file(&directory, "existing.png");
    let new = file(&directory, "new.png");
    let held = file(&directory, "held.png");
    let service = server().await;
    let old = service
        .authorize(&context(), grant(&existing), Control::default())
        .await
        .unwrap();
    let (entered, release) = service.0.shared.pool.pause_for(held.clone());
    let task = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .authorize_batch(
                    &context(),
                    vec![grant(&existing), grant(&new), grant(&held)],
                    Control::default(),
                )
                .await
        })
    };
    entered.await.unwrap();
    assert_eq!(service.status().grants, 2);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(service.status().grants, 1);
    assert!(service
        .0
        .shared
        .registry
        .lock()
        .unwrap()
        .grants
        .contains_key(&old.id));
    release.send(()).unwrap();
    service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_has_a_deadline_for_uninterruptible_os_work_and_never_reopens_admission() {
    let directory = tempfile::tempdir().unwrap();
    let path = file(&directory, "held.png");
    let service = server().await;
    let (entered, release) = service.0.shared.pool.pause_for(path.clone());
    let task = {
        let service = service.clone();
        let path = path.clone();
        tokio::spawn(async move {
            service
                .authorize(&context(), grant(&path), Control::default())
                .await
        })
    };
    entered.await.unwrap();
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "assets_shutdown_timeout"
    );
    assert_eq!(
        service
            .authorize(&context(), grant(&path), Control::default())
            .await
            .unwrap_err()
            .code,
        "assets_shutdown"
    );
    assert_eq!(service.status().active_io, 1);
    release.send(()).unwrap();
    assert_eq!(task.await.unwrap().unwrap_err().code, "assets_shutdown");
    service.shutdown().await.unwrap();
    service.shutdown().await.unwrap();
    let status = service.status();
    assert!(!status.listener_running);
    assert_eq!(status.active_io, 0);
    assert_eq!(status.open_files, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn file_size_and_registry_bytes_are_enforced_before_publication() {
    let directory = tempfile::tempdir().unwrap();
    let path = file(&directory, "size.png");
    for config in [
        Config {
            max_file_bytes: 1,
            ..Config::default()
        },
        Config {
            max_registry_bytes: 1,
            ..Config::default()
        },
    ] {
        let service = AssetServer::start(config, context()).await.unwrap();
        assert_eq!(
            service
                .authorize(&context(), grant(&path), Control::default())
                .await
                .unwrap_err()
                .code,
            "assets_limit"
        );
        assert_eq!(service.status().grants, 0);
        service.shutdown().await.unwrap();
    }
}
