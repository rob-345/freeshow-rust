//! Bounded native path validation; metadata functions run only on worker threads.
use serde_json::{json, Value};
use std::{path::PathBuf, time::UNIX_EPOCH};

pub const MAX_FILES: usize = 256;
pub const MAX_PATH_BYTES: usize = 32 * 1024;
pub const MAX_TOTAL_PATH_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub struct Gesture<T> {
    pub sequence: u64,
    pub context: T,
}

pub struct DragSession<T> {
    pub sequence: u64,
    active: Option<Gesture<T>>,
}

impl<T: Clone> DragSession<T> {
    pub fn new() -> Self {
        Self {
            sequence: 0,
            active: None,
        }
    }
    pub fn enter(&mut self, context: T) {
        self.sequence += 1;
        self.active = Some(Gesture {
            sequence: self.sequence,
            context,
        });
    }
    pub fn current(&self) -> Option<Gesture<T>> {
        self.active.clone()
    }
    pub fn leave(&mut self) {
        self.active = None;
    }
    pub fn accept_drop(&mut self) -> Option<Gesture<T>> {
        self.active.take()
    }
    // Leave after Drop is ordinary platform cleanup. It must not cancel the
    // metadata job for an already accepted drop. A new Enter supersedes it.
    pub fn accepts_completion(&self, sequence: u64) -> bool {
        self.sequence == sequence
    }
}

pub fn path_descriptors(paths: &[PathBuf]) -> Result<Vec<Value>, String> {
    if paths.is_empty() || paths.len() > MAX_FILES {
        return Err(format!(
            "A native file drop must contain between 1 and {MAX_FILES} paths"
        ));
    }
    let mut total = 0;
    paths
        .iter()
        .map(|path| {
            if !path.is_absolute() {
                return Err("Native file-drop paths must be absolute".into());
            }
            let text = path
                .to_str()
                .ok_or("Non-Unicode native file paths are not supported by the renderer")?;
            if text.contains('\0') || text.len() > MAX_PATH_BYTES {
                return Err("Native file-drop path is invalid or too long".into());
            }
            total += text.len();
            if total > MAX_TOTAL_PATH_BYTES {
                return Err("Native file-drop payload is too large".into());
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(text);
            Ok(json!({"path":text,"name":name}))
        })
        .collect()
}

pub fn file_descriptors(paths: &[PathBuf]) -> Result<Vec<Value>, String> {
    let mut descriptors = path_descriptors(paths)?;
    for (file, path) in descriptors.iter_mut().zip(paths) {
        let metadata =
            std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
        if !metadata.is_dir() && !metadata.is_file() {
            return Err("Only files and directories can be dropped".into());
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|time| time.as_millis() as u64);
        file["directory"] = json!(metadata.is_dir());
        file["size"] = json!(metadata.len());
        file["lastModified"] = json!(modified);
    }
    Ok(descriptors)
}
