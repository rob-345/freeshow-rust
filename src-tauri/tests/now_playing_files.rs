#[path = "../src/now_playing_files.rs"]
mod files;
use files::*;
use freeshow_core::{
    protocol::Request,
    runtime::Runtime,
    service::{LibraryPhase, LibraryService},
    Error,
};
use serde_json::json;
use std::{fs, io::Read, sync::mpsc, time::Duration};

struct Fixture {
    dir: tempfile::TempDir,
    library: LibraryService,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(dir.path().join("app"), dir.path().join("selected")).unwrap(),
        )
        .unwrap();
        Self { dir, library }
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
}
async fn effect(phase: &LibraryPhase, intent: Intent) -> Observation {
    submit(phase, intent).unwrap().finish().await.unwrap()
}

#[tokio::test]
async fn selected_data_paths_source_empty_skip_and_buffer_always_truncate() {
    let f = Fixture::new();
    let phase = f.phase().await;
    let paths = effect(&phase, Intent::Paths).await.paths;
    assert_eq!(paths.folder, f.dir.path().join("selected/Audio"));
    assert!(!paths.folder.exists());
    assert_eq!(
        effect(&phase, Intent::EnsureFolder).await.success,
        Some(true)
    );
    assert_eq!(
        effect(&phase, Intent::Exists(Leaf::Text)).await.success,
        Some(false)
    );
    let first = effect(&phase, Intent::WriteText(String::new())).await;
    assert_eq!(first.success, Some(true));
    assert!(!first.skipped_equal_text);
    let same = effect(&phase, Intent::WriteText(String::new())).await;
    assert_eq!(same.success, Some(false));
    assert!(same.skipped_equal_text);
    assert_eq!(
        effect(&phase, Intent::WriteCover(vec![1, 2, 3, 4]))
            .await
            .success,
        Some(true)
    );
    let mut held = fs::File::open(&paths.cover).unwrap();
    assert_eq!(
        effect(&phase, Intent::WriteCover(vec![8, 9])).await.success,
        Some(true)
    );
    let mut bytes = Vec::new();
    held.read_to_end(&mut bytes).unwrap();
    assert_eq!(
        bytes,
        [8, 9],
        "write truncates the actual held inode rather than atomically replacing it"
    );
    assert_eq!(
        effect(&phase, Intent::WriteCover(vec![8, 9])).await.success,
        Some(true),
        "identical source Buffer is still written"
    );
    assert_eq!(
        effect(&phase, Intent::Exists(Leaf::Cover)).await.success,
        Some(true)
    );
    assert_eq!(
        effect(&phase, Intent::DeleteCover).await.success,
        Some(true)
    );
    assert_eq!(
        effect(&phase, Intent::Exists(Leaf::Cover)).await.success,
        Some(false)
    );
    phase.finish().await.unwrap();
    f.library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn source_utf8_replacement_comparison_skips_without_rewriting_original_bytes() {
    let f = Fixture::new();
    let phase = f.phase().await;
    let paths = effect(&phase, Intent::EnsureFolder).await.paths;
    // These exact malformed subsequences are also checked against real Node
    // Buffer.toString in the source helper probe; strict read_to_string differs.
    let corpus: &[(&[u8], &str)] = &[
        (&[0xef, 0xbb, 0xbf, b'A'], "\u{feff}A"),
        (&[0xff, 0xfe, b'A'], "\u{fffd}\u{fffd}A"),
        (&[0xe2, 0x82], "\u{fffd}"),
        (&[0xed, 0xa0, 0x80], "\u{fffd}\u{fffd}\u{fffd}"),
        (&[0xf0, 0x9f, 0x92, b'A'], "\u{fffd}A"),
        (&[0xc0, 0xaf], "\u{fffd}\u{fffd}"),
        (&[b'A', 0, b'B'], "A\0B"),
    ];
    for (bytes, text) in corpus {
        fs::write(&paths.text, bytes).unwrap();
        let result = effect(&phase, Intent::WriteText((*text).into())).await;
        assert_eq!(result.success, Some(false));
        assert!(result.skipped_equal_text);
        assert_eq!(fs::read(&paths.text).unwrap(), *bytes);
    }
    phase.finish().await.unwrap();
}

#[tokio::test]
async fn caught_folder_write_delete_errors_and_existing_directory_empty_read_continue() {
    let f = Fixture::new();
    let phase = f.phase().await;
    let paths = effect(&phase, Intent::Paths).await.paths;
    fs::write(&paths.folder, b"a file blocking the Audio folder").unwrap();
    let folder = effect(&phase, Intent::EnsureFolder).await;
    assert_eq!(folder.success, Some(false));
    assert!(!folder.diagnostics.is_empty());
    assert_eq!(folder.paths, paths);
    let write = effect(&phase, Intent::WriteCover(vec![1])).await;
    assert_eq!(write.success, Some(false));
    assert!(!write.diagnostics.is_empty());
    fs::remove_file(&paths.folder).unwrap();
    fs::create_dir(&paths.folder).unwrap();
    fs::create_dir(&paths.text).unwrap();
    let empty = effect(&phase, Intent::WriteText(String::new())).await;
    assert_eq!(empty.success, Some(false));
    assert!(empty.skipped_equal_text);
    let nonempty = effect(&phase, Intent::WriteText("A".into())).await;
    assert_eq!(nonempty.success, Some(false));
    assert!(!nonempty.skipped_equal_text);
    assert!(!nonempty.diagnostics.is_empty());
    fs::create_dir(&paths.cover).unwrap();
    let deleted = effect(&phase, Intent::DeleteCover).await;
    assert_eq!(deleted.success, Some(false));
    assert!(paths.cover.is_dir());
    phase.finish().await.unwrap();
}

#[tokio::test]
async fn resource_rejections_are_explicit_and_do_not_fabricate_helper_false() {
    let f = Fixture::new();
    let phase = f.phase().await;
    let paths = effect(&phase, Intent::EnsureFolder).await.paths;
    assert!(matches!(
        submit(&phase, Intent::WriteText("x".repeat(MAX_TEXT + 1))),
        Err(Error::NativeOperation {
            code: "now_playing_file_resource",
            ..
        })
    ));
    assert!(matches!(
        submit(&phase, Intent::WriteCover(vec![0; MAX_COVER + 1])),
        Err(Error::NativeOperation {
            code: "now_playing_file_resource",
            ..
        })
    ));
    let file = fs::File::create(&paths.text).unwrap();
    file.set_len(MAX_COMPARE as u64 + 1).unwrap();
    let failure = submit(&phase, Intent::WriteText(String::new()))
        .unwrap()
        .finish()
        .await
        .unwrap_err();
    assert!(matches!(
        failure,
        Error::NativeOperation {
            code: "now_playing_file_resource",
            ..
        }
    ));
    assert_eq!(
        fs::metadata(&paths.text).unwrap().len(),
        MAX_COMPARE as u64 + 1
    );
    phase.finish().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_file_ticket_finishes_real_write_before_later_library_work() {
    let f = Fixture::new();
    let phase = f.phase().await;
    let paths = effect(&phase, Intent::EnsureFolder).await.paths;
    let (entered, entry) = tokio::sync::oneshot::channel();
    let (release, gate) = mpsc::channel();
    let hold = phase
        .submit(f.request(), move |_| {
            entered.send(()).unwrap();
            gate.recv().unwrap();
            Ok(vec![])
        })
        .unwrap();
    entry.await.unwrap();
    drop(
        submit(
            &phase,
            Intent::WriteText("accepted despite caller drop".into()),
        )
        .unwrap(),
    );
    phase.close();
    let library = f.library.clone();
    let request = f.request();
    let path = paths.text.clone();
    let later = tokio::spawn(async move {
        library
            .commit(request, "main", move |_| {
                assert_eq!(fs::read_to_string(&path)?, "accepted despite caller drop");
                Ok(vec![])
            })
            .await
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!later.is_finished());
    assert!(!paths.text.exists());
    release.send(()).unwrap();
    hold.finish().await.unwrap();
    phase.finish().await.unwrap();
    later.await.unwrap().unwrap();
    f.library.finish_writes().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn source_write_follows_existing_symlink_and_changes_its_target_inode() {
    use std::os::unix::fs::{symlink, MetadataExt};
    let f = Fixture::new();
    let phase = f.phase().await;
    let paths = effect(&phase, Intent::EnsureFolder).await.paths;
    let target = f.dir.path().join("existing-cover-target");
    fs::write(&target, b"before").unwrap();
    let inode = fs::metadata(&target).unwrap().ino();
    symlink(&target, &paths.cover).unwrap();
    assert_eq!(
        effect(&phase, Intent::WriteCover(vec![4, 5, 6]))
            .await
            .success,
        Some(true)
    );
    assert!(fs::symlink_metadata(&paths.cover).unwrap().is_symlink());
    assert_eq!(fs::metadata(&target).unwrap().ino(), inode);
    assert_eq!(fs::read(&target).unwrap(), [4, 5, 6]);
    phase.finish().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn nonregular_fixed_leaf_is_an_explicit_capability_error_without_waiting_for_a_peer() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let f = Fixture::new();
    let phase = f.phase().await;
    let paths = effect(&phase, Intent::EnsureFolder).await.paths;
    for path in [&paths.text, &paths.cover] {
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: valid NUL-terminated pathname, checked syscall result.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    }
    for intent in [
        Intent::WriteText("must not wait on a FIFO".into()),
        Intent::WriteCover(vec![1]),
    ] {
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            submit(&phase, intent).unwrap().finish(),
        )
        .await
        .unwrap();
        assert!(matches!(
            result,
            Err(Error::NativeOperation {
                code: "now_playing_file_resource",
                ..
            })
        ));
    }
    phase.finish().await.unwrap();
}
