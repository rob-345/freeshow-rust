//! Genuine Windows fixtures only; cross-Clippy compiles but never certifies them.
use super::*;
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::windows::{ffi::OsStringExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};
pub(super) fn budget(config: BrowseConfig) -> Budget {
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
fn ordinary_path(path: &Path) -> String {
    path.to_str()
        .unwrap()
        .strip_prefix(r"\\?\")
        .unwrap_or(path.to_str().unwrap())
        .to_owned()
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = PathBuf::from(ordinary_path(&temporary.path().canonicalize().unwrap()));
        Self {
            _temp: temporary,
            root,
        }
    }
    fn path(&self) -> String {
        ordinary_path(&self.root)
    }
}
struct ChildGuard(Child);
impl ChildGuard {
    fn finish(&mut self) {
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "real Windows source child failed");
                return;
            }
            if Instant::now() >= until {
                let _ = self.0.kill();
                self.0.wait().unwrap();
                panic!("real source fixture child timed out and was reaped");
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
fn oracle(requests: Value) -> Value {
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
    fs::write(&input, serde_json::to_vec(&requests).unwrap()).unwrap();
    let mut child = ChildGuard(
        Command::new(
            std::env::var_os("FREESHOW_BROWSE_WINDOWS_NODE").unwrap_or_else(|| "node".into()),
        )
        .arg(root.join("tests/parity/lib/browse-windows-listing-oracle.mjs"))
        .current_dir(&root)
        .stdin(File::open(input).unwrap())
        .stdout(File::create(&output).unwrap())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap(),
    );
    child.finish();
    let mut bytes = Vec::new();
    File::open(output)
        .unwrap()
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 4 * 1024 * 1024);
    let result: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["node"], "22.21.1");
    assert_eq!(result["uv"], "1.51.0");
    assert_eq!(
        result["platform"], "win32",
        "path.win32 on Linux is not real Windows evidence"
    );
    assert_eq!(result["lineEndingControls"], true);
    assert_eq!(result["controlledThumbnailEffect"], true);
    result
}
fn process_payload(command: &str, payload: &Value, config: BrowseConfig) -> Result<BrowseReply> {
    let request = Request::parse(command, payload, &config)?;
    process(request, &mut budget(config))
}
fn compare(command: &str, payload: Value) -> (BrowseReply, Value) {
    let source = oracle(json!([{ "command":command,"data":payload }]));
    let expected = &source["results"][0];
    let reply = process_payload(command, &payload, BrowseConfig::default()).unwrap();
    // Parallel source stat/recurse completions can reorder the outer object and
    // thumbnail callbacks. Compare complete values and each files[] order, not
    // invented universal outer-map/callback scheduling equivalence.
    if command == "READ_FOLDER" {
        let actual = reply.data.as_object().unwrap();
        let expected_data = expected["data"].as_object().unwrap();
        assert_eq!(actual.len(), expected_data.len());
        for (path, record) in actual {
            record_comparison::assert_record(
                record,
                expected_data
                    .get(path)
                    .expect("same complete source path set"),
                expected["statsBits"].get(path),
                path,
            );
        }
        assert_eq!(
            expected["statsBits"].as_object().unwrap().len(),
            actual
                .values()
                .filter(|record| record.get("stats").is_some())
                .count(),
            "one raw Number-bit observation for every file and no unrelated paths"
        );
        let mut own_undefined: Vec<_> = actual
            .iter()
            .filter(|(_, record)| record.get("noMedia").is_some_and(Value::is_null))
            .map(|(path, _)| path.clone())
            .collect();
        let mut source_undefined: Vec<_> = expected["undefinedFields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect();
        own_undefined.sort();
        source_undefined.sort();
        assert_eq!(own_undefined, source_undefined);
        let mut actual_paths: Vec<_> = reply
            .thumbnail_intents
            .iter()
            .map(|intent| intent.input.clone())
            .collect();
        let mut expected_paths: Vec<_> = expected["thumbnails"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect();
        actual_paths.sort();
        expected_paths.sort();
        assert_eq!(actual_paths, expected_paths);
    } else {
        assert_eq!(reply.data, expected["data"]);
    }
    (reply, source)
}

#[test]
fn windows_native_multibatch_scandir_matches_actual_node_order_without_sorting() {
    let fixture = Fixture::new();
    for index in (0..180).rev() {
        fs::write(
            fixture
                .root
                .join(format!("{:03}-音楽-🙌-{}.txt", index, "長".repeat(25))),
            [],
        )
        .unwrap();
    }
    for name in [
        "z.bin",
        "A.bin",
        "é.bin",
        "e\u{301}.bin",
        ".hidden",
        "last.PNG",
    ] {
        fs::write(fixture.root.join(name), []).unwrap();
    }
    let source = oracle(json!([{ "command":"READ_DIRECTORY_NAMES","data":fixture.path() }]));
    let names = source["results"][0]["data"].as_array().unwrap();
    assert_eq!(names.len(), 186);
    let actual = scandir::readdir(&fixture.path(), &mut budget(BrowseConfig::default())).unwrap();
    let expected: Vec<_> = names
        .iter()
        .map(|name| lexical(paths::join(&fixture.path(), name.as_str().unwrap())).unwrap())
        .collect();
    assert_eq!(
        actual, expected,
        "native files order, including records spanning many8KiB queries"
    );
    let (reply, _) = compare("READ_FOLDER", json!({"path":fixture.path(),"depth":-0.25}));
    assert_eq!(reply.data[fixture.path()]["files"], json!(expected));
}
#[test]
fn windows_full_recursive_source_stats_unicode_spelling_and_depth() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join("子 folder").join("deeper")).unwrap();
    fs::write(fixture.root.join("Alpha.WAV"), b"no media decoding").unwrap();
    fs::write(fixture.root.join("子 folder").join("音楽 🙌.PNG"), []).unwrap();
    fs::write(
        fixture
            .root
            .join("子 folder")
            .join("deeper")
            .join("nested.bin"),
        b"1",
    )
    .unwrap();
    for depth in [0.0, 0.5, 1.0, 2.0] {
        compare("READ_FOLDER", json!({"path":fixture.path(),"depth":depth}));
    }
    let raw = format!("{}/", fixture.path().replace('\\', "/"));
    let (reply, source) = compare(
        "READ_FOLDER",
        json!({"path":[raw.clone(),fixture.path()],"depth":0}),
    );
    assert!(
        reply.data.get(&raw).is_some(),
        "caller root spelling is not normalized away"
    );
    assert!(reply.data[&raw]["files"]
        .as_array()
        .unwrap()
        .iter()
        .all(|path| !path.as_str().unwrap().contains('/')));
    assert!(source["results"][0]["inputAfter"]["path"].is_array());
}
#[test]
fn windows_capture_uses_first_four_native_media_entries_and_source_cutoffs() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join("slides").join("level2").join("level3")).unwrap();
    fs::create_dir(fixture.root.join("empty")).unwrap();
    for name in [
        "z.png",
        "a.jpg",
        "b.webm",
        "c.PNG",
        "d.mp4",
        "e.gif",
        "notes.txt",
    ] {
        fs::write(
            fixture.root.join("slides").join(name),
            b"capture only planned",
        )
        .unwrap();
    }
    fs::write(fixture.root.join("root.MOV"), []).unwrap();
    fs::write(
        fixture.root.join("slides").join("level2").join("deep.jpg"),
        [],
    )
    .unwrap();
    let (reply, _) = compare(
        "READ_FOLDER",
        json!({"path":fixture.path(),"captureFolderContent":true,"generateThumbnails":true,"depth":0}),
    );
    assert_eq!(
        reply
            .thumbnail_intents
            .iter()
            .filter(|intent| intent.reason == ThumbnailReason::FolderCapture)
            .count(),
        4
    );
    assert_eq!(
        reply
            .thumbnail_intents
            .iter()
            .filter(|intent| intent.reason == ThumbnailReason::RootMedia)
            .count(),
        1
    );
    compare(
        "READ_FOLDER",
        json!({"path":fixture.path(),"captureFolderContent":true,"depth":1}),
    );
    compare(
        "READ_FOLDER",
        json!({"path":fixture.path(),"captureFolderContent":true,"depth":2}),
    );
}
#[test]
fn windows_similar_preserves_native_ties_duplicates_utf16_and_directory_candidates() {
    let fixture = Fixture::new();
    let other = fixture.root.join("other");
    fs::create_dir(&other).unwrap();
    for name in [
        "SongA.wav",
        "SongB.mp3",
        "SongC.png",
        "SongB.txt",
        "songA.wav",
        "SongAB.flac",
        "音楽🙌.mp3",
        "音楽🙏.wav",
        "é.mp3",
        "e\u{301}.mp3",
    ] {
        fs::write(fixture.root.join(name), []).unwrap();
    }
    fs::create_dir(fixture.root.join("SongD")).unwrap();
    for name in ["SongE.wav", "SongF.wav"] {
        fs::write(other.join(name), []).unwrap();
    }
    let paths = vec![
        ordinary_path(&fixture.root.join("SongA.wav")),
        ordinary_path(&fixture.root.join("音楽🙌.mp3")),
        ordinary_path(&other.join("SongE.wav")),
        ordinary_path(&fixture.root.join("SongA.wav")),
    ];
    let (reply, _) = compare("GET_SIMILAR", json!({"paths":paths}));
    assert!(reply
        .data
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value["name"] == "SongD"));
    compare(
        "GET_SIMILAR",
        json!({"paths":[paths[0].replace('\\', "/")]}),
    );
    compare("GET_SIMILAR", json!({"paths":[]}));
    assert!(!reply
        .data
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value["path"] == paths[0]));
}
#[test]
fn windows_missing_file_and_locked_directory_use_real_source_empty_listing() {
    let fixture = Fixture::new();
    let file = fixture.root.join("regular.bin");
    fs::write(&file, []).unwrap();
    for path in [
        ordinary_path(&file),
        ordinary_path(&fixture.root.join("missing")),
    ] {
        let (reply, _) = compare("READ_FOLDER", json!({"path":path}));
        assert_eq!(reply.data, json!({}));
    }
    compare(
        "GET_SIMILAR",
        json!({"paths":[ordinary_path(&fixture.root.join("missing").join("original.wav"))]}),
    );
    // The existing stat READ_ATTRIBUTES query is share-exempt; enumeration's
    // actual data/list access conflicts with this held exclusive directory open.
    let locked = fixture.root.join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("hidden-by-lock.txt"), []).unwrap();
    let guard = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
        .open(&locked)
        .unwrap();
    let (reply, source) = compare("READ_FOLDER", json!({"path":ordinary_path(&locked)}));
    assert_eq!(reply.data[ordinary_path(&locked)]["files"], json!([]));
    assert!(source["results"][0]["data"]
        .get(ordinary_path(&locked))
        .is_some());
    drop(guard);
}
#[test]
fn windows_unc_device_verbatim_relative_and_ambiguous_spelling_are_explicitly_unsupported() {
    for path in [
        r"\\server\share\x",
        r"\\?\C:\x",
        r"\\.\NUL",
        "relative",
        r"C:relative",
        r"C:\a\..\b",
        r"C:\a.\b",
        r"C:\a:stream",
        r"C:\CON",
    ] {
        let error = process_payload(
            "READ_FOLDER",
            &json!({"path":path}),
            BrowseConfig::default(),
        )
        .unwrap_err();
        assert!(
            error.code.starts_with("unsupported_"),
            "{path}: {}",
            error.code
        );
    }
}
#[test]
fn windows_unpaired_utf16_filename_is_rejected_without_lossy_alias() {
    let fixture = Fixture::new();
    let invalid = std::ffi::OsString::from_wide(&[b'x' as u16, 0xd800, b'.' as u16, b't' as u16]);
    fs::write(fixture.root.join(invalid), []).unwrap();
    let error =
        scandir::readdir(&fixture.path(), &mut budget(BrowseConfig::default())).unwrap_err();
    assert_eq!(error.code, "unsupported_windows_listing");
    assert!(error.message.contains("Unpaired"));
}
#[test]
fn windows_entry_output_math_cancel_and_deadline_caps_apply_to_native_jobs() {
    let fixture = Fixture::new();
    for name in ["SongA.wav", "SongB.wav", "SongC.wav"] {
        fs::write(fixture.root.join(name), []).unwrap();
    }
    let payload = json!({"path":fixture.path()});
    for config in [
        BrowseConfig {
            entries: 1,
            ..BrowseConfig::default()
        },
        BrowseConfig {
            output_bytes: 32,
            ..BrowseConfig::default()
        },
        BrowseConfig {
            operations: 1,
            ..BrowseConfig::default()
        },
    ] {
        assert_eq!(
            process_payload("READ_FOLDER", &payload, config)
                .unwrap_err()
                .code,
            "browse_resource_limit"
        );
    }
    let mut stale = budget(BrowseConfig::default());
    stale.started = Instant::now() - Duration::from_secs(30);
    assert_eq!(
        process(
            Request::Folder {
                roots: vec![fixture.path()],
                depth: 0.0,
                generate: false,
                capture: false
            },
            &mut stale
        )
        .unwrap_err()
        .code,
        "browse_resource_limit"
    );
    let mut cancelled = budget(BrowseConfig::default());
    cancelled.request_cancel.cancel();
    assert_eq!(
        scandir::readdir(&fixture.path(), &mut cancelled)
            .unwrap_err()
            .code,
        "cancelled"
    );
    let error = process_payload(
        "GET_SIMILAR",
        &json!({"paths":[ordinary_path(&fixture.root.join("SongA.wav"))]}),
        BrowseConfig {
            levenshtein_cells: 1,
            ..BrowseConfig::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "browse_resource_limit");
}
#[test]
fn windows_junction_follows_source_fixed_local_target_without_authorization_claim() {
    let fixture = Fixture::new();
    let target = Fixture::new();
    fs::write(target.root.join("target.txt"), []).unwrap();
    let link = fixture.root.join("junction");
    let mut child = ChildGuard(
        Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target.root)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    child.finish();
    struct Remove(PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = fs::remove_dir(&self.0);
        }
    }
    let _remove = Remove(link.clone());
    let (reply, _) = compare(
        "READ_FOLDER",
        json!({"path":ordinary_path(&link),"depth":0}),
    );
    assert_eq!(
        reply.data[ordinary_path(&link)]["files"],
        json!([ordinary_path(&link.join("target.txt"))])
    );
}
// The two controlled tests serialize this single one-shot hook.
static HOOK_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
#[tokio::test]
async fn windows_accepted_native_handle_survives_caller_drop_and_shutdown_waiter_drop() {
    let _serial = HOOK_SERIAL.lock().await;
    let fixture = Fixture::new();
    fs::write(fixture.root.join("actual.txt"), []).unwrap();
    let (entered_send, entered) = mpsc::channel();
    let (release_send, release) = mpsc::channel();
    *scandir::AFTER_OPEN.lock().unwrap() = Some((
        fixture.path(),
        Box::new(move || {
            entered_send.send(()).unwrap();
            release.recv().unwrap();
        }),
    ));
    let service = BrowseService::new(BrowseConfig {
        workers: 1,
        ..BrowseConfig::default()
    })
    .unwrap();
    let worker = service.clone();
    let payload = json!({"path":fixture.path()});
    let request = tokio::spawn(async move { worker.dispatch("READ_FOLDER", &payload).await });
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if entered.try_recv().is_ok() {
            break;
        }
        assert!(Instant::now() < until);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(
        service
            .dispatch("READ_FOLDER", &json!({"path":fixture.path()}))
            .await
            .unwrap_err()
            .code,
        "browse_busy"
    );
    request.abort();
    let _ = request.await;
    assert_eq!(
        service.inner.permits.available_permits(),
        0,
        "actual opened worker still owns quota"
    );
    let mut retirement = Box::pin(service.shutdown());
    assert!(
        tokio::time::timeout(Duration::from_millis(25), retirement.as_mut())
            .await
            .is_err()
    );
    drop(retirement);
    assert_eq!(
        service.inner.permits.available_permits(),
        0,
        "observer timeout is not native drain"
    );
    release_send.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        service
            .dispatch("READ_FOLDER", &json!({"path":fixture.path()}))
            .await
            .unwrap_err()
            .code,
        "browse_shutdown"
    );
    assert_eq!(
        Arc::strong_count(&service.inner),
        1,
        "actual worker caller retired"
    );
}

