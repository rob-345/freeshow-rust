#![cfg(target_os = "windows")]

#[path = "../src/now_playing_cover_file.rs"]
mod cover;
use cover::*;
use freeshow_core::{
    protocol::Request,
    runtime::Runtime,
    service::{LibraryPhase, LibraryService},
};
use std::{
    fs::{self, File, FileTimes},
    io::Read,
    mem::{size_of, MaybeUninit},
    os::windows::{
        fs::{symlink_file, MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::ERROR_SHARING_VIOLATION,
    Storage::FileSystem::{
        FileAttributeTagInfo, FileIdInfo, GetFileInformationByHandleEx,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    },
};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Fixture {
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
    fn leaf(&self) -> std::path::PathBuf {
        self.audio().join("NowPlayingCover.png")
    }
    fn write(&self, bytes: &[u8]) {
        fs::create_dir_all(self.audio()).unwrap();
        fs::write(self.leaf(), bytes).unwrap();
    }
    async fn phase(&self) -> LibraryPhase {
        self.library
            .begin_phase(
                Request {
                    channel: "MAIN".into(),
                    data: serde_json::Value::Null,
                    listener_id: None,
                    context: Some(self.library.context().unwrap()),
                },
                "main",
            )
            .await
            .unwrap()
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
        assert_eq!(self.files.retained(), 0);
    }
}
fn file(value: &Prepared) -> &HeldCover {
    match value.outcome() {
        Outcome::File(file) => file,
        _ => panic!("Actual held Windows file required"),
    }
}
fn failed(value: &Prepared) -> &Failure {
    match value.outcome() {
        Outcome::Failed(error) => error,
        _ => panic!("Explicit native failure required"),
    }
}
fn observation_evidence(value: &Prepared) -> String {
    let diagnostic =
        |error: &Failure| format!("{} os={:?} {}", error.code(), error.os_code, error.message);
    let outcome = match value.outcome() {
        Outcome::Missing => "Missing".into(),
        Outcome::File(_) => "File".into(),
        Outcome::Failed(error) => format!("Failed({})", diagnostic(error)),
    };
    let folder = value
        .observation()
        .folder_failure()
        .map(|failure| diagnostic(failure.diagnostic()))
        .unwrap_or_else(|| "none".into());
    let diagnostics = value.observation().diagnostics();
    format!(
        "outcome={outcome}; folder_failure={folder}; diagnostics={} {:?}",
        diagnostics.len(),
        diagnostics
            .iter()
            .take(4)
            .map(diagnostic)
            .collect::<Vec<_>>()
    )
}
async fn entered(value: tokio::sync::oneshot::Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(5), value)
        .await
        .unwrap()
        .unwrap();
}
struct TestChild(Child);
impl TestChild {
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.0.kill();
                self.0.wait().unwrap();
                panic!("private fixture child timed out and was reaped");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for TestChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
// At most one controlled fixture child exists; all exits retain kill/reap.
// Junction creation is a Windows built-in, with no symlink privilege required.
fn junction(link: &std::path::Path, target: &std::path::Path) {
    let mut child = TestChild(
        Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    assert!(
        child.wait().success(),
        "actual private fixture junction creation failed"
    );
    assert_ne!(
        fs::symlink_metadata(link).unwrap().file_attributes() & 0x400,
        0
    );
}
#[derive(Debug, PartialEq, Eq)]
struct EntryIdentity {
    volume: u64,
    file: [u8; 16],
}
#[derive(Debug, PartialEq, Eq)]
struct JunctionSnapshot {
    identity: EntryIdentity,
    attributes: u32,
    tag: u32,
    target: PathBuf,
}
fn no_follow_fixture_handle(path: &Path) -> File {
    fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .unwrap()
}
fn fixture_information<T>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS) -> T {
    let mut value = MaybeUninit::<T>::zeroed();
    // SAFETY: the two private call sites pair the exact FILE_ID_INFO or
    // FILE_ATTRIBUTE_TAG_INFO ABI with its information class and fixed size.
    // The owned no-follow handle remains live, and only success is initialized.
    let success = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            class,
            value.as_mut_ptr().cast(),
            size_of::<T>() as u32,
        )
    };
    assert_ne!(success, 0, "{}", std::io::Error::last_os_error());
    unsafe { value.assume_init() }
}
fn fixture_identity(file: &File) -> EntryIdentity {
    let value: FILE_ID_INFO = fixture_information(file, FileIdInfo);
    EntryIdentity {
        volume: value.VolumeSerialNumber,
        file: value.FileId.Identifier,
    }
}
fn junction_snapshot(path: &Path) -> JunctionSnapshot {
    let file = no_follow_fixture_handle(path);
    let value: FILE_ATTRIBUTE_TAG_INFO = fixture_information(&file, FileAttributeTagInfo);
    assert_ne!(value.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT, 0);
    JunctionSnapshot {
        identity: fixture_identity(&file),
        attributes: value.FileAttributes,
        tag: value.ReparseTag,
        target: fs::read_link(path).unwrap(),
    }
    // The snapshot handle closes here, before the paused production call runs.
}
fn assert_absent(path: &Path) {
    let actual = fs::symlink_metadata(path).unwrap_err();
    assert_eq!(actual.kind(), std::io::ErrorKind::NotFound, "{actual}");
}
fn original_node_stat(file: &std::path::Path) -> serde_json::Value {
    let root = std::env::var_os("FREESHOW_COVER_WINDOWS_ORACLE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_owned()
        });
    let executable =
        std::env::var_os("FREESHOW_COVER_WINDOWS_NODE").unwrap_or_else(|| "node".into());
    let script = r#"const fs = require('node:fs');
