use super::*;
use std::io::Write;
use std::sync::{Arc, Barrier};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};
fn payload(id: &str, paths: Vec<String>) -> Value {
    json!({"id":id,"paths":paths})
}
fn roots(dir: &TempDir) -> ImportRoots {
    ImportRoots {
        imports_root: dir.path().join("Imports"),
    }
}
fn zip(dir: &TempDir, name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
    let path = dir.path().join(name);
    let mut writer = ZipWriter::new(std::fs::File::create(&path).unwrap());
    for (name, bytes) in entries {
        writer
            .start_file(
                *name,
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
    path
}
fn paths(path: &Path) -> Vec<String> {
    vec![path.to_str().unwrap().into()]
}
async fn error(service: &ImportService, payload: &Value, dir: &TempDir) -> ImportError {
    match service.prepare("IMPORT_FILES", payload, roots(dir)).await {
        Err(error) => error,
        Ok(_) => panic!("expected rejection"),
    }
}
#[tokio::test]
async fn lease_limits_retained_staging_and_shutdown_never_waits_for_plan() {
    let dir = tempfile::tempdir().unwrap();
    let service = ImportService::new(ImportConfig {
        max_prepared: 1,
        ..ImportConfig::default()
    })
    .unwrap();
    let retained = service
        .prepare("IMPORT_FILES", &payload("txt", vec![]), roots(&dir))
        .await
        .unwrap();
    assert_eq!(
        error(&service, &payload("txt", vec![]), &dir).await.code,
        "busy"
    );
    service.shutdown().await.unwrap();
    assert_eq!(
        retained.plan.ensure_current().unwrap_err().code,
        "cancelled"
    );
    assert_eq!(
        error(&service, &payload("txt", vec![]), &dir).await.code,
        "shutdown"
    );
}
#[tokio::test]
async fn cancellation_deadline_limits_and_missing_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("file.txt");
    std::fs::write(&source, b"12345").unwrap();
    let service = ImportService::new(ImportConfig {
        text_bytes: 4,
        ..ImportConfig::default()
    })
    .unwrap();
    assert_eq!(
        error(&service, &payload("txt", paths(&source)), &dir)
            .await
            .code,
        "import_resource_limit"
    );
    let absent = service
        .prepare(
            "IMPORT_FILES",
            &payload("txt", paths(&dir.path().join("absent"))),
            roots(&dir),
        )
        .await
        .unwrap();
    assert_eq!(absent.events[0].message.data["data"][0]["content"], "");
    drop(absent);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(service
        .prepare_with_cancel(
            "IMPORT_FILES",
            &payload("txt", paths(&source)),
            roots(&dir),
            cancel
        )
        .await
        .is_err());
    service.shutdown().await.unwrap();
    let service = ImportService::new(ImportConfig {
        timeout: Duration::from_nanos(1),
        ..ImportConfig::default()
    })
    .unwrap();
    assert_eq!(
        error(&service, &payload("txt", paths(&source)), &dir)
            .await
            .code,
        "import_resource_limit"
    );
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn payloads_unknown_formats_and_output_limits_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let service = ImportService::new(ImportConfig {
        output_bytes: 40,
        ..ImportConfig::default()
    })
    .unwrap();
    assert_eq!(
        service
            .prepare("OTHER", &json!({}), roots(&dir))
            .await
            .err()
            .unwrap()
            .code,
        "unsupported_command"
    );
    for value in [
        json!({}),
        payload("pdf", vec!["a.pdf".into()]),
        payload("txt", vec!["x".repeat(32769)]),
        payload("txt", vec!["x\0y".into()]),
        json!({"id":"txt","paths":[1]}),
    ] {
        assert!(service
            .prepare("IMPORT_FILES", &value, roots(&dir))
            .await
            .is_err());
    }
    assert_eq!(
        error(&service, &payload("txt", vec!["absent.txt".into()]), &dir)
            .await
            .code,
        "import_resource_limit"
    );
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn unsafe_archive_paths_declared_sizes_and_encryption_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let service = ImportService::new(ImportConfig::default()).unwrap();
    let source = zip(&dir, "bad.project", &[("safe.txt", b"abc")]);
    let bytes = std::fs::read(&source).unwrap();
    let central = bytes
        .windows(4)
        .position(|bytes| bytes == b"PK\x01\x02")
        .unwrap();
    for (offset, value, expected) in [
        (central + 24, u32::MAX, "unsupported_archive"),
        (central + 24, 200_000_000, "import_resource_limit"),
        (central + 38, 0o120777u32 << 16, "import_invalid_path"),
    ] {
        let mut data = bytes.clone();
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        std::fs::write(&source, data).unwrap();
        let err = error(&service, &payload("freeshow_project", paths(&source)), &dir).await;
        assert_eq!(err.code, expected);
        assert_eq!(err.prior_events[0].message.data, "popup.importing");
    }
    let mut data = bytes.clone();
    data[6] |= 1;
    data[central + 8] |= 1;
    std::fs::write(&source, data).unwrap();
    let err = error(&service, &payload("freeshow_project", paths(&source)), &dir).await;
    assert_eq!(err.code, "import_encrypted_archive");
    assert_eq!(
        err.prior_events[1].message.data,
        "Can't decompress, this file is password protected!"
    );
    for malicious in [b"../x.txt".as_slice(), b"/bad.txt", b"C:/x.txt"] {
        let mut data = bytes.clone();
        data[30..38].copy_from_slice(malicious);
        data[central + 46..central + 54].copy_from_slice(malicious);
        std::fs::write(&source, data).unwrap();
        assert_eq!(
            error(&service, &payload("freeshow_project", paths(&source)), &dir)
                .await
                .code,
            "import_invalid_path"
        );
    }
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn crc_and_bundle_json_failures_do_not_publish_partial_imports() {
    let dir = tempfile::tempdir().unwrap();
    let source = zip(
        &dir,
        "corrupt.project",
        &[("data.json", b"{\"files\":[]}"), ("safe.txt", b"abc")],
    );
    let mut data = std::fs::read(&source).unwrap();
    let offset = data.windows(3).position(|bytes| bytes == b"abc").unwrap();
    data[offset] = b'z';
    std::fs::write(&source, data).unwrap();
    let service = ImportService::new(ImportConfig::default()).unwrap();
    let err = error(&service, &payload("freeshow_project", paths(&source)), &dir).await;
    assert_eq!(err.code, "import_zip_error");
    assert!(err
        .prior_events
        .iter()
        .all(|event| event.message.channel == "ALERT"));
    for content in ["{", "null", "{\"files\":123}"] {
        let source = zip(&dir, "data.project", &[("data.json", content.as_bytes())]);
        assert_eq!(
            error(&service, &payload("freeshow_project", paths(&source)), &dir)
                .await
                .code,
            "import_invalid_bundle"
        );
    }
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn staging_identity_hash_unresolved_and_no_final_writes() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let source = zip(
        &dir,
        "ok.project",
        &[
            (
                "data.json",
                b"{ \"files\": [\"/original/a.png\",\"/original/missing.png\"], \"unknown\":true }",
            ),
            ("a.png", b"bytes"),
        ],
    );
    let service = ImportService::new(ImportConfig {
        staging_root: Some(staging.clone()),
        ..ImportConfig::default()
    })
    .unwrap();
    let prepared = service
        .prepare(
            "IMPORT_FILES",
            &payload("freeshow_project", paths(&source)),
            roots(&dir),
        )
        .await
        .unwrap();
    assert!(!dir.path().join("Imports").exists());
    assert_eq!(prepared.plan.installs.len(), 1);
    assert_eq!(prepared.plan.unresolved.len(), 1);
    let install = &prepared.plan.installs[0];
    assert_eq!(
        install.expected_source,
        FileIdentity::read(&install.staged_path).unwrap()
    );
    assert_eq!(
        install.sha256,
        "277089d91c0bdf4f2e6862ba7e4a07605119431f5d13f726dd352b06f1b206a9"
    );
    assert!(prepared.events[1].message.data["data"][0]["content"]
        .as_str()
        .unwrap()
        .starts_with("{ \"files\""));
    let private = prepared.plan.staging_directory().unwrap().to_owned();
    service.shutdown().await.unwrap();
    assert!(private.exists());
    assert!(prepared.plan.ensure_current().is_err());
    drop(prepared);
    service.inner.cleanup.fence().await.unwrap();
    assert!(!private.exists());
    assert_eq!(std::fs::read_dir(staging).unwrap().count(), 0);
}
#[tokio::test]
async fn entry_count_depth_ratio_and_aggregate_bytes_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let source = zip(
        &dir,
        "two.zip",
        &[("a.txt", b"abc"), ("folder/b.txt", b"def")],
    );
    for config in [
        ImportConfig {
            max_entries: 1,
            ..ImportConfig::default()
        },
        ImportConfig {
            max_depth: 1,
            ..ImportConfig::default()
        },
        ImportConfig {
            expanded_bytes: 5,
            ..ImportConfig::default()
        },
        ImportConfig {
            input_bytes: 10,
            ..ImportConfig::default()
        },
        ImportConfig {
            archive_bytes: 10,
            ..ImportConfig::default()
        },
        ImportConfig {
            operations: 1,
            ..ImportConfig::default()
        },
    ] {
        let service = ImportService::new(config).unwrap();
        let code = error(&service, &payload("txt", paths(&source)), &dir)
            .await
            .code;
        assert!(matches!(
            code,
            "import_resource_limit" | "import_invalid_path"
        ));
        service.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn destination_symlink_and_overlapping_bundles_never_clobber() {
    let dir = tempfile::tempdir().unwrap();
    let source = zip(
        &dir,
        "one.project",
        &[
            ("data.json", b"{\"files\":[\"/original/a.png\"]}"),
            ("a.png", b"first"),
        ],
    );
    let second = zip(
        &dir,
        "two.project",
        &[
            ("data.json", b"{\"files\":[\"/original/a.png\"]}"),
            ("a.png", b"second"),
        ],
    );
    let service = ImportService::new(ImportConfig::default()).unwrap();
    let prepared = service
        .prepare(
            "IMPORT_FILES",
            &payload(
                "freeshow_project",
                vec![
                    source.to_str().unwrap().into(),
                    second.to_str().unwrap().into(),
                ],
            ),
            roots(&dir),
        )
        .await
        .unwrap();
    assert_eq!(prepared.plan.installs.len(), 1);
    assert_eq!(
        std::fs::read(&prepared.plan.installs[0].staged_path).unwrap(),
        b"first"
    );
    #[cfg(unix)]
    {
        let target = prepared
            .plan
            .imports_root
            .join(prepared.plan.installs[0].destination.as_path());
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(dir.path().join("absent"), target).unwrap();
        drop(prepared);
        assert_eq!(
            error(&service, &payload("freeshow_project", paths(&source)), &dir)
                .await
                .code,
            "import_invalid_path"
        );
    }
    service.shutdown().await.unwrap();
}
// Deterministically pause request admission on an independent runtime thread;
// shutdown must handle both the no-permit and already-held-permit orderings.
#[tokio::test]
async fn shutdown_racing_admission_cannot_start_a_late_worker() {
    for after in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let service = Arc::new(ImportService::new(ImportConfig::default()).unwrap());
        let reached = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let a = reached.clone();
        let b = release.clone();
        let hook: Box<dyn FnOnce() + Send> = Box::new(move || {
            a.wait();
            b.wait();
        });
        if after {
            *service.inner.after_admission.lock().unwrap() = Some(hook);
        } else {
            *service.inner.before_admission.lock().unwrap() = Some(hook);
        }
        let thread_service = service.clone();
        let root = roots(&dir);
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(thread_service.prepare("IMPORT_FILES", &payload("txt", vec![]), root))
                .err()
                .unwrap()
                .code
        });
        reached.wait();
        service.inner.cancel.cancel();
        service.inner.sender.lock().unwrap().take();
        if after {
            assert_eq!(
                service.inner.jobs.available_permits(),
                service.inner.config.workers - 1
            );
        } else {
            assert_eq!(
                service.inner.jobs.available_permits(),
                service.inner.config.workers
            );
        }
        release.wait();
        assert_eq!(thread.join().unwrap(), "shutdown");
        service.shutdown().await.unwrap();
        assert_eq!(
            service.inner.jobs.available_permits(),
            service.inner.config.workers
        );
    }
}

