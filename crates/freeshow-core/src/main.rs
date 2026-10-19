use freeshow_core::{
    protocol::Request,
    runtime::{Runtime, IMPLEMENTED_MAIN},
    show, Error, Result, WireError,
};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
};

fn evaluate(input: Value, runtime: &mut Runtime) -> Result<Value> {
    Ok(match input["op"].as_str().unwrap_or("") {
        "capabilities" => json!({"main": IMPLEMENTED_MAIN}),
        "runtime.context" => json!(runtime.context()),
        "output.route" => {
            let endpoints: Vec<freeshow_core::output::Endpoint> =
                serde_json::from_value(input["endpoints"].clone())?;
            serde_json::to_value(freeshow_core::output::route(&input["message"], &endpoints)?)?
        }
        "output.bounds" => {
            let displays: Vec<freeshow_core::output::Display> =
                serde_json::from_value(input["displays"].clone())?;
            let main = serde_json::from_value(input["main"].clone())?;
            serde_json::to_value(freeshow_core::output::resolve_bounds(
                &input["output"],
                &displays,
                main,
                input["autoPosition"].as_bool().unwrap_or(false),
                input["isMac"].as_bool().unwrap_or(false),
            )?)?
        }
        "batch" => Value::Array(
            input["cases"]
                .as_array()
                .ok_or_else(|| Error::Invalid("cases must be an array".into()))?
                .iter()
                .map(|case| evaluate(case.clone(), runtime))
                .collect::<Result<Vec<_>>>()?,
        ),
        "show.parse" => show::parse_json(
            input["value"]
                .as_str()
                .ok_or_else(|| Error::Invalid("value must be a string".into()))?,
        )
        .unwrap_or(Value::Null),
        "show.trim" => show::trim(&input["value"]),
        "show.text" => json!(show::text_content(&input["value"])),
        "show.searchText" => json!(show::search_text(&input["value"])),
        "show.empty" => json!(show::is_empty(&input["value"])),
        "dialog.folder" => {
            let path = input["selected"].as_str().map(std::path::Path::new);
            serde_json::to_value(freeshow_core::dialog::folder_selected(
                &input["data"],
                path,
            )?)?
        }
        "dialog.files" => {
            let paths: Vec<PathBuf> = serde_json::from_value(input["selected"].clone())?;
            serde_json::to_value(freeshow_core::dialog::files_selected(
                &input["data"],
                &paths,
            )?)?
        }
        "backup.retention" => {
            let backups: Vec<freeshow_core::backup::Backup> =
                serde_json::from_value(input["backups"].clone())?;
            json!(freeshow_core::backup::retention_paths(
                &backups,
                input["cutoffMs"]
                    .as_i64()
                    .ok_or_else(|| Error::Invalid("cutoffMs must be an integer".into()))?,
                input["ignoreFloor"].as_bool().unwrap_or(false)
            ))
        }
        "bench.show.text" => {
            let corpus = input["corpus"]
                .as_array()
                .ok_or_else(|| Error::Invalid("corpus must be an array".into()))?;
            let rounds = input["rounds"].as_u64().unwrap_or(10).min(1000);
            let iterations = input["iterations"].as_u64().unwrap_or(10).min(10000);
            let mut samples = Vec::new();
            let mut checksum = 0usize;
            for round in 0..rounds + 3 {
                let start = std::time::Instant::now();
                let mut sum = 0usize;
                for _ in 0..iterations {
                    for value in corpus {
                        // UTF-16 length matches JavaScript's string.length.
                        sum = sum.wrapping_add(
                            std::hint::black_box(show::text_content(std::hint::black_box(value)))
                                .encode_utf16()
                                .count(),
                        );
                    }
                }
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                if round >= 3 {
                    samples.push(elapsed);
                }
                checksum = sum;
            }
            json!({"samplesMs":samples,"checksum":checksum})
        }
        "runtime" => {
            let mut request: Request = serde_json::from_value(input["request"].clone())?;
            // The legacy parity adapter supplies the current context. Explicit
            // contexts are preserved for stale-session/generation tests.
            if request.context.is_none() {
                request.context = Some(runtime.context());
            }
            serde_json::to_value(
                runtime.dispatch(request, input["window"].as_str().unwrap_or("main"))?,
            )?
        }
        op => return Err(Error::Unsupported(op.into())),
    })
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let option = |flag| {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|index| args.get(index + 1))
            .map(PathBuf::from)
    };
    // The headless test/benchmark runner always uses temporary isolated roots by
    // default. It is also useful for querying the Rust runtime without a GUI.
    let temp = tempfile::tempdir()?;
    let mut runtime = Runtime::open(
        option("--app-data").unwrap_or_else(|| temp.path().join("app")),
        option("--data-dir"),
    )?;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for line in io::stdin().lock().lines() {
        let line = line?;
        let result = serde_json::from_str(&line)
            .map_err(Error::from)
            .and_then(|input| evaluate(input, &mut runtime));
        let response = match result {
            Ok(value) => json!({"ok":true,"value":value}),
            Err(error) => json!({"ok":false,"error":WireError::from(error)}),
        };
        serde_json::to_writer(&mut output, &response)?;
        writeln!(&mut output)?;
        output.flush()?;
    }
    Ok(())
}
