use super::*;
use serde_json::{json, Value};
use std::{fs, sync::Mutex};
static SERIAL: Mutex<()> = Mutex::new(());
fn checker(root: &Path) -> Checker {
    Checker::new(TrustedUserData::from_host(root).unwrap(), Limits::default()).unwrap()
}
fn query() -> NemotronNoModel {
    NemotronNoModel::explicit(ModelId::Undefined).unwrap()
}
fn job(checker: &Checker) -> Job {
    checker
        .try_begin(query(), Platform::native(), Control::default())
        .unwrap()
}
fn read(prepared: Prepared) -> StatusRead {
    match prepared {
        Prepared::Ready(r) => r,
        Prepared::Stamp(s) => s.apply().unwrap(),
    }
}
fn model_dir(root: &Path) -> PathBuf {
    root.join("bin").join("nemotron").join("models")
}
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::create_dir_all(model_dir(&root)).unwrap();
    (temp, root)
}
fn tiny(root: &Path) {
    for (name, _) in FILES {
        fs::write(model_dir(root).join(name), name).unwrap();
    }
}
fn stamp(root: &Path) -> Value {
    let mut files = serde_json::Map::new();
    for (name, sha) in FILES {
        let m = fs::metadata(model_dir(root).join(name)).unwrap();
        let (s, t) = engine::values(&m).unwrap();
        files.insert(name.into(), json!({"sha256":sha,"size":s,"mtimeMs":t}));
    }
    json!({"revision":REVISION,"files":files})
}
fn write_stamp(root: &Path) {
    fs::write(
        model_dir(root).join("model.json"),
        serde_json::to_vec(&stamp(root)).unwrap(),
    )
    .unwrap();
}
fn err<T>(result: Result<T>) -> Error {
    match result {
        Err(e) => e,
        Ok(_) => panic!("expected actual error"),
    }
}
fn reference() -> Value {
    serde_json::from_str(include_str!("../../../tests/parity/ai-status-fixture.json")).unwrap()
}

#[test]
fn source_selection_and_native_root_bounds() {
    let _serial = SERIAL.lock().unwrap();
    for m in [
        ModelId::Undefined,
        ModelId::Null,
        ModelId::Bool(false),
        ModelId::Number(0.),
        ModelId::Number(-0.),
        ModelId::Number(f64::NAN),
        ModelId::Text(""),
    ] {
        assert!(NemotronNoModel::explicit(m).is_ok())
    }
    for m in [
        ModelId::Bool(true),
        ModelId::Number(1.),
        ModelId::Number(f64::INFINITY),
        ModelId::Text("tiny"),
        ModelId::Other,
    ] {
        assert_eq!(
            err(NemotronNoModel::explicit(m)).code,
            "ai_status_unsupported"
        )
    }
    assert!(
        NemotronNoModel::from_payload(&json!({"engineId":"nemotron","customPath":"ignored"}))
            .is_ok()
    );
    for v in [
        Value::Null,
        json!({}),
        json!({"engineId":"openai"}),
        json!({"engineId":"nemotron","modelId":[]}),
    ] {
        assert!(NemotronNoModel::from_payload(&v).is_err())
    }
    assert!(TrustedUserData::from_host("relative").is_err());
    let deep = (0..20).fold(Value::Null, |v, _| json!([v]));
    assert_eq!(
        err(NemotronNoModel::from_payload(
            &json!({"engineId":"nemotron","x":deep})
        ))
        .code,
        "ai_status_limit"
    );
}

#[test]
fn actual_files_missing_outdated_and_later_missing_precedence() {
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    let c = checker(&root);
    assert_eq!(
        serde_json::to_value(read(job(&c).prepare().unwrap()).reply()).unwrap(),
        json!({"nemotron":{"ready":false,"error":"The local model is missing"}})
    );
    tiny(&root);
    let r = read(job(&c).prepare().unwrap());
    assert_eq!(r.evidence().hashed, FILES.map(|v| v.0));
    assert_eq!(
        serde_json::to_value(r.reply()).unwrap()["nemotron"]["error"],
        "The local model is outdated"
    );
    drop(r);
    fs::remove_file(model_dir(&root).join("silero_vad.onnx")).unwrap();
    let r = read(job(&c).prepare().unwrap());
    assert_eq!(
        r.evidence().hashed,
        FILES[..4].iter().map(|v| v.0).collect::<Vec<_>>()
    );
    assert_eq!(
        serde_json::to_value(r.reply()).unwrap()["nemotron"]["error"],
        "The local model is missing"
    );
    assert!(!model_dir(&root).join("model.json").exists());
}

