#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::{LibraryService, LibraryStatus},
    WireError,
};
use serde_json::json;
use std::path::PathBuf;
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

struct AppState {
    library: LibraryService,
    app_data: PathBuf,
    error_log: PathBuf,
}

#[tauri::command]
fn runtime_context(app: tauri::AppHandle) -> Result<RuntimeContext, WireError> {
    app.state::<AppState>()
        .library
        .context()
        .map_err(WireError::from)
}

#[tauri::command]
fn library_status(app: tauri::AppHandle) -> Result<LibraryStatus, WireError> {
    app.state::<AppState>()
        .library
        .status()
        .map_err(WireError::from)
}

fn contextual_events(
    app: &tauri::AppHandle,
    request: &Request,
    mut events: Vec<Event>,
) -> Result<Vec<Event>, WireError> {
    app.state::<AppState>()
        .library
        .contextualize(request, &mut events)
        .map_err(WireError::from)?;
    Ok(events)
}

#[tauri::command]
async fn ipc_dispatch(
    window: tauri::WebviewWindow,
    request: Request,
) -> Result<Vec<Event>, WireError> {
    if request.channel == "MAIN" {
        let command = request.data["channel"].as_str().unwrap_or("");
        let reply = |data| {
            vec![Event::reply(
                "MAIN",
                command,
                data,
                request.listener_id.clone(),
            )]
        };
        let native_error = |error: tauri::Error| WireError::new("window_error", error.to_string());
        match command {
            "MAXIMIZED" => return Ok(reply(json!(window.is_maximized().map_err(native_error)?))),
            "MINIMIZE" => {
                window.minimize().map_err(native_error)?;
                return Ok(vec![]);
            }
            "MAXIMIZE" => {
                if window.is_maximized().map_err(native_error)? {
                    window.unmaximize().map_err(native_error)?;
                } else {
                    window.maximize().map_err(native_error)?;
                }
                return Ok(reply(json!(window.is_maximized().map_err(native_error)?)));
            }
            "FULLSCREEN" => {
                window
                    .set_fullscreen(!window.is_fullscreen().map_err(native_error)?)
                    .map_err(native_error)?;
                return Ok(vec![]);
            }
            "CLOSE" => {
                window.app_handle().exit(0);
                return Ok(vec![]);
            }
            "GET_OS" => {
                return Ok(reply(
                    json!({"platform": match std::env::consts::OS { "windows" => "win32", "macos" => "darwin", other => other }, "arch": match std::env::consts::ARCH { "x86_64" => "x64", "aarch64" => "arm64", other => other }, "name": std::env::var("HOSTNAME").unwrap_or_default() }),
                ))
            }
            "GET_PATHS" => {
                let paths = window.app_handle().path();
                return Ok(reply(json!({
                    "pictures": paths.picture_dir().ok().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default(),
                    "videos": paths.video_dir().ok().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default(),
                    "music": paths.audio_dir().ok().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default()
                })));
            }
            _ => {}
        }
    }
    let label = window.label().to_string();
    let app = window.app_handle().clone();
    let command = request.data["channel"].as_str().unwrap_or("").to_string();
    let data = &request.data["data"];
    let automatic_backup = data["customTriggers"]["isAutoBackup"]
        .as_bool()
        .unwrap_or(false);
    // Only native pickers/openers use the blocking pool. Ordinary library work
    // awaits bounded admission without occupying a pool thread while queued.
    let needs_native = request.channel == "MAIN"
        && (matches!(
            command.as_str(),
            "URL"
                | "SYSTEM_OPEN"
                | "OPEN_FOLDER_PATH"
                | "OPEN_APPDATA"
                | "OPEN_CACHE"
                | "OPEN_LOG"
                | "OPEN_FOLDER"
                | "OPEN_FILE"
        ) || (command == "RESTORE"
            && data["path"].as_str().is_none_or(|path| path.is_empty())
            && data.get("paths").is_none()));
    let prepared = if needs_native {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || prepare_native(app, request))
            .await
            .map_err(|error| WireError::new("native_error", error.to_string()))??
    } else {
        Prepared::Forward(request)
    };
    let library = app.state::<AppState>().library.clone();
    let events = match prepared {
        Prepared::Forward(request) => library
            .dispatch(request, &label)
            .await
            .map_err(WireError::from)?,
        Prepared::Reply(events) => events,
        Prepared::Contextual {
            request,
            events,
            refresh_shows,
        } => {
            let mut events = contextual_events(&app, &request, events)?;
            if refresh_shows {
                let mut refreshed = Vec::new();
                for command in ["FULL_SHOWS_LIST", "SHOWS"] {
                    refreshed.extend(
                        library
                            .dispatch(
                                Request {
                                    channel: "MAIN".into(),
                                    data: json!({"channel":command}),
                                    listener_id: None,
                                    context: request.context.clone(),
                                },
                                &label,
                            )
                            .await
                            .map_err(WireError::from)?,
                    );
                }
                refreshed.append(&mut events);
                contextual_events(&app, &request, refreshed)?
            } else {
                events
            }
        }
    };
    if command == "SAVE" && !automatic_backup {
        let paths = events
            .iter()
            .filter(|event| event.message.channel == "BACKUP")
            .filter_map(|event| {
                event
                    .message
                    .data
                    .as_ref()?
                    .get("path")?
                    .as_str()
                    .map(String::from)
            })
            .collect::<Vec<_>>();
        if !paths.is_empty() {
            tauri::async_runtime::spawn_blocking(move || {
                for path in paths {
                    if app.opener().open_path(&path, None::<&str>).is_err() {
                        let _ = app.opener().reveal_item_in_dir(&path);
                    }
                }
            })
            .await
            .map_err(|error| WireError::new("native_error", error.to_string()))?;
        }
    }
    Ok(events)
}

