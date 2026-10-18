use crate::{
    backup, location,
    protocol::{Event, Request, RuntimeContext},
    restore, show,
    storage::{self, Stores, STORE_IDS},
    Error, Result,
};
use serde_json::{json, Map, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

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
    "UPDATE_DATA_PATH",
    "SHOW",
    "SHOWS",
    "FULL_SHOWS_LIST",
    "GET_EMPTY_SHOWS",
    "SAVE",
    "READ_FILE",
    "OUTPUT",
    "BACKUPS",
    "DELETE_BACKUP",
    "RESTORE",
    "REFRESH_SHOWS",
];

pub struct Runtime {
    pub stores: Stores,
    pub version: String,
    pub development: bool,
    runtime_id: String,
    library_generation: u64,
}

pub(crate) struct PreparedDispatch {
    events: Vec<Event>,
    backup: Option<backup::Snapshot>,
}

impl From<Vec<Event>> for PreparedDispatch {
    fn from(events: Vec<Event>) -> Self {
        Self {
            events,
            backup: None,
        }
    }
}

impl PreparedDispatch {
    pub(crate) fn has_backup(&self) -> bool {
        self.backup.is_some()
    }

    pub(crate) fn finish(mut self) -> Vec<Event> {
        if let Some(snapshot) = self.backup {
            append_backup_result(&mut self.events, snapshot.finish());
        }
        self.events
    }
}

/// An immutable root/context descriptor; it never constructs or writes stores.
#[derive(Clone)]
pub(crate) struct ShowReader {
    data: PathBuf,
    context: RuntimeContext,
}

impl ShowReader {
    pub(crate) fn dispatch(&self, request: Request) -> Result<Vec<Event>> {
        if request.channel != "MAIN" || request.data["channel"] != "SHOW" {
            return Err(Error::Unsupported("show reader command".into()));
        }
        if request.context.as_ref() != Some(&self.context) {
            return Err(Error::StaleLibrary {
                recovery_path: None,
                recovery_error: None,
            });
        }
        let mut event = Event::reply(
            "MAIN",
            "SHOW",
            load_show(&self.data, &request.data["data"])?,
            request.listener_id,
        );
        event.context = Some(self.context.clone());
        Ok(vec![event])
    }
}

impl Runtime {
    pub fn open(app_data: PathBuf, data_override: Option<PathBuf>) -> Result<Self> {
        let data = location::startup_path(&app_data, data_override)?;
        Self::new(app_data, data)
    }

    pub fn new(app_data: PathBuf, data: PathBuf) -> Result<Self> {
        Ok(Self {
            stores: Stores::new(app_data, data)?,
            version: "1.6.7-beta.2-rust.1".into(),
            development: cfg!(debug_assertions),
            runtime_id: uuid::Uuid::new_v4().to_string(),
            library_generation: 0,
        })
    }

    pub fn context(&self) -> RuntimeContext {
        RuntimeContext {
            runtime_id: self.runtime_id.clone(),
            library_generation: self.library_generation,
            data_path: self.stores.data.clone(),
        }
    }

    pub fn validate_context(&self, request: &Request) -> Result<()> {
        let expected = self.context();
        let snapshot_context = request.data["data"].get("runtimeContext");
        let snapshot_matches = request.data["channel"] != "SAVE"
            || snapshot_context.is_none()
            || snapshot_context
                .and_then(|value| serde_json::from_value::<RuntimeContext>(value.clone()).ok())
                .as_ref()
                == Some(&expected);
        if request.context.as_ref() == Some(&expected) && snapshot_matches {
            return Ok(());
        }
        let mut recovery_path = None;
        let mut recovery_error = None;
        // Preserve an old save as an isolated recovery artifact. Applying it to
        // either library automatically could overwrite newer user edits.
        if request.channel == "MAIN" && request.data["channel"] == "SAVE" {
            let path = self
                .stores
                .app_data
                .join("Recovery")
                .join(format!("save-{}.json", uuid::Uuid::new_v4()));
            let snapshot = json!({"version":1,"createdAt":chrono::Utc::now().to_rfc3339(),"request":request,"activeContext":self.context()});
            match storage::atomic_write(&path, &snapshot, true) {
                Ok(()) => recovery_path = Some(path),
                Err(error) => recovery_error = Some(error.to_string()),
            }
        }
        Err(Error::StaleLibrary {
            recovery_path,
            recovery_error,
        })
    }