#[tokio::test]
async fn windows_actual_open_directory_is_retained_across_parent_name_replacement() {
    let _serial = HOOK_SERIAL.lock().await;
    let fixture = Fixture::new();
    let path = fixture.root.join("scan");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("original.txt"), []).unwrap();
    let spelling = ordinary_path(&path);
    let (entered_send, entered) = mpsc::channel();
    let (release_send, release) = mpsc::channel();
    *scandir::AFTER_OPEN.lock().unwrap() = Some((
        spelling.clone(),
        Box::new(move || {
            entered_send.send(()).unwrap();
            release.recv().unwrap();
        }),
    ));
    let work_path = spelling.clone();
    let worker = std::thread::spawn(move || {
        scandir::readdir(&work_path, &mut budget(BrowseConfig::default()))
    });
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if entered.try_recv().is_ok() {
            break;
        }
        assert!(Instant::now() < until);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    fs::rename(&path, fixture.root.join("moved")).unwrap();
    fs::create_dir(&path).unwrap();
    fs::write(path.join("replacement.txt"), []).unwrap();
    release_send.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while !worker.is_finished() {
        assert!(Instant::now() < until);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let result = worker.join().unwrap().unwrap();
    assert_eq!(result, [lexical(paths::join(&spelling, "original.txt")).unwrap()], "the actual opened directory is enumerated, although returned child spellings are source path joins");
    assert!(
        !path.join("original.txt").exists(),
        "this is not an immutable path snapshot or content authorization"
    );
    assert_eq!(
        scandir::readdir(&spelling, &mut budget(BrowseConfig::default())).unwrap(),
        [lexical(paths::join(&spelling, "replacement.txt")).unwrap()]
    );
}