const {createRequire} = require('node:module');
const path = require('node:path');
const etag = createRequire(path.join(process.argv[2], 'package.json'))('etag');
const stat = fs.statSync(process.argv[1]);
process.stdout.write(JSON.stringify({node:process.versions.node,size:stat.size,mtimeMs:stat.mtimeMs,mtimeDate:stat.mtime.getTime(),etag:etag(stat)}));"#;
    let mut child = TestChild(
        Command::new(executable)
            .args(["-e", script])
            .arg(file)
            .arg(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    assert!(
        child.wait().success(),
        "actual Node fs/stat/etag source oracle failed"
    );
    let mut output = Vec::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .take(4097)
        .read_to_end(&mut output)
        .unwrap();
    assert!(output.len() <= 4096);
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["node"], "22.21.1", "use the pinned source runtime");
    value
}
fn epoch_nanoseconds(time: SystemTime) -> i128 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i128::try_from(duration.as_nanos()).unwrap(),
        Err(before_epoch) => -i128::try_from(before_epoch.duration().as_nanos()).unwrap(),
    }
}

#[tokio::test]
async fn windows_real_mkdir_missing_and_regular_audio_blocker_preserve_actual_observation() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    let missing = f.prepare(&phase).await;
    assert!(
        matches!(missing.outcome(), Outcome::Missing),
        "{}",
        observation_evidence(&missing)
    );
    assert!(
        f.audio().is_dir(),
        "Real Audio mkdir did not create a directory: {}",
        observation_evidence(&missing)
    );
    assert!(missing.observation().folder_failure().is_none());
    assert_eq!(
        missing.observation().context(),
        &f.library.context().unwrap()
    );
    let checked = missing
        .observation()
        .revalidate(&phase, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert!(checked.stat().unwrap().is_none());
    drop(checked);
    drop(missing);
    fs::remove_dir(f.audio()).unwrap();
    fs::write(f.audio(), b"original blocker bytes").unwrap();
    let blocker = f.prepare(&phase).await;
    assert!(
        matches!(blocker.outcome(), Outcome::Missing),
        "Actual regular-Audio parent continuation: {}",
        observation_evidence(&blocker)
    );
    let actual = blocker.observation().folder_failure().unwrap();
    assert_eq!(actual.folder(), f.audio());
    assert_eq!(actual.diagnostic().code(), "now_playing_cover_io");
    assert!(actual.diagnostic().os_code.is_some());
    assert!(actual.diagnostic().message.contains("NTSTATUS"));
    assert!(actual.diagnostic().message.len() <= 1024);
    assert_eq!(blocker.observation().diagnostics().len(), 1);
    assert_eq!(fs::read(f.audio()).unwrap(), b"original blocker bytes");
    drop(blocker);
    f.done(phase).await;
}

