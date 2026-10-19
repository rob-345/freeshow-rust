//! Private pure parity driver. Built in a temporary directory, never a sidecar.
#[path = "../../../crates/freeshow-browse/src/paths/windows.rs"]
mod windows;
use std::io::{BufRead, Read, Write};
fn hex(value: &str) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        write!(&mut result, "{byte:02x}").unwrap();
    }
    result
}
fn decode(value: &str) -> Option<String> {
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    let bytes = pairs
        .iter()
        .map(|pair| {
            let digit = |byte| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                _ => None,
            };
            Some(digit(pair[0])? * 16 + digit(pair[1])?)
        })
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}
fn response(line: &str) -> Option<String> {
    let fields = line
        .trim_end_matches(['\r', '\n'])
        .split('\t')
        .collect::<Vec<_>>();
    let error =
        |error: windows::PathError| format!("error\t{}\t{}", error.code, hex(error.message));
    match fields.as_slice() {
        ["P", path] => {
            let path = decode(path)?;
            Some(match windows::parse(&path) {
                Ok(parts) => {
                    let base = windows::basename(&path).unwrap();
                    let ext = windows::extension(&path).unwrap();
                    let name = windows::stem(&path).unwrap();
                    let dir = windows::parent(&path).unwrap();
                    assert_eq!(
                        (base, ext, name, dir),
                        (parts.base, parts.ext, parts.name, parts.dir)
                    );
                    format!(
                        "ok\t{}\t{}\t{}\t{}\t{}",
                        hex(parts.root),
                        hex(dir),
                        hex(base),
                        hex(ext),
                        hex(name)
                    )
                }
                Err(value) => error(value),
            })
        }
        ["J", parent, name] => Some(match windows::join(&decode(parent)?, &decode(name)?) {
            Ok(value) => format!("ok\t{}", hex(&value)),
            Err(value) => error(value),
        }),
        _ => None,
    }
}
fn main() {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::BufWriter::new(std::io::stdout().lock());
    loop {
        let mut line = String::new();
        let count = (&mut input).take(131_121).read_line(&mut line).unwrap();
        if count == 0 {
            break;
        }
        if count > 131_120 {
            writeln!(output, "error\tbrowse_resource_limit").unwrap();
            break;
        }
        writeln!(
            output,
            "{}",
            response(&line).unwrap_or_else(|| "error\tinvalid_request".into())
        )
        .unwrap();
    }
}
