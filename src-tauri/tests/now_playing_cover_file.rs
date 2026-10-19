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
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::time::Duration;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Fixture {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    temp: tempfile::TempDir,
    library: LibraryService,
    files: CoverFiles,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(temp.path().join("app"), temp.path().join("selected")).unwrap(),
        )
        .unwrap();
        Self {
            temp,
            library,
            files: CoverFiles::default(),
        }
    }
    fn root(&self) -> std::path::PathBuf {
        self.library.context().unwrap().data_path
    }
    fn audio(&self) -> std::path::PathBuf {
        self.root().join("Audio")
    }
    #[cfg(target_os = "linux")]
    fn leaf(&self) -> std::path::PathBuf {
        self.audio().join("NowPlayingCover.png")
    }
    async fn phase(&self) -> LibraryPhase {
        let request: Request = serde_json::from_value(
            json!({"channel":"MAIN","data":null,"context":self.library.context().unwrap()}),
        )
        .unwrap();
        self.library.begin_phase(request, "main").await.unwrap()
    }
    #[cfg(target_os = "linux")]
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
    }
}
#[cfg(target_os = "linux")]
fn file(value: &Prepared) -> &HeldCover {
    match value.outcome() {
        Outcome::File(file) => file,
        _ => panic!("Expected actual held regular cover"),
    }
}
fn failed(value: &Prepared) -> &Failure {
    match value.outcome() {
        Outcome::Failed(error) => error,
        _ => panic!("Expected explicit native failure"),
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn absent_audio_is_actually_created_missing_is_not_later_open_failure_and_blocker_emits_one_observation(
) {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    assert!(!f.audio().exists());
    let missing = f.prepare(&phase).await;
    assert!(matches!(missing.outcome(), Outcome::Missing));
    assert!(f.audio().is_dir());
    assert!(missing.observation().folder_failure().is_none());
    let valid = missing
        .observation()
        .revalidate(&phase, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert!(valid.stat().unwrap().is_none());
    drop(valid);
    drop(missing);
    fs::remove_dir(f.audio()).unwrap();
    fs::write(f.audio(), b"actual unchanged blocker").unwrap();
    let blocked = f.prepare(&phase).await;
    assert!(matches!(blocked.outcome(), Outcome::Missing));
    let observation = blocked.observation().folder_failure().unwrap();
    assert_eq!(observation.folder(), f.audio());
    assert!(observation.diagnostic().os_code.is_some());
    assert!(observation.diagnostic().message.len() <= 1024);
    assert_eq!(fs::read(f.audio()).unwrap(), b"actual unchanged blocker");
    assert_eq!(blocked.observation().diagnostics().len(), 1);
    drop(blocked);
    assert_eq!(f.files.retained(), 0);
    f.done(phase).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn actual_folder_failure_survives_later_changed_failure_and_inaccessible_source_exists_false()
{
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    fs::write(f.audio(), b"regular blocker").unwrap();
    let (entered, release) = f.files.test_pause(TestStage::AudioAnchored);
    let ticket = f.files.submit(&phase, Control::default()).unwrap();
    entered.await.unwrap();
    fs::rename(f.audio(), f.root().join("old-blocker")).unwrap();
    fs::create_dir(f.audio()).unwrap();
    release.send(()).unwrap();
    let changed = ticket.finish().await.unwrap();
    assert_eq!(failed(&changed).kind, FailureKind::Changed);
    assert_eq!(
        changed
            .observation()
            .folder_failure()
            .unwrap()
            .diagnostic()
            .os_code,
        Some(libc::EEXIST)
    );
    assert_eq!(
        fs::read(f.root().join("old-blocker")).unwrap(),
        b"regular blocker"
    );
    drop(changed);
    if unsafe { libc::geteuid() } != 0 {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(f.root(), fs::Permissions::from_mode(0o0)).unwrap();
        let inaccessible = f.prepare(&phase).await;
        // Restore before any assertion can panic and leave fixture cleanup denied.
        fs::set_permissions(f.root(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(inaccessible.outcome(), Outcome::Missing));
        assert_eq!(
            inaccessible
                .observation()
                .folder_failure()
                .unwrap()
                .diagnostic()
                .os_code,
            Some(libc::EACCES)
        );
        assert_eq!(inaccessible.observation().diagnostics().len(), 1);
        drop(inaccessible);
    }
    f.done(phase).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn opaque_and_empty_regular_bytes_read_only_from_held_descriptor_with_raw_signed_times_and_size_limits(
) {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    for bytes in [&b"not a PNG\0\xffopaque"[..], &b""[..]] {
        f.write(bytes);
        let opened = f.prepare(&phase).await;
        let held = file(&opened);
        assert_eq!(held.stat().length(), bytes.len() as u64);
        let mut out = [0u8; 64];
        let n = held.read_at(0, &mut out).unwrap();
        assert_eq!(&out[..n], bytes);
        assert!(held.read_at(0, &mut vec![0; MAX_CHUNK + 1]).is_err());
        let valid = held
            .revalidate(&phase, Control::default())
            .unwrap()
            .finish()
            .await
            .unwrap();
        assert_eq!(valid.stat().unwrap(), Some(held.stat()));
        drop(valid);
        drop(opened);
    }
    let file = fs::OpenOptions::new().write(true).open(f.leaf()).unwrap();
    file.set_len(MAX_FILE).unwrap();
    let time = std::time::UNIX_EPOCH - Duration::new(1, 250_000_000);
    file.set_times(fs::FileTimes::new().set_modified(time))
        .unwrap();
    let exact = f.prepare(&phase).await;
    assert_eq!(self::file(&exact).stat().length(), MAX_FILE);
    assert_eq!(self::file(&exact).stat().modified_seconds(), -2);
    assert_eq!(
        self::file(&exact).stat().modified_nanoseconds(),
        750_000_000
    );
    assert!((0..1_000_000_000).contains(&self::file(&exact).stat().changed_nanoseconds()));
    drop(exact);
    file.set_len(MAX_FILE + 1).unwrap();
    let oversized = f.prepare(&phase).await;
    assert_eq!(failed(&oversized).kind, FailureKind::Resource);
    drop(oversized);
    f.done(phase).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn four_process_wide_leases_survive_consumption_phase_fence_and_cross_context_handoff_until_actual_descriptor_drop(
) {
    let _serial = SERIAL.lock().await;
    let a = Fixture::new();
    let b = Fixture::new();
    a.write(b"A");
    b.write(b"B");
    let ap = a.phase().await;
    let bp = b.phase().await;
    let mut results = vec![
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
    let (observation, outcome) = results.pop().unwrap().into_parts();
    drop(observation);
    assert_eq!(
        a.files.retained(),
        4,
        "bare diagnostic disposal cannot release held descriptor quota"
    );
    let Outcome::File(held) = outcome else {
        panic!("Expected file")
    };
    ap.finish().await.unwrap();
    assert_eq!(
        a.files.retained(),
        4,
        "Phase fence is not descriptor/result disposal or IO thread join"
    );
    drop(held);
    assert_eq!(a.files.retained(), 3);
    let extra = b.prepare(&bp).await;
    assert_eq!(a.files.retained(), 4);
    drop(extra);
    drop(results);
    assert_eq!(a.files.retained(), 0);
    bp.finish().await.unwrap();
    a.library.finish_writes().await.unwrap();
    b.library.finish_writes().await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn revalidation_continuation_and_ready_receipt_remain_bounded_and_caller_drop_releases_after_real_return(
) {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    f.write(b"held");
    let prepared = f.prepare(&phase).await;
    let first = file(&prepared)
        .revalidate(&phase, Control::default())
        .unwrap();
    assert_eq!(
        file(&prepared)
            .revalidate(&phase, Control::default())
            .err()
            .unwrap()
            .kind,
        FailureKind::Busy
    );
    let ready = first.finish().await.unwrap();
    assert_eq!(ready.context(), prepared.observation().context());
    assert!(ready.stat().unwrap().is_some());
    assert_eq!(
        file(&prepared)
            .revalidate(&phase, Control::default())
            .err()
            .unwrap()
            .kind,
        FailureKind::Busy,
        "actual completion does not release ready observation ownership"
    );
    drop(ready);
    let abandoned = file(&prepared)
        .revalidate(&phase, Control::default())
        .unwrap();
    drop(abandoned);
    // A separate accepted cover operation is a real FIFO fence, not a sleep or
    // observer timeout. The abandoned validator must return before this result.
    let fence = f.prepare(&phase).await;
    drop(fence);
    let again = file(&prepared)
        .revalidate(&phase, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    drop(prepared);
    assert_eq!(
        f.files.retained(),
        1,
        "validation holds root/Audio/leaf guards"
    );
    drop(again);
    assert_eq!(f.files.retained(), 0);
    f.done(phase).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropped_caller_keeps_actual_blocked_ordered_work_and_ready_results_charged_until_real_return(
) {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    let (entered, release) = f.files.test_pause(TestStage::RootAnchored);
    let first = f.files.submit(&phase, Control::default()).unwrap();
    entered.await.unwrap();
    drop(first);
    assert_eq!(f.files.retained(), 1);
    let remaining: Vec<_> = (0..3)
        .map(|_| f.files.submit(&phase, Control::default()).unwrap())
        .collect();
    assert_eq!(
        f.files
            .submit(&phase, Control::default())
            .err()
            .unwrap()
            .kind,
        FailureKind::Busy
    );
    release.send(()).unwrap();
    phase.finish().await.unwrap();
    assert!(
        f.audio().is_dir(),
        "accepted mkdir completed after caller drop"
    );
    assert_eq!(
        f.files.retained(),
        3,
        "ready but uncollected observations still own their guard leases"
    );
    drop(remaining);
    assert_eq!(f.files.retained(), 0);
    f.library.finish_writes().await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn exists_true_then_disappearance_or_permission_failure_is_failed_not_missing() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    f.write(b"before");
    let (entered, release) = f.files.test_pause(TestStage::BeforeLeafOpen);
    let ticket = f.files.submit(&phase, Control::default()).unwrap();
    entered.await.unwrap();
    fs::remove_file(f.leaf()).unwrap();
    release.send(()).unwrap();
    let missing_after_exists = ticket.finish().await.unwrap();
    assert_eq!(failed(&missing_after_exists).kind, FailureKind::Io);
    assert_eq!(failed(&missing_after_exists).os_code, Some(libc::ENOENT));
    drop(missing_after_exists);
    if unsafe { libc::geteuid() } != 0 {
        use std::os::unix::fs::PermissionsExt;
        f.write(b"cannot read");
        fs::set_permissions(f.leaf(), fs::Permissions::from_mode(0o0)).unwrap();
        let denied = f.prepare(&phase).await;
        assert_eq!(failed(&denied).kind, FailureKind::Io);
        assert_eq!(failed(&denied).os_code, Some(libc::EACCES));
        drop(denied);
        fs::set_permissions(f.leaf(), fs::Permissions::from_mode(0o600)).unwrap();
    }
    f.done(phase).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn anchored_root_audio_and_leaf_swaps_reject_before_headers_without_following_new_targets() {
    let _serial = SERIAL.lock().await;
    for stage in [
        TestStage::RootAnchored,
        TestStage::AudioAnchored,
        TestStage::LeafOpened,
    ] {
        let f = Fixture::new();
        let phase = f.phase().await;
        f.write(b"authorized original");
        let (entered, release) = f.files.test_pause(stage);
        let ticket = f.files.submit(&phase, Control::default()).unwrap();
        entered.await.unwrap();
        match stage {
            TestStage::RootAnchored => {
                fs::rename(f.root(), f.temp.path().join("held-root")).unwrap();
                f.write(b"replacement root bytes");
            }
            TestStage::AudioAnchored => {
                fs::rename(f.audio(), f.root().join("held-audio")).unwrap();
                f.write(b"replacement parent bytes");
            }
            TestStage::LeafOpened => {
                fs::rename(f.leaf(), f.audio().join("held.png")).unwrap();
                fs::write(f.leaf(), b"replacement leaf bytes").unwrap();
            }
            _ => unreachable!(),
        }
        release.send(()).unwrap();
        let result = ticket.finish().await.unwrap();
        assert_eq!(failed(&result).kind, FailureKind::Changed);
        drop(result);
        f.done(phase).await;
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn revalidation_checks_live_context_parent_leaf_and_version_while_reads_remain_on_original_inode(
) {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    f.write(b"old");
    let prepared = f.prepare(&phase).await;
    let held = file(&prepared);
    fs::rename(f.leaf(), f.audio().join("old.png")).unwrap();
    fs::write(f.leaf(), b"new").unwrap();
    let checked = held
        .revalidate(&phase, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(checked.stat().unwrap_err().kind, FailureKind::Changed);
    let mut bytes = [0; 3];
    held.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"old");
    drop(checked);
    drop(prepared);
    let current = f.prepare(&phase).await;
    fs::OpenOptions::new()
        .write(true)
        .open(f.leaf())
        .unwrap()
        .set_len(1)
        .unwrap();
    let checked = file(&current)
        .revalidate(&phase, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(checked.stat().unwrap_err().kind, FailureKind::Changed);
    drop(checked);
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

#[cfg(target_os = "linux")]
#[tokio::test]
async fn symlink_roots_parents_leaves_directories_and_fifos_are_explicitly_unsupported() {
    let _serial = SERIAL.lock().await;
    use std::os::unix::fs::symlink;
    for variant in ["root", "audio", "leaf", "directory", "fifo"] {
        let f = Fixture::new();
        let phase = f.phase().await;
        let outside = f.temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("NowPlayingCover.png"), b"outside").unwrap();
        match variant {
            "root" => {
                fs::rename(f.root(), f.temp.path().join("real-root")).unwrap();
                symlink(f.temp.path().join("real-root"), f.root()).unwrap();
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
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }
            _ => unreachable!(),
        }
        let result = f.prepare(&phase).await;
        assert_eq!(failed(&result).kind, FailureKind::Unsupported);
        drop(result);
        assert_eq!(
            fs::read(outside.join("NowPlayingCover.png")).unwrap(),
            b"outside"
        );
        f.done(phase).await;
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancellation_before_admission_and_after_acceptance_preserves_real_cleanup_and_budget() {
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
    let (entered, release) = f.files.test_pause(TestStage::RootAnchored);
    let control = Control::default();
    let ticket = f.files.submit(&phase, control.clone()).unwrap();
    entered.await.unwrap();
    control.cancel.cancel();
    assert_eq!(f.files.retained(), 1);
    release.send(()).unwrap();
    let result = ticket.finish().await.unwrap();
    assert_eq!(failed(&result).kind, FailureKind::Cancelled);
    assert_eq!(f.files.retained(), 1);
    drop(result);
    assert_eq!(f.files.retained(), 0);
    assert!(!f.audio().exists());
    let expired = Control {
        deadline: std::time::Instant::now() - Duration::from_secs(1),
        ..Control::default()
    };
    assert_eq!(
        f.files.submit(&phase, expired).err().unwrap().kind,
        FailureKind::Deadline
    );
    f.done(phase).await;
}

#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn unverified_os_is_explicitly_unavailable_without_creating_audio_or_returning_false_missing()
{
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    let result = f.prepare(&phase).await;
    assert_eq!(failed(&result).kind, FailureKind::Unsupported);
    assert!(!f.audio().exists());
    drop(result);
    f.done(phase).await;
}