#[tokio::test]
async fn windows_raced_broken_junction_after_absent_probe_is_not_followed_or_replaced() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let outside = f.temp.path().join("absent-external-target");
    assert_absent(&f.audio());
    assert_absent(&outside);
    let phase = f.phase().await;
    let (ready, release) = f.files.test_pause_windows_before_mkdir();
    let ticket = f.files.submit(&phase, Control::default()).unwrap();
    // No Audio existed until the actual initial FILE_OPEN returned missing.
    // Install a real broken junction only after that probe, immediately before
    // the production FILE_CREATE. This is not the at-start reparse fixture.
    entered(ready).await;
    junction(&f.audio(), &outside);
    let before = junction_snapshot(&f.audio());
    assert_absent(&outside);
    release.send(()).unwrap();
    let value = ticket.finish().await.unwrap();

    // Check actual effects independently of the returned outcome. A candidate
    // that creates the junction target or replaces the entry fails these checks
    // even if a later metadata probe rejects the resulting object.
    assert_absent(&outside);
    assert_eq!(junction_snapshot(&f.audio()), before);
    let evidence = observation_evidence(&value);
    let actual = value.observation().folder_failure().unwrap();
    assert_eq!(actual.folder(), f.audio());
    assert_eq!(actual.diagnostic().kind, FailureKind::Io, "{evidence}");
    assert!(actual.diagnostic().os_code.is_some(), "{evidence}");
    // Depending on whether FILE_CREATE detects the existing name first or the
    // object parser encounters its reparse, either is a real rejection. Never
    // accept INVALID_PARAMETER as evidence that the new create options work.
    assert!(
        actual.diagnostic().message.contains("0xC0000035")
            || actual.diagnostic().message.contains("0xC000050B"),
        "{evidence}"
    );
    assert_eq!(failed(&value).kind, FailureKind::Unsupported, "{evidence}");
    assert_eq!(
        value
            .observation()
            .revalidate(&phase, Control::default())
            .err()
            .unwrap()
            .kind,
        FailureKind::Unsupported,
        "Failed preparation must not retain external-target authority: {evidence}"
    );
    eprintln!("Actual raced broken-junction rejection: {evidence}");
    drop(value);
    f.done(phase).await;
}

#[tokio::test]
async fn windows_raced_regular_collision_after_absent_probe_preserves_identity_and_bytes() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    assert_absent(&f.audio());
    let phase = f.phase().await;
    let (ready, release) = f.files.test_pause_windows_before_mkdir();
    let ticket = f.files.submit(&phase, Control::default()).unwrap();
    entered(ready).await;
    fs::write(f.audio(), b"raced ordinary entry must survive").unwrap();
    let before = fixture_identity(&no_follow_fixture_handle(&f.audio()));
    release.send(()).unwrap();
    let value = ticket.finish().await.unwrap();
    assert_eq!(
        fixture_identity(&no_follow_fixture_handle(&f.audio())),
        before
    );
    assert_eq!(
        fs::read(f.audio()).unwrap(),
        b"raced ordinary entry must survive"
    );
    let evidence = observation_evidence(&value);
    let actual = value.observation().folder_failure().unwrap();
    assert_eq!(actual.folder(), f.audio());
    assert_eq!(actual.diagnostic().kind, FailureKind::Io, "{evidence}");
    assert!(actual.diagnostic().os_code.is_some(), "{evidence}");
    assert!(
        actual.diagnostic().message.contains("NTSTATUS"),
        "{evidence}"
    );
    assert!(
        !actual.diagnostic().message.contains("0xC000000D"),
        "{evidence}"
    );
    // The regular-file RootDirectory continuation is not certified as missing
    // for every Windows error. Preserve the existing status classification:
    // either an already certified Missing or an explicit actual IO failure.
    match value.outcome() {
        Outcome::Missing => {}
        Outcome::Failed(error) => assert_eq!(error.kind, FailureKind::Io, "{evidence}"),
        Outcome::File(_) => panic!("A collided ordinary Audio cannot grant a cover: {evidence}"),
    }
    eprintln!("Actual raced ordinary-collision continuation: {evidence}");
    drop(value);
    f.done(phase).await;
}