// Self-contained for portable comparator controls; no Windows API is simulated.
mod record_comparison {
    use serde_json::Value;

    const STAT_KEYS: [&str; 14] = [
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

    fn assert_exact(actual: &Value, expected: &Value, label: &str) {
        match (actual, expected) {
            (Value::Object(actual), Value::Object(expected)) => {
                assert_eq!(
                    actual.keys().collect::<Vec<_>>(),
                    expected.keys().collect::<Vec<_>>(),
                    "source object keys/order {label}"
                );
                for (key, value) in expected {
                    assert_exact(&actual[key], value, label);
                }
            }
            (Value::Array(actual), Value::Array(expected)) => {
                assert_eq!(actual.len(), expected.len(), "source array length {label}");
                for (actual, expected) in actual.iter().zip(expected) {
                    assert_exact(actual, expected, label);
                }
            }
            _ => assert_eq!(actual, expected, "complete source value/type {label}"),
        }
    }

    pub(super) fn assert_record(
        actual: &Value,
        expected: &Value,
        stats_bits: Option<&Value>,
        label: &str,
    ) {
        let actual = actual.as_object().expect("native record object");
        let expected = expected.as_object().expect("source record object");
        assert_eq!(
            actual.keys().collect::<Vec<_>>(),
            expected.keys().collect::<Vec<_>>(),
            "complete source record keys/order {label}"
        );
        for (name, value) in expected {
            if name != "stats" {
                assert_exact(&actual[name], value, label);
                continue;
            }
            let native = actual[name].as_object().expect("native Stats object");
            let projected = value.as_object().expect("source Stats object");
            let bits = stats_bits
                .expect("source captured Number bits before JSON")
                .as_object()
                .expect("source Number-bit object");
            for fields in [native, projected, bits] {
                assert_eq!(
                    fields.keys().map(String::as_str).collect::<Vec<_>>(),
                    STAT_KEYS,
                    "all14 Stats fields and their original order {label}"
                );
            }
            for key in STAT_KEYS {
                let number = native[key].as_f64().expect("native Stats Number");
                let source_number = projected[key].as_f64().expect("source Stats Number");
                assert_eq!(
                    number, source_number,
                    "source JSON Number value {label}:{key}"
                );
                // JSON loses -0's sign and has no integer/f64 distinction in
                // JavaScript. The pre-JSON capture remains the bitwise oracle.
                let actual_bits = number
                    .to_le_bytes()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                assert_eq!(
                    actual_bits,
                    bits[key].as_str().expect("source Number-bit string"),
                    "source native Number bits {label}:{key}"
                );
            }
        }
        assert_eq!(
            stats_bits.is_some(),
            expected.contains_key("stats"),
            "only file records carry a Stats-bit observation {label}"
        );
    }

    #[cfg(test)]
    fn fixture() -> (Value, Value, Value) {
        use serde_json::{json, Map};
        let mut native = Map::new();
        let mut projected = Map::new();
        let mut bits = Map::new();
        for (index, key) in STAT_KEYS.iter().enumerate() {
            let integer = match *key {
                "size" => 21,
                "uid" | "gid" | "rdev" => 0,
                _ => index + 1,
            };
            let number = integer as f64;
            native.insert((*key).into(), json!(number));
            projected.insert((*key).into(), json!(integer));
            bits.insert(
                (*key).into(),
                json!(number
                    .to_le_bytes()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()),
            );
        }
        (
            json!({"isFolder":false,"path":"C:\\音楽.wav","name":"音楽.wav","thumbnailPath":"","stats":native}),
            json!({"isFolder":false,"path":"C:\\音楽.wav","name":"音楽.wav","thumbnailPath":"","stats":projected}),
            Value::Object(bits),
        )
    }

    #[cfg(test)]
    fn rejects(actual: &Value, expected: &Value, bits: Option<&Value>) {
        assert!(
            std::panic::catch_unwind(|| assert_record(actual, expected, bits, "control")).is_err()
        );
    }

    #[test]
    fn integer_float_representation_matches_all_source_number_bits() {
        let (native, source, bits) = fixture();
        assert_ne!(native["stats"]["size"], source["stats"]["size"]);
        assert_record(&native, &source, Some(&bits), "integer21 versus float21.0");
        let mut negative_zero = native.clone();
        negative_zero["stats"]["uid"] = serde_json::json!(-0.0);
        let mut negative_bits = bits.clone();
        negative_bits["uid"] = serde_json::json!("0000000000000080");
        assert_record(
            &negative_zero,
            &source,
            Some(&negative_bits),
            "source JSON loses -0 sign",
        );
        rejects(&negative_zero, &source, Some(&bits));
    }

    #[test]
    fn every_stat_bit_key_order_type_and_observation_is_required() {
        use serde_json::json;
        let (native, source, bits) = fixture();
        for key in STAT_KEYS {
            let mut changed = native.clone();
            let number = changed["stats"][key].as_f64().unwrap();
            changed["stats"][key] = json!(f64::from_bits(number.to_bits() + 1));
            rejects(&changed, &source, Some(&bits));
        }
        for target in 0..3 {
            for mutation in 0..3 {
                let (mut native, mut source, mut bits) = fixture();
                let fields = match target {
                    0 => native["stats"].as_object_mut().unwrap(),
                    1 => source["stats"].as_object_mut().unwrap(),
                    _ => bits.as_object_mut().unwrap(),
                };
                let removed = fields.remove("dev").unwrap();
                match mutation {
                    1 => {
                        fields.insert("dev".into(), removed);
                    }
                    2 => {
                        fields.insert("unexpectedDev".into(), removed);
                    }
                    _ => {}
                }
                rejects(&native, &source, Some(&bits));
            }
        }
        let mut wrong_bits = bits.clone();
        wrong_bits["size"] = json!("0100000000003540");
        rejects(&native, &source, Some(&wrong_bits));
        wrong_bits["size"] = json!(21);
        rejects(&native, &source, Some(&wrong_bits));
        let mut wrong_type = native.clone();
        wrong_type["stats"]["size"] = json!("21");
        rejects(&wrong_type, &source, Some(&bits));
        let mut wrong_type = source.clone();
        wrong_type["stats"]["size"] = json!("21");
        rejects(&native, &wrong_type, Some(&bits));
        rejects(&native, &source, None);
    }

    #[test]
    fn non_stats_shapes_types_and_array_object_order_remain_strict() {
        use serde_json::json;
        let (native, source, bits) = fixture();
        let mut changed = native.clone();
        changed["path"] = json!("C:\\different.wav");
        rejects(&changed, &source, Some(&bits));
        changed = native.clone();
        changed["isFolder"] = json!(0);
        rejects(&changed, &source, Some(&bits));
        for reorder in [false, true] {
            changed = native.clone();
            let fields = changed.as_object_mut().unwrap();
            let removed = fields.remove("name").unwrap();
            if reorder {
                fields.insert("name".into(), removed);
            }
            rejects(&changed, &source, Some(&bits));
        }
        changed = native.clone();
        changed["extra"] = json!(null);
        rejects(&changed, &source, Some(&bits));
        let source = json!({"isFolder":true,"path":"C:\\folder","files":["first","second"],"noMedia":null,"details":{"x":1,"y":2}});
        assert_record(&source, &source, None, "complete folder");
        rejects(&source, &source, Some(&bits));
        for files in [
            json!(["second", "first"]),
            json!(["first"]),
            json!({"0":"first","1":"second"}),
        ] {
            let mut changed = source.clone();
            changed["files"] = files;
            rejects(&changed, &source, None);
        }
        let mut changed = source.clone();
        changed["details"] = json!({"y":2,"x":1});
        rejects(&changed, &source, None);
        changed["details"] = json!({"x":1.0,"y":2});
        rejects(&changed, &source, None);
    }
}
