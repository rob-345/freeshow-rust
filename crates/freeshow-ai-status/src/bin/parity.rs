//! Private fixture driver, not a renderer/native grant interface.
use freeshow_ai_status::*;
use serde_json::json;
use std::{
    io::{Read, Write},
    path::PathBuf,
};
fn run() -> Result<serde_json::Value> {
    let args: Vec<_> = std::env::args_os().skip(1).take(3).collect();
    let root = PathBuf::from(args.first().ok_or(Error {
        code: "fixture",
        message: "root required",
    })?);
    let platform = if args.get(1).is_some_and(|s| s == "intel-mac") {
        Platform::MacIntel
    } else {
        Platform::native()
    };
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| Error {
            code: "fixture",
            message: "stdin failed",
        })?;
    if bytes.len() > 65536 {
        return Err(Error {
            code: "fixture",
            message: "stdin bound",
        });
    }
    let payload = serde_json::from_slice(&bytes).map_err(|_| Error {
        code: "fixture",
        message: "invalid JSON",
    })?;
    let checker = Checker::new(TrustedUserData::from_host(root)?, Limits::default())?;
    let result = checker
        .try_begin(
            NemotronNoModel::from_payload(&payload)?,
            platform,
            Control::default(),
        )?
        .prepare()?;
    let read = match result {
        Prepared::Ready(r) => r,
        Prepared::Stamp(s) => s.apply()?,
    };
    Ok(
        json!({"ok":true,"value":read.reply(),"evidence":read.evidence(),"writeDiagnostic":read.write_diagnostic()}),
    )
}
fn main() {
    let output = match run() {
        Ok(v) => v,
        Err(e) => json!({"ok":false,"error":e}),
    };
    let bytes = serde_json::to_vec(&output).expect("bounded result");
    std::io::stdout().write_all(&bytes).expect("stdout");
}
