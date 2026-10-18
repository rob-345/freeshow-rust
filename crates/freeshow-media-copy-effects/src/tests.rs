use super::*;
use freeshow_media_copy::*;
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
struct Fixture {
    dir: TempDir,
    context: CopyContext,
    service: MediaCopyService,
    source: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let stage = dir.path().join("stage");
        std::fs::create_dir(&stage).unwrap();
        let source = dir.path().join("original.bin");
        std::fs::write(&source, b"opaque\0\xffmedia").unwrap();
        let context = CopyContext::new(
            dir.path().join("data"),
            MediaSyncRoot::Configured(dir.path().join("media")),
            None,
            Arc::new(StoreSnapshot::new(
                json!({"projects":{}}),
                json!({}),
                json!({}),
            )),
        )
        .unwrap();
        let service = MediaCopyService::new(CopyConfig {
            staging_root: Some(stage),
            ..Default::default()
        })
        .unwrap();
        Self {
            dir,
            context,
            service,
            source,
        }
    }
    #[cfg(target_os = "linux")]
    async fn prepare(&self) -> PreparedMediaCopy {
        self.service
            .prepare(
                "MEDIA_FOLDER_COPY",
                &json!({"paths":[self.source]}),
                &self.context,
            )
            .await
            .unwrap()
    }
}
#[cfg(target_os = "linux")]
fn roots(plan: &PreparedMediaCopy) -> TrustedCopyRoots<'_> {
    TrustedCopyRoots {
        data_root: plan.captured_data_root(),
        media_root: plan.destination_root(),
    }
}
#[cfg(target_os = "linux")]
fn item(plan: &PreparedMediaCopy) -> &Install {
    plan.effects()
        .iter()
        .find_map(|e| {
            if let CopyEffect::Install { file } = e {
                Some(file.as_ref())
            } else {
                None
            }
        })
        .unwrap()
}
#[cfg(target_os = "linux")]
fn hooked(
    plan: &PreparedMediaCopy,
    pin: &CommitLease,
    cancel: &CancellationToken,
    hook: &mut dyn FnMut(linux::Point),
) -> super::Result<Receipt> {
    linux::apply(plan, pin, EffectLimits::default(), cancel, hook)
}
#[tokio::test]
async fn exact_bytes_and_single_use_pin_or_explicit_platform() {
    let f = Fixture::new();
    #[cfg(not(target_os = "linux"))]
    {
        assert!(f.dir.path().is_dir());
        assert_eq!(
            f.service
                .prepare(
                    "MEDIA_FOLDER_COPY",
                    &json!({"paths":[f.source]}),
                    &f.context
                )
                .await
                .err()
                .unwrap()
                .code,
            "unsupported_platform"
        );
    }
    #[cfg(target_os = "linux")]
    {
        let plan = f.prepare().await;
        let pin = plan.pin_for_commit().unwrap();
        assert!(plan.pin_for_commit().is_err());
        let target = plan
            .destination_root()
            .join(item(&plan).destination().as_path());
        let receipt = apply(roots(&plan), &plan, &pin, &CancellationToken::new()).unwrap();
        assert_eq!(receipt.reply(), Some(true));
        assert_eq!(receipt.installed().len(), 1);
        assert_eq!(std::fs::read(&target).unwrap(), b"opaque\0\xffmedia");
        assert!(std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".freeshow-copy-")));
        drop(pin);
        drop(plan);
    }
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn foreign_pin_root_limit_and_pre_cancel_do_not_write() {
    let f = Fixture::new();
    let a = f.prepare().await;
    let b = f.prepare().await;
    let pa = a.pin_for_commit().unwrap();
    let pb = b.pin_for_commit().unwrap();
    assert_eq!(
        apply(roots(&a), &a, &pb, &CancellationToken::new())
            .unwrap_err()
            .code,
        "invalid_commit_lease"
    );
    assert_eq!(
        apply(
            TrustedCopyRoots {
                data_root: f.dir.path(),
                media_root: a.destination_root()
            },
            &a,
            &pa,
            &CancellationToken::new()
        )
        .unwrap_err()
        .code,
        "copy_effect_root_mismatch"
    );
    assert_eq!(
        apply_with_limits(
            roots(&a),
            &a,
            &pa,
            EffectLimits {
                max_install_bytes: 1,
                ..Default::default()
            },
            &CancellationToken::new()
        )
        .unwrap_err()
        .code,
        "copy_effect_limit"
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        apply(roots(&a), &a, &pa, &cancel).unwrap_err().code,
        "cancelled"
    );
    assert!(!a.destination_root().exists());
    drop((pa, pb, a, b));
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn source_stage_target_and_parent_tamper_are_rejected_before_effects() {
    for variant in 0..4 {
        let f = Fixture::new();
        let plan = f.prepare().await;
        let pin = plan.pin_for_commit().unwrap();
        match variant {
            0 => std::fs::write(&f.source, b"changed source").unwrap(),
            1 => std::fs::write(item(&plan).staged_path(), b"changed stage").unwrap(),
            2 => {
                let target = plan
                    .destination_root()
                    .join(item(&plan).destination().as_path());
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(target, b"external").unwrap();
            }
            _ => std::os::unix::fs::symlink(f.dir.path(), plan.destination_root()).unwrap(),
        }
        assert!(apply(roots(&plan), &plan, &pin, &CancellationToken::new()).is_err());
        if variant < 2 {
            assert!(!plan.destination_root().exists());
        }
        drop((pin, plan));
        f.service.shutdown().await.unwrap();
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn changed_existing_and_followed_symlink_targets_are_not_adopted_or_replaced() {
    for symlink in [false, true] {
        let f = Fixture::new();
        let initial = f.prepare().await;
        let target = initial
            .destination_root()
            .join(item(&initial).destination().as_path());
        drop(initial);
        f.service.flush_released().await.unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let bytes = if symlink {
            let external = f.dir.path().join("external.bin");
            std::fs::write(&external, b"before").unwrap();
            std::os::unix::fs::symlink(&external, &target).unwrap();
            external
        } else {
            std::fs::write(&target, b"before").unwrap();
            target.clone()
        };
        let plan = f.prepare().await;
        let pin = plan.pin_for_commit().unwrap();
        std::fs::write(&bytes, b"external changed existing").unwrap();
        let error = apply(roots(&plan), &plan, &pin, &CancellationToken::new()).unwrap_err();
        assert_eq!(error.code, "copy_effect_guard_changed");
        assert!(!error.rollback_incomplete);
        assert_eq!(std::fs::read(target).unwrap(), b"external changed existing");
        drop((pin, plan));
        f.service.shutdown().await.unwrap();
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn prior_source_alerts_and_failures_survive_a_later_hard_source_guard_failure() {
    // Real non-root filesystem permission evidence. Root cannot exercise this
    // ordinary EACCES path; its lack of evidence is explicit in test output.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("EACCES fixture unavailable as root; no permission equivalence claimed");
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let data = f.dir.path().join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o555)).unwrap();
    let context = CopyContext::new(
        &data,
        MediaSyncRoot::Default,
        None,
        Arc::new(StoreSnapshot::new(
            json!({"projects":{}}),
            json!({}),
            json!({}),
        )),
    )
    .unwrap();
    let plan = f
        .service
        .prepare("MEDIA_FOLDER_COPY", &json!({"paths":[f.source]}), &context)
        .await
        .unwrap();
    let pin = plan.pin_for_commit().unwrap();
    let error = hooked(&plan, &pin, &CancellationToken::new(), &mut |p| {
        if p == linux::Point::AfterPreflight {
            std::fs::write(&f.source, b"changed source after preflight").unwrap()
        }
    })
    .unwrap_err();
    assert_eq!(error.code, "copy_effect_source_changed");
    assert!(!error.rollback_incomplete);
    assert_eq!(error.observed.alerts.len(), 2);
    assert_eq!(error.observed.failures.len(), 1);
    assert!(matches!(
        error.observed.failures[0].phase,
        FailurePhase::ParentUnavailable
    ));
    assert!(!plan.destination_root().exists());
    std::fs::set_permissions(data, std::fs::Permissions::from_mode(0o755)).unwrap();
    drop((pin, plan));
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancellation_mid_copy_removes_owned_files_and_directories() {
    let f = Fixture::new();
    std::fs::write(&f.source, vec![7; 100_000]).unwrap();
    let plan = f.prepare().await;
    let pin = plan.pin_for_commit().unwrap();
    let cancel = CancellationToken::new();
    let error = hooked(&plan, &pin, &cancel, &mut |p| {
        if p == linux::Point::CopyChunk {
            cancel.cancel()
        }
    })
    .unwrap_err();
    assert_eq!(error.code, "cancelled");
    assert!(!error.rollback_incomplete, "{error:?}");
    assert!(!plan.destination_root().exists());
    drop((pin, plan));
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn late_collision_is_preserved_and_rollback_reports_nonempty_external_parent() {
    let f = Fixture::new();
    let plan = f.prepare().await;
    let pin = plan.pin_for_commit().unwrap();
    let target = plan
        .destination_root()
        .join(item(&plan).destination().as_path());
    let error = hooked(&plan, &pin, &CancellationToken::new(), &mut |p| {
        if p == linux::Point::BeforePromotion(0) {
            std::fs::write(&target, b"external collision").unwrap()
        }
    })
    .unwrap_err();
    assert_eq!(error.code, "copy_effect_collision");
    assert!(error.rollback_incomplete);
    assert_eq!(std::fs::read(target).unwrap(), b"external collision");
    drop((pin, plan));
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn promoted_external_changes_are_preserved_without_adopting_inode() {
    for replace in [false, true] {
        let f = Fixture::new();
        let plan = f.prepare().await;
        let pin = plan.pin_for_commit().unwrap();
        let target = plan
            .destination_root()
            .join(item(&plan).destination().as_path());
        let error = hooked(&plan, &pin, &CancellationToken::new(), &mut |p| {
            if p == linux::Point::PromotionRenamed(0) {
                if replace {
                    std::fs::remove_file(&target).unwrap();
                }
                std::fs::write(&target, b"external changed").unwrap()
            }
        })
        .unwrap_err();
        assert!(error.rollback_incomplete);
        assert_eq!(std::fs::read(target).unwrap(), b"external changed");
        drop((pin, plan));
        f.service.shutdown().await.unwrap();
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn second_copy_cancellation_rolls_back_first_install_and_preserves_existing_root() {
    let f = Fixture::new();
    let second = f.dir.path().join("second.bin");
    std::fs::write(&second, b"second").unwrap();
    let media = f.dir.path().join("media");
    std::fs::create_dir(&media).unwrap();
    let existing = media.join("unrelated.bin");
    std::fs::write(&existing, b"keep external bytes").unwrap();
    let plan = f
        .service
        .prepare(
            "MEDIA_FOLDER_COPY",
            &json!({"paths":[f.source,second]}),
            &f.context,
        )
        .await
        .unwrap();
    let pin = plan.pin_for_commit().unwrap();
    let cancel = CancellationToken::new();
    let error = hooked(&plan, &pin, &cancel, &mut |p| {
        if p == linux::Point::BeforePromotion(1) {
            cancel.cancel()
        }
    })
    .unwrap_err();
    assert_eq!(error.code, "cancelled");
    assert!(!error.rollback_incomplete, "{error:?}");
    assert_eq!(std::fs::read(existing).unwrap(), b"keep external bytes");
    assert_eq!(std::fs::read_dir(media).unwrap().count(), 1);
    drop((pin, plan));
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn budgets_reject_before_effects_or_stop_mid_copy_and_roll_back() {
    for kind in 0..3 {
        let f = Fixture::new();
        std::fs::write(&f.source, vec![5; 100_000]).unwrap();
        let plan = f.prepare().await;
        let pin = plan.pin_for_commit().unwrap();
        let limits = match kind {
            0 => EffectLimits {
                max_operations: 1,
                ..Default::default()
            },
            1 => EffectLimits {
                timeout: Duration::from_millis(10),
                ..Default::default()
            },
            _ => EffectLimits {
                max_chunks: 6,
                ..Default::default()
            },
        };
        let mut chunks = 0;
        let error = linux::apply(&plan, &pin, limits, &CancellationToken::new(), &mut |p| {
            if p == linux::Point::AfterPreflight && kind == 1 {
                std::thread::sleep(Duration::from_millis(20));
            }
            if p == linux::Point::CopyChunk {
                chunks += 1;
            }
        })
        .unwrap_err();
        assert_eq!(
            error.code,
            if kind == 1 {
                "copy_effect_timeout"
            } else {
                "copy_effect_limit"
            }
        );
        assert!(!error.rollback_incomplete, "{error:?}");
        assert!(!plan.destination_root().exists());
        if kind == 2 {
            assert!(chunks > 0);
        }
        drop((pin, plan));
        f.service.shutdown().await.unwrap();
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn parent_rename_source_and_stage_changes_during_apply_are_hard_failures() {
    for kind in 0..3 {
        let f = Fixture::new();
        let plan = f.prepare().await;
        let pin = plan.pin_for_commit().unwrap();
        let root = plan.destination_root().to_owned();
        let moved = f.dir.path().join("moved-media");
        let error = hooked(&plan, &pin, &CancellationToken::new(), &mut |p| {
            if p == linux::Point::BeforePromotion(0) {
                match kind {
                    0 => std::fs::rename(&root, &moved).unwrap(),
                    1 => std::fs::write(&f.source, b"external changed source").unwrap(),
                    _ => std::fs::write(item(&plan).staged_path(), b"external changed stage")
                        .unwrap(),
                }
            }
        })
        .unwrap_err();
        if kind == 0 {
            assert!(error.rollback_incomplete);
            assert!(moved.exists());
            assert!(!root.exists());
        } else {
            assert!(!error.rollback_incomplete, "{error:?}");
            assert!(!root.exists());
        }
        drop((pin, plan));
        f.service.shutdown().await.unwrap();
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn rollback_continues_after_diagnostic_capacity_is_exhausted() {
    let f = Fixture::new();
    let mut inputs = vec![];
    for index in 0..22 {
        let folder = f.dir.path().join(format!("source-{index}"));
        std::fs::create_dir(&folder).unwrap();
        let source = folder.join("input.bin");
        std::fs::write(&source, b"before").unwrap();
        inputs.push(source);
    }
    let plan = f
        .service
        .prepare("MEDIA_FOLDER_COPY", &json!({"paths":inputs}), &f.context)
        .await
        .unwrap();
    let pin = plan.pin_for_commit().unwrap();
    let targets: Vec<_> = plan
        .effects()
        .iter()
        .filter_map(|e| {
            if let CopyEffect::Install { file } = e {
                Some(plan.destination_root().join(file.destination().as_path()))
            } else {
                None
            }
        })
        .collect();
    let cancel = CancellationToken::new();
    let error = hooked(&plan, &pin, &cancel, &mut |p| {
        if p == linux::Point::AfterPromotion(21) {
            cancel.cancel();
        }
        if p == linux::Point::BeforeRollback {
            for path in targets.iter().skip(1) {
                std::fs::write(path, b"external after").unwrap();
            }
        }
    })
    .unwrap_err();
    assert_eq!(error.code, "cancelled");
    assert!(error.rollback_incomplete);
    assert!(error.observed.diagnostics.len() <= 16);
    assert!(
        error
            .observed
            .diagnostics
            .iter()
            .map(String::len)
            .sum::<usize>()
            <= 16384
    );
    assert!(error.observed.diagnostics_omitted > 0);
    assert!(
        !targets[0].exists(),
        "cleanup must continue after diagnostics fill"
    );
    for path in targets.iter().skip(1) {
        assert_eq!(std::fs::read(path).unwrap(), b"external after");
    }
    drop((pin, plan));
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn accepted_pin_survives_request_cancel_expiry_and_shutdown_until_drop() {
    let mut f = Fixture::new();
    f.service.shutdown().await.unwrap();
    f.service = MediaCopyService::new(CopyConfig {
        timeout: Duration::from_millis(200),
        prepared_ttl: Duration::from_millis(300),
        ..Default::default()
    })
    .unwrap();
    let request_cancel = CancellationToken::new();
    let plan = f
        .service
        .prepare_with_cancel(
            "MEDIA_FOLDER_COPY",
            &json!({"paths":[f.source]}),
            &f.context,
            request_cancel.clone(),
        )
        .await
        .unwrap();
    let pin = plan.pin_for_commit().unwrap();
    request_cancel.cancel();
    tokio::time::sleep(Duration::from_millis(420)).await;
    assert_eq!(
        f.service.shutdown().await.unwrap_err().code,
        "copy_commit_in_progress"
    );
    assert!(plan.staging_directory().exists());
    let receipt = apply(roots(&plan), &plan, &pin, &CancellationToken::new()).unwrap();
    assert_eq!(receipt.installed().len(), 1);
    let stage = plan.staging_directory().to_owned();
    drop(plan);
    assert!(stage.exists());
    drop(pin);
    f.service.shutdown().await.unwrap();
    assert!(!stage.exists());
}
#[test]
fn diagnostics_bound_utf8_and_count_without_stopping_cleanup_bookkeeping() {
    let mut e = EffectError::new("fixture", "x".repeat(4000));
    for _ in 0..100 {
        e.detail("🙌".repeat(5000));
    }
    assert!(e.message.len() <= 2048);
    assert!(e.observed.diagnostics.len() <= 16);
    assert!(
        e.observed
            .diagnostics
            .iter()
            .map(String::len)
            .sum::<usize>()
            <= 16384
    );
    assert!(e.observed.diagnostics_omitted > 0);
}