#[tokio::test]
async fn windows_held_bytes_explicit_offsets_signed_native_times_and_resource_bounds() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    f.write(b"0123456789\0\xff");
    fs::OpenOptions::new()
        .write(true)
        .open(f.leaf())
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH - Duration::from_millis(1250)))
        .unwrap();
    let phase = f.phase().await;
    let prepared = f.prepare(&phase).await;
    assert_eq!(file(&prepared).context(), &f.library.context().unwrap());
    let stat = file(&prepared).stat();
    assert_eq!(
        (
            stat.length(),
            stat.modified_seconds(),
            stat.modified_nanoseconds()
        ),
        (12, -2, 750_000_000)
    );
    assert!((0..1_000_000_000).contains(&stat.changed_nanoseconds()));
    assert!(stat.changed_seconds() > 0);
    let again = f.prepare(&phase).await;
    assert_eq!(
        (stat.device(), stat.inode()),
        (file(&again).stat().device(), file(&again).stat().inode())
    );
    drop(again);
    for (offset, expected) in [(8, &b"89\0\xff"[..]), (0, &b"0123"[..]), (4, &b"4567"[..])] {
        let mut bytes = [0; 4];
        assert_eq!(file(&prepared).read_at(offset, &mut bytes).unwrap(), 4);
        assert_eq!(bytes, expected);
    }
    assert!(file(&prepared)
        .read_at(0, &mut vec![0; MAX_CHUNK + 1])
        .is_err());
    assert!(file(&prepared).read_at(MAX_FILE, &mut [0; 1]).is_err());
    let valid = file(&prepared)
        .revalidate(&phase, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(valid.context(), file(&prepared).context());
    assert_eq!(valid.stat().unwrap().unwrap().length(), 12);
    drop(valid);
    drop(prepared);
    let os_file = fs::OpenOptions::new().write(true).open(f.leaf()).unwrap();
    os_file.set_len(MAX_FILE).unwrap();
    let maximum = f.prepare(&phase).await;
    assert_eq!(file(&maximum).stat().length(), MAX_FILE);
    drop(maximum);
    os_file.set_len(MAX_FILE + 1).unwrap();
    let oversized = f.prepare(&phase).await;
    assert_eq!(failed(&oversized).kind, FailureKind::Resource);
    drop(oversized);
    drop(os_file);
    fs::remove_file(f.leaf()).unwrap();
    fs::create_dir(f.leaf()).unwrap();
    let directory = f.prepare(&phase).await;
    assert_eq!(failed(&directory).kind, FailureKind::Unsupported);
    drop(directory);
    f.done(phase).await;
}