#[tokio::test]
async fn buffered_result_drop_and_full_reaper_queue_release_staging_leases() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let source = zip(
        &dir,
        "one.project",
        &[
            ("data.json", b"{\"files\":[\"/original/a.png\"]}"),
            ("a.png", b"first"),
        ],
    );
    let service = ImportService::new(ImportConfig {
        max_prepared: 16,
        staging_root: Some(staging.clone()),
        ..ImportConfig::default()
    })
    .unwrap();
    let data = payload("freeshow_project", paths(&source));
    let prepared = service
        .prepare("IMPORT_FILES", &data, roots(&dir))
        .await
        .unwrap();
    assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 1);
    // A completed result buffered in a oneshot is dropped on whichever thread
    // drops the receiver, including the canceled async caller. It must only
    // enqueue cleanup, with no timing assumptions about worker scheduling.
    let (send, receive) = tokio::sync::oneshot::channel();
    assert!(send.send(prepared).is_ok());
    drop(receive);
    service.inner.cleanup.fence().await.unwrap();
    assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
    let mut plans = vec![];
    for _ in 0..16 {
        plans.push(
            service
                .prepare("IMPORT_FILES", &data, roots(&dir))
                .await
                .unwrap(),
        );
    }
    assert_eq!(error(&service, &data, &dir).await.code, "busy");
    // All retained plans may close concurrently with shutdown, bounded by their
    // own leases rather than parser permits. Shutdown also has a reserved slot.
    let thread = std::thread::spawn(move || drop(plans));
    service.shutdown().await.unwrap();
    thread.join().unwrap();
    service.inner.cleanup.fence().await.unwrap();
    assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
    assert!(!service.cleanup_failed());
}

