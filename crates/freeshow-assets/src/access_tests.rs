use super::*;
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

fn context() -> Context {
    Context {
        runtime_id: "native-open".into(),
        library_generation: 1,
        data_path: "/private/日本語".into(),
    }
}
fn grant(path: &Path) -> NativeGrant {
    NativeGrant {
        source_path: "exact 日本語 literal% source.wav".into(),
        path: path.into(),
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
async fn settled(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn source(dir: &tempfile::TempDir) -> PathBuf {
    let path = dir.path().join("source 日本語.wav");
    fs::write(&path, b"first real bytes").unwrap();
    path
}
async fn opened(server: &AssetServer) -> AuthorizedMediaRead {
    server
        .open_registered_media(
            &context(),
            RegisteredMedia::SourceKey("exact 日本語 literal% source.wav"),
            Control::default(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn only_existing_current_registry_keys_and_ids_open_actual_bytes_without_creating_grants() {
    let dir = tempfile::tempdir().unwrap();
    let path = source(&dir);
    let server = server().await;
    let public = server
        .authorize(&context(), grant(&path), Control::default())
        .await
        .unwrap();
    let mut read = opened(&server).await;
    assert_eq!(read.authority().source_path(), public.source_path);
    assert_eq!(read.authority().context(), &context());
    assert_eq!(
        read.authority().canonical_path(),
        fs::canonicalize(&path).unwrap()
    );
    let before = read
        .authority()
        .revalidate(Control::default())
        .await
        .unwrap();
    let mut bytes = Vec::new();
    read.file_mut().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"first real bytes");
    read.file_mut().seek(SeekFrom::Start(0)).unwrap();
    let after = read
        .authority()
        .revalidate(Control::default())
        .await
        .unwrap();
    assert!(before.same_file(&after));
    assert!(before.same_version(&after));
    assert_eq!(after.length(), 16);
    let by_id = server
        .open_registered_media(
            &context(),
            RegisteredMedia::GrantId(&public.id),
            Control::default(),
        )
        .await
        .unwrap();
    for key in [
        path.to_str().unwrap(),
        public.url.as_str(),
        "unregistered",
        "exact 日本語 literal% source.wav/",
    ] {
        assert_eq!(
            server
                .open_registered_media(
                    &context(),
                    RegisteredMedia::SourceKey(key),
                    Control::default()
                )
                .await
                .unwrap_err()
                .code,
            "assets_unregistered"
        );
    }
    assert_eq!(server.status().grants, 1);
    drop((read, by_id, before, after));
    server.shutdown().await.unwrap();
    assert!(server.status().file_workers_joined);
}

#[tokio::test]
async fn foreign_full_contexts_foreign_ids_revocation_and_context_expiry_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let path = source(&dir);
    let server = server().await;
    let public = server
        .authorize(&context(), grant(&path), Control::default())
        .await
        .unwrap();
    let read = opened(&server).await;
    for foreign in [
        Context {
            runtime_id: "foreign".into(),
            ..context()
        },
        Context {
            data_path: "/foreign".into(),
            ..context()
        },
        Context {
            library_generation: 2,
            ..context()
        },
    ] {
        assert_eq!(
            server
                .open_registered_media(
                    &foreign,
                    RegisteredMedia::GrantId(&public.id),
                    Control::default()
                )
                .await
                .unwrap_err()
                .code,
            "assets_stale_context"
        );
    }
    assert_eq!(
        server
            .open_registered_media(
                &context(),
                RegisteredMedia::GrantId("00000000000000000000000000000000"),
                Control::default()
            )
            .await
            .unwrap_err()
            .code,
        "assets_unregistered"
    );
    let foreign = AssetServer::start(Config::default(), context())
        .await
        .unwrap();
    let foreign_grant = foreign
        .authorize(&context(), grant(&path), Control::default())
        .await
        .unwrap();
    assert_eq!(
        server
            .open_registered_media(
                &context(),
                RegisteredMedia::GrantId(&foreign_grant.id),
                Control::default()
            )
            .await
            .unwrap_err()
            .code,
        "assets_unregistered"
    );
    foreign.shutdown().await.unwrap();
    server.revoke(&public.id);
    assert_eq!(
        read.authority()
            .revalidate(Control::default())
            .await
            .unwrap_err()
            .code,
        "assets_revoked"
    );
    assert_eq!(
        server
            .open_registered_media(
                &context(),
                RegisteredMedia::GrantId(&public.id),
                Control::default()
            )
            .await
            .unwrap_err()
            .code,
        "assets_unregistered"
    );
    server
        .activate_context(Context {
            library_generation: 2,
            ..context()
        })
        .unwrap();
    assert_eq!(
        read.authority()
            .revalidate(Control::default())
            .await
            .unwrap_err()
            .code,
        "assets_stale_context"
    );
    drop(read);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn replaced_path_never_mixes_new_bytes_with_the_held_identity_and_reauthorization_revokes_old_authority(
) {
    let dir = tempfile::tempdir().unwrap();
    let path = source(&dir);
    let server = server().await;
    let old = server
        .authorize(&context(), grant(&path), Control::default())
        .await
        .unwrap();
    let read = opened(&server).await;
    let (mut file, authority) = read.into_parts();
    fs::rename(&path, path.with_extension("old")).unwrap();
    fs::write(&path, b"new different identity").unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"first real bytes");
    assert_eq!(
        authority
            .revalidate(Control::default())
            .await
            .unwrap_err()
            .code,
        "assets_changed"
    );
    assert_eq!(
        server
            .open_registered_media(
                &context(),
                RegisteredMedia::GrantId(&old.id),
                Control::default()
            )
            .await
            .unwrap_err()
            .code,
        "assets_changed"
    );
    let new = server
        .authorize(&context(), grant(&path), Control::default())
        .await
        .unwrap();
    assert_ne!(old.id, new.id);
    assert_eq!(
        authority
            .revalidate(Control::default())
            .await
            .unwrap_err()
            .code,
        "assets_revoked"
    );
    let mut current = opened(&server).await;
    let mut bytes = Vec::new();
    current.file_mut().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"new different identity");
    drop((file, authority, current));
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn same_identity_changes_in_length_modified_and_change_times_are_observed() {
    let dir = tempfile::tempdir().unwrap();
    let path = source(&dir);
    let server = server().await;
    server
        .authorize(&context(), grant(&path), Control::default())
        .await
        .unwrap();
    let read = opened(&server).await;
    let old_modified = fs::metadata(&path).unwrap().modified().unwrap();
    // The actual filesystem can stamp several fast writes with the same coarse
    // clock tick. Wait for an observed native tick, not a fabricated timestamp.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let probe = dir.path().join("native-clock-probe");
        fs::write(&probe, b"tick").unwrap();
        if fs::metadata(probe).unwrap().modified().unwrap() > old_modified {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual filesystem timestamp did not advance"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    fs::write(&path, b"other real bytes").unwrap();
    let writer = fs::OpenOptions::new().write(true).open(&path).unwrap();
    writer
        .set_times(fs::FileTimes::new().set_modified(old_modified + Duration::from_secs(2)))
        .unwrap();
    assert_eq!(
        read.authority()
            .revalidate(Control::default())
            .await
            .unwrap_err()
            .code,
        "assets_changed"
    );
    writer
        .set_times(fs::FileTimes::new().set_modified(old_modified))
        .unwrap();
    drop(writer);
    assert_eq!(
        read.authority()
            .revalidate(Control::default())
            .await
            .unwrap_err()
            .code,
        "assets_changed",
        "restoring mtime does not hide the native change timestamp"
    );
    let second = opened(&server).await;
    assert!(read
        .authority()
        .observation()
        .same_file(second.authority().observation()));
    assert!(!read
        .authority()
        .observation()
        .same_version(second.authority().observation()));
    fs::write(&path, b"longer changed source bytes").unwrap();
    assert_eq!(
        second
            .authority()
            .revalidate(Control::default())
            .await
            .unwrap_err()
            .code,
        "assets_changed"
    );
    drop((read, second));
    server.shutdown().await.unwrap();
}

fn symlink(target: &Path, path: &Path, directory: bool) -> bool {
    #[cfg(unix)]
    let result = {
        let _ = directory;
        std::os::unix::fs::symlink(target, path)
    };
    #[cfg(windows)]
    let result = if directory {
        std::os::windows::fs::symlink_dir(target, path)
    } else {
        std::os::windows::fs::symlink_file(target, path)
    };
    match result {
        Ok(()) => true,
        #[cfg(windows)]
        Err(error) if error.raw_os_error() == Some(1314) => {
            eprintln!("Native Windows symlink privilege unavailable: {error}");
            false
        }
        Err(error) => panic!("actual symlink setup failed: {error}"),
    }
}

#[tokio::test]
async fn canonical_leaf_and_parent_symlink_swaps_cannot_gain_protected_file_authority() {
    for parent in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let protected = dir.path().join("protected");
        fs::create_dir(&protected).unwrap();
        let allowed = dir.path().join("allowed");
        fs::create_dir(&allowed).unwrap();
        let path = allowed.join("source.wav");
        fs::write(&path, b"permitted").unwrap();
        fs::write(protected.join("source.wav"), b"never disclose").unwrap();
        let server = AssetServer::start(
            Config {
                protected_roots: vec![fs::canonicalize(&protected).unwrap()],
                ..Config::default()
            },
            context(),
        )
        .await
        .unwrap();
        server
            .authorize(&context(), grant(&path), Control::default())
            .await
            .unwrap();
        let read = opened(&server).await;
        #[cfg(windows)]
        let mut read = Some(read);
        #[cfg(not(windows))]
        let read = Some(read);
        let swapped = if parent { &allowed } else { &path };
        match fs::rename(swapped, swapped.with_extension("old")) {
            Ok(()) => (),
            #[cfg(windows)]
            Err(error) if parent && matches!(error.raw_os_error(), Some(5 | 32)) => {
                // Windows can pin a directory while its child descriptors are
                // open. Prove the refused mutation still exposes only the old
                // permitted bytes; do not pretend a parent swap took place.
                let held = read.as_mut().unwrap();
                let mut bytes = Vec::new();
                held.file_mut().read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, b"permitted");
                held.authority()
                    .revalidate(Control::default())
                    .await
                    .unwrap();
                let mut fresh = opened(&server).await;
                let mut bytes = Vec::new();
                fresh.file_mut().read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, b"permitted");
                drop((fresh, read.take()));
                // Worker-owned anchor disposal is independent of exported
                // descriptor disposal. The successful real rename, rather
                // than a counter or cancellation signal, proves OS release.
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match fs::rename(swapped, swapped.with_extension("old")) {
                        Ok(()) => break,
                        Err(error) if matches!(error.raw_os_error(), Some(5 | 32)) => {
                            assert!(
                                Instant::now() < deadline,
                                "Actual parent remained pinned after descriptor disposal: {error}"
                            );
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        Err(error) => {
                            panic!("Parent swap after actual descriptor disposal failed: {error}")
                        }
                    }
                }
                assert_eq!(server.status().native_reads, 0);
            }
            Err(error) => panic!("Actual registered-path swap failed (parent={parent}): {error}"),
        }
        let target = if parent {
            protected.clone()
        } else {
            protected.join("source.wav")
        };
        if symlink(&target, swapped, parent) {
            if let Some(read) = &read {
                assert_eq!(
                    read.authority()
                        .revalidate(Control::default())
                        .await
                        .unwrap_err()
                        .code,
                    "assets_protected"
                );
            }
            assert_eq!(
                server
                    .open_registered_media(
                        &context(),
                        RegisteredMedia::SourceKey("exact 日本語 literal% source.wav"),
                        Control::default()
                    )
                    .await
                    .unwrap_err()
                    .code,
                "assets_protected"
            );
        }
        drop(read);
        server.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn four_whole_descriptors_ready_results_and_retained_observations_share_one_quota() {
    let dir = tempfile::tempdir().unwrap();
    let path = source(&dir);
    let server = server().await;
    server
        .authorize(&context(), grant(&path), Control::default())
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for _ in 0..NATIVE_READ_LIMIT {
        let owner = server.clone();
        let task = tokio::spawn(async move { opened(&owner).await });
        // One worker/one queued request is an independent smaller IO bound.
        // Keep completed task results uncollected before admitting the next.
        settled(|| task.is_finished()).await;
        tasks.push(task);
    }
    let mut reads = Vec::new();
    for task in tasks {
        reads.push(task.await.unwrap());
    }
    assert_eq!(server.status().native_reads, NATIVE_READ_LIMIT);
    assert_eq!(
        server
            .open_registered_media(
                &context(),
                RegisteredMedia::SourceKey("exact 日本語 literal% source.wav"),
                Control::default()
            )
            .await
            .unwrap_err()
            .code,
        "assets_busy"
    );
    let mut observations = Vec::new();
    for read in &reads {
        observations.push(
            read.authority()
                .revalidate(Control::default())
                .await
                .unwrap(),
        );
    }
    drop(reads);
    assert_eq!(server.status().native_reads, NATIVE_READ_LIMIT);
    assert_eq!(
        server
            .open_registered_media(
                &context(),
                RegisteredMedia::SourceKey("exact 日本語 literal% source.wav"),
                Control::default()
            )
            .await
            .unwrap_err()
            .code,
        "assets_busy"
    );
    drop(observations);
    assert_eq!(server.status().native_reads, 0);
    let read = opened(&server).await;
    assert_eq!(
        server.shutdown().await.unwrap_err().code,
        "assets_shutdown_timeout",
        "a native consumer still owns its actual descriptor/authority"
    );
    assert!(server.status().file_workers_joined);
    assert_eq!(
        read.authority()
            .revalidate(Control::default())
            .await
            .unwrap_err()
            .code,
        "assets_shutdown"
    );
    drop(read);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropped_or_canceled_callers_keep_actual_accepted_io_and_leases_until_the_worker_finishes()
{
    for cancel in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = source(&dir);
        let server = server().await;
        server
            .authorize(&context(), grant(&path), Control::default())
            .await
            .unwrap();
        let (entered, release) = server
            .0
            .shared
            .pool
            .pause_for(fs::canonicalize(&path).unwrap());
        let control = Control::default();
        let token = control.cancel.clone();
        let owner = server.clone();
        let task = tokio::spawn(async move {
            owner
                .open_registered_media(
                    &context(),
                    RegisteredMedia::SourceKey("exact 日本語 literal% source.wav"),
                    control,
                )
                .await
        });
        entered.await.unwrap();
        if cancel {
            token.cancel();
            assert_eq!(task.await.unwrap().unwrap_err().code, "assets_cancelled");
        } else {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
        assert_eq!(server.status().native_reads, 1);
        assert_eq!(server.status().active_io, 1);
        assert_eq!(
            server.shutdown().await.unwrap_err().code,
            "assets_shutdown_timeout"
        );
        assert!(!server.status().file_workers_joined);
        release.send(()).unwrap();
        server.shutdown().await.unwrap();
        assert_eq!(server.status().native_reads, 0);
        assert_eq!(server.status().open_files, 0);
        assert!(server.status().file_workers_joined);
    }
}

#[tokio::test]
async fn context_switch_and_deadlines_reject_without_releasing_an_unfinished_worker_lease() {
    for deadline in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = source(&dir);
        let server = server().await;
        server
            .authorize(&context(), grant(&path), Control::default())
            .await
            .unwrap();
        let (entered, release) = server
            .0
            .shared
            .pool
            .pause_for(fs::canonicalize(&path).unwrap());
        let control = Control::new(
            Instant::now() + Duration::from_millis(if deadline { 20 } else { 2000 }),
            CancellationToken::new(),
        );
        let owner = server.clone();
        let task = tokio::spawn(async move {
            owner
                .open_registered_media(
                    &context(),
                    RegisteredMedia::SourceKey("exact 日本語 literal% source.wav"),
                    control,
                )
                .await
        });
        entered.await.unwrap();
        if !deadline {
            server
                .activate_context(Context {
                    library_generation: 2,
                    ..context()
                })
                .unwrap();
        }
        assert_eq!(
            task.await.unwrap().unwrap_err().code,
            if deadline {
                "assets_deadline"
            } else {
                "assets_stale_context"
            }
        );
        assert_eq!(server.status().native_reads, 1);
        release.send(()).unwrap();
        settled(|| server.status().native_reads == 0).await;
        server.shutdown().await.unwrap();
        assert!(server.status().file_workers_joined);
    }
}

#[tokio::test]
async fn dropped_revalidation_retains_its_real_anchor_and_result_lease_until_actual_completion() {
    let dir = tempfile::tempdir().unwrap();
    let path = source(&dir);
    let server = server().await;
    server
        .authorize(&context(), grant(&path), Control::default())
        .await
        .unwrap();
    let read = opened(&server).await;
    let (entered, release) = server
        .0
        .shared
        .pool
        .pause_for(fs::canonicalize(&path).unwrap());
    let task = tokio::spawn(async move { read.authority().revalidate(Control::default()).await });
    entered.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(server.status().native_reads, 1);
    assert_eq!(server.status().open_files, 1);
    release.send(()).unwrap();
    settled(|| server.status().native_reads == 0).await;
    server.shutdown().await.unwrap();
    assert!(server.status().file_workers_joined);
    assert_eq!(server.status().open_files, 0);
}

#[tokio::test]
async fn canceled_shutdown_observers_keep_real_thread_joins_after_logical_completion_and_panics_remain_sticky(
) {
    for panic in [false, true] {
        let server = server().await;
        let (entered, release) = server.0.shared.pool.pause_exit(panic);
        let owner = server.clone();
        let observer = tokio::spawn(async move { owner.shutdown().await });
        entered.await.unwrap();
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        assert_eq!(server.status().open_files, 0);
        assert!(!server.status().file_workers_joined);
        assert_eq!(
            server.shutdown().await.unwrap_err().code,
            "assets_shutdown_timeout"
        );
        assert!(!server.status().file_workers_joined);
        release.send(()).unwrap();
        if panic {
            assert_eq!(server.shutdown().await.unwrap_err().code, "assets_worker");
            assert_eq!(server.shutdown().await.unwrap_err().code, "assets_worker");
        } else {
            server.shutdown().await.unwrap();
            server.shutdown().await.unwrap();
        }
        assert!(server.status().file_workers_joined);
    }
}