#[tokio::test]
async fn windows_global_four_slots_follow_descriptors_and_ready_revalidation_not_phase_fences() {
    let _serial = SERIAL.lock().await;
    let a = Fixture::new();
    let b = Fixture::new();
    a.write(b"A");
    b.write(b"B");
    let ap = a.phase().await;
    let bp = b.phase().await;
    let mut values = vec![
        a.prepare(&ap).await,
        a.prepare(&ap).await,
        b.prepare(&bp).await,
        b.prepare(&bp).await,
    ];
    assert_eq!((a.files.retained(), b.files.retained()), (4, 4));
    assert_eq!(
        a.files.submit(&ap, Control::default()).err().unwrap().kind,
        FailureKind::Busy
    );
    let (observation, outcome) = values.pop().unwrap().into_parts();
    drop(observation);
    assert_eq!(a.files.retained(), 4);
    drop(outcome);
    assert_eq!(a.files.retained(), 3);
    let abandoned = file(&values[0])
        .revalidate(&ap, Control::default())
        .unwrap();
    drop(abandoned);
    // An actual later FIFO preparation witnesses failed-send validator disposal.
    let fence = a.prepare(&ap).await;
    drop(fence);
    let checked = file(&values[0])
        .revalidate(&ap, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(
        file(&values[0])
            .revalidate(&ap, Control::default())
            .err()
            .unwrap()
            .kind,
        FailureKind::Busy
    );
    drop(values);
    assert_eq!(
        a.files.retained(),
        1,
        "ready native validation keeps actual held handles"
    );
    ap.finish().await.unwrap();
    assert_eq!(
        a.files.retained(),
        1,
        "an ordered phase fence is not descriptor disposal"
    );
    drop(checked);
    bp.finish().await.unwrap();
    a.library.finish_writes().await.unwrap();
    b.library.finish_writes().await.unwrap();
    assert_eq!(a.files.retained(), 0);
}

#[tokio::test]
async fn windows_root_audio_and_opened_leaf_swaps_fail_actual_handle_revalidation() {
    let _serial = SERIAL.lock().await;
    for variant in ["root", "audio", "leaf"] {
        let f = Fixture::new();
        f.write(b"original");
        let phase = f.phase().await;
        let (ready, release) = match variant {
            "root" => f.files.test_pause_windows_root(),
            "audio" => f.files.test_pause_windows_audio(),
            "leaf" => f.files.test_pause_windows_leaf(),
            _ => unreachable!(),
        };
        let ticket = f.files.submit(&phase, Control::default()).unwrap();
        entered(ready).await;
        match variant {
            "root" => {
                fs::rename(f.root(), f.temp.path().join("old-selected")).unwrap();
                fs::create_dir(f.root()).unwrap();
                f.write(b"new root");
            }
            "audio" => {
                fs::rename(f.audio(), f.root().join("old-audio")).unwrap();
                f.write(b"new parent");
            }
            "leaf" => {
                fs::rename(f.leaf(), f.audio().join("old.png")).unwrap();
                fs::write(f.leaf(), b"new leaf").unwrap();
            }
            _ => unreachable!(),
        }
        release.send(()).unwrap();
        let value = ticket.finish().await.unwrap();
        assert_eq!(failed(&value).kind, FailureKind::Changed, "{variant}");
        drop(value);
        f.done(phase).await;
    }
}

#[tokio::test]
async fn windows_exists_true_then_actual_open_failure_never_turns_into_missing() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    f.write(b"probe positive");
    let phase = f.phase().await;
    let (ready, release) = f.files.test_pause_windows_before_leaf();
    let ticket = f.files.submit(&phase, Control::default()).unwrap();
    entered(ready).await;
    fs::rename(f.leaf(), f.audio().join("moved.png")).unwrap();
    release.send(()).unwrap();
    let value = ticket.finish().await.unwrap();
    let error = failed(&value);
    assert_eq!(error.kind, FailureKind::Io);
    assert!(error.os_code.is_some());
    assert!(error.message.contains("NTSTATUS"));
    assert!(value.observation().folder_failure().is_none());
    drop(value);
    f.done(phase).await;
}

#[tokio::test]
async fn windows_real_sharing_failure_is_explicit_not_an_unproven_exists_false() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    f.write(b"share locked");
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(f.leaf())
        .unwrap();
    let phase = f.phase().await;
    let value = f.prepare(&phase).await;
    assert_eq!(failed(&value).kind, FailureKind::Io);
    assert_eq!(
        failed(&value).os_code,
        Some(ERROR_SHARING_VIOLATION as i32),
        "The actual sharing fixture must not pass on an unrelated IO failure"
    );
    assert!(value.observation().folder_failure().is_none());
    drop(value);
    drop(lock);
    f.done(phase).await;
}

