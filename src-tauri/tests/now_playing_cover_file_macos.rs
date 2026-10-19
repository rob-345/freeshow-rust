//! Real Darwin filesystem tests. Cross-compilation is not execution evidence.
#![cfg(target_os = "macos")]
#[allow(dead_code)]
#[path = "../src/now_playing_cover_file.rs"]
mod cover;
use cover::*;
use freeshow_core::{
    protocol::Request,
    runtime::Runtime,
    service::{LibraryPhase, LibraryService},
};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::PathBuf,
    time::{Duration, Instant},
};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Fixture {
    temp: tempfile::TempDir,
    base: PathBuf,
    library: LibraryService,
    files: CoverFiles,
}
impl Fixture {
    fn new() -> Self {
        Self::with_depth(0)
    }
    fn with_depth(depth: usize) -> Self {
        let temp = tempfile::tempdir().unwrap();
        // Native fixture setup only: /var and /tmp may themselves be symlinks
        // on macOS. Production roots are never canonicalized by the backend.
        let base = temp.path().canonicalize().unwrap();
        let mut root = base.join("parent");
        for _ in 0..depth {
            root.push("p");
        }
        root.push("selected 敬拜");
        let library = LibraryService::start(Runtime::new(base.join("app"), root).unwrap()).unwrap();
        Self {
            temp,
            base,
            library,
            files: CoverFiles::default(),
        }
    }
    fn root(&self) -> PathBuf {
        self.library.context().unwrap().data_path
    }
    fn audio(&self) -> PathBuf {
        self.root().join("Audio")
    }
    fn leaf(&self) -> PathBuf {
        self.audio().join("NowPlayingCover.png")
    }
    fn request(&self) -> Request {
        serde_json::from_value(
            json!({"channel":"MAIN","data":null,"context":self.library.context().unwrap()}),
        )
        .unwrap()
    }
    async fn phase(&self) -> LibraryPhase {
        self.library
            .begin_phase(self.request(), "main")
            .await
            .unwrap()
    }
    fn write(&self, bytes: &[u8]) {
        fs::create_dir_all(self.audio()).unwrap();
        fs::write(self.leaf(), bytes).unwrap();
    }
    async fn prepare(&self, phase: &LibraryPhase) -> Prepared {
        self.files
            .submit(phase, Control::default())
            .unwrap()
            .finish()
            .await
            .unwrap()
    }
    async fn done(self, phase: LibraryPhase) {
        phase.finish().await.unwrap();
        self.library.finish_writes().await.unwrap();
        // The canonical native base remains within this real temporary owner.
        assert!(self.temp.path().exists());
    }
}
fn file(prepared: &Prepared) -> &HeldCover {
    match prepared.outcome() {
        Outcome::File(file) => file,
        _ => panic!("Expected held Darwin regular-file authority"),
    }
}
fn failure(prepared: &Prepared) -> &Failure {
    match prepared.outcome() {
        Outcome::Failed(failure) => failure,
        _ => panic!("Expected explicit failure, never invented Missing"),
    }
}

#[tokio::test]
async fn absent_audio_is_created_and_real_regular_blocker_keeps_one_mkdir_failure_and_missing() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    assert!(!f.audio().exists());
    let missing = f.prepare(&phase).await;
    assert!(matches!(missing.outcome(), Outcome::Missing));
    assert!(f.audio().is_dir());
    assert!(missing.observation().folder_failure().is_none());
    assert_eq!(
        missing.observation().diagnostics()[0].os_code,
        Some(libc::ENOENT)
    );
    let validation = missing
        .observation()
        .revalidate(&phase, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert!(validation.stat().unwrap().is_none());
    drop(validation);
    drop(missing);
    fs::remove_dir(f.audio()).unwrap();
    fs::write(f.audio(), b"unchanged actual blocker").unwrap();
    let blocked = f.prepare(&phase).await;
    assert!(matches!(blocked.outcome(), Outcome::Missing));
    let observed = blocked.observation().folder_failure().unwrap();
    assert_eq!(observed.folder(), f.audio());
    assert_eq!(observed.diagnostic().os_code, Some(libc::EEXIST));
    assert!(observed.diagnostic().message.len() <= 1024);
    assert_eq!(blocked.observation().diagnostics().len(), 1);
    assert_eq!(
        blocked.observation().diagnostics()[0].os_code,
        Some(libc::ENOTDIR)
    );
    assert_eq!(fs::read(f.audio()).unwrap(), b"unchanged actual blocker");
    drop(blocked);
    assert_eq!(f.files.retained(), 0);
    f.done(phase).await;
}

