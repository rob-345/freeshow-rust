//! These tests execute only on genuine Windows. Cross checks compile them only.
use super::*;
use serde_json::json;
use std::{
    fs::{self, FileTimes, OpenOptions},
    io::Read,
    os::windows::fs::{symlink_file, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant, UNIX_EPOCH},
};

fn budget(config: BrowseConfig) -> Budget {
    Budget {
        config,
        cancel: CancellationToken::new(),
        request_cancel: CancellationToken::new(),
        started: Instant::now(),
        operations: 0,
        entries: 0,
        strings: 0,
        cells: 0,
        substrings: 0,
    }
}
fn observed(path: &Path) -> Option<Stat> {
    stat(path.to_str().unwrap(), &mut budget(BrowseConfig::default())).unwrap()
}
struct ChildGuard(Child);
impl ChildGuard {
    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "controlled actual Windows fixture child failed"
                );
                return;
            }
            if Instant::now() >= deadline {
                let _ = self.0.kill();
                self.0.wait().unwrap();
                panic!("controlled fixture child timed out and was reaped");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn oracle(paths: &[String]) -> Value {
    oracle_transport(paths, false)
}
fn oracle_transport(paths: &[String], crlf: bool) -> Value {
    let root = std::env::var_os("FREESHOW_BROWSE_WINDOWS_ORACLE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_owned()
        });
    let temporary = tempfile::tempdir().unwrap();
    let input = temporary.path().join("input.json");
    let output = temporary.path().join("output.json");
    fs::write(&input, serde_json::to_vec(paths).unwrap()).unwrap();
    let mut command = Command::new(
        std::env::var_os("FREESHOW_BROWSE_WINDOWS_NODE").unwrap_or_else(|| "node".into()),
    );
    command.arg(root.join("tests/parity/lib/browse-windows-oracle.mjs"));
    if crlf {
        command.arg("--crlf-source-control");
    }
    let mut child = ChildGuard(
        command
            .current_dir(&root)
            .stdin(fs::File::open(input).unwrap())
            .stdout(fs::File::create(&output).unwrap())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    child.finish();
    let mut bytes = Vec::new();
    fs::File::open(output)
        .unwrap()
        .take(512 * 1024 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 512 * 1024);
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["node"], "22.21.1");
    assert_eq!(
        value["sourceSha256"],
        "98e563c55026d2dbbdd217ee9b94962ed7fc8b82d893795355bd7dc0f54fc5de"
    );
    assert_eq!(
        value["lineEndingControls"],
        json!({"crlfAccepted":true,"sourceMutationRejected":true})
    );
    assert_eq!(
        value["platform"], "win32",
        "Linux simulation is not native Windows evidence"
    );
    value
}
fn compare(paths: &[String]) -> Value {
    let source = oracle(paths);
    let expected_keys = [
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
    for (path, expected) in paths.iter().zip(source["values"].as_array().unwrap()) {
        let actual = stat(path, &mut budget(BrowseConfig::default())).unwrap();
        if expected["value"].is_null() {
            assert!(actual.is_none(), "{path}");
            assert_eq!(expected["diagnostics"].as_array().unwrap().len(), 1);
        } else {
            let actual = actual.expect("real original stat succeeded");
            compare_bits(&actual.json, expected, path);
            assert_eq!(actual.directory, expected["value"]["folder"], "{path}");
            assert_eq!(
                actual
                    .json
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                expected_keys
            );
            assert_eq!(expected["keys"], json!(expected_keys));
            assert!(expected["fields"]
                .as_object()
                .unwrap()
                .values()
                .all(|value| value == "number"));
            assert!(expected["diagnostics"].as_array().unwrap().is_empty());
        }
    }
    source
}
fn compare_bits(actual: &Value, source: &Value, path: &str) {
    let source = source["bits"].as_object().unwrap();
    assert_eq!(actual.as_object().unwrap().len(), source.len());
    for (name, bits) in source {
        let bytes = actual[name].as_f64().unwrap().to_le_bytes();
        let actual = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            actual,
            bits.as_str().unwrap(),
            "actual Node Number bits {path}:{name}"
        );
    }
}
fn junction(link: &Path, target: &Path) {
    let mut child = ChildGuard(
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
    child.finish();
    assert_ne!(
        fs::symlink_metadata(link).unwrap().file_attributes() & 0x400,
        0
    );
}

#[test]
fn real_windows_source_stats_order_types_files_folders_links_allocation_and_raw_spelling() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let file = root.join("音楽 🙌.WAV");
    let empty = root.join("empty.bin");
    let hard = root.join("hardlink.wav");
    let directory = root.join("directory");
    fs::write(&file, vec![0x5a; 65_537]).unwrap();
    fs::write(&empty, []).unwrap();
    fs::create_dir(&directory).unwrap();
    fs::hard_link(&file, &hard).unwrap();
    let ordinary = file
        .to_str()
        .unwrap()
        .strip_prefix(r"\\?\")
        .unwrap()
        .to_owned();
    let paths = [
        root.to_str().unwrap().to_owned(),
        file.to_str().unwrap().to_owned(),
        hard.to_str().unwrap().to_owned(),
        empty.to_str().unwrap().to_owned(),
        directory.to_str().unwrap().to_owned(),
        ordinary.clone(),
        ordinary.replace('\\', "/"),
        format!("{ordinary}\\"),
    ];
    let source = compare(&paths);
    let values = source["values"].as_array().unwrap();
    assert_eq!(values[1]["value"]["stat"]["nlink"], 2.0);
    assert_eq!(
        values[1]["value"]["stat"]["ino"],
        values[2]["value"]["stat"]["ino"]
    );
    assert_eq!(values[4]["value"]["stat"]["size"], 0.0);
    assert_eq!(values[1]["value"]["path"], paths[1]);
    assert_eq!(values[7]["value"]["path"], paths[7]);
    assert_eq!(values[1]["value"]["extension"], "wav");
}

#[test]
fn real_windows_readonly_mode_matches_source_without_inventing_executable_bits() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("program.exe");
    fs::write(&file, b"metadata only, not an executable").unwrap();
    let value = compare(&[file.to_str().unwrap().into()]);
    assert_eq!(
        value["values"][0]["value"]["stat"]["mode"],
        (0o100000 | 0o666) as f64
    );
    struct Reset(PathBuf, fs::Permissions);
    impl Drop for Reset {
        fn drop(&mut self) {
            fs::set_permissions(&self.0, self.1.clone()).unwrap();
        }
    }
    let reset = Reset(file.clone(), fs::metadata(&file).unwrap().permissions());
    let mut permissions = fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&file, permissions).unwrap();
    let value = compare(&[file.to_str().unwrap().into()]);
    assert_eq!(
        value["values"][0]["value"]["stat"]["mode"],
        (0o100000 | 0o444) as f64
    );
    drop(reset);
}

