//! Actual Darwin filesystem/source comparisons. Cross-compile != execution.
#![cfg(target_os = "macos")]
#[path = "support/macos.rs"]
mod evidence;
use freeshow_browse::{BrowseConfig, BrowseService};
use serde_json::{json, Value};
use std::{
    ffi::CString,
    fs::{self, File, FileTimes},
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct RestorePermissions(PathBuf, fs::Permissions);
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, self.1.clone());
    }
}
const KEYS: &[&str] = &[
    "dev",
    "mode",
    "nlink",
    "uid",
    "gid",
    "rdev",
    "blksize",
    "ino",
    "size",
    "blocks",
    "atimeMs",
    "mtimeMs",
    "ctimeMs",
    "birthtimeMs",
];
fn info(path: impl AsRef<Path>) -> Value {
    json!({"command":"FILE_INFO","data":path.as_ref().to_str().unwrap()})
}
fn write(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"actual bounded stat fixture").unwrap();
    path
}
fn check_stats(value: &Value) {
    if let Some(object) = value.as_object() {
        if object.contains_key("mtimeMs") {
            assert_eq!(object.keys().map(String::as_str).collect::<Vec<_>>(), KEYS);
            assert!(object.values().all(Value::is_number));
        }
        for value in object.values() {
            check_stats(value);
        }
    }
}
async fn compare(service: &BrowseService, cases: &[Value]) -> Value {
    let source = evidence::oracle("--requests", &json!(cases));
    assert_eq!(source["actualFilesystemObserved"], true);
    for (index, case) in cases.iter().enumerate() {
        let reply = service
            .dispatch(case["command"].as_str().unwrap(), &case["data"])
            .await
            .unwrap();
        assert!(reply.thumbnail_intents.is_empty());
        check_stats(&reply.data);
        let mut actual = reply.data;
        if case["command"] == "READ_FOLDER" {
            let mut own_undefined = vec![];
            for (path, record) in actual.as_object_mut().unwrap() {
                if record["isFolder"] == true && record.get("noMedia") == Some(&Value::Null) {
                    own_undefined.push(path.clone());
                    record.as_object_mut().unwrap().remove("noMedia");
                }
            }
            own_undefined.sort();
            assert_eq!(
                json!(own_undefined),
                source["results"][index]["undefinedNoMedia"]
            );
        }
        evidence::assert_value(
            &actual,
            &source["results"][index]["data"],
            &source["results"][index]["numberBits"],
        );
    }
    source
}