#[tokio::test]
async fn opaque_empty_and_limit_files_use_positional_reads_and_raw_signed_darwin_times() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    for bytes in [&b"opaque\0\xffnot a PNG"[..], &b""[..]] {
        f.write(bytes);
        let opened = f.prepare(&phase).await;
        let held = file(&opened);
        let actual = fs::metadata(f.leaf()).unwrap();
        assert_eq!(held.stat().device(), actual.dev());
        assert_eq!(held.stat().inode(), actual.ino());
        assert_eq!(held.stat().length(), bytes.len() as u64);
        assert_eq!(held.stat().modified_seconds(), actual.mtime());
        assert_eq!(held.stat().modified_nanoseconds(), actual.mtime_nsec());
        assert_eq!(held.stat().changed_seconds(), actual.ctime());
        assert_eq!(held.stat().changed_nanoseconds(), actual.ctime_nsec());
        let mut out = [0; 64];
        let count = held.read_at(0, &mut out).unwrap();
        assert_eq!(&out[..count], bytes);
        assert_eq!(held.read_at(0, &mut out).unwrap(), count);
        assert!(held.read_at(0, &mut vec![0; MAX_CHUNK + 1]).is_err());
        assert!(held.read_at(MAX_FILE, &mut [0; 1]).is_err());
        let checked = held
            .revalidate(&phase, Control::default())
            .unwrap()
            .finish()
            .await
            .unwrap();
        assert_eq!(checked.stat().unwrap(), Some(held.stat()));
        drop(checked);
        drop(opened);
    }
    let writable = fs::OpenOptions::new().write(true).open(f.leaf()).unwrap();
    writable.set_len(MAX_FILE).unwrap();
    writable
        .set_times(
            fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH - Duration::new(1, 250_000_000)),
        )
        .unwrap();
    let exact = f.prepare(&phase).await;
    assert_eq!(file(&exact).stat().length(), MAX_FILE);
    let native = fs::metadata(f.leaf()).unwrap();
    assert_eq!(file(&exact).stat().modified_seconds(), native.mtime());
    assert_eq!(
        file(&exact).stat().modified_nanoseconds(),
        native.mtime_nsec()
    );
    assert_eq!(
        i128::from(file(&exact).stat().modified_seconds()) * 1_000_000_000
            + i128::from(file(&exact).stat().modified_nanoseconds()),
        -1_250_000_000,
        "Preserve the actual Darwin representation without normalizing it"
    );
    drop(exact);
    writable.set_len(MAX_FILE + 1).unwrap();
    let large = f.prepare(&phase).await;
    assert_eq!(failure(&large).kind, FailureKind::Resource);
    drop(large);
    f.done(phase).await;
}

#[tokio::test]
async fn global_four_leases_span_contexts_and_held_descriptor_outlives_discarded_observation() {
    let _serial = SERIAL.lock().await;
    let a = Fixture::new();
    let b = Fixture::new();
    a.write(b"A");
    b.write(b"B");
    let ap = a.phase().await;
    let bp = b.phase().await;
    let mut prepared = vec![
        a.prepare(&ap).await,
        a.prepare(&ap).await,
        b.prepare(&bp).await,
        b.prepare(&bp).await,
    ];
    assert_eq!(a.files.retained(), 4);
    assert_eq!(b.files.retained(), 4);
    assert_eq!(
        a.files.submit(&ap, Control::default()).err().unwrap().kind,
        FailureKind::Busy
    );
    let (observation, outcome) = prepared.pop().unwrap().into_parts();
    drop(observation);
    assert_eq!(a.files.retained(), 4);
    bp.finish().await.unwrap();
    assert_eq!(
        a.files.retained(),
        4,
        "A phase fence does not dispose held descriptors"
    );
    drop(outcome);
    let replacement = a.prepare(&ap).await;
    assert_eq!(a.files.retained(), 4);
    drop(replacement);
    drop(prepared);
    assert_eq!(a.files.retained(), 0);
    b.done(bp).await;
    a.done(ap).await;
}

