use super::*;
use serde_json::json;
use std::{
    cell::RefCell,
    io::{Read, Write},
    process::{Command, Stdio},
};

fn oracle(input: Value) -> Value {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut child =
        Command::new(std::env::var_os("FREESHOW_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg(root.join("tests/parity/lib/device-id-oracle.mjs"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    let input = serde_json::to_vec(&input).unwrap();
    assert!(input.len() <= 32768);
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    // Read both pipes while the child runs: Windows anonymous pipes may fill
    // before child exit. These two bounded fixture readers are not native work.
    let (status, output, error) = std::thread::scope(|scope| {
        let output = scope.spawn(move || {
            let mut bytes = vec![];
            stdout.take(32769).read_to_end(&mut bytes).unwrap();
            bytes
        });
        let error = scope.spawn(move || {
            let mut bytes = vec![];
            stderr.take(8193).read_to_end(&mut bytes).unwrap();
            bytes
        });
        child.stdin.take().unwrap().write_all(&input).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                break None;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        (status, output.join().unwrap(), error.join().unwrap())
    });
    let status = status.expect("Bounded immutable Device ID oracle did not exit");
    assert!(error.len() <= 8192);
    assert!(
        status.success(),
        "Immutable source oracle failed: {}",
        String::from_utf8_lossy(&error)
    );
    assert!(output.len() <= 32768);
    serde_json::from_slice(&output).unwrap()
}
fn fixture(input: &Value) -> Value {
    let calls = RefCell::new(vec![]);
    let absent = input["configAbsent"] == true;
    let getter = !absent && input["getterAbsent"] != true;
    if getter && input["loadFailed"] != true {
        calls.borrow_mut().push("get");
    }
    let snapshot = if input["loadFailed"] == true || (getter && input["readFailed"] == true) {
        ConfigSnapshot::ReadFailed
    } else {
        ConfigSnapshot::Read {
            value: getter.then(|| input.get("value").cloned()).flatten(),
            can_set: !absent && input["setterAbsent"] != true,
        }
    };
    let prepared = prepare_with(
        snapshot,
        &JobControl::default(),
        |_| {
            calls.borrow_mut().push("legacy");
            if input["legacyFailed"] == true {
                Err(Failure::Legacy)
            } else {
                Ok(input["legacy"]
                    .as_str()
                    .unwrap_or("fixture-legacy-id")
                    .into())
            }
        },
        || {
            calls.borrow_mut().push("random");
            if input["randomFailed"] == true {
                Err(())
            } else {
                Ok(std::array::from_fn(|index| index as u8))
            }
        },
    )
    .unwrap();
    let mut writes = vec![];
    let result = if let Some(intent) = prepared.write_intent() {
        calls.borrow_mut().push("set");
        writes.push(json!({"key":intent.key(),"value":intent.value()}));
        if input["writeFailed"] == true {
            WriteResult::Failed
        } else {
            WriteResult::Committed
        }
    } else {
        WriteResult::NotRequired
    };
    json!({"reply":prepared.finish(result).unwrap(),"writes":writes,"calls":calls.into_inner()})
}

#[test]
fn actual_pinned_helper_preserves_stored_json_falsy_values_and_outer_failures() {
    let mut cases = vec![
        json!({}),
        json!({"loadFailed":true}),
        json!({"readFailed":true}),
        json!({"configAbsent":true}),
        json!({"getterAbsent":true}),
        json!({"setterAbsent":true}),
        json!({"legacyFailed":true}),
        json!({"legacyFailed":true,"randomFailed":true}),
        json!({"writeFailed":true}),
        json!({"legacyFailed":true,"writeFailed":true}),
    ];
    for value in [
        Value::Null,
        json!(false),
        json!(0),
        json!(-0.0),
        json!(""),
        json!(true),
        json!(42),
        json!("stored 🐟"),
        json!([]),
        json!({}),
        json!({"z":0,"2":"two","__proto__":{"own":true},"constructor":"data"}),
    ] {
        cases.push(
            json!({"value":value,"legacyFailed":true,"randomFailed":true,"writeFailed":true}),
        );
    }
    let expected = oracle(json!({"mode":"plans","cases":cases}));
    for (index, case) in cases.iter().enumerate() {
        assert_eq!(
            fixture(case),
            expected[index],
            "Controlled source case {index}"
        );
    }
}

#[test]
fn stored_values_never_touch_provider_random_or_write_and_write_failure_is_outer_fallback() {
    for value in [
        json!(true),
        json!(1),
        json!("stored"),
        json!([]),
        json!({"own":"value"}),
    ] {
        let prepared = prepare_with(
            ConfigSnapshot::Read {
                value: Some(value.clone()),
                can_set: true,
            },
            &JobControl::default(),
            |_| panic!("Stored identity must bypass legacy provider"),
            || panic!("Stored identity must bypass entropy"),
        )
        .unwrap();
        assert_eq!(prepared.origin(), Origin::Stored);
        assert!(prepared.write_intent().is_none());
        assert_eq!(prepared.finish(WriteResult::NotRequired).unwrap(), value);
    }
    let prepared = prepare_with(
        ConfigSnapshot::Read {
            value: None,
            can_set: true,
        },
        &JobControl::default(),
        |_| Ok("prepared-id".into()),
        || panic!("Successful legacy provider must bypass entropy"),
    )
    .unwrap();
    assert_eq!(prepared.write_intent().unwrap().key(), "machineId");
    assert_eq!(prepared.finish(WriteResult::Failed).unwrap(), FALLBACK);
}

#[test]
fn resource_cancellation_deadline_and_capability_failures_never_fabricate_identity() {
    let mut nested = json!(true);
    for _ in 0..9 {
        nested = json!([nested]);
    }
    for value in [
        json!("x".repeat(INPUT_BYTES + 1)),
        json!(vec![false; 257]),
        json!({"escape":"\n".repeat(4096)}),
        nested,
    ] {
        let error = prepare_with(
            ConfigSnapshot::Read {
                value: Some(value),
                can_set: true,
            },
            &JobControl::default(),
            |_| panic!("Bounded input must be checked first"),
            || panic!("No random fallback for resource rejection"),
        )
        .err()
        .unwrap();
        assert_eq!(error.code, "device_id_resource_limit");
    }
    let cancelled = JobControl::default();
    cancelled.cancel.cancel();
    assert_eq!(
        prepare(ConfigSnapshot::ReadFailed, &cancelled)
            .err()
            .unwrap()
            .code,
        "device_id_cancelled"
    );
    let expired = JobControl {
        deadline: Instant::now(),
        ..JobControl::default()
    };
    assert_eq!(
        prepare(ConfigSnapshot::ReadFailed, &expired)
            .err()
            .unwrap()
            .code,
        "device_id_deadline"
    );
    let control = JobControl::default();
    assert_eq!(
        prepare_with(
            ConfigSnapshot::Read {
                value: None,
                can_set: true
            },
            &control,
            |control| {
                control.cancel.cancel();
                Err(Failure::Legacy)
            },
            || panic!("Cancellation must precede fallback UUID generation")
        )
        .err()
        .unwrap()
        .code,
        "device_id_cancelled"
    );
    assert_eq!(
        prepare_with(
            ConfigSnapshot::Read {
                value: None,
                can_set: true
            },
            &JobControl::default(),
            |_| Err(Error::unsupported().into()),
            || panic!("Unsupported is not certified legacy failure")
        )
        .err()
        .unwrap()
        .code,
        "device_id_unsupported"
    );
    let prepared = prepare_with(
        ConfigSnapshot::ReadFailed,
        &JobControl::default(),
        |_| panic!("Read failure skips provider"),
        || panic!("Read failure skips entropy"),
    )
    .unwrap();
    assert_eq!(
        prepared.finish(WriteResult::Committed).unwrap_err().code,
        "device_id_commit"
    );
}

#[test]
fn actual_entropy_fallback_is_uuid_v4_and_is_not_generated_before_needed() {
    let run = || {
        prepare_with(
            ConfigSnapshot::Read {
                value: None,
                can_set: false,
            },
            &JobControl::default(),
            |_| Err(Failure::Legacy),
            || {
                let mut bytes = [0; 16];
                getrandom::fill(&mut bytes).map_err(|_| ())?;
                Ok(bytes)
            },
        )
        .unwrap()
        .finish(WriteResult::NotRequired)
        .unwrap()
    };
    let first = run();
    let second = run();
    let text = first.as_str().unwrap();
    assert!(first != second);
    assert_eq!(text.len(), 36);
    assert_eq!(&text[14..15], "4");
    assert!("89ab".contains(&text[19..20]));
    assert!(text.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-'));
}

#[test]
fn actual_dependency_expose_and_js_whitespace_are_certified_for_ascii_identity_tokens() {
    let whitespace = "\t\n\u{000b}\u{000c}\r \u{00a0}\u{1680}\u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";
    let mut cases = vec![];
    let mut expected = vec![];
    for (platform, kind) in [
        ("linux", Platform::Linux),
        ("win32", Platform::Windows),
        ("darwin", Platform::Mac),
    ] {
        for token in ["", "ABcd-0123", "A\0B", "REG_SZ", "x=\"Y\"", "Hello.Local"] {
            let output = match kind {
                Platform::Linux => format!("{whitespace}{token}{whitespace}"),
                Platform::Windows => format!("header REG_SZ {whitespace}{token}{whitespace}"),
                Platform::Mac => {
                    format!("header IOPlatformUUID = \"{token}\"{whitespace}\nignored")
                }
            };
            // Transport synthetic bytes as integer array; never native IDs.
            cases.push(json!({"platform":platform,"bytes":output.as_bytes()}));
            expected.push(digest(kind, output.as_bytes()).unwrap());
        }
    }
    let script_input = json!({"mode":"exposeBytes","cases":cases});
    let actual = oracle(script_input);
    for (index, expected) in expected.iter().enumerate() {
        assert_eq!(actual[index]["ok"], true);
        assert_eq!(actual[index]["hash"], *expected);
    }
    assert!(matches!(
        digest(Platform::Windows, b"no type marker"),
        Err(Failure::Legacy)
    ));
    assert!(matches!(
        digest(Platform::Mac, b"no property"),
        Err(Failure::Legacy)
    ));
    for bytes in ["not-certified-İ".as_bytes(), &[0xff][..]] {
        assert!(matches!(
            digest(Platform::Linux, bytes),
            Err(Failure::Capability(Error {
                code: "device_id_unsupported",
                ..
            }))
        ));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn real_private_files_match_original_cat_concat_head_and_hostname_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("hostname a's α");
    let b = dir.path().join("b's machine-id");
    let cases = [
        (Some("ABCD\nsecond"), Some("wrong\n")),
        (Some("AB"), Some("CD\n")),
        (Some(""), Some("BC\n")),
        (Some("AB"), None),
        (None, None),
        (Some(""), Some("")),
        (Some("\n"), None),
        (Some("A\u{feff} \t"), Some("BC\n")),
    ];
    for (first, second) in cases {
        for (path, bytes) in [(&a, first), (&b, second)] {
            let _ = std::fs::remove_file(path);
            if let Some(bytes) = bytes {
                std::fs::write(path, bytes).unwrap();
            }
        }
        let output = linux_bytes([&a, &b], &JobControl::default(), || {
            Ok(b"Captured-Host".to_vec())
        })
        .unwrap();
        let expected =
            oracle(json!({"mode":"linuxFiles","paths":[a,b],"hostname":"Captured-Host"}));
        assert_eq!(digest(Platform::Linux, &output).unwrap(), expected["hash"]);
    }
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    std::fs::create_dir(&a).unwrap();
    let output = linux_bytes([&a, &b], &JobControl::default(), || {
        Ok(b"Captured-Host".to_vec())
    })
    .unwrap();
    assert_eq!(
        digest(Platform::Linux, &output).unwrap(),
        oracle(json!({"mode":"linuxFiles","paths":[a,b],"hostname":"Captured-Host"}))["hash"]
    );
    std::fs::remove_dir(&a).unwrap();
    std::fs::write(&b, b"linked-native-source\n").unwrap();
    std::os::unix::fs::symlink(&b, &a).unwrap();
    let output = linux_bytes([&a, &b], &JobControl::default(), || {
        panic!("First real line must not need hostname")
    })
    .unwrap();
    assert_eq!(
        digest(Platform::Linux, &output).unwrap(),
        oracle(json!({"mode":"linuxFiles","paths":[a,b],"hostname":"unused"}))["hash"]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn native_linux_nonregular_and_oversized_sources_are_explicit_not_invented_ids() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::write(&a, vec![b'x'; PROVIDER_BYTES + 1]).unwrap();
    assert!(matches!(
        linux_bytes([&a, &b], &JobControl::default(), || panic!(
            "Resource failure cannot become hostname success"
        )),
        Err(Failure::Capability(Error {
            code: "device_id_resource_limit",
            ..
        }))
    ));
    std::fs::remove_file(&a).unwrap();
    let path = std::ffi::CString::new(a.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(matches!(
        linux_bytes([&a, &b], &JobControl::default(), || panic!(
            "Unsupported pipe cannot become hostname success"
        )),
        Err(Failure::Capability(Error {
            code: "device_id_unsupported",
            ..
        }))
    ));
}

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
#[test]
fn actual_native_legacy_matches_installed_dependency_without_outputting_stable_id() {
    let mut salt = [0u8; 32];
    getrandom::fill(&mut salt).unwrap();
    let salt_hex = salt
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let prepared = prepare(
        ConfigSnapshot::Read {
            value: None,
            can_set: true,
        },
        &JobControl::default(),
    )
    .unwrap();
    assert_eq!(
        prepared.origin(),
        Origin::Legacy,
        "Native OS must really obtain the legacy identity"
    );
    let expected = oracle(json!({"mode":"nativeSalted","salt":salt_hex}));
    assert!(
        expected["ok"] == true,
        "Actual pinned dependency failed on this native OS"
    );
    let mut hash = Sha256::new();
    hash.update(salt);
    hash.update(prepared.write_intent().unwrap().value());
    let observed = format!("{:x}", hash.finalize());
    assert!(
        expected["digest"].as_str() == Some(&observed),
        "Actual native identity differs from the pinned dependency (values withheld)"
    );
    prepared.finish(WriteResult::Committed).unwrap();
}