enum Prepared {
    Forward(Request),
    Reply(Vec<Event>),
    Contextual {
        request: Request,
        events: Vec<Event>,
        refresh_shows: bool,
    },
}

fn prepare_native(app: tauri::AppHandle, mut request: Request) -> Result<Prepared, WireError> {
    let native_error = |error: String| WireError::new("native_error", error);
    let to_path = |file: tauri_plugin_dialog::FilePath| {
        file.into_path()
            .map_err(|error| native_error(error.to_string()))
    };
    let command = request.data["channel"].as_str().unwrap_or("").to_string();
    let data = request.data["data"].clone();
    if request.channel == "MAIN" {
        match command.as_str() {
            "URL" => {
                let url = data
                    .as_str()
                    .ok_or_else(|| native_error("URL must be a string".into()))?;
                app.opener()
                    .open_url(url, None::<&str>)
                    .map_err(|error| native_error(error.to_string()))?;
                return Ok(Prepared::Reply(vec![]));
            }
            "SYSTEM_OPEN" | "OPEN_FOLDER_PATH" | "OPEN_APPDATA" | "OPEN_CACHE" | "OPEN_LOG" => {
                let selected = if matches!(command.as_str(), "SYSTEM_OPEN" | "OPEN_FOLDER_PATH") {
                    PathBuf::from(
                        data.as_str()
                            .ok_or_else(|| native_error("path must be a string".into()))?,
                    )
                } else {
                    let state = app.state::<AppState>();
                    match command.as_str() {
                        "OPEN_CACHE" => state.app_data.join("Thumbnails"),
                        "OPEN_LOG" => state.error_log.clone(),
                        _ => state.app_data.clone(),
                    }
                };
                if !selected.exists() {
                    return Ok(Prepared::Reply(vec![Event::reply(
                        "MAIN",
                        "ALERT",
                        json!("This does not exist!"),
                        None,
                    )]));
                }
                if command == "SYSTEM_OPEN" {
                    app.opener()
                        .reveal_item_in_dir(&selected)
                        .map_err(|error| native_error(error.to_string()))?;
                } else if app
                    .opener()
                    .open_path(selected.to_string_lossy(), None::<&str>)
                    .is_err()
                {
                    app.opener()
                        .reveal_item_in_dir(&selected)
                        .map_err(|error| native_error(error.to_string()))?;
                }
                return Ok(Prepared::Reply(vec![]));
            }
            "OPEN_FOLDER" => {
                let mut picker = app.dialog().file();
                if let Some(parent) = app.get_webview_window("main") {
                    picker = picker.set_parent(&parent);
                }
                if let Some(title) = data["title"].as_str() {
                    picker = picker.set_title(title);
                }
                if let Some(path) = data["path"].as_str().filter(|path| !path.is_empty()) {
                    picker = picker.set_directory(path);
                }
                let selected = picker.blocking_pick_folder().map(to_path).transpose()?;
                let refresh_shows = selected.is_some() && data["channel"] == "SHOWS";
                let events = freeshow_core::dialog::folder_selected(&data, selected.as_deref())
                    .map_err(WireError::from)?;
                return Ok(Prepared::Contextual {
                    request,
                    events,
                    refresh_shows,
                });
            }
            "OPEN_FILE" | "RESTORE"
                if command == "OPEN_FILE"
                    || (data["path"].as_str().is_none_or(|path| path.is_empty())
                        && data.get("paths").is_none()) =>
            {
                let mut picker = app.dialog().file();
                if let Some(parent) = app.get_webview_window("main") {
                    picker = picker.set_parent(&parent);
                }
                if let Some(title) = data["title"].as_str() {
                    picker = picker.set_title(title);
                }
                if command == "RESTORE" {
                    let state = app.state::<AppState>();
                    let context = state.library.context().map_err(WireError::from)?;
                    picker = picker
                        .set_title("Restore FreeShow Backup")
                        .set_directory(context.data_path.join("Backups"))
                        .add_filter("FreeShow Backup Files", &["json", "zip"]);
                } else {
                    let extensions = data["filter"]["extensions"]
                        .as_array()
                        .map(|extensions| {
                            extensions
                                .iter()
                                .filter_map(|value| value.as_str())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if !extensions.is_empty() && !extensions.contains(&"*") {
                        picker = picker.add_filter(
                            data["filter"]["name"].as_str().unwrap_or("Files"),
                            &extensions,
                        );
                    }
                }
                let multiple = command == "RESTORE" || data["multiple"].as_bool().unwrap_or(true);
                let selected = if multiple {
                    picker.blocking_pick_files().unwrap_or_default()
                } else {
                    picker.blocking_pick_file().into_iter().collect()
                };
                let paths = selected
                    .into_iter()
                    .map(to_path)
                    .collect::<Result<Vec<_>, WireError>>()?;
                if command == "OPEN_FILE" {
                    let events = freeshow_core::dialog::files_selected(&data, &paths)
                        .map_err(WireError::from)?;
                    return Ok(Prepared::Contextual {
                        request,
                        events,
                        refresh_shows: false,
                    });
                }
                request.data["data"] = json!({"paths":paths});
            }
            _ => {}
        }
    }
    Ok(Prepared::Forward(request))
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_data = std::env::var_os("FREESHOW_RUST_APP_DATA")
                .map(PathBuf::from)
                .unwrap_or(app.path().app_data_dir()?);
            let data = std::env::var_os("FREESHOW_RUST_DATA_DIR").map(PathBuf::from);
            let runtime = Runtime::open(app_data.clone(), data)?;
            let error_log = runtime.stores.path("ERROR_LOG")?;
            app.manage(AppState {
                library: LibraryService::start(runtime)?,
                app_data,
                error_log,
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.emit(
                        "freeshow://ipc",
                        Event::reply("MAIN", "CLOSE", json!(false), None),
                    );
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            ipc_dispatch,
            runtime_context,
            library_status
        ])
        .run(tauri::generate_context!())
        .expect("FreeShow Rust could not start");
}
