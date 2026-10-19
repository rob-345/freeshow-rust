//! Tests of the explicit fixture boundary, never real-model integrity proof.
use super::*;
use sha2::Digest;
fn receipt() -> Value {
    serde_json::from_str(include_str!("../tests/fixed-tiny-source.json")).unwrap()
}
fn fixed_times(root: &Path) {
    let time = std::time::UNIX_EPOCH + Duration::from_millis(1700000000500);
    for (name, _) in FILES {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(model_dir(root).join(name))
            .unwrap();
        file.set_times(fs::FileTimes::new().set_accessed(time).set_modified(time))
            .unwrap();
        assert_eq!(file.metadata().unwrap().modified().unwrap(), time);
    }
}
fn source_value(read: &StatusRead) -> Value {
    serde_json::to_value(read.reply()).unwrap()
}

#[test]
fn fixed_fixture_runs_real_hashes_then_actual_write_retains_lease_and_rejects_changed_bytes() {
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    tiny(&root);
    fixed_times(&root);
    let c = checker(&root);
    assert_eq!(
        source_value(&read(job(&c).prepare().unwrap())),
        receipt()["sourceUninjected"]
    );
    let Prepared::Stamp(intent) = job(&c).prepare_fixed_tiny_fixture().unwrap() else {
        panic!("must require actual stamp write")
    };
    assert!(!model_dir(&root).join("model.json").exists());
    assert_eq!(intent.job.hash_bytes, 75);
    assert_eq!(intent.job.evidence.hashed, FILES.map(|f| f.0));
    assert!(!intent.job.evidence.fast_path);
    assert_eq!(c.retained(), 1);
    let ready = intent.apply().unwrap();
    assert!(source_value(&ready)["nemotron"]["ready"].as_bool().unwrap());
    assert_eq!(c.retained(), 1);
    assert_eq!(
        fs::read_to_string(model_dir(&root).join("model.json")).unwrap(),
        receipt()["sourceInjectedStamp"]
    );
    drop(ready);
    assert_eq!(c.retained(), 0);
    for (name, _) in FILES {
        let mut changed = name.as_bytes().to_vec();
        changed[0] = b'X';
        fs::write(model_dir(&root).join(name), &changed).unwrap();
        fixed_times(&root);
        // Native normal source shortcut intentionally still accepts restored
        // size/mtime. The explicit harness must hash instead of reusing it.
        let ordinary = read(job(&c).prepare().unwrap());
        assert!(ordinary.evidence().fast_path);
        assert_eq!(
            source_value(&ordinary)["nemotron"]["ready"],
            receipt()["sourceChangedStampFastPath"]["nemotron"]["ready"]
        );
        drop(ordinary);
        let Prepared::Ready(rejected) = job(&c).prepare_fixed_tiny_fixture().unwrap() else {
            panic!("changed bytes must remain outdated")
        };
        assert_eq!(
            source_value(&rejected),
            receipt()["sourceChangedWithoutStamp"]
        );
        assert_eq!(rejected.evidence().hashed, FILES.map(|f| f.0));
        assert!(!rejected.evidence().fast_path);
        drop(rejected);
        fs::write(model_dir(&root).join(name), name).unwrap();
        fixed_times(&root);
    }
    // A known fixture digest at the wrong leaf cannot classify that leaf.
    fs::write(model_dir(&root).join(FILES[0].0), FILES[1].0).unwrap();
    fixed_times(&root);
    assert_eq!(
        source_value(&read(job(&c).prepare_fixed_tiny_fixture().unwrap())),
        receipt()["sourceChangedWithoutStamp"]
    );
    assert_eq!(c.retained(), 0);
}

#[test]
fn fixed_fixture_actual_write_failure_and_original_apply_control_are_preserved() {
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    tiny(&root);
    let c = checker(&root);
    fs::create_dir(model_dir(&root).join("model.json")).unwrap();
    let Prepared::Stamp(intent) = job(&c).prepare_fixed_tiny_fixture().unwrap() else {
        panic!("stamp")
    };
    let r = intent.apply().unwrap();
    assert_eq!(r.write_diagnostic(), Some(&WriteDiagnostic::Failed));
    assert_eq!(
        source_value(&r)["nemotron"]["ready"],
        receipt()["sourceInjectedWriteFailure"]["nemotron"]["ready"]
    );
    drop(r);
    fs::remove_dir(model_dir(&root).join("model.json")).unwrap();
    let control = Control::default();
    let cancel = control.cancel.clone();
    let accepted = c.try_begin(query(), Platform::native(), control).unwrap();
    let Prepared::Stamp(intent) = accepted.prepare_fixed_tiny_fixture().unwrap() else {
        panic!("stamp")
    };
    cancel.cancel();
    assert_eq!(err(intent.apply()).code, "ai_status_cancelled");
    assert!(!model_dir(&root).join("model.json").exists());
    let Prepared::Stamp(mut intent) = job(&c).prepare_fixed_tiny_fixture().unwrap() else {
        panic!("stamp")
    };
    intent.job.control.deadline = Instant::now() - Duration::from_secs(1);
    assert_eq!(err(intent.apply()).code, "ai_status_deadline");
    assert!(!model_dir(&root).join("model.json").exists());
    assert_eq!(c.retained(), 0);
}

#[test]
fn fixed_fixture_cannot_bypass_actual_hash_read_budget_or_during_read_cancellation() {
    let _serial = SERIAL.lock().unwrap();
    let (_temp, root) = fixture();
    tiny(&root);
    let c = Checker::new(
        TrustedUserData::from_host(&root).unwrap(),
        Limits {
            hash_bytes: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        err(job(&c).prepare_fixed_tiny_fixture()).code,
        "ai_status_limit"
    );
    assert_eq!(c.retained(), 0);
    let c = checker(&root);
    let mut accepted = job(&c);
    accepted.after_chunk = Some(|j| j.control.cancel.cancel());
    assert_eq!(
        err(accepted.prepare_fixed_tiny_fixture()).code,
        "ai_status_cancelled"
    );
    assert_eq!(c.retained(), 0);
    let source_receipt = receipt();
    for (index, (name, _)) in FILES.into_iter().enumerate() {
        let row = &source_receipt["fixedFiles"][index];
        assert_eq!(row["utf8"], name);
        assert_eq!(
            row["sha256"],
            format!("{:x}", sha2::Sha256::digest(name.as_bytes()))
        );
    }
}
