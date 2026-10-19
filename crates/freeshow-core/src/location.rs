//! Select a data library without moving or deleting the previous library.
use crate::{
    storage::{self, Stores, STORE_IDS},
    Error, Result,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Explicit overrides take precedence. Otherwise reopen the fork's own saved
/// library; a missing selected volume is an error rather than a fresh library.
pub fn startup_path(app_data: &Path, override_path: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = override_path {
        return Ok(path);
    }
    let config = match fs::read(app_data.join("config.json")) {
        Ok(content) => serde_json::from_slice::<Value>(&content)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(error) => return Err(error.into()),
    };
    match config.get("dataPath").filter(|value| !value.is_null()) {
        None => Ok(app_data.join("Data")),
        Some(Value::String(path)) if !path.is_empty() => {
            let path = PathBuf::from(path);
            if !path.is_absolute() || !path.is_dir() {
                return Err(Error::Invalid(format!(
                    "selected data folder is unavailable: {}",
                    path.display()
                )));
            }
            Ok(path)
        }
        _ => Err(Error::Invalid(
            "configured dataPath must be an absolute folder path".into(),
        )),
    }
}

/// Existing destination values win. Missing portable stores are copied from the
/// current library before defaults are applied. Shows/media stay in each library,
/// as in the upstream data-root selector. Config is committed last.
pub fn select(
    stores: &mut Stores,
    new_path: &Path,
    old_path: &Path,
) -> Result<Vec<(String, Value)>> {
    if !new_path.is_absolute() || !old_path.is_absolute() {
        return Err(Error::Invalid("data folder paths must be absolute".into()));
    }
    if fs::canonicalize(old_path)? != fs::canonicalize(&stores.data)? {
        return Err(Error::Invalid(
            "the active data folder changed; select it again".into(),
        ));
    }
    let new_path = fs::canonicalize(new_path)?;
    if new_path == fs::canonicalize(&stores.data)? {
        return Ok(Vec::new());
    }
    // The root is explicitly selected, but managed subfolders/files must not
    // redirect writes through symlinks into an unrelated library.
    for folder in ["Config", "Shows", "Bibles"] {
        let directory = new_path.join(folder);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                return Err(Error::Invalid(format!(
                    "data subfolder must be a regular directory: {}",
                    directory.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&directory)?
            }
            Err(error) => return Err(error.into()),
        }
    }
    let target = Stores {
        app_data: stores.app_data.clone(),
        data: new_path.clone(),
    };
    let mut changes = Vec::new();
    let mut values = Vec::new();
    for id in STORE_IDS.iter().copied().filter(|id| storage::portable(id)) {
        let destination = target.path(id)?;
        let original = match fs::symlink_metadata(&destination) {
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err(Error::Invalid(format!(
                    "store destination must be a regular file: {}",
                    destination.display()
                )));
            }
            Ok(_) => Some(serde_json::from_slice::<Value>(&fs::read(&destination)?)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let stored = match &original {
            Some(value) => value.clone(),
            None => stores.get(id)?,
        };
        let object = stored
            .as_object()
            .ok_or_else(|| Error::Invalid(format!("store {id} must be an object")))?;
        let mut value = storage::defaults(id)?;
        value.as_object_mut().unwrap().extend(object.clone());
        if original.as_ref() != Some(&value) && (original.is_some() || value != json!({})) {
            let content = if storage::definitions()[id]["minify"]
                .as_bool()
                .unwrap_or(false)
            {
                serde_json::to_vec(&value)?
            } else {
                serde_json::to_vec_pretty(&value)?
            };
            changes.push((destination, content));
        }
        values.push((id.to_string(), value));
    }
    // Show indexes, search data and undo history belong to the previous library.
    // Commit their invalidation with the root selection so restart is consistent.
    for id in ["SHOWS", "CACHE", "HISTORY"] {
        let value = storage::defaults(id)?;
        if stores.get(id)? != value {
            changes.push((stores.path(id)?, serde_json::to_vec(&value)?));
        }
        values.push((id.to_string(), value));
    }
    // Physical output configuration is local, but its current cues and the
    // selected project/scene/overlay references belong to the old library.
    // Persist their reset in the same transaction, then reapply local settings
    // so mounted previews and native windows cannot retain same-ID old content.
    let mut settings = stores.get("SETTINGS")?;
    let original_settings = settings.clone();
    for (key, empty) in [
        ("activeProject", Value::Null),
        ("activeScenes", json!({})),
        ("lockedOverlays", json!({})),
    ] {
        if let Some(value) = settings.get_mut(key) {
            *value = empty;
        }
    }
    if let Some(outputs) = settings.get_mut("outputs").and_then(Value::as_object_mut) {
        for output in outputs.values_mut().filter_map(Value::as_object_mut) {
            for key in ["out", "show"] {
                let populated = output.get(key).is_some_and(|value| match value {
                    Value::Null => false,
                    Value::Object(values) => !values.is_empty(),
                    Value::Array(values) => !values.is_empty(),
                    _ => true,
                });
                if populated {
                    output.remove(key);
                }
            }
        }
    }
    if settings != original_settings {
        let content = if storage::definitions()["SETTINGS"]["minify"]
            .as_bool()
            .unwrap_or(false)
        {
            serde_json::to_vec(&settings)?
        } else {
            serde_json::to_vec_pretty(&settings)?
        };
        changes.push((stores.path("SETTINGS")?, content));
    }
    values.push(("SETTINGS".into(), settings));
    let mut config = stores.get("config")?;
    config["dataPath"] = json!(new_path);
    changes.push((stores.path("config")?, serde_json::to_vec_pretty(&config)?));
    let files = changes
        .iter()
        .map(|(path, content)| (path.as_path(), content.as_slice()))
        .collect::<Vec<_>>();
    crate::transaction::commit(&new_path, &files, "freeshow-location-", &mut |_| Ok(()))?;
    stores.data = new_path;
    Ok(values)
}