#[test]
fn real_windows_fractional_and_unsigned32_time_boundary_matches_node_f64_projection() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("times.wav");
    fs::write(&file, b"metadata").unwrap();
    for (seconds, nanos) in [
        (0, 0),
        (123, 123_456_700),
        (1_700_000_000, 999_999_900),
        (i32::MAX as u64, 500_100),
        (i32::MAX as u64 + 1, 500_100),
        (u32::MAX as u64, 500_100),
    ] {
        assert_eq!(
            nanos % 100,
            0,
            "fixture must align to native FILETIME ticks"
        );
        let modified = UNIX_EPOCH + Duration::new(seconds, nanos);
        fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_modified(modified)
                    .set_accessed(modified),
            )
            .unwrap();
        let metadata = fs::metadata(&file).unwrap();
        assert_eq!(
            metadata.modified().unwrap(),
            modified,
            "the real filesystem must retain the requested boundary timestamp"
        );
        assert_eq!(
            metadata.accessed().unwrap(),
            modified,
            "source/native equality after clamping is not boundary evidence"
        );
        let value = compare(&[file.to_str().unwrap().into()]);
        eprintln!(
            "actual Windows Node time seconds={seconds} nanos={nanos}: {}",
            value["values"][0]["value"]["stat"]
        );
    }
}

#[test]
fn real_windows_exceptional_times_are_explicit_unsupported_not_fabricated_null_or_wrapped_stat() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("exceptional.wav");
    fs::write(&file, b"native signed time remains actual").unwrap();
    for modified in [
        UNIX_EPOCH - Duration::from_millis(1250),
        UNIX_EPOCH + Duration::from_secs(u32::MAX as u64 + 1),
    ] {
        fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().modified().unwrap(),
            modified,
            "the exceptional date must actually exist before testing rejection"
        );
        let source = oracle(&[file.to_str().unwrap().into()]);
        assert!(
            !source["values"][0]["value"].is_null(),
            "actual Node outcome is required, not an assumed Windows date result"
        );
        let error = stat(file.to_str().unwrap(), &mut budget(BrowseConfig::default()))
            .err()
            .unwrap();
        assert_eq!(error.code, "unsupported_stat_backend");
        assert!(error.message.contains("unsigned32seconds"));
        eprintln!(
            "actual Node exceptional-time evidence (native intentionally gated): {}",
            source["values"][0]
        );
    }
}

