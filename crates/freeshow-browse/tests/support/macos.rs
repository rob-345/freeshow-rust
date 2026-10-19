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
