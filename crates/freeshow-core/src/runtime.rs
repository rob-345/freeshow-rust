use crate::{
    protocol::{Event, Request},
    show,
    storage::{self, Stores, STORE_IDS},
    Error, Result,
};
use serde_json::{json, Map, Value};
use std::{fs, path::PathBuf};

pub const IMPLEMENTED_MAIN: &[&str] = &[
    "LOG",
    "IS_DEV",
    "VERSION",
    "GET_CACHE_PATH",
    "SETTINGS",
    "SYNCED_SETTINGS",
    "STAGE",
    "PROJECTS",
    "OVERLAYS",
    "TEMPLATES",
    "EVENTS",
    "MEDIA",
    "THEMES",
    "DRIVE_API_KEY",
    "HISTORY",
    "USAGE",
    "CACHE",
    "GET_STORE_VALUE",
    "SET_STORE_VALUE",
    "DATA_PATH",
    "SHOW",
    "SHOWS",
    "FULL_SHOWS_LIST",
    "GET_EMPTY_SHOWS",
    "SAVE",
    "READ_FILE",
    "OUTPUT",
];

pub struct Runtime {
    pub stores: Stores,
    pub version: String,
    pub development: bool,
}

impl Runtime {
    pub fn new(app_data: PathBuf, data: PathBuf) -> Result<Self> {
        Ok(Self {
            stores: Stores::new(app_data, data)?,
            version: "1.6.7-beta.2-rust.1".into(),
            development: cfg!(debug_assertions),
        })
    }

    pub fn dispatch(&mut self, request: Request, window: &str) -> Result<Vec<Event>> {
        if request.channel == "STARTUP" && request.data["channel"] == "READY" {
            let mut event = Event::reply(
                "STARTUP",
                "TYPE",
                if window == "main" {
                    Value::Null
                } else {
                    json!("output")
                },
                request.listener_id,
            );
            event.message.auto_profile = Some(String::new());
            return Ok(vec![event]);
        }
        // LOADED is emitted by upstream despite not being in ValidChannels.
        if request.channel == "LOADED" {
            return Ok(vec![]);
        }
        if request.channel != "MAIN" {
            return Err(Error::Unsupported(request.channel));
        }
        let command = required_string(&request.data, "channel")?;
        let data = &request.data["data"];
        let mut events = Vec::new();
        let response = match command {
            "LOG" => {
                eprintln!("{data}");
                None
            }
            "IS_DEV" => Some(json!(self.development)),
            "VERSION" => Some(json!(self.version)),
            "GET_CACHE_PATH" => {
                let path = self.stores.app_data.join("Thumbnails");
                fs::create_dir_all(&path)?;
                Some(json!(path))
            }
            "DATA_PATH" => Some(json!(self.stores.data)),
            id if id != "SHOWS" && STORE_IDS.contains(&id) && IMPLEMENTED_MAIN.contains(&id) => {
                Some(self.stores.get(id)?)
            }
            "GET_STORE_VALUE" => Some(self.stores.get_key(
                required_string(data, "file")?,
                required_string(data, "key")?,
            )?),
            "SET_STORE_VALUE" => {
                let value = data
                    .get("value")
                    .ok_or_else(|| Error::Invalid("missing value".into()))?;
                self.stores.set_key(
                    required_string(data, "file")?,
                    required_string(data, "key")?,
                    value.clone(),
                )?;
                None
            }
            "SHOW" => Some(self.load_show(data)?),
            "SHOWS" => {
                let (shows, extra) = self.load_shows()?;
                events.extend(extra);
                Some(shows)
            }
            "FULL_SHOWS_LIST" => Some(json!(self
                .show_names()?
                .into_iter()
                .filter(|name| name.contains(".show") && name.len() > 5)
                .collect::<Vec<_>>())),
            "GET_EMPTY_SHOWS" => Some(self.empty_shows(&data["cached"])?),
            "SAVE" => {
                self.save(data)?;
                if !data["customTriggers"]["reset"].as_bool().unwrap_or(false) {
                    let mut info = Map::new();
                    for key in ["closeWhenFinished", "customTriggers"] {
                        if let Some(value) = data.get(key) {
                            info.insert(key.into(), value.clone());
                        }
                    }
                    events.push(Event::reply("MAIN", "SAVE2", Value::Object(info), None));
                } else {
                    for id in ["SETTINGS", "SYNCED_SETTINGS"] {
                        events.push(Event::reply("MAIN", id, self.stores.get(id)?, None));
                    }
                }
                None
            }
            "READ_FILE" => {
                let content = match fs::read_to_string(required_string(data, "path")?) {
                    Ok(text) => text,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(error) => return Err(error.into()),
                };
                Some(json!({"content":content}))
            }
            "OUTPUT" => Some(json!(if window == "main" { "false" } else { "true" })),
            _ => return Err(Error::Unsupported(format!("MAIN.{command}"))),
        };
        if let Some(data) = response {
            events.push(Event::reply("MAIN", command, data, request.listener_id));
        }
        Ok(events)
    }