#[test]
fn real_windows_source_failure_null_is_certified_for_missing_nondirectory_empty_and_dangling() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("regular");
    fs::write(&file, b"not a directory").unwrap();
    compare(&[
        String::new(),
        temp.path().join("missing").to_str().unwrap().into(),
        file.join("child").to_str().unwrap().into(),
    ]);
    let dangling = temp.path().join("dangling");
    match symlink_file(temp.path().join("absent-target"), &dangling) {
        Ok(()) => {
            compare(&[dangling.to_str().unwrap().into()]);
        }
        Err(error) if error.raw_os_error() == Some(1314) => {
            eprintln!("SKIP only actual dangling symlink: Windows privilege1314")
        }
        Err(error) => panic!("actual private symlink creation failed: {error}"),
    }
}

#[test]
fn real_windows_junction_and_available_file_symlink_follow_actual_source_targets() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    let file = target.join("song.mp3");
    fs::write(&file, b"actual controlled link target").unwrap();
    let link = temp.path().join("junction");
    junction(&link, &target);
    compare(&[
        link.to_str().unwrap().into(),
        link.join("song.mp3").to_str().unwrap().into(),
    ]);
    assert!(observed(&link).unwrap().directory);
    let file_link = temp.path().join("file-link.mp3");
    match symlink_file(&file, &file_link) {
        Ok(()) => { compare(&[file_link.to_str().unwrap().into()]); }
        Err(error) if error.raw_os_error() == Some(1314) => eprintln!("SKIP only actual file symlink: Windows privilege1314; mandatory junction already executed"),
        Err(error) => panic!("actual private symlink creation failed: {error}"),
    }
}

#[test]
fn real_windows_exclusive_data_lock_still_allows_actual_source_and_native_attribute_stat() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("share-locked.wav");
    fs::write(&file, b"protected by a real sharing handle").unwrap();
    let guard = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&file)
        .unwrap();
    // A real data-read conflict proves the fixture lock is active. Win32 does
    // not apply this sharing restriction to FILE_READ_ATTRIBUTES requests.
    assert_eq!(File::open(&file).unwrap_err().raw_os_error(), Some(32));
    let source = compare(&[file.to_str().unwrap().into()]);
    assert!(!source["values"][0]["value"].is_null());
    eprintln!(
        "actual source/native metadata success under exclusive data lock: {}",
        source["values"][0]
    );
    drop(guard);
    compare(&[file.to_str().unwrap().into()]);
}

#[test]
fn real_windows_oracle_crlf_transport_preserves_values_and_strict_source_gate() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("source-transport.wav");
    fs::write(&file, b"actual immutable source after CRLF normalization").unwrap();
    let paths = [file.to_str().unwrap().to_owned()];
    let checkout = oracle(&paths);
    let crlf = oracle_transport(&paths, true);
    assert_eq!(checkout["sourceTransport"], "checkout");
    assert_eq!(crlf["sourceTransport"], "forced-crlf");
    assert_eq!(checkout["sourceSha256"], crlf["sourceSha256"]);
    assert_eq!(checkout["values"], crlf["values"]);
}

#[test]
fn stat_path_operation_and_cancel_budgets_apply_before_native_work() {
    let mut limits = budget(BrowseConfig::default());
    limits.cancel.cancel();
    assert_eq!(
        stat(r"\\not-a-real-server\share", &mut limits)
            .err()
            .unwrap()
            .code,
        "cancelled"
    );
    let mut limits = budget(BrowseConfig {
        operations: 1,
        ..BrowseConfig::default()
    });
    assert_eq!(
        stat(r"C:\ordinary", &mut limits).err().unwrap().code,
        "browse_resource_limit"
    );
    let mut limits = budget(BrowseConfig {
        path_bytes: 2,
        ..BrowseConfig::default()
    });
    assert_eq!(
        stat(r"C:\ordinary", &mut limits).err().unwrap().code,
        "browse_resource_limit"
    );
    for path in [
        r"\\not-a-real-server\share",
        r"\\.\pipe\not-a-real-pipe",
        "NUL",
        r"C:relative",
        r"C:\dir\..\file",
        r"C:\file:stream",
        r"C:\dir.\leaf",
    ] {
        assert_eq!(
            stat(path, &mut budget(BrowseConfig::default()))
                .err()
                .unwrap()
                .code,
            "unsupported_stat_backend",
            "{path}"
        );
    }
}