#[tokio::test]
async fn windows_held_identity_survives_replacement_but_inplace_and_foreign_validation_fail() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    f.write(b"old");
    let phase = f.phase().await;
    let prepared = f.prepare(&phase).await;
    fs::rename(f.leaf(), f.audio().join("original.png")).unwrap();
    fs::write(f.leaf(), b"new").unwrap();
    let checked = file(&prepared)
        .revalidate(&phase, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(checked.stat().err().unwrap().kind, FailureKind::Changed);
    let mut bytes = [0; 3];
    file(&prepared).read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"old");
    drop(checked);
    drop(prepared);
    let current = f.prepare(&phase).await;
    fs::write(f.leaf(), b"NEW").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(f.leaf())
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_000_000)))
        .unwrap();
    let checked = file(&current)
        .revalidate(&phase, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(checked.stat().err().unwrap().kind, FailureKind::Changed);
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

#[tokio::test]
async fn windows_actual_folder_error_survives_later_audio_namespace_change() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    fs::write(f.audio(), b"blocker").unwrap();
    let phase = f.phase().await;
    let (attempted, continue_after_mkdir) = f.files.test_pause_windows_after_mkdir();
    let ticket = f.files.submit(&phase, Control::default()).unwrap();
    entered(attempted).await;
    // The genuine failed mkdir happened against this regular blocker. Replace
    // it before the continuation opens Audio: no held regular child or parent
    // rename is needed, and no unknown nondirectory RootDirectory status is
    // used as evidence of source Missing.
    fs::rename(f.audio(), f.root().join("previous-blocker")).unwrap();
    fs::create_dir(f.audio()).unwrap();
    let (ready, release) = f.files.test_pause_windows_audio();
    continue_after_mkdir.send(()).unwrap();
    entered(ready).await;
    // A held directory and actual Audio replacement exercise the identity
    // branch already supported by the native directory-swap regression.
    fs::rename(f.audio(), f.root().join("previous-audio")).unwrap();
    fs::create_dir(f.audio()).unwrap();
    release.send(()).unwrap();
    let value = ticket.finish().await.unwrap();
    assert_eq!(
        failed(&value).kind,
        FailureKind::Changed,
        "{}",
        observation_evidence(&value)
    );
    let folder = value.observation().folder_failure().unwrap();
    assert_eq!(folder.folder(), f.audio());
    assert!(folder.diagnostic().os_code.is_some());
    assert_eq!(
        fs::read(f.root().join("previous-blocker")).unwrap(),
        b"blocker"
    );
    drop(value);
    f.done(phase).await;
}

#[tokio::test]
async fn windows_dropped_caller_and_timeout_keep_blocked_ordered_io_until_real_return() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    let cancelled = Control::default();
    cancelled.cancel.cancel();
    assert_eq!(
        f.files.submit(&phase, cancelled).err().unwrap().kind,
        FailureKind::Cancelled
    );
    let expired = Control {
        deadline: Instant::now(),
        ..Control::default()
    };
    assert_eq!(
        f.files.submit(&phase, expired).err().unwrap().kind,
        FailureKind::Deadline
    );
    assert_eq!(f.files.retained(), 0);
    let token = tokio_util::sync::CancellationToken::new();
    let control = Control {
        cancel: token.clone(),
        deadline: Instant::now() + Duration::from_secs(10),
    };
    let (ready, release) = f.files.test_pause_windows_root();
    let ticket = f.files.submit(&phase, control).unwrap();
    entered(ready).await;
    assert_eq!(f.files.retained(), 1);
    drop(ticket);
    token.cancel();
    let mut drain = Box::pin(f.library.finish_writes());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), drain.as_mut())
            .await
            .is_err()
    );
    assert_eq!(
        f.files.retained(),
        1,
        "cancel/drop/observer timeout are not OS work disposal"
    );
    release.send(()).unwrap();
    phase.finish().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), drain.as_mut())
        .await
        .unwrap()
        .unwrap();
    drop(drain);
    assert_eq!(f.files.retained(), 0);
    assert!(!f.audio().exists());
}

#[tokio::test]
async fn windows_real_junctions_and_symlink_leaf_are_never_followed() {
    let _serial = SERIAL.lock().await;
    for variant in ["root", "audio", "leaf"] {
        let f = Fixture::new();
        let outside = f.temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let outside_file = outside.join("NowPlayingCover.png");
        fs::write(&outside_file, b"never native read").unwrap();
        match variant {
            "root" => {
                fs::rename(f.root(), f.temp.path().join("kept-root")).unwrap();
                junction(&f.root(), &outside);
            }
            "audio" => junction(&f.audio(), &outside),
            "leaf" => {
                fs::create_dir(f.audio()).unwrap();
                if let Err(error) = symlink_file(&outside_file, f.leaf()) {
                    if error.raw_os_error() == Some(1314) {
                        eprintln!("SKIP only leaf symlink: Windows privilege absent (1314); real root/Audio junction cases already executed");
                        f.library.finish_writes().await.unwrap();
                        continue;
                    }
                    panic!("actual Windows symlink fixture failed: {error}");
                }
            }
            _ => unreachable!(),
        }
        let phase = f.phase().await;
        let value = f.prepare(&phase).await;
        assert_eq!(failed(&value).kind, FailureKind::Unsupported, "{variant}");
        assert_eq!(fs::read(&outside_file).unwrap(), b"never native read");
        assert!(!outside.join("Audio").exists());
        drop(value);
        f.done(phase).await;
    }
}

