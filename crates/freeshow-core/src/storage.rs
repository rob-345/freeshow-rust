use crate::{Error, Result};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub const STORE_IDS: &[&str] = &[
    "SHOWS",
    "SETTINGS",
    "SYNCED_SETTINGS",
    "THEMES",
    "PROJECTS",
    "STAGE",
    "OVERLAYS",
    "TEMPLATES",
    "EVENTS",
    "HISTORY",
    "MEDIA",
    "CACHE",
    "CACHE_SYNC",
    "USAGE",
    "ERROR_LOG",
    "DRIVE_API_KEY",
    "ACCESS",
];

pub fn defaults(id: &str) -> Result<Value> {
    let upstream: Value =
        serde_json::from_str(include_str!("../../../tests/parity/fixtures/defaults.json"))?;
    Ok(match id {
        "config" => upstream["defaultConfig"].clone(),
        "SETTINGS" => upstream["defaultSettings"].clone(),
        "SYNCED_SETTINGS" => upstream["defaultSyncedSettings"].clone(),
        "PROJECTS" => json!({"projects":{},"folders":{},"projectTemplates":{}}),
        "ACCESS" => json!({"contentProviders":{},"secrets":{}}),
        id if STORE_IDS.contains(&id) => json!({}),
        _ => return Err(Error::Invalid(format!("unknown store {id}"))),
    })
}

pub fn portable(id: &str) -> bool {
    matches!(
        id,
        "SYNCED_SETTINGS" | "THEMES" | "PROJECTS" | "STAGE" | "OVERLAYS" | "TEMPLATES" | "EVENTS"
    )
}

pub fn filename(id: &str) -> Result<String> {
    defaults(id)?;
    Ok(format!(
        "{}.json",
        match id {
            "SYNCED_SETTINGS" => "settings_synced".into(),
            "DRIVE_API_KEY" | "ACCESS" => id.into(),
            _ => id.to_lowercase(),
        }
    ))
}

/// Write a complete file beside its destination, flush it, then atomically replace
/// the destination. A failed serialization/write never truncates the old file.
pub fn atomic_write(path: &Path, value: &Value, pretty: bool) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("file has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    if pretty {
        serde_json::to_writer_pretty(file.as_file_mut(), value)?;
    } else {
        serde_json::to_writer(file.as_file_mut(), value)?;
    }
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| Error::Io(error.error))?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn leaf_path(directory: &Path, name: &str, extension: &str) -> Result<PathBuf> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.chars().any(|ch| matches!(ch, '/' | '\\' | '\0'))
        || Path::new(name).is_absolute()
    {
        return Err(Error::Invalid(
            "file name must be a single nonempty path component".into(),
        ));
    }
    Ok(directory.join(format!("{name}.{extension}")))
}

pub struct Stores {
    pub app_data: PathBuf,
    pub data: PathBuf,
    cache: HashMap<String, Value>,
}

impl Stores {
    /// Both roots are supplied by the caller. The fork never discovers or writes
    /// the original Electron profile implicitly.
    pub fn new(app_data: PathBuf, data: PathBuf) -> Result<Self> {
        fs::create_dir_all(&app_data)?;
        fs::create_dir_all(data.join("Config"))?;
        fs::create_dir_all(data.join("Shows"))?;
        fs::create_dir_all(data.join("Bibles"))?;
        Ok(Self {
            app_data,
            data,
            cache: HashMap::new(),
        })
    }

    pub fn path(&self, id: &str) -> Result<PathBuf> {
        Ok(if portable(id) {
            self.data.join("Config")
        } else {
            self.app_data.clone()
        }
        .join(filename(id)?))
    }

    pub fn get(&mut self, id: &str) -> Result<Value> {
        if let Some(value) = self.cache.get(id) {
            return Ok(value.clone());
        }
        let mut value = defaults(id)?;
        match fs::read(self.path(id)?) {
            Ok(bytes) => {
                let stored: Value = serde_json::from_slice(&bytes)?;
                let stored = stored
                    .as_object()
                    .ok_or_else(|| Error::Invalid(format!("store {id} must be an object")))?;
                // electron-store/Conf fills only absent top-level defaults.
                value.as_object_mut().unwrap().extend(stored.clone());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.cache.insert(id.into(), value.clone());
        Ok(value)
    }

    pub fn set(&mut self, id: &str, value: Value) -> Result<()> {
        if !value.is_object() {
            return Err(Error::Invalid("store must be an object".into()));
        }
        let pretty = !matches!(
            id,
            "SHOWS" | "HISTORY" | "MEDIA" | "CACHE" | "CACHE_SYNC" | "USAGE"
        );
        atomic_write(&self.path(id)?, &value, pretty)?;
        self.cache.insert(id.into(), value); // Conf reapplies defaults at construction, not on every read.
        Ok(())
    }

    pub fn get_key(&mut self, id: &str, key: &str) -> Result<Value> {
        let value = self.get(id)?;
        if id == "MEDIA" {
            return Ok(value.get(key).cloned().unwrap_or(Value::Null));
        }
        let mut current = &value;
        for segment in key_segments(key)? {
            current = current.get(segment).unwrap_or(&Value::Null);
        }
        Ok(current.clone())
    }

    pub fn set_key(&mut self, id: &str, key: &str, value: Value) -> Result<()> {
        let mut store = self.get(id)?;
        let segments = if id == "MEDIA" {
            vec![key.to_string()]
        } else {
            key_segments(key)?
        };
        let mut current = &mut store;
        for segment in &segments[..segments.len() - 1] {
            if !current.is_object() {
                *current = json!({});
            }
            current = current
                .as_object_mut()
                .unwrap()
                .entry(segment.clone())
                .or_insert_with(|| json!({}));
        }
        if !current.is_object() {
            *current = json!({});
        }
        current
            .as_object_mut()
            .unwrap()
            .insert(segments.last().unwrap().clone(), value);
        self.set(id, store)
    }
}

fn key_segments(key: &str) -> Result<Vec<String>> {
    // dot-prop supports escaped periods (not array-index notation in this
    // upstream version). Reject prototype keys as dot-prop does.
    let mut result = Vec::new();
    let mut segment = String::new();
    let mut escaped = false;
    for ch in key.chars() {
        if escaped {
            if ch != '.' && ch != '\\' {
                segment.push('\\');
            }
            segment.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '.' {
            result.push(std::mem::take(&mut segment));
        } else {
            segment.push(ch);
        }
    }
    if escaped {
        segment.push('\\');
    }
    result.push(segment);
    if result
        .iter()
        .any(|part| matches!(part.as_str(), "__proto__" | "constructor" | "prototype"))
    {
        return Err(Error::Invalid("reserved store key".into()));
    }
    Ok(result)
}