#[test]
fn full_native_identity_is_checked_before_lossy_js_integer_conversion() {
    let mut legacy = BY_HANDLE_FILE_INFORMATION {
        dwVolumeSerialNumber: u32::MAX,
        nFileIndexHigh: 0x0020_0000,
        nFileIndexLow: 1,
        ..Default::default()
    };
    let index = (u64::from(legacy.nFileIndexHigh) << 32) | u64::from(legacy.nFileIndexLow);
    let mut id = Identity {
        volume: u64::from(u32::MAX),
        file: [0; 16],
    };
    id.file[..8].copy_from_slice(&index.to_le_bytes());
    assert_eq!(certify_identity(&id, &legacy).unwrap(), index);
    assert_eq!(
        index as f64, 9_007_199_254_740_992.0,
        "same binary64 rounding as Node's ordinary numeric Stats"
    );
    id.file[15] = 1;
    assert_eq!(
        certify_identity(&id, &legacy).err().unwrap().code,
        "unsupported_stat_backend"
    );
    id.file[15] = 0;
    legacy.nFileIndexLow += 1;
    assert!(certify_identity(&id, &legacy).is_err());
}

#[tokio::test]
async fn file_info_and_ordinary_listing_use_fixed_workers_with_separate_verbatim_capabilities() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("native.WAV");
    fs::write(&file, b"worker-owned metadata").unwrap();
    let source = oracle(&[file.to_str().unwrap().into()]);
    let service = BrowseService::new(BrowseConfig {
        workers: 1,
        ..BrowseConfig::default()
    })
    .unwrap();
    let reply = service.dispatch("FILE_INFO", &json!(file)).await.unwrap();
    let expected = &source["values"][0];
    assert_eq!(reply.data["path"], expected["value"]["path"]);
    assert_eq!(reply.data["extension"], expected["value"]["extension"]);
    assert_eq!(reply.data["folder"], expected["value"]["folder"]);
    compare_bits(&reply.data["stat"], expected, file.to_str().unwrap());
    let parent = temp.path().join("parent.with.dot");
    fs::create_dir(&parent).unwrap();
    let spellings = [
        parent.join("leaf"),
        parent.join(".hidden"),
        parent.join("multi.part.PNG"),
    ];
    for file in &spellings {
        fs::write(file, b"source Windows extname").unwrap();
    }
    let paths = spellings
        .iter()
        .map(|file| file.to_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let source = oracle(&paths);
    for (path, expected) in paths.iter().zip(source["values"].as_array().unwrap()) {
        let reply = service.dispatch("FILE_INFO", &json!(path)).await.unwrap();
        assert_eq!(reply.data["path"], expected["value"]["path"]);
        assert_eq!(reply.data["extension"], expected["value"]["extension"]);
        compare_bits(&reply.data["stat"], expected, path);
    }
    let canonical_root = temp.path().canonicalize().unwrap();
    let verbatim_root = canonical_root.to_str().unwrap();
    let ordinary_root = verbatim_root.strip_prefix(r"\\?\").unwrap();
    let ordinary_file = Path::new(ordinary_root).join("native.WAV");
    let folder = service
        .dispatch("READ_FOLDER", &json!({"path":ordinary_root}))
        .await
        .unwrap();
    assert_eq!(folder.data[ordinary_root]["isFolder"], true);
    assert_eq!(
        folder.data[ordinary_file.to_str().unwrap()]["stats"]["size"],
        expected["value"]["stat"]["size"]
    );
    assert!(service
        .dispatch("GET_SIMILAR", &json!({"paths":[ordinary_file]}))
        .await
        .unwrap()
        .data
        .is_array());
    for command in ["READ_FOLDER", "GET_SIMILAR"] {
        let payload = if command == "READ_FOLDER" {
            json!({"path":verbatim_root})
        } else {
            json!({"paths":[canonical_root.join("native.WAV")]})
        };
        assert_eq!(
            service
                .dispatch(command, &payload)
                .await
                .err()
                .unwrap()
                .code,
            "unsupported_windows_path"
        );
    }
    service.shutdown().await.unwrap();
    assert_eq!(
        service
            .dispatch("FILE_INFO", &json!(file))
            .await
            .err()
            .unwrap()
            .code,
        "browse_shutdown"
    );
}