#[tokio::test]
async fn windows_missing_is_an_observed_guard_result_not_a_promise_of_later_absence() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let phase = f.phase().await;
    let missing = f.prepare(&phase).await;
    f.write(b"appeared later");
    let checked = missing
        .observation()
        .revalidate(&phase, Control::default())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert!(matches!(missing.outcome(), Outcome::Missing));
    assert!(checked.stat().unwrap().is_none());
    drop(checked);
    drop(missing);
    f.done(phase).await;
}

#[tokio::test]
async fn windows_actual_node_stat_etag_matches_ordinary_times_and_records_gated_exceptional_times()
{
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    f.write(b"source stat bytes");
    let phase = f.phase().await;
    for (label, modified, same_projection) in [
        (
            "ordinary",
            UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            true,
        ),
        (
            "fractional",
            UNIX_EPOCH + Duration::new(1_700_000_000, 499_900),
            true,
        ),
        (
            "fractional_round_up",
            UNIX_EPOCH + Duration::new(1_700_000_000, 500_100),
            true,
        ),
        (
            "unsigned_32_upper_bound",
            UNIX_EPOCH + Duration::new(u32::MAX as u64, 500_100),
            true,
        ),
        ("pre_epoch", UNIX_EPOCH - Duration::from_millis(1250), false),
        (
            "past_unsigned_32",
            UNIX_EPOCH + Duration::from_secs(4_294_967_297),
            false,
        ),
    ] {
        let requested_nanoseconds = epoch_nanoseconds(modified);
        assert_eq!(
            requested_nanoseconds.rem_euclid(100),
            0,
            "{label}: fixture must be exactly representable as native 100ns ticks"
        );
        fs::OpenOptions::new()
            .write(true)
            .open(f.leaf())
            .unwrap()
            .set_times(FileTimes::new().set_modified(modified))
            .unwrap();
        let actual_modified = fs::metadata(f.leaf()).unwrap().modified().unwrap();
        assert_eq!(
            epoch_nanoseconds(actual_modified),
            requested_nanoseconds,
            "{label}: actual filesystem must preserve the requested time, not quantize or clamp the boundary case"
        );
        let value = f.prepare(&phase).await;
        let stat = file(&value).stat();
        assert!((0..1_000_000_000).contains(&stat.modified_nanoseconds()));
        assert_eq!(
            i128::from(stat.modified_seconds()) * 1_000_000_000
                + i128::from(stat.modified_nanoseconds()),
            requested_nanoseconds,
            "{label}: signed native fields must retain the exact real requested epoch nanoseconds"
        );
        let node = original_node_stat(&f.leaf());
        assert_eq!(node["size"].as_u64().unwrap(), stat.length());
        if same_projection {
            assert!((0..=u32::MAX as i64).contains(&stat.modified_seconds()));
            assert!((0..1_000_000_000).contains(&stat.modified_nanoseconds()));
            let posix = freeshow_remotes::CoverFileStat::from_posix(
                stat.length(),
                stat.modified_seconds(),
                stat.modified_nanoseconds(),
            )
            .unwrap();
            assert_eq!(node["etag"].as_str().unwrap(), posix.etag(), "{label}");
        } else {
            // The native guard must retain the true signed timestamp. Header
            // projection is deliberately unsupported outside the verified
            // interval; record actual Node evidence without guessing libuv's
            // exceptional FILETIME arithmetic or synthesizing POSIX headers.
            assert!(!(0..=u32::MAX as i64).contains(&stat.modified_seconds()));
        }
        eprintln!(
            "Windows real stat {label}: signed_native=({},{}), source={node}",
            stat.modified_seconds(),
            stat.modified_nanoseconds()
        );
        drop(value);
    }
    f.done(phase).await;
}