#[tokio::test]
async fn completed_uncollected_results_keep_all_four_leases_until_real_receipt_disposal() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    f.write(b"ready");
    let phase = f.phase().await;
    let mut tickets = Vec::new();
    for _ in 0..4 {
        tickets.push(f.files.submit(&phase, Control::default()).unwrap());
    }
    phase.finish().await.unwrap();
    assert_eq!(f.files.retained(), 4);
    let other = Fixture::new();
    let other_phase = other.phase().await;
    assert_eq!(
        other
            .files
            .submit(&other_phase, Control::default())
            .err()
            .unwrap()
            .kind,
        FailureKind::Busy
    );
    drop(tickets.pop());
    assert_eq!(f.files.retained(), 3);
    let admitted = other.prepare(&other_phase).await;
    assert_eq!(f.files.retained(), 4);
    drop(admitted);
    drop(tickets);
    assert_eq!(f.files.retained(), 0);
    other.done(other_phase).await;
    f.done(phase).await;
}

#[tokio::test]
async fn accepted_caller_drop_does_not_cancel_actual_ordered_preparation_or_release_early() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    let (entered, release) = f.files.test_pause_macos(MacTestStage::RootAnchored);
    let ticket = f.files.submit(&phase, Control::default()).unwrap();
    entered.await.unwrap();
    drop(ticket);
    assert_eq!(f.files.retained(), 1);
    assert!(!f.audio().exists());
    release.send(()).unwrap();
    phase.finish().await.unwrap();
    assert!(
        f.audio().is_dir(),
        "Accepted mkdir completed after caller dropped"
    );
    assert_eq!(f.files.retained(), 0);
    f.done(phase).await;
}

#[tokio::test]
async fn root_parent_leaf_swaps_and_exists_true_then_unlink_never_return_false_missing() {
    let _serial = SERIAL.lock().await;
    for stage in [
        MacTestStage::RootAnchored,
        MacTestStage::AudioAnchored,
        MacTestStage::BeforeLeafOpen,
        MacTestStage::LeafOpened,
    ] {
        let f = Fixture::new();
        f.write(b"original authorized bytes");
        let phase = f.phase().await;
        let (entered, release) = f.files.test_pause_macos(stage);
        let ticket = f.files.submit(&phase, Control::default()).unwrap();
        entered.await.unwrap();
        match stage {
            MacTestStage::RootAnchored => {
                fs::rename(f.root(), f.base.join("held-root")).unwrap();
                f.write(b"replacement root");
            }
            MacTestStage::AudioAnchored => {
                fs::rename(f.audio(), f.root().join("held-audio")).unwrap();
                f.write(b"replacement Audio");
            }
            MacTestStage::BeforeLeafOpen => fs::remove_file(f.leaf()).unwrap(),
            MacTestStage::LeafOpened => {
                fs::rename(f.leaf(), f.audio().join("held-cover")).unwrap();
                fs::write(f.leaf(), b"replacement leaf").unwrap();
            }
        }
        release.send(()).unwrap();
        let result = ticket.finish().await.unwrap();
        let error = failure(&result);
        if stage == MacTestStage::BeforeLeafOpen {
            assert_eq!(error.kind, FailureKind::Io);
            assert_eq!(error.os_code, Some(libc::ENOENT));
        } else {
            assert_eq!(error.kind, FailureKind::Changed);
        }
        drop(result);
        f.done(phase).await;
    }
}

#[tokio::test]
async fn real_mkdir_failure_is_preserved_when_later_parent_check_or_cancellation_fails() {
    let _serial = SERIAL.lock().await;
    for cancel in [false, true] {
        let f = Fixture::new();
        let phase = f.phase().await;
        fs::write(f.audio(), b"original blocker").unwrap();
        let (entered, release) = f.files.test_pause_macos(MacTestStage::AudioAnchored);
        let control = Control::default();
        let ticket = f.files.submit(&phase, control.clone()).unwrap();
        entered.await.unwrap();
        if cancel {
            control.cancel.cancel();
        } else {
            fs::rename(f.audio(), f.root().join("original-blocker")).unwrap();
            fs::create_dir(f.audio()).unwrap();
        }
        release.send(()).unwrap();
        let result = ticket.finish().await.unwrap();
        assert_eq!(
            failure(&result).kind,
            if cancel {
                FailureKind::Cancelled
            } else {
                FailureKind::Changed
            }
        );
        let original = result.observation().folder_failure().unwrap();
        assert_eq!(original.folder(), f.audio());
        assert_eq!(original.diagnostic().os_code, Some(libc::EEXIST));
        assert_eq!(
            fs::read(if cancel {
                f.audio()
            } else {
                f.root().join("original-blocker")
            })
            .unwrap(),
            b"original blocker"
        );
        drop(result);
        f.done(phase).await;
    }
}

