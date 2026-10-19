use crate::{Error, Result};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
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
    if id == "config" {
        static DEFAULTS: OnceLock<Value> = OnceLock::new();
        return Ok(DEFAULTS
            .get_or_init(|| {
                serde_json::from_str(include_str!("../../../tests/parity/fixtures/defaults.json"))
                    .expect("generated defaults JSON")
            })
            .get("defaultConfig")
            .unwrap()
            .clone());
    }
    definitions()
        .get(id)
        .map(|value| value["defaults"].clone())
        .ok_or_else(|| Error::Invalid(format!("unknown store {id}")))
}

pub fn definitions() -> &'static Value {
    static STORES: OnceLock<Value> = OnceLock::new();
    STORES.get_or_init(|| {
        serde_json::from_str(include_str!("../../../tests/parity/fixtures/stores.json"))
            .expect("generated stores JSON")
    })
}

pub fn portable(id: &str) -> bool {
    definitions()[id]["portable"].as_bool().unwrap_or(false)
}

pub fn filename(id: &str) -> Result<String> {
    if id == "config" {
        return Ok("config.json".into());
    }
    definitions()[id]["fileName"]
        .as_str()
        .map(|name| format!("{name}.json"))
        .ok_or_else(|| Error::Invalid(format!("unknown store {id}")))
}

/// Write a complete file beside its destination, flush it, then atomically replace
/// the destination. A failed serialization/write never truncates the old file.
pub fn atomic_write(path: &Path, value: &Value, pretty: bool) -> Result<()> {
    atomic_replace(path, |file| {
        if pretty {
            serde_json::to_writer_pretty(file, value)?;
        } else {
            serde_json::to_writer(file, value)?;
        }
        Ok(())
    })
}

pub fn atomic_write_bytes(path: &Path, content: &[u8]) -> Result<()> {
    atomic_replace(path, |file| {
        file.write_all(content)?;
        Ok(())
    })
}

pub fn atomic_replace(path: &Path, write: impl FnOnce(&mut fs::File) -> Result<()>) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("file has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    if path.exists() {
        // Respect existing read-only/locking behavior and retain file modes.
        let existing = fs::OpenOptions::new().write(true).open(path)?;
        file.as_file()
            .set_permissions(existing.metadata()?.permissions())?;
    }
    write(file.as_file_mut())?;
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
}

impl Stores {
    /// Both roots are supplied by the caller. The fork never discovers or writes
    /// the original Electron profile implicitly.
    pub fn new(app_data: PathBuf, data: PathBuf) -> Result<Self> {
        fs::create_dir_all(&app_data)?;
        fs::create_dir_all(data.join("Config"))?;
        fs::create_dir_all(data.join("Shows"))?;
        fs::create_dir_all(data.join("Bibles"))?;
        let stores = Self { app_data, data };
        // Conf applies top-level defaults once, when its stores are constructed.
        for id in std::iter::once(&"config").chain(STORE_IDS.iter()) {
            stores.initialize(id)?;
        }
        Ok(stores)
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
        defaults(id)?;
        match fs::read(self.path(id)?) {
            Ok(bytes) => {
                let value: Value = serde_json::from_slice(&bytes)?;
                if !value.is_object() {
                    return Err(Error::Invalid(format!("store {id} must be an object")));
                }
                Ok(value)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
            Err(error) => Err(error.into()),
        }
    }

    fn initialize(&self, id: &str) -> Result<()> {
        let mut value = defaults(id)?;
        let mut original = json!({});
        match fs::read(self.path(id)?) {
            Ok(bytes) => {
                let stored: Value = serde_json::from_slice(&bytes)?;
                let stored = stored
                    .as_object()
                    .ok_or_else(|| Error::Invalid(format!("store {id} must be an object")))?;
                // electron-store/Conf fills only absent top-level defaults.
                value.as_object_mut().unwrap().extend(stored.clone());
                original = Value::Object(stored.clone());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if value != original {
            atomic_write(
                &self.path(id)?,
                &value,
                !definitions()[id]["minify"].as_bool().unwrap_or(false),
            )?;
        }
        Ok(())
    }

    pub fn set(&mut self, id: &str, value: Value) -> Result<()> {
        if !value.is_object() {
            return Err(Error::Invalid("store must be an object".into()));
        }
        let pretty = !definitions()[id]["minify"].as_bool().unwrap_or(false);
        atomic_write(&self.path(id)?, &value, pretty)?;
        Ok(())
    }

    pub fn get_key(&mut self, id: &str, key: &str) -> Result<Value> {
        let value = self.get(id)?;
        if id == "MEDIA" {
            return Ok(value.get(key).cloned().unwrap_or(Value::Null));
        }
        let mut current = &value;
        for segment in key_segments(key)? {
            current = if let Value::Array(array) = current {
                array_index(&segment).and_then(|index| array.get(index))
            } else {
                current.get(segment)
            }
            .unwrap_or(&Value::Null);
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
        set_nested(&mut store, &segments, value)?;
        self.set(id, store)
    }
}

fn array_index(segment: &str) -> Option<usize> {
    segment
        .parse::<u32>()
        .ok()
        .filter(|index| *index != u32::MAX && index.to_string() == segment)
        .map(|index| index as usize)
}

fn set_nested(current: &mut Value, segments: &[String], value: Value) -> Result<()> {
    if segments.is_empty() {
        *current = value;
        return Ok(());
    }
    let segment = &segments[0];
    if let Value::Array(array) = current {
        // JavaScript's non-index array properties are not serialized by JSON.
        let Some(index) = array_index(segment) else {
            return Ok(());
        };
        if index > 1_000_000 {
            return Err(Error::Invalid(
                "store array index exceeds the supported limit".into(),
            ));
        }
        if array.len() <= index {
            array.resize(index + 1, Value::Null);
        }
        return set_nested(&mut array[index], &segments[1..], value);
    }
    if !current.is_object() {
        *current = json!({});
    }
    let child = current
        .as_object_mut()
        .unwrap()
        .entry(segment.clone())
        .or_insert(Value::Null);
    set_nested(child, &segments[1..], value)
}

fn key_segments(key: &str) -> Result<Vec<String>> {
    // dot-prop supports escaped periods (not array-index notation in this
    // upstream version). Reject prototype keys as dot-prop does.
    let mut result = Vec::new();
    let mut pieces = key.split('.').peekable();
    while let Some(piece) = pieces.next() {
        let mut segment = piece.to_string();
        while segment.ends_with('\\') && pieces.peek().is_some() {
            segment.pop();
            segment.push('.');
            segment.push_str(pieces.next().unwrap());
        }
        result.push(segment);
    }
    if result
        .iter()
        .any(|part| matches!(part.as_str(), "__proto__" | "constructor" | "prototype"))
    {
        return Err(Error::Invalid("reserved store key".into()));
    }
    Ok(result)
}