#[test]
fn shutdown_without_tokio_runtime_never_panics() {
    use std::future::Future;
    use std::task::{Context, Poll, Wake, Waker};
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let service = ImportService::new(ImportConfig::default()).unwrap();
    let mut future = Box::pin(service.shutdown());
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    for _ in 0..1000 {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(result) => {
                if let Err(error) = result {
                    assert_eq!(error.code, "runtime_required");
                }
                return;
            }
            Poll::Pending => std::thread::park_timeout(Duration::from_millis(1)),
        }
    }
    panic!("Shutdown failed to resolve without a runtime");
}

#[tokio::test]
async fn request_cancellation_revokes_retained_plans() {
    let dir = tempfile::tempdir().unwrap();
    let service = ImportService::new(ImportConfig::default()).unwrap();
    let cancel = CancellationToken::new();
    let prepared = service
        .prepare_with_cancel(
            "IMPORT_FILES",
            &payload("txt", vec![]),
            roots(&dir),
            cancel.clone(),
        )
        .await
        .unwrap();
    assert!(prepared.plan.ensure_current().is_ok());
    cancel.cancel();
    assert_eq!(
        prepared.plan.ensure_current().unwrap_err().code,
        "cancelled"
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_private_cleanup_is_diagnostic_and_stops_new_staging() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let source = zip(
        &dir,
        "one.project",
        &[
            ("data.json", b"{\"files\":[\"/original/a.png\"]}"),
            ("a.png", b"first"),
        ],
    );
    let service = ImportService::new(ImportConfig {
        staging_root: Some(staging),
        ..ImportConfig::default()
    })
    .unwrap();
    let prepared = service
        .prepare(
            "IMPORT_FILES",
            &payload("freeshow_project", paths(&source)),
            roots(&dir),
        )
        .await
        .unwrap();
    let private = prepared.plan.staging_directory().unwrap().to_owned();
    // Simulate external replacement of private staging with a non-directory.
    // Cleanup fails instead of silently releasing capacity and staging forever.
    std::fs::rename(&private, dir.path().join("moved-stage")).unwrap();
    std::fs::write(&private, b"unexpected replacement").unwrap();
    drop(prepared);
    assert_eq!(
        service.inner.cleanup.fence().await.unwrap_err().code,
        "cleanup_failure"
    );
    assert!(service.cleanup_failed());
    assert_eq!(
        error(&service, &payload("txt", vec![]), &dir).await.code,
        "cleanup_failure"
    );
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "cleanup_failure"
    );
}

