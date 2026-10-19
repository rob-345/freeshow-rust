use super::*;
#[cfg(target_os = "linux")]
use crate::linux::Point;
#[cfg(target_os = "linux")]
use freeshow_thumbnails::{file_path_hash_code, CacheEffect, CaptureCompletion};
use freeshow_thumbnails::{ThumbnailConfig, ThumbnailRoots, ThumbnailService};
use std::fs;
use tokio_util::sync::CancellationToken;
#[cfg(target_os = "linux")]
fn png() -> Vec<u8> {
    let mut data = vec![];
    {
        let mut encoder = png::Encoder::new(&mut data, 4, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[122; 32])
            .unwrap();
    }
    data
}
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    input: PathBuf,
    service: ThumbnailService,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        fs::create_dir(&root).unwrap();
        let input = dir.path().join("video.mp4");
        fs::write(&input, b"fixture metadata-only source").unwrap();
        let service = ThumbnailService::new(ThumbnailConfig {
            max_prepared: 8,
            staging_root: Some(dir.path().into()),
            ..ThumbnailConfig::default()
        })
        .unwrap();
        Self {
            _dir: dir,
            root,
            input,
            service,
        }
    }
    #[cfg(target_os = "linux")]
    fn destination(&self, size: u32) -> PathBuf {
        self.root.join(format!(
            "{}-{size}.png",
            file_path_hash_code(self.input.to_str().unwrap())
        ))
    }
    #[cfg(target_os = "linux")]
    async fn pending(&self, cancel: CancellationToken) -> PreparedThumbnail {
        self.service
            .prepare_capture(
                self.input.to_str().unwrap(),
                4,
                &ThumbnailRoots {
                    cache_root: self.root.clone(),
                },
                cancel,
            )
            .await
            .unwrap()
    }
    #[cfg(target_os = "linux")]
    async fn generated(&self, cancel: CancellationToken) -> PreparedThumbnail {
        let pending = self.pending(cancel).await;
        let id = pending.capture.as_ref().unwrap().id.clone();
        self.service
            .complete(pending, &id, CaptureCompletion::Png(&png()))
            .await
            .unwrap()
    }
    #[cfg(target_os = "linux")]
    fn stale(&self, size: u32) {
        fs::write(self.destination(size), b"original cached bytes").unwrap();
        let file = fs::File::options()
            .write(true)
            .open(self.destination(size))
            .unwrap();
        file.set_times(
            std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
        )
        .unwrap();
    }
}
#[test]
fn diagnostics_remain_bounded_utf8() {
    let mut error = EffectError::new("test", "敬拜".repeat(10000));
    assert!(error.message.len() <= 2048);
    for _ in 0..100 {
        error.detail("🙌".repeat(10000));
    }
    assert!(error.diagnostics.len() <= MAX_DETAILS);
    assert!(error.diagnostics.iter().map(String::len).sum::<usize>() <= MAX_DIAGNOSTIC_BYTES);
    assert!(error.diagnostics_omitted > 0);
    assert!(std::str::from_utf8(error.message.as_bytes()).is_ok());
    let mut error = EffectError::new("test", "");
    for _ in 0..100 {
        error.detail("敬拜".repeat(10000));
    }
    assert!(error.diagnostics.iter().map(String::len).sum::<usize>() <= MAX_DIAGNOSTIC_BYTES);
    assert!(error.diagnostics.iter().all(|detail| !detail.is_empty()));
    assert_eq!(error.diagnostics.len() + error.diagnostics_omitted, 100);
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn installs_actual_completed_png_exact_bytes_and_rejects_reapply() {
    let f = Fixture::new();
    let plan = f.generated(CancellationToken::new()).await;
    let receipt = apply(&f.root, &plan).unwrap();
    assert_eq!(receipt.effects().len(), 1);
    assert!(receipt.effects()[0].installed);
    assert_eq!(fs::read(f.destination(4)).unwrap(), png());
    assert!(apply(&f.root, &plan).is_err());
    assert_eq!(fs::read(f.destination(4)).unwrap(), png());
    drop(plan);
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn stale_prepare_apply_ack_complete_apply_is_two_phase_and_single_use() {
    let f = Fixture::new();
    f.stale(4);
    let mut plan = f.pending(CancellationToken::new()).await;
    let foreign = f.pending(CancellationToken::new()).await;
    let mut receipt = apply(&f.root, &plan).unwrap();
    assert!(!f.destination(4).exists());
    let mut foreign = foreign;
    assert_eq!(
        receipt
            .acknowledge_preparation(&mut foreign)
            .err()
            .unwrap()
            .code,
        "invalid_preparation_acknowledgement"
    );
    assert!(!foreign.effects.is_empty());
    let original_root = plan.cache_root.clone();
    plan.cache_root = f._dir.path().join("foreign-root");
    assert!(receipt.acknowledge_preparation(&mut plan).is_err());
    assert!(!plan.effects.is_empty());
    plan.cache_root = original_root;
    let original_effects = plan.effects.clone();
    plan.effects.clear();
    assert!(receipt.acknowledge_preparation(&mut plan).is_err());
    plan.effects = original_effects;
    receipt.acknowledge_preparation(&mut plan).unwrap();
    assert!(plan.effects.is_empty());
    assert!(receipt.acknowledge_preparation(&mut plan).is_err());
    let id = plan.capture.as_ref().unwrap().id.clone();
    let completed = f
        .service
        .complete(plan, &id, CaptureCompletion::Png(&png()))
        .await
        .unwrap();
    let receipt = apply(&f.root, &completed).unwrap();
    assert_eq!(receipt.effects().len(), 1);
    assert_eq!(fs::read(f.destination(4)).unwrap(), png());
    drop((foreign, completed));
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn guarded_replace_uses_anchor_across_rename_ctime_changes() {
    let f = Fixture::new();
    f.stale(4);
    let plan = f.generated(CancellationToken::new()).await;
    assert!(matches!(
        &plan.effects[0],
        CacheEffect::Install {
            expected_destination: freeshow_thumbnails::CacheGuard::Existing(_),
            ..
        }
    ));
    apply(&f.root, &plan).unwrap();
    assert_eq!(fs::read(f.destination(4)).unwrap(), png());
    assert_eq!(fs::read_dir(&f.root).unwrap().count(), 1);
    drop(plan);
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn target_collision_source_staged_and_root_tamper_fail_before_mutation() {
    for scenario in 0..5 {
        let f = Fixture::new();
        let mut plan = f.generated(CancellationToken::new()).await;
        match scenario {
            0 => fs::write(f.destination(4), b"late external target").unwrap(),
            1 => fs::write(&f.input, b"changed source").unwrap(),
            2 => {
                if let CacheEffect::Install { staged_path, .. } = &plan.effects[0] {
                    fs::write(staged_path, b"changed stage").unwrap();
                }
            }
            3 => {
                plan.cache_root = f._dir.path().join("other");
                fs::create_dir(&plan.cache_root).unwrap();
            }
            4 => {
                plan.effects.push(plan.effects[0].clone());
            }
            _ => unreachable!(),
        };
        assert!(apply(&f.root, &plan).is_err());
        if scenario == 0 {
            assert_eq!(fs::read(f.destination(4)).unwrap(), b"late external target");
        } else {
            assert!(!f.destination(4).exists());
        }
        drop(plan);
        f.service.shutdown().await.unwrap();
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancel_during_copy_and_after_backup_rolls_back_original() {
    for point in [
        Point::CopyChunk,
        Point::AfterBackup(0),
        Point::AfterPromotion(0),
    ] {
        let f = Fixture::new();
        f.stale(4);
        let cancel = CancellationToken::new();
        let plan = f.generated(cancel.clone()).await;
        let mut fired = false;
        let error = linux::apply(&f.root, &plan, EffectLimits::default(), &mut |at| {
            if at == point {
                fired = true;
                cancel.cancel();
            }
        })
        .err()
        .unwrap();
        assert!(fired);
        assert_eq!(error.code, "cancelled");
        assert!(!error.rollback_incomplete, "{:?}", error);
        assert_eq!(
            fs::read(f.destination(4)).unwrap(),
            b"original cached bytes"
        );
        assert_eq!(fs::read_dir(&f.root).unwrap().count(), 1);
        drop(plan);
        f.service.shutdown().await.unwrap();
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn changed_promoted_target_is_preserved_with_incomplete_rollback() {
    let f = Fixture::new();
    f.stale(4);
    let cancel = CancellationToken::new();
    let plan = f.generated(cancel.clone()).await;
    let error = linux::apply(&f.root, &plan, EffectLimits::default(), &mut |point| {
        if point == Point::AfterPromotion(0) {
            fs::write(f.destination(4), b"external edit after promotion").unwrap();
            cancel.cancel();
        }
    })
    .err()
    .unwrap();
    assert!(error.rollback_incomplete);
    assert_eq!(
        fs::read(f.destination(4)).unwrap(),
        b"external edit after promotion"
    );
    let private = fs::read_dir(&f.root)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_type().unwrap().is_dir())
        .unwrap()
        .path();
    assert_eq!(
        fs::read(private.join("backup-0")).unwrap(),
        b"original cached bytes"
    );
    drop(plan);
    f.service.shutdown().await.unwrap();
}
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn unverified_platform_is_explicit() {
    let f = Fixture::new();
    let result = f
        .service
        .prepare_capture(
            f.input.to_str().unwrap(),
            4,
            &ThumbnailRoots {
                cache_root: f.root.clone(),
            },
            CancellationToken::new(),
        )
        .await;
    assert_eq!(result.err().unwrap().code, "unsupported_platform");
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn four_ctime_invalidations_and_metadata_changes_are_guarded() {
    let f = Fixture::new();
    for size in [900, 500, 250, 100] {
        f.stale(size);
    }
    let plan = f
        .service
        .prepare(
            "DOES_MEDIA_EXIST",
            &serde_json::json!({"path":f.input,"creationTime":1}),
            &ThumbnailRoots {
                cache_root: f.root.clone(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(plan.effects.len(), 4);
    let receipt = apply(&f.root, &plan).unwrap();
    assert_eq!(receipt.effects().len(), 4);
    assert!(receipt.effects().iter().all(|e| !e.installed));
    assert_eq!(fs::read_dir(&f.root).unwrap().count(), 0);
    drop(plan);
    f.service.shutdown().await.unwrap();
    for ctime_only in [true, false] {
        let f = Fixture::new();
        f.stale(4);
        let plan = f.pending(CancellationToken::new()).await;
        if ctime_only {
            let expected = match &plan.effects[0] {
                CacheEffect::Invalidate { expected, .. } => expected.clone(),
                _ => unreachable!(),
            };
            let link = f._dir.path().join("external-link");
            let mut observed = false;
            for _ in 0..100 {
                tokio::time::sleep(Duration::from_millis(2)).await;
                if link.exists() {
                    fs::remove_file(&link).unwrap();
                }
                fs::hard_link(f.destination(4), &link).unwrap();
                let now = freeshow_thumbnails::trusted_file_identity(
                    &fs::File::open(f.destination(4)).unwrap(),
                )
                .unwrap();
                assert_eq!(
                    (now.mtime_seconds, now.mtime_nanos),
                    (expected.mtime_seconds, expected.mtime_nanos)
                );
                if (now.ctime_seconds, now.ctime_nanos)
                    != (expected.ctime_seconds, expected.ctime_nanos)
                {
                    observed = true;
                    break;
                }
            }
            assert!(
                observed,
                "Filesystem did not expose a changed ctime for hardlink mutation"
            );
        } else {
            fs::File::options()
                .write(true)
                .open(f.destination(4))
                .unwrap()
                .set_times(
                    fs::FileTimes::new()
                        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(2)),
                )
                .unwrap();
        }
        assert!(apply(&f.root, &plan).is_err());
        assert_eq!(
            fs::read(f.destination(4)).unwrap(),
            b"original cached bytes"
        );
        drop(plan);
        f.service.shutdown().await.unwrap();
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn all_target_guards_preflight_before_first_invalidation() {
    let f = Fixture::new();
    for size in [900, 500, 250, 100] {
        f.stale(size);
    }
    // Prepare against real source so all four exact source-hash cache paths belong.
    let plan = f
        .service
        .prepare(
            "DOES_MEDIA_EXIST",
            &serde_json::json!({"path":f.input,"creationTime":1}),
            &ThumbnailRoots {
                cache_root: f.root.clone(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    fs::write(f.destination(100), b"last target changed").unwrap();
    assert!(apply(&f.root, &plan).is_err());
    for size in [900, 500, 250] {
        assert_eq!(
            fs::read(f.destination(size)).unwrap(),
            b"original cached bytes"
        );
    }
    drop(plan);
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn late_no_clobber_collision_preserves_external_target_and_old_backup() {
    let f = Fixture::new();
    f.stale(4);
    let plan = f.generated(CancellationToken::new()).await;
    let error = linux::apply(&f.root, &plan, EffectLimits::default(), &mut |point| {
        if point == Point::BeforePromotion(0) {
            fs::write(f.destination(4), b"external collision").unwrap();
        }
    })
    .err()
    .unwrap();
    assert_eq!(error.code, "thumbnail_effect_collision");
    assert!(error.rollback_incomplete);
    assert_eq!(fs::read(f.destination(4)).unwrap(), b"external collision");
    let retained = fs::read_dir(&f.root)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_type().unwrap().is_dir())
        .unwrap()
        .path();
    assert_eq!(
        fs::read(retained.join("backup-0")).unwrap(),
        b"original cached bytes"
    );
    assert!(!retained.join("new-0").exists());
    drop(plan);
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn source_parent_stage_and_cache_symlinks_or_replacements_are_rejected() {
    use std::os::unix::fs::symlink;
    for scenario in 0..4 {
        let f = Fixture::new();
        let plan = f.generated(CancellationToken::new()).await;
        let outside = f._dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("unrelated"), b"external unchanged").unwrap();
        match scenario {
            0 => {
                fs::remove_file(&f.input).unwrap();
                symlink(outside.join("unrelated"), &f.input).unwrap();
            }
            1 => {
                fs::rename(&f.root, f._dir.path().join("old-cache")).unwrap();
                symlink(&outside, &f.root).unwrap();
            }
            2 => {
                if let CacheEffect::Install { staged_path, .. } = &plan.effects[0] {
                    fs::remove_file(staged_path).unwrap();
                    symlink(outside.join("unrelated"), staged_path).unwrap();
                }
            }
            3 => {
                symlink(outside.join("unrelated"), f.destination(4)).unwrap();
            }
            _ => unreachable!(),
        };
        assert!(apply(&f.root, &plan).is_err());
        assert_eq!(
            fs::read(outside.join("unrelated")).unwrap(),
            b"external unchanged"
        );
        drop(plan);
        f.service.shutdown().await.unwrap();
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn parent_rename_during_copy_cannot_write_replacement_tree() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let plan = f.generated(CancellationToken::new()).await;
    let moved = f._dir.path().join("moved-cache");
    let outside = f._dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let mut changed = false;
    let error = linux::apply(&f.root, &plan, EffectLimits::default(), &mut |point| {
        if point == Point::CopyChunk && !changed {
            changed = true;
            fs::rename(&f.root, &moved).unwrap();
            symlink(&outside, &f.root).unwrap();
        }
    })
    .err()
    .unwrap();
    assert!(changed);
    assert!(!error.rollback_incomplete, "{error:?}");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&moved).unwrap().count(), 0);
    drop(plan);
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn sparse_no_read_video_is_only_metadata_guarded() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    fs::File::options()
        .write(true)
        .open(&f.input)
        .unwrap()
        .set_len(64 * 1024 * 1024)
        .unwrap();
    fs::set_permissions(&f.input, fs::Permissions::from_mode(0o0)).unwrap();
    let plan = f.generated(CancellationToken::new()).await;
    apply(&f.root, &plan).unwrap();
    assert_eq!(fs::read(f.destination(4)).unwrap(), png());
    drop(plan);
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn limits_and_canceled_ack_do_not_advance_plan() {
    let f = Fixture::new();
    let generated = f.generated(CancellationToken::new()).await;
    for limits in [
        EffectLimits {
            max_bytes: 1,
            ..EffectLimits::default()
        },
        EffectLimits {
            max_operations: 1,
            ..EffectLimits::default()
        },
        EffectLimits {
            max_chunks: 1,
            ..EffectLimits::default()
        },
    ] {
        assert!(apply_with_limits(&f.root, &generated, limits).is_err());
        assert!(!f.destination(4).exists());
        assert_eq!(fs::read_dir(&f.root).unwrap().count(), 0);
    }
    drop(generated);
    f.stale(4);
    let cancel = CancellationToken::new();
    let mut pending = f.pending(cancel.clone()).await;
    let mut receipt = apply(&f.root, &pending).unwrap();
    cancel.cancel();
    assert!(receipt.acknowledge_preparation(&mut pending).is_err());
    assert!(!pending.effects.is_empty());
    drop(pending);
    f.service.shutdown().await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn generated_outcome_without_install_never_reports_success() {
    let f = Fixture::new();
    let mut plan = f.generated(CancellationToken::new()).await;
    plan.effects.clear();
    assert_eq!(
        apply(&f.root, &plan).err().unwrap().code,
        "thumbnail_effect_invalid_plan"
    );
    assert!(!f.destination(4).exists());
    drop(plan);
    f.service.shutdown().await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn arbitrary_same_png_replacement_after_persist_is_never_adopted() {
    let f = Fixture::new();
    f.stale(4);
    let plan = f.generated(CancellationToken::new()).await;
    let external = f._dir.path().join("external-same-png");
    fs::write(&external, png()).unwrap();
    let error = linux::apply(&f.root, &plan, EffectLimits::default(), &mut |point| {
        if point == Point::PromotionRenamed(0) {
            fs::rename(&external, f.destination(4)).unwrap();
        }
    })
    .err()
    .unwrap();
    assert!(error.rollback_incomplete);
    assert_eq!(fs::read(f.destination(4)).unwrap(), png());
    let retained = fs::read_dir(&f.root)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_type().unwrap().is_dir())
        .unwrap()
        .path();
    assert_eq!(
        fs::read(retained.join("backup-0")).unwrap(),
        b"original cached bytes"
    );
    drop(plan);
    f.service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn canceled_four_effect_batch_compensates_every_original() {
    let f = Fixture::new();
    for size in [900, 500, 250, 100] {
        f.stale(size);
    }
    let cancel = CancellationToken::new();
    let plan = f
        .service
        .prepare(
            "DOES_MEDIA_EXIST",
            &serde_json::json!({"path":f.input,"creationTime":1}),
            &ThumbnailRoots {
                cache_root: f.root.clone(),
            },
            cancel.clone(),
        )
        .await
        .unwrap();
    let error = linux::apply(&f.root, &plan, EffectLimits::default(), &mut |point| {
        if point == Point::AfterBackup(3) {
            cancel.cancel();
        }
    })
    .err()
    .unwrap();
    assert!(!error.rollback_incomplete, "{error:?}");
    for size in [900, 500, 250, 100] {
        assert_eq!(
            fs::read(f.destination(size)).unwrap(),
            b"original cached bytes"
        );
    }
    assert_eq!(fs::read_dir(&f.root).unwrap().count(), 4);
    drop(plan);
    f.service.shutdown().await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn oversized_existing_cache_is_resource_rejected_without_read_or_delete() {
    let f = Fixture::new();
    f.stale(4);
    fs::File::options()
        .write(true)
        .open(f.destination(4))
        .unwrap()
        .set_len(33 * 1024 * 1024)
        .unwrap();
    fs::File::options()
        .write(true)
        .open(f.destination(4))
        .unwrap()
        .set_times(
            fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
        )
        .unwrap();
    let plan = f.pending(CancellationToken::new()).await;
    let error = apply(&f.root, &plan).err().unwrap();
    assert_eq!(error.code, "thumbnail_effect_limit");
    assert_eq!(
        fs::metadata(f.destination(4)).unwrap().len(),
        33 * 1024 * 1024
    );
    assert_eq!(fs::read_dir(&f.root).unwrap().count(), 1);
    drop(plan);
    f.service.shutdown().await.unwrap();
}
