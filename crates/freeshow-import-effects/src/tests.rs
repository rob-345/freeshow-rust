use super::*;
use freeshow_imports::{ImportConfig, ImportRoots, ImportService, PreparedImport};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

struct Fixture {
    _temporary: TempDir,
    base: PathBuf,
    root: PathBuf,
    source: PathBuf,
    service: ImportService,
    prepared: PreparedImport,
    cancel: CancellationToken,
}
impl Fixture {
    async fn new(files: &[(&str, &[u8])]) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        // macOS /var and /tmp aliases must not turn these tests into accidental
        // tests of intentionally unsupported symlinked trusted ancestors.
        let base = fs::canonicalize(temporary.path()).unwrap();
        let root = base.join("Imports");
        let stage = base.join("staging");
        fs::create_dir(&stage).unwrap();
        let source = base.join("bundle.project");
        let mut archive = ZipWriter::new(File::create(&source).unwrap());
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        archive.start_file("data.json", options).unwrap();
        let mut names: Vec<_> = files
            .iter()
            .map(|(name, _)| format!("/freeshow-effects-fixture/{name}"))
            .collect();
        names.push("/freeshow-effects-fixture/absent.png".into());
        archive
            .write_all(
                json!({"files": names, "unknown": true})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        for (name, bytes) in files {
            archive.start_file(*name, options).unwrap();
            archive.write_all(bytes).unwrap();
        }
        archive.finish().unwrap();
        let service = ImportService::new(ImportConfig {
            staging_root: Some(stage),
            ..ImportConfig::default()
        })
        .unwrap();
        let cancel = CancellationToken::new();
        let prepared = service
            .prepare_with_cancel(
                "IMPORT_FILES",
                &json!({"id":"freeshow_project","paths":[source]}),
                ImportRoots {
                    imports_root: root.clone(),
                },
                cancel.clone(),
            )
            .await
            .unwrap();
        Self {
            _temporary: temporary,
            base,
            root,
            source,
            service,
            prepared,
            cancel,
        }
    }
    fn target(&self, index: usize) -> PathBuf {
        self.root
            .join(self.prepared.plan.installs[index].destination.as_path())
    }
    async fn finish(self) {
        drop(self.prepared);
        self.service.shutdown().await.unwrap();
    }
}
fn injected() -> EffectError {
    EffectError::new("import_test_failure", "Injected after installation")
}
fn effect_files(root: &Path) -> Vec<PathBuf> {
    let mut found = vec![];
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.extend(effect_files(&path));
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[cfg(windows)]
#[tokio::test]
async fn parent_guards_cover_complete_windows_roots_and_all_existing_ancestors() {
    let fixture = Fixture::new(&[("image.png", b"media")]).await;
    for base in [
        fixture.base.clone(),
        fixture._temporary.path().to_path_buf(),
    ] {
        let mut budget = Budget {
            plan: &fixture.prepared.plan,
            config: EffectConfig::default(),
            started: Instant::now(),
            operations: 0,
        };
        let mut parents = Parents::default();
        parents.absolute(&base, false, &mut budget).unwrap();
        let expected: Vec<_> = base.ancestors().map(Path::to_path_buf).collect();
        assert_eq!(parents.0.len(), expected.len());
        for ancestor in expected {
            assert!(parents.0.contains_key(&ancestor), "{}", ancestor.display());
            parents.observe(&ancestor).unwrap();
        }
    }
    fixture.finish().await;
}

#[test]
fn rollback_diagnostics_are_utf8_bounded_without_skipping_later_cleanup() {
    let temporary = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temporary.path()).unwrap();
    let mut parents = Parents::default();
    for ancestor in base.ancestors() {
        parents.observe(ancestor).unwrap();
    }
    let good = base.join("owned");
    fs::write(&good, b"owned").unwrap();
    let mut journal = Journal::default();
    journal.files.push(OwnedFile {
        path: good.clone(),
        expected: path_identity(&good).unwrap(),
        sha256: format!("{:x}", Sha256::digest(b"owned")),
    });
    for index in 0..80 {
        let path = base.join(format!("changed-{index}"));
        fs::write(&path, b"external").unwrap();
        journal.files.push(OwnedFile {
            expected: path_identity(&path).unwrap(),
            path,
            sha256: "0".repeat(64),
        });
    }
    let mut error = injected();
    rollback(&journal, &parents, &mut error, Duration::from_secs(5));
    assert!(
        !good.exists(),
        "Cleanup continues after diagnostic capacity is exhausted"
    );
    assert_eq!(error.rollback_failures.len(), MAX_ROLLBACK_DETAILS);
    assert_eq!(error.rollback_omitted, 80 - MAX_ROLLBACK_DETAILS);
    assert_eq!(effect_files(&base).len(), 80);
    let unicode = "漢字".repeat(20_000);
    let mut large = EffectError::new("import_test", &unicode);
    for _ in 0..200 {
        large.rollback_failure(
            Path::new(&unicode),
            &EffectError::new("import_test", &unicode),
        );
    }
    assert!(large.message.len() <= 8192);
    assert!(large.rollback_failures.len() <= MAX_ROLLBACK_DETAILS);
    assert!(
        large
            .rollback_failures
            .iter()
            .map(String::len)
            .sum::<usize>()
            <= MAX_DIAGNOSTIC_BYTES
    );
    assert!(large.rollback_omitted > 0);
    assert!(large.to_string().len() < MAX_DIAGNOSTIC_BYTES + 9000);
    assert!(large.to_string().contains("omitted"));
}