#[tokio::test]
async fn real_files_directories_hardlinks_and_sparse_size_match_all_numeric_source_fields() {
    let _serial = SERIAL.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let file = write(temp.path(), "Café 敬拜 🙌.MP3");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
    let hardlink = temp.path().join("hardlink.wav");
    fs::hard_link(&file, &hardlink).unwrap();
    let sparse = temp.path().join("sparse.bin");
    let handle = File::create(&sparse).unwrap();
    handle.set_len((1_u64 << 32) + 17).unwrap();
    assert_eq!(fs::metadata(&sparse).unwrap().len(), (1_u64 << 32) + 17);
    assert!(
        fs::metadata(&sparse).unwrap().blocks() < 1024,
        "only sparse metadata, no 4GiB allocation"
    );
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    compare(
        &service,
        &[
            info(&file),
            info(&hardlink),
            info(&sparse),
            info(temp.path()),
        ],
    )
    .await;
    let actual = service
        .dispatch("FILE_INFO", &json!(hardlink))
        .await
        .unwrap();
    assert_eq!(actual.data["stat"]["nlink"].as_f64().unwrap(), 2.0);
    assert_eq!(actual.data["extension"], "wav");
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn symlinks_follow_real_targets_and_missing_loops_fifo_and_devices_match_source_stat() {
    let _serial = SERIAL.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let file = write(temp.path(), "real.txt");
    let linked = temp.path().join("link.MP3");
    let folder = temp.path().join("folder-link");
    let dangling = temp.path().join("dangling");
    let cycle = temp.path().join("cycle");
    symlink(&file, &linked).unwrap();
    symlink(temp.path(), &folder).unwrap();
    symlink(temp.path().join("absent"), &dangling).unwrap();
    symlink(&cycle, &cycle).unwrap();
    let fifo = temp.path().join("fifo");
    let name = CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: fixed private temporary pathname, no descriptor or body read.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    compare(
        &service,
        &[
            info(linked),
            info(folder),
            info(dangling),
            info(cycle),
            info(fifo),
            info(""),
            info(file.join("not-directory")),
        ],
    )
    .await;
    // /dev/null is shared with the entire runner. Other processes can update
    // its timestamps while the source child and native worker take turns.
    // Keep private files exact; bracket only actually changed device times.
    let cases = json!([info("/dev/null")]);
    let before = evidence::oracle("--requests", &cases);
    let native = service
        .dispatch("FILE_INFO", &json!("/dev/null"))
        .await
        .unwrap();
    let after = evidence::oracle("--requests", &cases);
    assert_eq!(before["actualFilesystemObserved"], true);
    assert_eq!(after["actualFilesystemObserved"], true);
    assert!(native.thumbnail_intents.is_empty());
    check_stats(&native.data);
    evidence::assert_live_device(&native.data, &before["results"][0], &after["results"][0]);
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn signed_pre_epoch_fractional_and_large_time_fields_match_actual_node_without_birth_substitution(
) {
    let _serial = SERIAL.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let path = write(temp.path(), "time.txt");
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    for modified in [
        UNIX_EPOCH - Duration::new(50, 654_321_000),
        UNIX_EPOCH + Duration::new(123, 123_456_789),
        UNIX_EPOCH + Duration::new(1_700_000_000, 12_348),
    ] {
        file.set_times(
            FileTimes::new()
                .set_accessed(UNIX_EPOCH - Duration::new(100, 123_456_000))
                .set_modified(modified),
        )
        .unwrap();
        let metadata = file.metadata().unwrap();
        let requested = match modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => i128::try_from(duration.as_nanos()).unwrap(),
            Err(before) => -i128::try_from(before.duration().as_nanos()).unwrap(),
        };
        assert_eq!(i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()), requested, "real timestamp fixture must not silently quantize; preserve Darwin signed representation");
        compare(&service, &[info(&path)]).await;
        // Filesystems may adjust creation time when mtime is backdated. The
        // complete source-bit comparison checks the actual birthtime; the
        // synthetic projection test separately covers distinct field values.
    }
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn real_posix_listing_depth_capture_no_media_and_unicode_similarity_preserve_source_shapes() {
    let _serial = SERIAL.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("tree 敬拜");
    let original = write(&root, "a/song-1.MP3");
    for name in [
        "a/song-2.mp3",
        "a/song-10.wav",
        "a/😀.mp3",
        "a/🙌.mp3",
        "a/nested/one.txt",
        "a/nested/deep/two.txt",
        "a/nested/deep/more/three.txt",
        "a/plain/readme.txt",
        "b/other.txt",
    ] {
        write(&root, name);
    }
    fs::create_dir_all(root.join("a/empty")).unwrap();
    let a = root.join("a");
    let b = root.join("b");
    let mut cases = vec![];
    for depth in [-1.0, 0.0, 0.5, 1.0, 2.0] {
        for capture in [false, true] {
            cases.push(json!({"command":"READ_FOLDER","data":{"path":a,"depth":depth,"captureFolderContent":capture}}));
        }
    }
    cases.push(json!({"command":"READ_FOLDER","data":{"path":[a,b,a],"depth":0}}));
    cases.push(
        json!({"command":"READ_FOLDER","data":{"path":format!("{}/../a/",a.display()),"depth":0}}),
    );
    cases.push(json!({"command":"GET_SIMILAR","data":{"paths":[original]}}));
    cases.push(json!({"command":"GET_SIMILAR","data":{"paths":[root.join("a/🙌.mp3")]}}));
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    compare(&service, &cases).await;
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn actual_permission_errors_preserve_original_null_empty_shapes_and_require_non_root() {
    let _serial = SERIAL.lock().await;
    // SAFETY: credential observation only, without changes to process identity.
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "real permission evidence requires non-root CI"
    );
    let temp = tempfile::tempdir().unwrap();
    let file = write(temp.path(), "denied/secret.txt");
    let denied = file.parent().unwrap();
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    let restore = RestorePermissions(
        denied.to_owned(),
        fs::metadata(denied).unwrap().permissions(),
    );
    fs::set_permissions(denied, fs::Permissions::from_mode(0o0)).unwrap();
    let expected = compare(
        &service,
        &[
            info(file.clone()),
            json!({"command":"READ_FOLDER","data":{"path":denied}}),
            json!({"command":"GET_SIMILAR","data":{"paths":[file]}}),
        ],
    )
    .await;
    drop(restore);
    assert!(expected["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| error["code"] == "EACCES"));
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn malformed_resource_and_cancellation_failures_remain_explicit_and_shutdown_joins() {
    let _serial = SERIAL.lock().await;
    let service = BrowseService::new(BrowseConfig::default()).unwrap();
    for value in [json!("bad\0path"), json!("x".repeat(32769))] {
        assert_eq!(
            service
                .dispatch("FILE_INFO", &value)
                .await
                .unwrap_err()
                .code,
            "browse_resource_limit"
        );
    }
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        service
            .dispatch_with_cancel("FILE_INFO", &json!("/"), cancel)
            .await
            .unwrap_err()
            .code,
        "cancelled"
    );
    service.shutdown().await.unwrap();
    assert_eq!(
        service
            .dispatch("FILE_INFO", &json!("/"))
            .await
            .unwrap_err()
            .code,
        "browse_shutdown"
    );
    service.shutdown().await.unwrap();
}