#[tokio::test]
async fn before_header_revalidation_detects_changes_but_held_reads_are_not_immutable_snapshots() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    f.write(b"old");
    let old = f.prepare(&phase).await;
    fs::rename(f.leaf(), f.audio().join("old-cover")).unwrap();
    fs::write(f.leaf(), b"new").unwrap();
    let checked = file(&old)
        .revalidate(&phase, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(checked.stat().unwrap_err().kind, FailureKind::Changed);
    let mut out = [0; 3];
    file(&old).read_at(0, &mut out).unwrap();
    assert_eq!(&out, b"old");
    drop(checked);
    drop(old);
    let writable = fs::OpenOptions::new().write(true).open(f.leaf()).unwrap();
    let original_modified = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    writable
        .set_times(fs::FileTimes::new().set_modified(original_modified))
        .unwrap();
    let current = f.prepare(&phase).await;
    let expected = file(&current).stat().clone();
    assert_eq!(expected.length(), 3);
    assert_eq!(expected.modified_seconds(), 1_700_000_000);
    // Keep both identity and length unchanged: this regression must exercise
    // the version guard rather than pass solely because the body got shorter.
    // Explicit real mtime changes avoid any assumption about clock resolution.
    fs::write(f.leaf(), b"xyz").unwrap();
    writable
        .set_times(fs::FileTimes::new().set_modified(original_modified + Duration::from_secs(2)))
        .unwrap();
    let modified = fs::metadata(f.leaf()).unwrap();
    assert_eq!(modified.dev(), expected.device());
    assert_eq!(modified.ino(), expected.inode());
    assert_eq!(modified.len(), expected.length());
    assert_eq!(modified.mtime(), 1_700_000_002);
    assert_ne!(
        (modified.mtime(), modified.mtime_nsec()),
        (expected.modified_seconds(), expected.modified_nanoseconds())
    );
    // The held descriptor still exposes the actual in-place write, not a copy.
    let count = file(&current).read_at(0, &mut out).unwrap();
    assert_eq!(&out[..count], b"xyz");
    let changed = file(&current)
        .revalidate(&phase, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(changed.stat().unwrap_err().kind, FailureKind::Changed);
    drop(changed);
    let other = Fixture::new();
    let other_phase = other.phase().await;
    assert_eq!(
        file(&current)
            .revalidate(&other_phase, Control::default())
            .err()
            .unwrap()
            .kind,
        FailureKind::Changed
    );
    drop(current);
    other.done(other_phase).await;
    f.done(phase).await;
}

#[tokio::test]
async fn symlink_ancestors_parents_leaves_and_nonregular_variants_are_never_followed() {
    let _serial = SERIAL.lock().await;
    for variant in ["ancestor", "root", "audio", "leaf", "directory", "fifo"] {
        let f = Fixture::new();
        let phase = f.phase().await;
        let outside = f.base.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("NowPlayingCover.png"), b"outside unchanged").unwrap();
        match variant {
            "ancestor" => {
                fs::rename(f.base.join("parent"), f.base.join("original-parent")).unwrap();
                symlink(f.base.join("original-parent"), f.base.join("parent")).unwrap();
            }
            "root" => {
                fs::rename(f.root(), f.base.join("original-root")).unwrap();
                symlink(f.base.join("original-root"), f.root()).unwrap();
            }
            "audio" => symlink(&outside, f.audio()).unwrap(),
            "leaf" => {
                fs::create_dir(f.audio()).unwrap();
                symlink(outside.join("NowPlayingCover.png"), f.leaf()).unwrap();
            }
            "directory" => fs::create_dir_all(f.leaf()).unwrap(),
            "fifo" => {
                fs::create_dir(f.audio()).unwrap();
                let path = std::ffi::CString::new(f.leaf().as_os_str().as_encoded_bytes()).unwrap();
                // SAFETY: fixed temporary fixture pathname and permission mode.
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }
            _ => unreachable!(),
        }
        let prepared = f.prepare(&phase).await;
        assert_eq!(failure(&prepared).kind, FailureKind::Unsupported);
        assert_eq!(
            fs::read(outside.join("NowPlayingCover.png")).unwrap(),
            b"outside unchanged"
        );
        drop(prepared);
        f.done(phase).await;
    }
}