    fn show_path(&self, name: &str) -> Result<PathBuf> {
        storage::leaf_path(&self.stores.data.join("Shows"), name, "show")
    }

    fn show_names(&self) -> Result<Vec<String>> {
        let mut files = fs::read_dir(self.stores.data.join("Shows"))?
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>>>()?;
        files.sort();
        Ok(files)
    }

    pub fn load_show(&self, data: &Value) -> Result<Value> {
        let id = data["id"].as_str().unwrap_or("");
        let name = data["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .unwrap_or(id);
        let text = match fs::read_to_string(self.show_path(name)?) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({"error":"not_found","id":id}))
            }
            Err(error) => return Err(error.into()),
        };
        let Some(mut content) = show::parse_json(&text).filter(show::truthy) else {
            return Ok(json!({"error":"not_found","id":id}));
        };
        if !id.is_empty() {
            if let Some(array) = content.as_array_mut() {
                if !array.is_empty() {
                    array[0] = json!(id);
                }
            }
        }
        Ok(json!({"id":id,"content":content}))
    }

    fn load_shows(&mut self) -> Result<(Value, Vec<Event>)> {
        let cached = self.stores.get("SHOWS")?;
        let mut index = Map::new();
        let mut text = Map::new();
        let mut events = Vec::new();
        if cached.as_object().is_some_and(|cache| !cache.is_empty()) {
            events.push(Event::reply("MAIN", "SHOWS", cached, None));
        }
        for name in self.show_names()? {
            if name.len() <= 5 || !name.to_lowercase().ends_with(".show") {
                continue;
            }
            let path = self.stores.data.join("Shows").join(&name);
            let Some(mut parsed) = show::parse_json(&fs::read_to_string(&path)?) else {
                continue;
            };
            let Some(array) = parsed
                .as_array_mut()
                .filter(|array| array.len() >= 2 && array[1].is_object())
            else {
                continue;
            };
            let mut id = array[0].as_str().unwrap_or("").to_string();
            if id.is_empty() || index.contains_key(&id) {
                id = uuid::Uuid::new_v4().simple().to_string();
                array[0] = json!(id);
                storage::atomic_write(&path, &Value::Array(array.clone()), false)?;
            }
            let search = show::search_text(&array[1]);
            let mut show_data = array[1].clone();
            show_data["name"] = json!(&name[..name.len() - 5]);
            index.insert(id.clone(), show::trim(&show_data));
            if !search.is_empty() {
                text.insert(id, json!(search));
            }
        }
        if !text.is_empty() {
            let mut cache = self.stores.get("CACHE")?;
            if !cache["text"].is_object() {
                cache["text"] = json!({});
            }
            cache["text"].as_object_mut().unwrap().extend(text);
            events.push(Event::reply("MAIN", "CACHE", cache, None));
        }
        let shows = Value::Object(index);
        self.stores.set("SHOWS", shows.clone())?;
        Ok((shows, events))
    }

    fn empty_shows(&self, cached: &Value) -> Result<Value> {
        let mut empty = Vec::new();
        for name in self.show_names()? {
            if !name.contains(".show") {
                continue;
            }
            let Ok(content) = fs::read_to_string(self.stores.data.join("Shows").join(&name)) else {
                continue;
            };
            let Some(parsed) = show::parse_json(&content) else {
                continue;
            };
            let Some(id) = parsed[0].as_str() else {
                continue;
            };
            let show = cached
                .get(id)
                .filter(|value| show::truthy(value))
                .unwrap_or(&parsed[1]);
            if show.is_object() && show::is_empty(show) {
                empty.push(json!({"id":id,"name":name.replacen(".show", "", 1)}));
            }
        }
        Ok(json!(empty))
    }

    pub fn save(&mut self, data: &Value) -> Result<()> {
        if !data.is_object() {
            return Err(Error::Invalid("SAVE requires an object".into()));
        }
        // Refuse unsupported backup before any writes; never acknowledge a backup
        // as completed just because the underlying save worked.
        if data["customTriggers"]["backup"].as_bool().unwrap_or(false) {
            return Err(Error::Unsupported("SAVE.backup".into()));
        }
        // Validate names before any mutation, including the path used by rename.
        for field in ["showsCache", "scripturesCache"] {
            for (id, value) in data[field].as_object().into_iter().flatten() {
                if !show::truthy(value) {
                    continue;
                }
                let name = value["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id);
                storage::leaf_path(&self.stores.data, name, "show")?;
            }
        }
        for field in ["renamedShows", "deletedShows"] {
            for value in data[field].as_array().into_iter().flatten() {
                let name = value["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .or_else(|| value["id"].as_str())
                    .unwrap_or("");
                self.show_path(name)?;
                if field == "renamedShows" {
                    self.show_path(required_string(value, "oldName")?)?;
                }
            }
        }
        let reset = data["customTriggers"]["reset"].as_bool().unwrap_or(false);
        for id in STORE_IDS {
            let value = if reset && matches!(*id, "SETTINGS" | "SYNCED_SETTINGS") {
                Some(storage::defaults(id)?)
            } else {
                data.get(*id).filter(|value| show::truthy(value)).cloned()
            };
            if let Some(value) = value {
                self.stores.set(id, value)?;
            }
        }
        for rename in data["renamedShows"].as_array().into_iter().flatten() {
            let id = required_string(rename, "id")?;
            if data["deletedShows"]
                .as_array()
                .is_some_and(|deleted| deleted.iter().any(|entry| entry["id"] == id))
            {
                continue;
            }
            let old = self.show_path(required_string(rename, "oldName")?)?;
            let name = rename["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .unwrap_or(id);
            if old.exists() {
                fs::rename(old, self.show_path(name)?)?;
            }
        }
        for (field, directory, extension) in [
            ("showsCache", "Shows", "show"),
            ("scripturesCache", "Bibles", "fsb"),
        ] {
            for (id, value) in data[field].as_object().into_iter().flatten() {
                if !show::truthy(value) {
                    continue;
                }
                let name = value["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id);
                let file = storage::leaf_path(&self.stores.data.join(directory), name, extension)?;
                storage::atomic_write(&file, &json!([id, value]), false)?;
            }
        }
        for entry in data["deletedShows"].as_array().into_iter().flatten() {
            let Some(id) = entry["id"].as_str().filter(|id| !id.is_empty()) else {
                continue;
            };
            if data["showsCache"].get(id).is_some_and(show::truthy) {
                continue;
            }
            let name = entry["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .unwrap_or(id);
            let path = self.show_path(name)?;
            if let Ok(text) = fs::read_to_string(&path) {
                if show::parse_json(&text).is_some_and(|show| show[0] == id) {
                    fs::remove_file(path)?;
                }
            }
        }
        Ok(())
    }
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| Error::Invalid(format!("{key} must be a string")))
}