#[test]
fn genuine_tiny_stamp_fast_path_is_not_model_usable_evidence() {
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    tiny(&root);
    write_stamp(&root);
    let c = checker(&root);
    let r = read(job(&c).prepare().unwrap());
    assert!(r.evidence().fast_path);
    assert!(r.evidence().hashed.is_empty());
    assert_eq!(r.evidence().stats.len(), 5);
    assert_eq!(
        serde_json::to_value(r.reply()).unwrap(),
        json!({"nemotron":{"ready":true,"localPath":model_dir(&root)}})
    );
}

#[test]
fn four_whole_leases_cross_checker_replacement_ready_and_prepared() {
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    let c = checker(&root);
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(read(job(&c).prepare().unwrap()))
    }
    let replacement = checker(&root);
    assert_eq!(replacement.retained(), 4);
    assert_eq!(
        err(replacement.try_begin(query(), Platform::native(), Control::default())).code,
        "ai_status_busy"
    );
    let _borrow = held[0].reply();
    assert_eq!(replacement.retained(), 4);
    held.pop();
    let accepted = job(&replacement);
    assert_eq!(replacement.retained(), 4);
    drop(accepted);
    drop(held);
    assert_eq!(c.retained(), 0);
    tiny(&root);
    let expected = FILES.map(|v| v.1);
    let intent = engine::prepare(job(&c), Some(expected)).unwrap();
    assert!(matches!(intent, Prepared::Stamp(_)));
    assert_eq!(c.retained(), 1);
    drop(intent);
    assert_eq!(c.retained(), 0);
}

