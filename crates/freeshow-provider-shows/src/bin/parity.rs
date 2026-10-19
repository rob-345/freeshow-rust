//! Private bounded test driver; typed fixture input is not provider/IPC authority.
use freeshow_provider_shows::{self as shows, SourceValue as S};
use serde_json::{json, Value};
use std::io::{Read, Write};
fn decode(v: &Value, depth: usize, nodes: &mut usize) -> Result<S, String> {
    *nodes += 1;
    if depth > 16 || *nodes > 65536 {
        return Err("fixture budget".into());
    }
    let a = v.as_array().ok_or("fixture tuple")?;
    let tag = a.first().and_then(Value::as_str).ok_or("fixture tag")?;
    Ok(match tag {
        "u" => S::Undefined,
        "null" => S::Null,
        "b" => S::Bool(a.get(1).and_then(Value::as_bool).ok_or("fixture bool")?),
        "s" => S::String(
            a.get(1)
                .and_then(Value::as_str)
                .ok_or("fixture string")?
                .into(),
        ),
        "n" => S::Number(f64::from_bits(
            u64::from_str_radix(
                a.get(1).and_then(Value::as_str).ok_or("fixture number")?,
                16,
            )
            .map_err(|_| "fixture bits")?,
        )),
        "a" => S::Array(
            a.get(1)
                .and_then(Value::as_array)
                .ok_or("fixture array")?
                .iter()
                .map(|v| decode(v, depth + 1, nodes))
                .collect::<Result<_, _>>()?,
        ),
        "o" => {
            let mut out = Vec::new();
            for field in a.get(1).and_then(Value::as_array).ok_or("fixture object")? {
                let pair = field.as_array().ok_or("fixture pair")?;
                out.push((
                    pair.first()
                        .and_then(Value::as_str)
                        .ok_or("fixture key")?
                        .into(),
                    decode(pair.get(1).ok_or("fixture value")?, depth + 1, nodes)?,
                ));
            }
            S::Object(out)
        }
        _ => return Err("fixture tag".into()),
    })
}
fn describe(v: &S) -> Value {
    match v {
        S::Undefined => json!(["u"]),
        S::Null => json!(["null"]),
        S::Bool(b) => json!(["b", b]),
        S::String(s) => json!(["s", s]),
        S::Number(n) => json!(["n", format!("{:016x}", n.to_bits())]),
        S::Array(a) => json!(["a", a.iter().map(describe).collect::<Vec<_>>()]),
        S::Object(o) => json!([
            "o",
            o.iter()
                .map(|(k, v)| json!([k, describe(v)]))
                .collect::<Vec<_>>()
        ]),
    }
}
fn one(v: &Value) -> Result<Value, String> {
    let args = v["args"]
        .as_array()
        .ok_or("args")?
        .iter()
        .map(|a| decode(a, 0, &mut 0))
        .collect::<Result<Vec<_>, _>>()?;
    let uids = v["uids"]
        .as_array()
        .ok_or("uids")?
        .iter()
        .map(|s| s.as_str().ok_or("uid"))
        .collect::<Result<Vec<_>, _>>()?;
    let clock = v["clock"]
        .as_array()
        .ok_or("clock")?
        .iter()
        .map(|s| {
            u64::from_str_radix(s.as_str().ok_or("clock")?, 16)
                .map(f64::from_bits)
                .map_err(|_| "clock")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let env = shows::Environment {
        uids: &uids,
        now_millis: &clock,
    };
    let arg = |i| args.get(i).unwrap_or(&S::Undefined);
    if v["method"] == "parse" {
        return Ok(match shows::parse_lyrics(arg(0)) {
            Ok(value) => json!({"ok":true,"value":value}),
            Err(e) => json!({"ok":false,"error":e}),
        });
    }
    let result = match v["method"].as_str() {
        Some("empty") => shows::create_empty_show(arg(0), arg(1), arg(2), arg(3), env),
        Some("song") => shows::create_song_show(arg(0), arg(1), arg(2), arg(3), env),
        Some("media") => {
            let S::Array(files) = arg(1) else {
                return Err("files".into());
            };
            shows::create_media_show(arg(0), files, env)
        }
        _ => return Err("method".into()),
    };
    Ok(match result {
        Ok(v) => {
            json!({"ok":true,"typed":describe(&v.value),"json":v.value.json_projection().map_err(|e|e.to_string())?,"uidCalls":v.uid_calls,"nowCalls":v.now_calls,"dateCalls":v.date_calls,"workBytes":v.work_bytes,"operations":v.operations})
        }
        Err(e) => json!({"ok":false,"error":e}),
    })
}
fn main() {
    let result = (|| -> Result<Value, String> {
        let mut input = String::new();
        std::io::stdin()
            .take(16 * 1024 * 1024 + 1)
            .read_to_string(&mut input)
            .map_err(|e| e.to_string())?;
        if input.len() > 16 * 1024 * 1024 {
            return Err("fixture input limit".into());
        }
        let value: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let cases = value.as_array().ok_or("cases")?;
        if cases.len() > 128 {
            return Err("fixture cases".into());
        }
        Ok(Value::Array(
            cases.iter().map(one).collect::<Result<_, _>>()?,
        ))
    })();
    let output = match result {
        Ok(v) => v,
        Err(e) => json!({"driverError":e}),
    };
    let bytes = serde_json::to_vec(&output).unwrap();
    if bytes.len() > 32 * 1024 * 1024 {
        eprintln!("fixture output limit");
        std::process::exit(1);
    }
    std::io::stdout().write_all(&bytes).unwrap();
}
