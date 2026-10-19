use crate::{protocol::Event, Error, Result};
use serde_json::{json, Map, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn folder_selected(data: &Value, selected: Option<&Path>) -> Result<Vec<Event>> {
    let Some(path) = selected else {
        return Ok(Vec::new());
    };
    let channel = data["channel"]
        .as_str()
        .ok_or_else(|| Error::Invalid("folder picker channel must be a string".into()))?;
    Ok(vec![Event::reply(
        "MAIN",
        "OPEN_FOLDER2",
        json!({"channel":channel,"path":path}),
        None,
    )])
}

pub fn files_selected(data: &Value, selected: &[PathBuf]) -> Result<Vec<Event>> {
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let channel = data["channel"]
        .as_str()
        .ok_or_else(|| Error::Invalid("file picker channel must be a string".into()))?;
    let mut content = Map::new();
    if data["read"].as_bool().unwrap_or(false) {
        for path in selected {
            let text = fs::read(path)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            content.insert(path.to_string_lossy().into_owned(), json!(text));
        }
    }
    let mut response = json!({"channel":channel,"files":selected,"content":content});
    if let Some(id) = data.get("id") {
        response["id"] = id.clone();
    }
    Ok(vec![Event::reply("MAIN", "OPEN_FILE2", response, None)])
}
