//! JSON-lines trusted test boundary; the production planner has no IPC API.
use freeshow_asset_plans::*;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    io::{self, BufRead, Read, Write},
    path::PathBuf,
};

fn invalid() -> Error {
    Error {
        code: "asset_plan_shape",
        field: "test_host",
        message: "Invalid trusted test input",
    }
}
fn limits(value: &Value) -> Limits {
    let mut limits = Limits::default();
    for (target, name) in [
        (&mut limits.shows, "shows"),
        (&mut limits.container_entries, "containerEntries"),
        (&mut limits.containers, "containers"),
        (&mut limits.operations, "operations"),
        (&mut limits.references, "references"),
        (&mut limits.path_bytes, "pathBytes"),
        (&mut limits.input_path_bytes, "inputPathBytes"),
        (&mut limits.candidates, "candidates"),
        (&mut limits.candidate_bytes, "candidateBytes"),
        (&mut limits.diagnostics, "diagnostics"),
        (&mut limits.diagnostic_bytes, "diagnosticBytes"),
    ] {
        if let Some(value) = value[name].as_u64() {
            *target = value.try_into().unwrap_or(usize::MAX);
        }
    }
    limits
}
fn execute(input: &Value) -> Result<Value, Error> {
    let planner = Planner::new(limits(&input["limits"]))?;
    let context: Context =
        serde_json::from_value(input["context"].clone()).map_err(|_| invalid())?;
    let protected: HashSet<_> = input["protected"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let unknown: HashSet<_> = input["unclassified"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let classifier = |path: &str| {
        if protected.contains(path) {
            Some(Protection::Protected)
        } else if unknown.contains(path) {
            None
        } else {
            Some(Protection::Plain)
        }
    };
    let op = input["op"].as_str().ok_or_else(invalid)?;
    let result = match op {
        "show" => planner.plan(Committed::show(&context, &input["data"], &classifier))?,
        "show_reply" => {
            planner.plan(Committed::show_reply(&context, &input["data"], &classifier))?
        }
        "save" => planner.plan(Committed::save_snapshot(
            &context,
            &input["data"],
            &classifier,
        ))?,
        _ => {
            let files = input["files"].as_array().ok_or_else(invalid)?;
            if files.len() > 32768 {
                return Err(invalid());
            }
            let paths: Vec<_> = files
                .iter()
                .map(|file| file["path"].as_str().map(PathBuf::from).ok_or_else(invalid))
                .collect::<Result<_, _>>()?;
            let source: Vec<_> = files
                .iter()
                .zip(&paths)
                .map(|(file, path)| {
                    let source = file["sourcePath"]
                        .as_str()
                        .unwrap_or_else(|| path.to_str().unwrap());
                    let protection = match file["protection"].as_str() {
                        Some("plain") => Some(Protection::Plain),
                        Some("protected") => Some(Protection::Protected),
                        _ => None,
                    };
                    AcceptedPath::new(source, path, protection)
                })
                .collect();
            match op {
                "selection" => planner.plan(AcceptedNative::selection(&context, &source))?,
                "import" => planner.plan(AcceptedNative::committed_imports(&context, &source))?,
                "locate" => {
                    if source.len() > 1 {
                        return Err(invalid());
                    }
                    planner.plan(AcceptedNative::located(&context, source.first().copied()))?
                }
                "thumbnail" => {
                    if source.len() != 1 {
                        return Err(invalid());
                    }
                    planner.plan(AcceptedNative::generated_thumbnail(&context, source[0]))?
                }
                "drop" => {
                    let files: Vec<_> = files
                        .iter()
                        .zip(&source)
                        .map(|(file, path)| DroppedFile {
                            path: *path,
                            is_directory: file["directory"] == true,
                        })
                        .collect();
                    planner.plan(AcceptedNative::drop_files(&context, &files))?
                }
                "browse" => {
                    let thumbnail_paths: Vec<_> = files
                        .iter()
                        .map(|file| {
                            file["thumbnailPath"]
                                .as_str()
                                .filter(|path| !path.is_empty())
                                .map(PathBuf::from)
                        })
                        .collect();
                    let records: Vec<_> = files
                        .iter()
                        .zip(&source)
                        .zip(&thumbnail_paths)
                        .map(|((file, path), thumbnail)| BrowsedFile {
                            path: *path,
                            is_folder: file["isFolder"] == true,
                            thumbnail: thumbnail.as_ref().map(|path| {
                                AcceptedPath::new(
                                    path.to_str().unwrap(),
                                    path,
                                    Some(Protection::Plain),
                                )
                            }),
                        })
                        .collect();
                    planner.plan(AcceptedNative::browse(&context, &records))?
                }
                _ => return Err(invalid()),
            }
        }
    };
    Ok(
        json!({"context":result.context,"grants":result.grants.iter().map(|grant| json!({"sourcePath":grant.source_path,"path":grant.path,"provenance":format!("{:?}",grant.provenance),"protection":format!("{:?}",grant.protection)})).collect::<Vec<_>>(),"diagnostics":result.diagnostics,"stats":result.stats}),
    )
}
fn main() {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut line = String::new();
        let read = (&mut input).take(4 * 1024 * 1024 + 1).read_line(&mut line);
        if matches!(read, Ok(0)) {
            break;
        }
        let result = if read.is_err() || line.len() > 4 * 1024 * 1024 {
            Err(invalid())
        } else {
            serde_json::from_str::<Value>(&line)
                .map_err(|_| invalid())
                .and_then(|value| execute(&value))
        };
        let value = match result {
            Ok(value) => json!({"ok":true,"value":value}),
            Err(error) => json!({"ok":false,"error":error}),
        };
        if writeln!(output, "{value}")
            .and_then(|_| output.flush())
            .is_err()
        {
            break;
        }
        if read.is_err() || line.len() > 4 * 1024 * 1024 {
            break;
        }
    }
}