#[tokio::test]
async fn cleanup_barrier_waits_released_staging_without_waiting_live_plans_or_bypassing_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let service = ImportService::new(ImportConfig {
        max_prepared: 1,
        ..ImportConfig::default()
    })
    .unwrap();
    let source = zip(
        &dir,
        "barrier.project",
        &[
            ("data.json", b"{\"files\":[\"/original/a.png\"]}"),
            ("a.png", b"original bytes"),
        ],
    );
    let prepared = service
        .prepare(
            "IMPORT_FILES",
            &payload("freeshow_project", paths(&source)),
            roots(&dir),
        )
        .await
        .unwrap();
    let staging = prepared.plan.staging_directory().unwrap().to_owned();
    tokio::time::timeout(Duration::from_secs(2), service.finish_cleanup())
        .await
        .unwrap()
        .unwrap();
    assert!(staging.is_dir());
    assert_eq!(
        error(&service, &payload("txt", vec![]), &dir).await.code,
        "busy"
    );
    drop(prepared);
    service.finish_cleanup().await.unwrap();
    assert!(!staging.exists());
    let next = service
        .prepare("IMPORT_FILES", &payload("txt", vec![]), roots(&dir))
        .await
        .unwrap();
    drop(next);
    service.shutdown().await.unwrap();
}
