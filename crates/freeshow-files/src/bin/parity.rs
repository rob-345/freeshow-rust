//! Line-delimited differential probe. This is not the desktop IPC entrypoint.
use freeshow_files::{paths, Error, FilesService, JobControl, Limits, LocateRequest, MediaRoot};
use serde_json::{json, Value};
use std::{
    future::Future,
    io::{self, BufRead, Read},
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    time::{Duration, Instant},
};

struct ThreadWake(std::thread::Thread);
impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}
fn wait<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut Context::from_waker(&waker)) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error {
        code: "files_invalid".into(),
        message: message.into(),
        thumbnail_invalidations: Vec::new(),
        diagnostics: Vec::new(),
    }
}

fn execute(input: &Value) -> Result<Value, Error> {
    match input["op"].as_str().unwrap_or("locate") {
        "batch" => {
            let cases = input["cases"]
                .as_array()
                .ok_or_else(|| invalid("cases must be an array"))?;
            if cases.len() > 10_000 {
                return Err(invalid("Too many probe cases"));
            }
            Ok(Value::Array(
                cases
                    .iter()
                    .map(|case| match execute(case) {
                        Ok(value) => json!({"ok":true,"value":value}),
                        Err(error) => json!({"ok":false,"error":error}),
                    })
                    .collect(),
            ))
        }
        "helpers" => {
            let text = input["path"].as_str().unwrap_or("");
            let windows = input["windows"].as_bool().unwrap_or(cfg!(windows));
            let normalized = paths::normalize_local_path_for(text, windows);
            let parent = paths::dirname_for(text, windows);
            Ok(
                json!({"normalized":normalized,"hash":paths::file_path_hash(text),"basename":paths::basename_for(text,windows),"dirname":parent,"parentId":format!("{}_{}",paths::basename_for(&parent,windows),paths::file_path_hash(&parent)),"folded":paths::normalized_file_name(text)}),
            )
        }
        "join" => {
            let parts = input["parts"]
                .as_array()
                .ok_or_else(|| invalid("parts missing"))?
                .iter()
                .map(|part| {
                    part.as_str()
                        .ok_or_else(|| invalid("join parts must be strings"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!(paths::join_for(
                &parts,
                input["windows"].as_bool().unwrap_or(cfg!(windows))
            )))
        }
        "foldRange" => {
            let start = input["start"]
                .as_u64()
                .ok_or_else(|| invalid("start missing"))?;
            let end = input["end"]
                .as_u64()
                .ok_or_else(|| invalid("end missing"))?;
            if start > end || end > 0x110000 || end - start > 8192 {
                return Err(invalid("Invalid Unicode scalar range"));
            }
            Ok(json!((start..end)
                .filter_map(|cp| char::from_u32(cp as u32))
                .map(|character| paths::normalized_file_name(&character.to_string()))
                .collect::<Vec<_>>()))
        }
        operation @ ("locate" | "folder") => {
            let mut limits = Limits::default();
            for (field, value) in [
                (&mut limits.max_entries, &input["limits"]["entries"]),
                (&mut limits.max_directories, &input["limits"]["directories"]),
                (&mut limits.max_operations, &input["limits"]["operations"]),
                (&mut limits.max_work_bytes, &input["limits"]["workBytes"]),
            ] {
                if let Some(value) = value.as_u64() {
                    *field = value.try_into().map_err(|_| invalid("Limit too large"))?;
                }
            }
            let root: MediaRoot = if input.get("root").is_some() {
                serde_json::from_value(input["root"].clone())
                    .map_err(|error| invalid(error.to_string()))?
            } else {
                MediaRoot::from_host(
                    input["configured"].as_str(),
                    input["dataRoot"].as_str().unwrap_or(""),
                )
            };
            let service = FilesService::start(limits)?;
            let mut control = JobControl::default();
            if let Some(ms) = input["deadlineMs"].as_u64() {
                control.deadline = Instant::now() + Duration::from_millis(ms.min(60_000));
            }
            if input["cancelled"] == true {
                control.cancel.cancel();
            }
            let result = if operation == "folder" {
                wait(service.media_folder_path(root, control)).map(|result| json!(result))
            } else {
                let request: Result<LocateRequest, _> =
                    serde_json::from_value(input["request"].clone());
                match request {
                    Ok(request) => {
                        wait(service.locate(request, root, control)).map(|result| json!(result))
                    }
                    Err(error) => Err(invalid(error.to_string())),
                }
            };
            wait(service.shutdown())?;
            result
        }
        _ => Err(invalid("Unknown probe operation")),
    }
}

fn main() {
    let mut stdin = io::stdin().lock();
    loop {
        let mut line = String::new();
        let read = (&mut stdin).take(16 * 1024 * 1024 + 1).read_line(&mut line);
        if matches!(read, Ok(0)) {
            break;
        }
        let result = if read.is_err() || line.len() > 16 * 1024 * 1024 {
            Err(invalid("Probe input exceeds UTF-8/byte bounds"))
        } else {
            serde_json::from_str::<Value>(&line)
                .map_err(|error| invalid(error.to_string()))
                .and_then(|input| execute(&input))
        };
        println!(
            "{}",
            match result {
                Ok(value) => json!({"ok":true,"value":value}),
                Err(error) => json!({"ok":false,"error":error}),
            }
        );
        if read.is_err() || line.len() > 16 * 1024 * 1024 {
            break;
        }
    }
}