#[tokio::test]
async fn real_prepared_zip_installs_unicode_multichunk_bytes_hash_and_keeps_unresolved() {
    let bytes = (0..100_000)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let fixture = Fixture::new(&[("été 漢字.png", &bytes), ("empty.wav", b"")]).await;
    assert!(!fixture.root.exists());
    let receipt = apply(&fixture.root, &fixture.prepared.plan).unwrap();
    assert_eq!(receipt.installed.len(), 2);
    assert_eq!(receipt.unresolved_media, 1);
    assert!(receipt.created_directories.contains(&PathBuf::new()));
    assert!(receipt
        .created_directories
        .contains(&PathBuf::from("Projects")));
    for install in &fixture.prepared.plan.installs {
        let actual = fs::read(fixture.root.join(install.destination.as_path())).unwrap();
        assert_eq!(actual, fs::read(&install.staged_path).unwrap());
        assert_eq!(format!("{:x}", Sha256::digest(&actual)), install.sha256);
    }
    assert_eq!(
        effect_files(&fixture.root).len(),
        2,
        "No installation temporaries"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn raw_empty_transport_creates_no_imports_directory() {
    let fixture = Fixture::new(&[]).await;
    let prepared = fixture
        .service
        .prepare(
            "IMPORT_FILES",
            &json!({"id":"txt","paths":[]}),
            ImportRoots {
                imports_root: fixture.root.clone(),
            },
        )
        .await
        .unwrap();
    let receipt = apply(&fixture.root, &prepared.plan).unwrap();
    assert!(receipt.installed.is_empty());
    assert!(receipt.created_directories.is_empty());
    assert!(!fixture.root.exists());
    drop(prepared);
    let raw = fixture.base.join("raw.freeshow");
    fs::write(&raw, b"{\"name\":\"raw\"}").unwrap();
    for id in ["txt", "freeshow"] {
        let prepared = fixture
            .service
            .prepare(
                "IMPORT_FILES",
                &json!({"id":id,"paths":[raw]}),
                ImportRoots {
                    imports_root: fixture.root.clone(),
                },
            )
            .await
            .unwrap();
        assert!(apply(&fixture.root, &prepared.plan)
            .unwrap()
            .created_directories
            .is_empty());
        assert!(!fixture.root.exists());
        drop(prepared);
    }
    fixture.finish().await;
}

#[tokio::test]
async fn exact_root_byte_and_deadline_bounds_fail_before_any_effect() {
    let fixture = Fixture::new(&[("a.png", b"abcd")]).await;
    assert_eq!(
        apply(&fixture.base.join("elsewhere"), &fixture.prepared.plan)
            .unwrap_err()
            .code,
        "import_root_mismatch"
    );
    assert_eq!(
        apply_with_config(
            &fixture.root,
            &fixture.prepared.plan,
            EffectConfig {
                max_bytes: 3,
                ..EffectConfig::default()
            }
        )
        .unwrap_err()
        .code,
        "import_effect_limit"
    );
    assert_eq!(
        apply_with_config(
            &fixture.root,
            &fixture.prepared.plan,
            EffectConfig {
                timeout: Duration::from_nanos(1),
                ..EffectConfig::default()
            }
        )
        .unwrap_err()
        .code,
        "import_effect_limit"
    );
    assert_eq!(
        apply_with_config(
            &fixture.root,
            &fixture.prepared.plan,
            EffectConfig {
                max_bytes: MAX_BYTES + 1,
                ..EffectConfig::default()
            }
        )
        .unwrap_err()
        .code,
        "import_effect_configuration"
    );
    assert!(!fixture.root.exists());
    fixture.finish().await;
}

#[tokio::test]
async fn changed_stage_identity_and_hash_are_independent_guards() {
    let mut fixture = Fixture::new(&[("a.png", b"original")]).await;
    let staged = fixture.prepared.plan.installs[0].staged_path.clone();
    fs::write(&staged, b"changed!!").unwrap();
    assert_eq!(
        apply(&fixture.root, &fixture.prepared.plan)
            .unwrap_err()
            .code,
        "import_source_changed"
    );
    fixture.prepared.plan.installs[0].expected_source = FileIdentity::read(&staged).unwrap();
    assert_eq!(
        apply(&fixture.root, &fixture.prepared.plan)
            .unwrap_err()
            .code,
        "import_source_hash"
    );
    assert!(!fixture.root.exists());
    fixture.finish().await;
}

#[tokio::test]
async fn existing_media_guards_are_readonly_and_revalidated() {
    let fixture = Fixture::new(&[("a.png", b"original")]).await;
    apply(&fixture.root, &fixture.prepared.plan).unwrap();
    let prepared = fixture
        .service
        .prepare(
            "IMPORT_FILES",
            &json!({"id":"freeshow_project","paths":[fixture.source]}),
            ImportRoots {
                imports_root: fixture.root.clone(),
            },
        )
        .await
        .unwrap();
    assert!(prepared.plan.installs.is_empty());
    assert_eq!(prepared.plan.existing.len(), 1);
    let receipt = apply(&fixture.root, &prepared.plan).unwrap();
    assert_eq!(receipt.existing_media, 1);
    assert!(receipt.created_directories.is_empty());
    let path = fixture
        .root
        .join(prepared.plan.existing[0].destination.as_path());
    fs::write(&path, b"external media").unwrap();
    assert_eq!(
        apply(&fixture.root, &prepared.plan).unwrap_err().code,
        "import_existing_changed"
    );
    assert_eq!(fs::read(path).unwrap(), b"external media");
    drop(prepared);
    fixture.finish().await;
}

#[tokio::test]
async fn existing_destination_preflight_and_late_noclobber_collision_preserve_external_file() {
    let fixture = Fixture::new(&[("a.png", b"one"), ("b.png", b"two")]).await;
    let first = fixture.target(0);
    let second = fixture.target(1);
    let error = execute(
        &fixture.root,
        &fixture.prepared.plan,
        EffectConfig::default(),
        |step, index, _| {
            if step == Checkpoint::BeforePersist && index == 1 {
                fs::write(&second, b"external").unwrap();
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "import_destination_exists");
    assert!(!first.exists(), "Earlier owned install rolled back");
    assert_eq!(fs::read(&second).unwrap(), b"external");
    assert_eq!(effect_files(&fixture.root), vec![second.clone()]);
    assert!(
        !error.rollback_failures.is_empty(),
        "Nonempty created directories are preserved and reported"
    );
    assert_eq!(
        apply(&fixture.root, &fixture.prepared.plan)
            .unwrap_err()
            .code,
        "import_destination_exists"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn cancellation_after_install_rolls_back_owned_files_and_new_directories() {
    let fixture = Fixture::new(&[("a.png", b"one"), ("b.png", b"two")]).await;
    let error = execute(
        &fixture.root,
        &fixture.prepared.plan,
        EffectConfig::default(),
        |step, _, _| {
            if step == Checkpoint::AfterInstall {
                fixture.cancel.cancel();
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "cancelled");
    assert!(error.rollback_failures.is_empty(), "{error}");
    assert!(!fixture.root.exists());
    fixture.finish().await;
}

#[tokio::test]
async fn cancellation_in_the_copy_loop_removes_the_owned_partial_temporary() {
    let bytes = vec![42; CHUNK * 3];
    let fixture = Fixture::new(&[("a.png", &bytes)]).await;
    let error = execute(
        &fixture.root,
        &fixture.prepared.plan,
        EffectConfig::default(),
        |step, _, _| {
            if step == Checkpoint::CopyChunk {
                fixture.cancel.cancel();
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "cancelled");
    assert!(error.rollback_failures.is_empty(), "{error}");
    assert!(!fixture.root.exists());
    fixture.finish().await;
}

#[tokio::test]
async fn final_revalidation_rejects_an_installed_file_changed_during_the_transaction() {
    let fixture = Fixture::new(&[("a.png", b"original")]).await;
    let target = fixture.target(0);
    let error = execute(
        &fixture.root,
        &fixture.prepared.plan,
        EffectConfig::default(),
        |step, _, path| {
            if step == Checkpoint::AfterInstall {
                fs::write(path, b"external").unwrap();
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "import_installed_changed");
    assert_eq!(fs::read(target).unwrap(), b"external");
    assert!(!error.rollback_failures.is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn rollback_preserves_external_modifications_and_preexisting_directories() {
    let fixture = Fixture::new(&[("a.png", b"original")]).await;
    fs::create_dir_all(fixture.root.join("Projects")).unwrap();
    let target = fixture.target(0);
    let error = execute(
        &fixture.root,
        &fixture.prepared.plan,
        EffectConfig::default(),
        |step, _, path| {
            if step == Checkpoint::AfterInstall {
                fs::write(path, b"external replacement").unwrap();
                return Err(injected());
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "import_test_failure");
    assert_eq!(fs::read(target).unwrap(), b"external replacement");
    assert!(fixture.root.join("Projects").is_dir());
    assert!(!error.rollback_failures.is_empty());
    assert!(error.to_string().contains("rollback incomplete"));
    fixture.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn inode_replacement_with_identical_bytes_is_preserved_on_rollback() {
    let fixture = Fixture::new(&[("a.png", b"original")]).await;
    let target = fixture.target(0);
    let mut original_handle = None;
    let error = execute(
        &fixture.root,
        &fixture.prepared.plan,
        EffectConfig::default(),
        |step, _, path| {
            if step == Checkpoint::AfterInstall {
                original_handle = Some(File::open(path).unwrap());
                fs::remove_file(path).unwrap();
                fs::write(path, b"original").unwrap();
                return Err(injected());
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(fs::read(target).unwrap(), b"original");
    assert!(!error.rollback_failures.is_empty());
    drop(original_handle);
    fixture.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn static_destination_and_stage_symlinks_are_rejected_without_following() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new(&[("a.png", b"original")]).await;
    let outside = fixture.base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::create_dir(&fixture.root).unwrap();
    symlink(&outside, fixture.root.join("Projects")).unwrap();
    assert_eq!(
        apply(&fixture.root, &fixture.prepared.plan)
            .unwrap_err()
            .code,
        "import_invalid_path"
    );
    assert!(effect_files(&outside).is_empty());
    fs::remove_file(fixture.root.join("Projects")).unwrap();
    let staged = &fixture.prepared.plan.installs[0].staged_path;
    fs::remove_file(staged).unwrap();
    let external = outside.join("keep");
    fs::write(&external, b"external").unwrap();
    symlink(&external, staged).unwrap();
    assert_eq!(
        apply(&fixture.root, &fixture.prepared.plan)
            .unwrap_err()
            .code,
        "import_invalid_path"
    );
    assert_eq!(fs::read(external).unwrap(), b"external");
    fixture.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn observed_parent_replacement_before_persist_is_reported_not_claimed_raceproof() {
    let fixture = Fixture::new(&[("a.png", b"original")]).await;
    let moved = fixture.root.join("externally-moved");
    let error = execute(
        &fixture.root,
        &fixture.prepared.plan,
        EffectConfig::default(),
        |step, _, path| {
            if step == Checkpoint::BeforePersist {
                fs::rename(path.parent().unwrap(), &moved).unwrap();
                fs::create_dir(path.parent().unwrap()).unwrap();
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "import_parent_changed");
    assert!(!error.rollback_failures.is_empty());
    assert!(!fixture.target(0).exists());
    assert_eq!(
        effect_files(&moved).len(),
        1,
        "A renamed owned temp is preserved with explicit incomplete rollback"
    );
    fixture.finish().await;
}
