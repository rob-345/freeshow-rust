use super::*;
use serde_json::json;
use std::sync::Condvar;
use tempfile::TempDir;
fn context(dir: &TempDir) -> CopyContext {
    CopyContext::new(
        dir.path().join("data"),
        MediaSyncRoot::Configured(dir.path().join("media")),
        None,
        Arc::new(StoreSnapshot::new(
            json!({"projects":{}}),
            json!({}),
            json!({}),
        )),
    )
    .unwrap()
}
fn setup() -> (TempDir, CopyContext, CopyConfig, PathBuf) {
    let dir = TempDir::new().unwrap();
    let context = context(&dir);
    let stage = dir.path().join("stages");
    std::fs::create_dir(&stage).unwrap();
    let config = CopyConfig {
        staging_root: Some(stage),
        ..Default::default()
    };
    let file = dir.path().join("original.bin");
    std::fs::write(&file, b"opaque\0\xffmedia").unwrap();
    (dir, context, config, file)
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}
#[cfg(target_os = "linux")]
async fn drained(service: &MediaCopyService) {
    for _ in 0..100 {
        if service.inner.leases.available_permits() == service.inner.config.max_prepared {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("cleanup did not release retained leases");
}
fn park(
    service: &MediaCopyService,
) -> (
    Arc<(Mutex<bool>, Condvar)>,
    tokio::sync::mpsc::UnboundedReceiver<()>,
) {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let pause = gate.clone();
    let (send, receive) = tokio::sync::mpsc::unbounded_channel();
    *service.inner.hooks.process.lock().unwrap() = Some(Arc::new(move || {
        send.send(()).unwrap();
        let mut open = pause.0.lock().unwrap();
        while !*open {
            open = pause.1.wait(open).unwrap();
        }
    }));
    (gate, receive)
}
fn release(gate: &Arc<(Mutex<bool>, Condvar)>) {
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}
fn task(
    service: &MediaCopyService,
    context: &CopyContext,
    file: &Path,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<Result<PreparedMediaCopy>> {
    let service = service.clone();
    let context = context.clone();
    let data = json!({"paths":[file]});
    tokio::spawn(async move {
        service
            .prepare_with_cancel("MEDIA_FOLDER_COPY", &data, &context, cancel)
            .await
    })
}
#[tokio::test]
async fn terminal_and_both_admission_orderings() {
    let (_, context, config, _) = setup();
    assert!(MediaCopyService::new(CopyConfig {
        max_prepared: 0,
        ..config.clone()
    })
    .is_err());
    for after in [false, true] {
        let service = MediaCopyService::new(config.clone()).unwrap();
        let cancel = service.inner.cancel.clone();
        let hook: Box<dyn FnOnce() + Send> = Box::new(move || cancel.cancel());
        if after {
            *service.inner.hooks.after.lock().unwrap() = Some(hook);
        } else {
            *service.inner.hooks.before.lock().unwrap() = Some(hook);
        }
        assert_eq!(
            service
                .prepare("MEDIA_FOLDER_COPY", &json!({"paths":[]}), &context)
                .await
                .err()
                .unwrap()
                .code,
            "copy_shutdown"
        );
        service.shutdown().await.unwrap();
        service.shutdown().await.unwrap();
    }
}
#[test]
fn explicit_no_runtime_shutdown_can_be_finished_later() {
    let (_, _, config, _) = setup();
    let service = MediaCopyService::new(config).unwrap();
    {
        let mut future = std::pin::pin!(service.shutdown());
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match std::future::Future::poll(future.as_mut(), &mut cx) {
            std::task::Poll::Ready(result) => {
                assert_eq!(result.unwrap_err().code, "runtime_required")
            }
            _ => panic!("no-runtime drain waited"),
        }
    }
    runtime().block_on(service.shutdown()).unwrap();
}
#[tokio::test]
async fn workers_queue_and_dropped_caller_are_bounded() {
    let (_dir, context, config, file) = setup();
    let service = MediaCopyService::new(config).unwrap();
    let (gate, mut started) = park(&service);
    let first = task(&service, &context, &file, CancellationToken::new());
    let second = task(&service, &context, &file, CancellationToken::new());
    started.recv().await.unwrap();
    started.recv().await.unwrap();
    let third = task(&service, &context, &file, CancellationToken::new());
    let fourth = task(&service, &context, &file, CancellationToken::new());
    tokio::task::yield_now().await;
    assert_eq!(
        service
            .prepare("MEDIA_FOLDER_COPY", &json!({"paths":[]}), &context)
            .await
            .err()
            .unwrap()
            .code,
        "copy_busy"
    );
    fourth.abort();
    let _ = fourth.await;
    assert_eq!(service.inner.leases.available_permits(), 0);
    release(&gate);
    drop(first.await.unwrap());
    drop(second.await.unwrap());
    drop(third.await.unwrap());
    service.shutdown().await.unwrap();
    assert_eq!(service.inner.leases.available_permits(), 4);
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn staged_bytes_guards_and_deferred_reply_do_not_write_final_roots() {
    let (_dir, context, config, file) = setup();
    let service = MediaCopyService::new(config).unwrap();
    let plan = service
        .prepare("MEDIA_FOLDER_COPY", &json!({"paths":[file]}), &context)
        .await
        .unwrap();
    assert!(!plan.destination_root().exists());
    assert!(matches!(
        plan.deferred_reply(),
        DeferredReply::MediaFolderCopy {
            changed_if_effects_resolved: true
        }
    ));
    let installed: Vec<_> = plan
        .effects()
        .iter()
        .filter_map(|e| {
            if let CopyEffect::Install { file } = e {
                Some(file)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(installed.len(), 1);
    let install = installed[0];
    assert_eq!(
        std::fs::read(install.staged_path()).unwrap(),
        b"opaque\0\xffmedia"
    );
    assert_eq!(install.expected_staged().length, 13);
    assert_eq!(install.source().path(), file.to_str().unwrap());
    let stage = plan.staging_directory().to_owned();
    drop(plan);
    drained(&service).await;
    assert!(!stage.exists());
    service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn retained_leases_expiry_and_shutdown_cleanup_do_not_wait_for_host_drop() {
    let (_dir, context, mut config, file) = setup();
    config.max_prepared = 1;
    config.timeout = Duration::from_millis(500);
    config.prepared_ttl = Duration::from_millis(800);
    let service = MediaCopyService::new(config).unwrap();
    let plan = service
        .prepare("MEDIA_FOLDER_COPY", &json!({"paths":[file]}), &context)
        .await
        .unwrap();
    let stage = plan.staging_directory().to_owned();
    assert_eq!(
        service
            .prepare("MEDIA_FOLDER_COPY", &json!({"paths":[]}), &context)
            .await
            .err()
            .unwrap()
            .code,
        "copy_busy"
    );
    tokio::time::sleep(Duration::from_millis(950)).await;
    assert_eq!(plan.ensure_current().unwrap_err().code, "copy_plan_expired");
    assert_eq!(
        plan.pin_for_commit().err().unwrap().code,
        "copy_plan_expired"
    );
    drained(&service).await;
    assert!(!stage.exists());
    let other = service
        .prepare("MEDIA_FOLDER_COPY", &json!({"paths":[]}), &context)
        .await
        .unwrap();
    let other_stage = other.staging_directory().to_owned();
    service.shutdown().await.unwrap();
    assert!(!other_stage.exists());
    assert!(other.ensure_current().is_err());
    drop(other);
    drop(plan);
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn accepted_commit_pin_survives_ttl_plan_drop_and_wrong_shutdown_order() {
    let (_dir, context, mut config, file) = setup();
    config.timeout = Duration::from_millis(500);
    config.prepared_ttl = Duration::from_millis(800);
    let service = MediaCopyService::new(config).unwrap();
    let plan = service
        .prepare("MEDIA_FOLDER_COPY", &json!({"paths":[file]}), &context)
        .await
        .unwrap();
    let stage = plan.staging_directory().to_owned();
    let pin = plan.pin_for_commit().unwrap();
    assert_eq!(
        plan.pin_for_commit().err().unwrap().code,
        "invalid_commit_lease"
    );
    drop(plan);
    tokio::time::sleep(Duration::from_millis(950)).await;
    assert!(stage.exists());
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "copy_commit_in_progress"
    );
    assert!(stage.exists());
    drop(pin);
    drained(&service).await;
    service.shutdown().await.unwrap();
    assert!(!stage.exists());
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancellation_and_source_mutation_mid_copy_never_produce_install_success() {
    for mutation in [false, true] {
        let (_dir, context, config, file) = setup();
        std::fs::write(&file, [b'x'; 100000]).unwrap();
        let service = MediaCopyService::new(config).unwrap();
        let token = CancellationToken::new();
        let cancel = token.clone();
        let source = file.clone();
        let first = Arc::new(AtomicBool::new(true));
        *service.inner.hooks.chunk.lock().unwrap() = Some(Arc::new(move || {
            if !first.swap(false, Ordering::AcqRel) {
                return;
            }
            if mutation {
                use std::io::Write;
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&source)
                    .unwrap()
                    .write_all(b"changed")
                    .unwrap();
            } else {
                cancel.cancel();
            }
        }));
        let error = service
            .prepare_with_cancel(
                "MEDIA_FOLDER_COPY",
                &json!({"paths":[file]}),
                &context,
                token,
            )
            .await
            .err()
            .unwrap();
        assert_eq!(
            error.code,
            if mutation {
                "stale_source_guard"
            } else {
                "cancelled"
            }
        );
        service.shutdown().await.unwrap();
        assert_eq!(
            std::fs::read_dir(service.inner.config.staging_root.as_ref().unwrap())
                .unwrap()
                .count(),
            0
        );
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn guarded_existing_prefix_missing_directory_and_duplicate_semantics() {
    let (dir, context, config, file) = setup();
    let existing = dir.path().join("other.bin");
    std::fs::write(&existing, b"new").unwrap();
    let destination = Path::new(&context.sync_root)
        .join(paths::parent_folder_id(existing.to_str().unwrap()))
        .join("other.bin");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, b"keep").unwrap();
    let folder = dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    let prefix = format!("{}-sibling/real", context.sync_root);
    let service = MediaCopyService::new(config).unwrap();
    let plan = service
        .prepare(
            "MEDIA_FOLDER_COPY",
            &json!({"paths":[prefix,dir.path().join("missing"),file,file,existing,folder]}),
            &context,
        )
        .await
        .unwrap();
    assert_eq!(plan.observations().len(), 3);
    assert!(plan
        .effects()
        .iter()
        .any(|e| matches!(e, CopyEffect::SkipExisting { .. })));
    assert!(plan
        .effects()
        .iter()
        .any(|e| matches!(e, CopyEffect::CopyAttemptFailure { .. })));
    assert_eq!(std::fs::read(&destination).unwrap(), b"keep");
    drop(plan);
    service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn symlink_sources_are_guarded_but_destination_redirection_and_special_files_fail_explicitly()
{
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    let (dir, context, config, file) = setup();
    let link = dir.path().join("link");
    symlink(&file, &link).unwrap();
    let service = MediaCopyService::new(config.clone()).unwrap();
    let plan = service
        .prepare("MEDIA_FOLDER_COPY", &json!({"paths":[link]}), &context)
        .await
        .unwrap();
    assert!(plan
        .effects()
        .iter()
        .any(|e| matches!(e,CopyEffect::Install{file}if file.source.link.mode&0o170000==0o120000)));
    drop(plan);
    service.shutdown().await.unwrap();
    let fifo = dir.path().join("fifo");
    let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let service = MediaCopyService::new(config.clone()).unwrap();
    assert_eq!(
        service
            .prepare("MEDIA_FOLDER_COPY", &json!({"paths":[fifo]}), &context)
            .await
            .err()
            .unwrap()
            .code,
        "unsupported_file_type"
    );
    service.shutdown().await.unwrap();
    symlink(dir.path(), Path::new(&context.sync_root)).unwrap();
    let service = MediaCopyService::new(config).unwrap();
    assert_eq!(
        service
            .prepare("MEDIA_FOLDER_COPY", &json!({"paths":[file]}), &context)
            .await
            .err()
            .unwrap()
            .code,
        "unsupported_destination_type"
    );
    service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn file_output_structure_and_deadline_resource_failures_are_not_source_boolean_success() {
    let (dir, context, config, file) = setup();
    let data = json!({"paths":[file]});
    for config in [
        CopyConfig {
            file_bytes: 1,
            ..config.clone()
        },
        CopyConfig {
            copied_bytes: 1,
            ..config.clone()
        },
        CopyConfig {
            output_bytes: 1,
            ..config.clone()
        },
        CopyConfig {
            operations: 1,
            ..config.clone()
        },
    ] {
        let service = MediaCopyService::new(config).unwrap();
        assert_eq!(
            service
                .prepare("MEDIA_FOLDER_COPY", &data, &context)
                .await
                .err()
                .unwrap()
                .code,
            "copy_resource_limit"
        );
        service.shutdown().await.unwrap();
    }
    let shows = dir.path().join("data/Shows");
    std::fs::create_dir_all(&shows).unwrap();
    std::fs::write(
        shows.join("deep.show"),
        format!("{}0{}", "[".repeat(100), "]".repeat(100)),
    )
    .unwrap();
    let service = MediaCopyService::new(config.clone()).unwrap();
    assert_eq!(
        service
            .prepare("BUNDLE_MEDIA_FILES", &json!({}), &context)
            .await
            .err()
            .unwrap()
            .code,
        "copy_resource_limit"
    );
    service.shutdown().await.unwrap();
    let service = MediaCopyService::new(CopyConfig {
        timeout: Duration::from_millis(5),
        ..config
    })
    .unwrap();
    let (gate, mut started) = park(&service);
    let future = task(&service, &context, &file, CancellationToken::new());
    started.recv().await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    release(&gate);
    assert_eq!(future.await.unwrap().err().unwrap().code, "copy_timeout");
    service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn bounded_diagnostics_keep_processing_and_report_omissions() {
    let (dir, context, config, _) = setup();
    let folders: Vec<_> = (0..20)
        .map(|i| {
            let path = dir.path().join(format!("folder{i}"));
            std::fs::create_dir(&path).unwrap();
            path
        })
        .collect();
    let service = MediaCopyService::new(config).unwrap();
    let plan = service
        .prepare("MEDIA_FOLDER_COPY", &json!({"paths":folders}), &context)
        .await
        .unwrap();
    assert_eq!(plan.diagnostics().len(), 16);
    assert_eq!(plan.omitted_diagnostics(), 4);
    assert_eq!(
        plan.effects()
            .iter()
            .filter(|e| matches!(e, CopyEffect::CopyAttemptFailure { .. }))
            .count(),
        20
    );
    drop(plan);
    service.shutdown().await.unwrap();
}
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn native_platform_contract_is_explicitly_unsupported() {
    let (_dir, context, config, file) = setup();
    let service = MediaCopyService::new(config).unwrap();
    assert_eq!(
        service
            .prepare("MEDIA_FOLDER_COPY", &json!({"paths":[file]}), &context)
            .await
            .err()
            .unwrap()
            .code,
        "unsupported_platform"
    );
    service.shutdown().await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn one_service_keeps_frozen_stores_roots_and_commit_pins_separate() {
    let (left_dir, _, config, left_file) = setup();
    let (right_dir, _, _, right_file) = setup();
    let make = |dir: &TempDir, file: &Path| {
        CopyContext::new(
            dir.path().join("data"),
            MediaSyncRoot::Default,
            None,
            Arc::new(StoreSnapshot::new(
                json!({"projects":{"one":{"shows":[{"type":"video","id":file}]}}}),
                json!({}),
                json!({}),
            )),
        )
        .unwrap()
    };
    let left = make(&left_dir, &left_file);
    let right = make(&right_dir, &right_file);
    let service = MediaCopyService::new(config).unwrap();
    let data = json!({});
    let (a, b) = tokio::join!(
        service.prepare("BUNDLE_MEDIA_FILES", &data, &left),
        service.prepare("BUNDLE_MEDIA_FILES", &data, &right)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.candidates(), &[left_file.to_str().unwrap()]);
    assert_eq!(b.candidates(), &[right_file.to_str().unwrap()]);
    assert_ne!(a.destination_root(), b.destination_root());
    let pin = a.pin_for_commit().unwrap();
    a.validate_commit_lease(&pin).unwrap();
    assert_eq!(
        b.validate_commit_lease(&pin).unwrap_err().code,
        "invalid_commit_lease"
    );
    drop(pin);
    drop(a);
    drop(b);
    service.flush_released().await.unwrap();
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn cleanup_failure_racing_permit_admission_fails_closed_and_still_drains() {
    let (_dir, context, config, _) = setup();
    let service = MediaCopyService::new(config).unwrap();
    let failed = service.inner.cleanup.failed.clone();
    *service.inner.hooks.after.lock().unwrap() =
        Some(Box::new(move || failed.store(true, Ordering::Release)));
    assert_eq!(
        service
            .prepare("MEDIA_FOLDER_COPY", &json!({"paths":[]}), &context)
            .await
            .err()
            .unwrap()
            .code,
        "cleanup_failure"
    );
    assert_eq!(service.inner.leases.available_permits(), 4);
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "cleanup_failure"
    );
    assert!(service.inner.workers.lock().unwrap().is_empty());
    assert!(service.inner.cleanup_thread.lock().unwrap().is_none());
}