    pub(crate) fn show_reader(&self) -> ShowReader {
        ShowReader {
            data: self.stores.data.clone(),
            context: self.context(),
        }
    }

    pub fn dispatch(&mut self, request: Request, window: &str) -> Result<Vec<Event>> {
        Ok(self.prepare_dispatch(request, window)?.finish())
    }

    pub(crate) fn prepare_dispatch(
        &mut self,
        request: Request,
        window: &str,
    ) -> Result<PreparedDispatch> {
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
            return Ok(vec![event].into());
        }
        // LOADED is emitted by upstream despite not being in ValidChannels.
        if request.channel == "LOADED" {
            return Ok(vec![].into());
        }
        if request.channel != "MAIN" {
            return Err(Error::Unsupported(request.channel));
        }
        let command = required_string(&request.data, "channel")?;
        if storage::portable(command)
            || (command == "GET_STORE_VALUE"
                && storage::portable(request.data["data"]["file"].as_str().unwrap_or("")))
            || matches!(
                command,
                "SAVE"
                    | "UPDATE_DATA_PATH"
                    | "SET_STORE_VALUE"
                    | "RESTORE"
                    | "DELETE_BACKUP"
                    | "SHOW"
                    | "SHOWS"
                    | "FULL_SHOWS_LIST"
                    | "GET_EMPTY_SHOWS"
                    | "REFRESH_SHOWS"
                    | "BACKUPS"
            )
        {
            self.validate_context(&request)?;
        }
        let data = &request.data["data"];
        let mut events = Vec::new();
        let mut backup = None;
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
            "UPDATE_DATA_PATH" => {
                if self.library_generation == u64::MAX {
                    return Err(Error::Invalid("library generation exhausted".into()));
                }
                let previous = self.stores.data.clone();
                match location::select(
                    &mut self.stores,
                    &PathBuf::from(required_string(data, "newPath")?),
                    &PathBuf::from(required_string(data, "oldPath")?),
                ) {
                    Ok(values) => {
                        if self.stores.data != previous {
                            self.library_generation += 1;
                        }
                        events.push(Event::reply(
                            "MAIN",
                            "DATA_PATH",
                            json!(self.stores.data),
                            None,
                        ));
                        for (id, value) in values {
                            events.push(Event::reply("MAIN", &id, value, None));
                        }
                        Some(json!({"success":true,"path":self.stores.data}))
                    }
                    Err(error) => {
                        events.push(Event::reply(
                            "MAIN",
                            "ALERT",
                            error_alert("Could not change the data folder", &error),
                            None,
                        ));
                        Some(json!({"success":false,"path":self.stores.data}))
                    }
                }
            }
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
            "REFRESH_SHOWS" => {
                let (shows, _) = self.load_shows()?;
                if shows.as_object().is_some_and(|shows| !shows.is_empty()) {
                    events.push(Event::reply("MAIN", "REFRESH_SHOWS2", shows, None));
                }
                None
            }
            "BACKUPS" => Some(serde_json::to_value(backup::list(&self.stores.data)?)?),
            "DELETE_BACKUP" => {
                backup::remove(
                    &self.stores.data,
                    &PathBuf::from(required_string(data, "path")?),
                )?;
                None
            }
            "RESTORE" => {
                let paths = if let Some(path) = data["path"].as_str() {
                    vec![PathBuf::from(path)]
                } else if let Some(paths) = data["paths"].as_array() {
                    paths
                        .iter()
                        .map(|path| {
                            path.as_str().map(PathBuf::from).ok_or_else(|| {
                                Error::Invalid("restore paths must be strings".into())
                            })
                        })
                        .collect::<Result<Vec<_>>>()?
                } else {
                    return Err(Error::Unsupported("RESTORE.file_picker".into()));
                };
                match restore::restore(&self.stores, &paths) {
                    Ok(restored) => {
                        if restored.recognized {
                            events.push(Event::reply(
                                "MAIN",
                                "RESTORE2",
                                json!({"starting":true}),
                                None,
                            ));
                            for (id, value) in restored.stores {
                                events.push(Event::reply("MAIN", &id, value, None));
                            }
                            if restored.shows {
                                let (shows, _) = self.load_shows()?;
                                events.push(Event::reply("MAIN", "SHOWS", shows, None));
                            }
                        }
                        events.push(Event::reply(
                            "MAIN",
                            "RESTORE2",
                            json!({"finished":restored.recognized}),
                            None,
                        ));
                    }
                    Err(error) => {
                        events.push(Event::reply(
                            "MAIN",
                            "RESTORE2",
                            json!({"finished":false}),
                            None,
                        ));
                        events.push(Event::reply(
                            "MAIN",
                            "ALERT",
                            error_alert("Could not restore backup", &error),
                            None,
                        ));
                    }
                }
                None
            }
            "SAVE" => {
                let is_backup = data["customTriggers"]["backup"].as_bool().unwrap_or(false);
                let automatic = data["customTriggers"]["isAutoBackup"]
                    .as_bool()
                    .unwrap_or(false);
                let backup_only = is_backup
                    && automatic
                    && !data["customTriggers"]["autosave"]
                        .as_bool()
                        .unwrap_or(false)
                    && !data["closeWhenFinished"].as_bool().unwrap_or(false);
                if backup_only {
                    if let Some(settings) = data.get("SETTINGS").filter(|value| value.is_object()) {
                        self.stores.set("SETTINGS", settings.clone())?;
                    }
                } else {
                    self.save(data)?;
                }
                if backup_only || !data["customTriggers"]["reset"].as_bool().unwrap_or(false) {
                    let mut info = Map::new();
                    if backup_only {
                        info.insert("closeWhenFinished".into(), json!(false));
                    }
                    for key in ["closeWhenFinished", "customTriggers", "runtimeSaveToken"] {
                        if backup_only && key == "closeWhenFinished" {
                            continue;
                        }
                        if let Some(value) = data.get(key) {
                            info.insert(key.into(), value.clone());
                        }
                    }
                    events.push(Event::reply(
                        "MAIN",
                        "SAVE2",
                        Value::Object(info),
                        request.listener_id.clone(),
                    ));
                } else {
                    for id in ["SETTINGS", "SYNCED_SETTINGS"] {
                        events.push(Event::reply("MAIN", id, self.stores.get(id)?, None));
                    }
                }
                if is_backup {
                    match backup::prepare(&self.stores, automatic) {
                        Ok(snapshot) => backup = Some(snapshot),
                        Err(error) => append_backup_result(&mut events, Err(error)),
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
        Ok(PreparedDispatch { events, backup })
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
        load_show(&self.stores.data, data)
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

fn error_alert(context: &str, error: &Error) -> Value {
    // The shared alert component renders HTML. Escape filenames and parser
    // messages before including them in its content.
    let detail = error
        .to_string()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    json!(format!("<span>{context}: {detail}</span>"))
}

fn load_show(directory: &Path, data: &Value) -> Result<Value> {
    let id = data["id"].as_str().unwrap_or("");
    let name = data["name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .unwrap_or(id);
    let text = match fs::read_to_string(storage::leaf_path(&directory.join("Shows"), name, "show")?)
    {
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

fn append_backup_result(events: &mut Vec<Event>, result: Result<PathBuf>) {
    match result {
        Ok(path) => events.push(Event::reply(
            "MAIN",
            "BACKUP",
            json!({"finished":true,"path":path}),
            None,
        )),
        Err(error) => {
            events.push(Event::reply(
                "MAIN",
                "BACKUP",
                json!({"finished":false}),
                None,
            ));
            events.push(Event::reply(
                "MAIN",
                "ALERT",
                error_alert("Could not create backup", &error),
                None,
            ));
        }
    }
}
