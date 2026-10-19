//! Private bounded typed-fixture driver, never renderer authorization or parser proof.
use base64::{engine::general_purpose::STANDARD, Engine};
use freeshow_audio::CommonValue;
use freeshow_metadata_wire::{build, PictureRef};
use serde::Serialize;
use serde_json::Value;
use std::io::{self, Read, Write};

fn native(value: &Value, nodes: &mut usize, depth: usize) -> Result<CommonValue, &'static str> {
    *nodes += 1;
    if *nodes > 65536 || depth > 17 {
        return Err("typed fixture limit");
    }
    let a = value.as_array().ok_or("typed fixture tuple")?;
    let k = a
        .first()
        .and_then(Value::as_str)
        .ok_or("typed fixture kind")?;
    Ok(match k {
        "undefined" if a.len() == 1 => CommonValue::Undefined,
        "null" if a.len() == 1 => CommonValue::Null,
        "boolean" if a.len() == 2 => CommonValue::Bool(a[1].as_bool().ok_or("boolean")?),
        "string" if a.len() == 2 => CommonValue::String(a[1].as_str().ok_or("string")?.into()),
        "number" if a.len() == 2 => CommonValue::Number(match a[1].as_str() {
            Some("NaN") => f64::NAN,
            Some("Infinity") => f64::INFINITY,
            Some("-Infinity") => f64::NEG_INFINITY,
            Some("-0") => -0.0,
            _ => a[1].as_f64().ok_or("number")?,
        }),
        "array" if a.len() == 2 => CommonValue::Array(
            a[1].as_array()
                .ok_or("array")?
                .iter()
                .map(|v| native(v, nodes, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        "object" if a.len() == 2 => CommonValue::Object(
            a[1].as_array()
                .ok_or("fields")?
                .iter()
                .map(|v| {
                    let p = v.as_array().ok_or("pair")?;
                    if p.len() != 2 {
                        return Err("pair");
                    }
                    Ok((
                        p[0].as_str().ok_or("key")?.into(),
                        native(&p[1], nodes, depth + 1)?,
                    ))
                })
                .collect::<Result<_, _>>()?,
        ),
        _ => return Err("invalid typed fixture"),
    })
}
struct Output<W> {
    inner: W,
    bytes: usize,
}
impl<W: Write> Write for Output<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if b.len() > 32 * 1024 * 1024 - self.bytes {
            return Err(io::Error::other("driver output limit"));
        }
        let n = self.inner.write(b)?;
        self.bytes += n;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
#[derive(Serialize)]
struct Success {
    ok: bool,
    wire: freeshow_metadata_wire::MetadataWire,
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut text = String::new();
    io::stdin()
        .take(16 * 1024 * 1024 + 1)
        .read_to_string(&mut text)?;
    if text.len() > 16 * 1024 * 1024 {
        return Err("driver input limit".into());
    }
    let cases: Vec<Value> = serde_json::from_str(&text)?;
    if cases.len() > 16 {
        return Err("driver case limit".into());
    }
    let stdout = io::stdout();
    let mut output = Output {
        inner: stdout.lock(),
        bytes: 0,
    };
    output.write_all(b"[")?;
    for (i, case) in cases.into_iter().enumerate() {
        if i > 0 {
            output.write_all(b",")?;
        }
        let mut nodes = 0;
        let common = native(&case["common"], &mut nodes, 0)?;
        let list = case["pictures"].as_array().ok_or("pictures")?;
        if list.len() > 9 {
            return Err("fixture picture count".into());
        }
        let mut owned = Vec::new();
        for p in list {
            let fields = native(&p["fields"], &mut nodes, 0)?;
            let encoded = p["base64"].as_str().ok_or("picture base64")?;
            if encoded.len() > 5_594_000 {
                return Err("fixture byte limit".into());
            }
            let bytes = STANDARD.decode(encoded)?;
            owned.push((fields, bytes));
        }
        let pictures: Vec<_> = owned
            .iter()
            .map(|(fields, bytes)| PictureRef { fields, bytes })
            .collect();
        match build(
            case["filePath"].as_str().ok_or("filePath")?,
            &common,
            &pictures,
        ) {
            Ok(wire) => serde_json::to_writer(&mut output, &Success { ok: true, wire })?,
            Err(e) => serde_json::to_writer(
                &mut output,
                &serde_json::json!({"ok":false,"error":{"code":e.code(),"message":e.to_string()}}),
            )?,
        }
    }
    output.write_all(b"]\n")?;
    Ok(())
}