#[tokio::test]
async fn actual_read_permission_failures_are_failed_not_missing_and_do_not_claim_source_parity() {
    let _serial = SERIAL.lock().await;
    // SAFETY: read-only process credential observation; no identity changes.
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "Darwin permission evidence requires a non-root user; root must not count as a verified permission test"
    );
    let f = Fixture::new();
    f.write(b"cannot read after exists-true");
    let phase = f.phase().await;
    for path in [f.root(), f.audio(), f.leaf()] {
        let original = fs::metadata(&path).unwrap().permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
        let prepared = f.prepare(&phase).await;
        fs::set_permissions(&path, original).unwrap();
        assert_eq!(failure(&prepared).kind, FailureKind::Io);
        assert_eq!(failure(&prepared).os_code, Some(libc::EACCES));
        drop(prepared);
    }
    f.done(phase).await;
}

#[tokio::test]
async fn cancellation_deadline_and_root_work_bounds_do_not_dispose_accepted_results_early() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    let cancelled = Control::default();
    cancelled.cancel.cancel();
    assert_eq!(
        f.files.submit(&phase, cancelled).err().unwrap().kind,
        FailureKind::Cancelled
    );
    assert_eq!(f.files.retained(), 0);
    let expired = Control {
        deadline: Instant::now() - Duration::from_secs(1),
        ..Control::default()
    };
    assert_eq!(
        f.files.submit(&phase, expired).err().unwrap().kind,
        FailureKind::Deadline
    );
    let (entered, release) = f.files.test_pause_macos(MacTestStage::RootAnchored);
    let control = Control::default();
    let ticket = f.files.submit(&phase, control.clone()).unwrap();
    entered.await.unwrap();
    control.cancel.cancel();
    assert_eq!(f.files.retained(), 1);
    release.send(()).unwrap();
    let result = ticket.finish().await.unwrap();
    assert_eq!(failure(&result).kind, FailureKind::Cancelled);
    assert_eq!(f.files.retained(), 1);
    drop(result);
    assert_eq!(f.files.retained(), 0);
    assert!(!f.audio().exists());
    f.done(phase).await;
    let deep = Fixture::with_depth(130);
    let phase = deep.phase().await;
    let bounded = deep.prepare(&phase).await;
    assert_eq!(failure(&bounded).kind, FailureKind::Resource);
    assert!(!deep.audio().exists());
    drop(bounded);
    deep.done(phase).await;
}

#[tokio::test]
async fn validation_queued_active_ready_and_collected_receipts_keep_one_real_continuation_guard() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    f.write(b"valid");
    let phase = f.phase().await;
    let prepared = f.prepare(&phase).await;
    let ready = file(&prepared)
        .revalidate(&phase, Control::default())
        .unwrap();
    let validation = ready.finish().await.unwrap();
    assert!(validation.stat().is_ok());
    assert_eq!(
        file(&prepared)
            .revalidate(&phase, Control::default())
            .err()
            .unwrap()
            .kind,
        FailureKind::Busy
    );
    drop(validation);
    let (entered, wait) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let owner = phase
        .submit(f.request(), move |_| {
            let _ = entered.send(());
            blocked.recv().unwrap();
            Ok(vec![])
        })
        .unwrap();
    wait.await.unwrap();
    let queued = file(&prepared)
        .revalidate(&phase, Control::default())
        .unwrap();
    drop(queued);
    assert_eq!(
        file(&prepared)
            .revalidate(&phase, Control::default())
            .err()
            .unwrap()
            .kind,
        FailureKind::Busy
    );
    release.send(()).unwrap();
    owner.finish().await.unwrap();
    phase
        .submit(f.request(), |_| Ok(vec![]))
        .unwrap()
        .finish()
        .await
        .unwrap();
    let renewed = file(&prepared)
        .revalidate(&phase, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert!(renewed.stat().is_ok());
    drop(renewed);
    drop(prepared);
    assert_eq!(f.files.retained(), 0);
    f.done(phase).await;
}
