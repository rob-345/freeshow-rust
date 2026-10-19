use serde_json::Value;
use std::{
    io::{Read, Seek, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Reap(Child);
impl Drop for Reap {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
pub fn oracle(mode: &str, input: &Value) -> Value {
    let root = std::env::var_os("FREESHOW_BROWSE_MACOS_ORACLE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let executable =
        std::env::var_os("FREESHOW_BROWSE_MACOS_NODE").unwrap_or_else(|| "node".into());
    let bytes = serde_json::to_vec(input).unwrap();
    assert!(bytes.len() <= 65536);
    let mut stdin = tempfile::tempfile().unwrap();
    stdin.write_all(&bytes).unwrap();
    stdin.rewind().unwrap();
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut child = Reap(
        Command::new(executable)
            .arg(root.join("crates/freeshow-browse/tools/macos-source.mjs"))
            .arg(mode)
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout.try_clone().unwrap()))
            .stderr(Stdio::from(stderr.try_clone().unwrap()))
            .spawn()
            .expect("Pinned Node22.21.1 is required; source oracle is never silently skipped"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "bounded source oracle deadline; Drop kills/reaps actual child"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(stdout.metadata().unwrap().len() <= 1024 * 1024);
    assert!(stderr.metadata().unwrap().len() <= 65536);
    stdout.rewind().unwrap();
    stderr.rewind().unwrap();
    let mut output = String::new();
    stdout.read_to_string(&mut output).unwrap();
    let mut errors = String::new();
    stderr.read_to_string(&mut errors).unwrap();
    assert!(status.success(), "real source oracle failed: {errors}");
    serde_json::from_str(&output).unwrap()
}
pub fn assert_value(actual: &Value, expected: &Value, number_bits: &Value) {
    fn visit(
        actual: &Value,
        expected: &Value,
        path: &str,
        remaining: &mut serde_json::Map<String, Value>,
    ) {
        match (actual, expected) {
            (Value::Number(a), Value::Number(_)) => {
                // The expected JSON numeral establishes type only. Its parsed
                // decimal value is not source binary64 evidence.
                let source = remaining
                    .remove(path)
                    .unwrap_or_else(|| panic!("missing source numeric bits at {path}"));
                assert_eq!(
                    format!("{:016x}", a.as_f64().unwrap().to_bits()),
                    source.as_str().expect("source bits must be hex text"),
                    "exact source binary64 bits at {path}"
                );
            }
            (Value::Array(a), Value::Array(b)) => {
                assert_eq!(a.len(), b.len());
                for (index, (a, b)) in a.iter().zip(b).enumerate() {
                    visit(a, b, &format!("{path}/{index}"), remaining);
                }
            }
            (Value::Object(a), Value::Object(b)) => {
                assert_eq!(a.len(), b.len(), "all source properties must survive");
                for (key, value) in a {
                    let escaped = key.replace('~', "~0").replace('/', "~1");
                    visit(
                        value,
                        b.get(key)
                            .unwrap_or_else(|| panic!("missing source field {key}")),
                        &format!("{path}/{escaped}"),
                        remaining,
                    );
                }
            }
            _ => assert_eq!(actual, expected),
        }
    }
    let mut remaining = number_bits
        .as_object()
        .expect("source numeric bits must be an object")
        .clone();
    visit(actual, expected, "", &mut remaining);
    assert!(remaining.is_empty(), "unused source numeric bit evidence");
}

/// A shared device can be written by other processes between observations.
/// Changed timestamps must be bracketed by actual source observations. Every
/// unchanged number, and every other property, still requires exact source bits.
#[allow(
    dead_code,
    reason = "Shared support also serves pure projection unit tests"
)]
pub fn assert_live_device(actual: &Value, before: &Value, after: &Value) {
    let mut actual = actual.clone();
    let mut first = before["data"].clone();
    let mut last = after["data"].clone();
    let mut first_bits = before["numberBits"].clone();
    let mut last_bits = after["numberBits"].clone();
    for field in ["atimeMs", "mtimeMs", "ctimeMs", "birthtimeMs"] {
        let pointer = format!("/stat/{field}");
        let decode = |bits: &Value| {
            let text = bits[&pointer].as_str().expect("source timestamp bits");
            f64::from_bits(u64::from_str_radix(text, 16).expect("source hex bits"))
        };
        let low = decode(&first_bits);
        let high = decode(&last_bits);
        assert!(low.is_finite() && high.is_finite());
        assert!(first["stat"][field].is_number() && last["stat"][field].is_number());
        if low.to_bits() != high.to_bits() {
            let observed = actual["stat"][field]
                .as_f64()
                .expect("native numeric timestamp");
            assert!(
                observed.is_finite() && observed >= low.min(high) && observed <= low.max(high),
                "shared device {field} must be within actual source observations"
            );
            eprintln!("Shared device {field} changed between source observations; native value is bracketed");
            for value in [&mut actual, &mut first, &mut last] {
                value["stat"].as_object_mut().unwrap().remove(field);
            }
            first_bits.as_object_mut().unwrap().remove(&pointer);
            last_bits.as_object_mut().unwrap().remove(&pointer);
        }
    }
    // Detect replacement or any other changed device field rather than hiding
    // it behind the timestamp exception. No generic numeric tolerance or retry.
    // Source JSON decimals are type/shape evidence only. Restore the first
    // observation's exact captured bits before comparing it with the second.
    for (pointer, bits) in first_bits.as_object().unwrap() {
        let number = f64::from_bits(u64::from_str_radix(bits.as_str().unwrap(), 16).unwrap());
        let field = first.pointer_mut(pointer).expect("source numeric pointer");
        assert!(field.is_number());
        *field = Value::from(number);
    }
    assert_value(&first, &last, &last_bits);
    assert_value(&actual, &first, &first_bits);
}