#[test]
fn injected_digest_boundary_real_write_only_then_ready_and_exact_order() {
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    tiny(&root);
    let c = checker(&root);
    let Prepared::Stamp(intent) = engine::prepare(job(&c), Some(FILES.map(|v| v.1))).unwrap()
    else {
        panic!("must require write")
    };
    assert!(!model_dir(&root).join("model.json").exists());
    assert_eq!(intent.user_data(), root);
    let r = intent.apply().unwrap();
    assert!(r.write_diagnostic().is_none());
    let bytes = fs::read(model_dir(&root).join("model.json")).unwrap();
    let parsed: Value = serde_json::from_slice(&bytes).unwrap();
    let actual = stamp::parse(&bytes).unwrap().unwrap();
    for (name, sha) in FILES {
        let observed = engine::values(&fs::metadata(model_dir(&root).join(name)).unwrap()).unwrap();
        assert_eq!(
            actual.entry(name, sha).unwrap().numbers().unwrap(),
            observed
        );
    }
    assert_eq!(
        parsed["files"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        FILES.map(|v| v.0)
    );
    assert!(bytes.starts_with(b"{\n    \"revision\": "));
    assert!(!bytes.ends_with(b"\n"));
    drop(r);
    assert!(read(job(&c).prepare().unwrap()).evidence().fast_path);
}

#[test]
fn injected_digest_boundary_actual_write_failure_still_source_ready() {
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    tiny(&root);
    fs::create_dir(model_dir(&root).join("model.json")).unwrap();
    let c = checker(&root);
    let Prepared::Stamp(intent) = engine::prepare(job(&c), Some(FILES.map(|v| v.1))).unwrap()
    else {
        panic!("write branch")
    };
    let r = intent.apply().unwrap();
    assert_eq!(r.write_diagnostic(), Some(&WriteDiagnostic::Failed));
    assert_eq!(
        serde_json::to_value(r.reply()).unwrap()["nemotron"]["ready"],
        true
    );
    assert!(model_dir(&root).join("model.json").is_dir());
}

#[test]
fn injected_boundary_matches_independently_regenerated_original_stamp_bytes() {
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    tiny(&root);
    let time = std::time::UNIX_EPOCH + Duration::from_millis(1700000000500);
    for (name, _) in FILES {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(model_dir(&root).join(name))
            .unwrap();
        file.set_times(fs::FileTimes::new().set_accessed(time).set_modified(time))
            .unwrap();
        assert_eq!(file.metadata().unwrap().modified().unwrap(), time);
    }
    let c = checker(&root);
    let Prepared::Stamp(intent) = engine::prepare(job(&c), Some(FILES.map(|v| v.1))).unwrap()
    else {
        panic!("stamp")
    };
    let _ready = intent.apply().unwrap();
    assert_eq!(
        fs::read_to_string(model_dir(&root).join("model.json")).unwrap(),
        reference()["stampBody"].as_str().unwrap()
    );
}

#[cfg(unix)]
#[test]
fn original_node_posix_binary64_boundaries_and_real_filesystem_observation() {
    let _serial = SERIAL.lock().unwrap();
    for case in reference()["posixCases"].as_array().unwrap() {
        let seconds = case["seconds"].as_i64().unwrap();
        let nanos = case["nanos"].as_i64().unwrap();
        assert_eq!(
            format!("{:016x}", engine::posix_millis(seconds, nanos).to_bits()),
            case["bits"].as_str().unwrap()
        );
    }
    let (_temp, root) = fixture();
    let file = fs::File::create(root.join("actual-time")).unwrap();
    let time = std::time::UNIX_EPOCH + Duration::new(1700000000, 499841);
    file.set_times(fs::FileTimes::new().set_modified(time))
        .unwrap();
    assert_eq!(
        file.metadata().unwrap().modified().unwrap(),
        time,
        "do not claim a timestamp the filesystem did not retain"
    );
    let observed = engine::values(&file.metadata().unwrap()).unwrap().1;
    assert_eq!(
        observed.to_bits(),
        engine::posix_millis(1700000000, 499841).to_bits()
    );
}

#[test]
fn accepted_worker_retains_job_when_its_caller_drops_without_fake_drain() {
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    let c = checker(&root);
    let accepted = job(&c);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let owner = std::thread::spawn(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let actual = accepted.prepare();
        let _ = done_tx.send(actual);
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    drop(done_rx);
    assert_eq!(
        c.retained(),
        1,
        "public observer drop is not disposal of actual accepted Job"
    );
    release_tx.send(()).unwrap();
    owner.join().unwrap();
    assert_eq!(
        c.retained(),
        0,
        "only actual worker/result disposal releases permit"
    );
}

#[test]
fn original_accepted_cancel_and_deadline_cannot_be_renewed_at_apply() {
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    tiny(&root);
    let c = checker(&root);
    let control = Control::default();
    let token = control.cancel.clone();
    let j = c.try_begin(query(), Platform::native(), control).unwrap();
    let Prepared::Stamp(s) = engine::prepare(j, Some(FILES.map(|v| v.1))).unwrap() else {
        panic!("stamp")
    };
    token.cancel();
    assert_eq!(err(s.apply()).code, "ai_status_cancelled");
    assert!(!model_dir(&root).join("model.json").exists());
    let Prepared::Stamp(mut s) = engine::prepare(job(&c), Some(FILES.map(|v| v.1))).unwrap() else {
        panic!("stamp")
    };
    s.job.control.deadline = Instant::now() - Duration::from_secs(1);
    assert_eq!(err(s.apply()).code, "ai_status_deadline");
    assert_eq!(c.retained(), 0);
    assert!(!model_dir(&root).join("model.json").exists());
}

#[test]
fn finite_read_operation_and_stamp_json_limits() {
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    tiny(&root);
    let limits = Limits {
        hash_bytes: 2,
        ..Limits::default()
    };
    let c = Checker::new(TrustedUserData::from_host(&root).unwrap(), limits).unwrap();
    assert_eq!(err(job(&c).prepare()).code, "ai_status_limit");
    drop(c);
    let limits = Limits {
        operations: 1,
        ..Limits::default()
    };
    let c = Checker::new(TrustedUserData::from_host(&root).unwrap(), limits).unwrap();
    assert_eq!(err(job(&c).prepare()).code, "ai_status_limit");
    fs::write(
        model_dir(&root).join("model.json"),
        vec![b' '; STAMP_BYTES + 1],
    )
    .unwrap();
    let c = checker(&root);
    assert_eq!(err(job(&c).prepare()).code, "ai_status_limit");
    for raw in [
        format!("{}0{}", "[".repeat(17), "]".repeat(17)),
        format!("[{}]", "0,".repeat(4096) + "0"),
    ] {
        assert_eq!(err(stamp::parse(raw.as_bytes())).code, "ai_status_limit");
    }
}

#[test]
fn raw_json_numbers_duplicate_keys_unknown_huge_exponents_and_unicode_gaps() {
    let _serial = SERIAL.lock().unwrap();
    let raw = format!(
        r#"{{"revision":"bad","revision":"{REVISION}","unknown":1e400,"files":{{"encoder.int8.onnx":{{"sha256":"{}","size":9007199254740993,"mtimeMs":1700000000000.499841}}}}}}"#,
        FILES[0].1
    );
    let s = stamp::parse(raw.as_bytes()).unwrap().unwrap();
    let (size, time) = s.entry(FILES[0].0, FILES[0].1).unwrap().numbers().unwrap();
    assert_eq!(size, 9007199254740992.);
    assert_eq!(
        time.to_bits(),
        "1700000000000.499841".parse::<f64>().unwrap().to_bits()
    );
    for raw in [b"{bad".as_slice(), b"null", b"[]", b"true"] {
        assert!(stamp::parse(raw).unwrap().is_none())
    }
    assert_eq!(
        err(stamp::parse(br#"{"x":"\ud800"}"#)).code,
        "ai_status_unsupported"
    );
    assert!(stamp::parse(br#"{"x":"\ud83d\ude00"}"#).is_ok());
}

#[test]
fn intel_mac_branch_never_touches_the_filesystem() {
    let _serial = SERIAL.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let c = checker(&temp.path().canonicalize().unwrap().join("does-not-exist"));
    let r = read(
        c.try_begin(query(), Platform::MacIntel, Control::default())
            .unwrap()
            .prepare()
            .unwrap(),
    );
    assert_eq!(
        serde_json::to_value(r.reply()).unwrap(),
        json!({"nemotron":{"ready":false,"error":"Nemotron transcription is not supported on Intel Mac"}})
    );
    assert!(r.evidence().stats.is_empty());
}

#[cfg(unix)]
#[test]
fn nonregular_probe_symlinks_and_actual_denied_read_are_explicit() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    tiny(&root);
    let c = checker(&root);
    let first = model_dir(&root).join(FILES[0].0);
    fs::remove_file(&first).unwrap();
    fs::create_dir(&first).unwrap();
    assert_eq!(err(job(&c).prepare()).code, "ai_status_unsupported");
    fs::remove_dir(&first).unwrap();
    let external = root.join("external");
    fs::write(&external, "real external source bytes").unwrap();
    symlink(&external, &first).unwrap();
    assert_eq!(
        serde_json::to_value(read(job(&c).prepare().unwrap()).reply()).unwrap()["nemotron"]
            ["error"],
        "The local model is outdated"
    );
    fs::set_permissions(&external, fs::Permissions::from_mode(0o0)).unwrap();
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(err(job(&c).prepare()).code, "ai_status_io")
    }
    fs::set_permissions(&external, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn actual_chunked_hash_cancellation_and_deadline_drop_retained_worker_state() {
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    tiny(&root);
    fs::write(model_dir(&root).join(FILES[0].0), vec![5; 131072]).unwrap();
    let c = checker(&root);
    let mut accepted = job(&c);
    accepted.after_chunk = Some(|j| j.control.cancel.cancel());
    assert_eq!(err(accepted.prepare()).code, "ai_status_cancelled");
    assert_eq!(c.retained(), 0);
    let mut accepted = job(&c);
    accepted.after_chunk = Some(|j| j.control.deadline = Instant::now() - Duration::from_secs(1));
    assert_eq!(err(accepted.prepare()).code, "ai_status_deadline");
    assert_eq!(c.retained(), 0);
    fn send_sync<T: Send + Sync>() {}
    send_sync::<StatusRead>();
    send_sync::<StampIntent>();
}
