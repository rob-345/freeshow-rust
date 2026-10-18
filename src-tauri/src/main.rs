#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod browse;
mod drop;
mod files;
mod imports;
mod menu;
mod output;
mod system;

use freeshow_audio::{AudioConfig, AudioService};
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::{LibraryService, LibraryStatus},
    WireError,
};
use freeshow_files::{FilesService, Limits as FilesLimits};
use freeshow_media::{MediaConfig, MediaService};
use freeshow_network::{OscSender, OscSenderConfig};
use freeshow_system::{Limits as SystemLimits, SystemService};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

struct AppState {
    library: LibraryService,
    outputs: output::OutputService,
    media: MediaService,
    audio: AudioService,
    files: files::NativeFiles,
    imports: imports::NativeImports,
    browse: browse::NativeBrowse,
    system: SystemService,
    api: api::NativeApi,
    osc: OscSender,
    app_data: PathBuf,
    error_log: PathBuf,
    allow_exit: AtomicBool,
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

#[tauri::command]
fn output_status(app: tauri::AppHandle) -> Result<serde_json::Value, WireError> {
    app.state::<AppState>().outputs.status()
}

#[tauri::command]
fn imports_status(app: tauri::AppHandle) -> serde_json::Value {
    app.state::<AppState>().imports.status()
}

#[tauri::command]
fn api_status(app: tauri::AppHandle) -> serde_json::Value {
    app.state::<AppState>().api.status()
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
    let app = window.app_handle();
    if request.channel == "STARTUP" && request.data["channel"] == "READY" {
        let state = app.state::<AppState>();
        let role = if window.label() == "main" {
            serde_json::Value::Null
        } else if state.outputs.is_output(window.label())? {
            json!("output")
        } else {
            return Err(WireError::new("window_error", "Unknown window role"));
        };
        let mut event = Event::reply("STARTUP", "TYPE", role, request.listener_id);
        event.message.auto_profile = Some(String::new());
        event.context = Some(state.library.context().map_err(WireError::from)?);
        return Ok(vec![event]);
    }
    if request.channel == "LOADED" {
        return Ok(Vec::new());
    }
    if request.channel == "OUTPUT" {
        return app
            .state::<AppState>()
            .outputs
            .clone()
            .dispatch(app.clone(), request)
            .await;
    }
    if request.channel == "MAIN" {
        let command = request.data["channel"].as_str().unwrap_or("");
        if api::COMMANDS.contains(&command) {
            return app
                .state::<AppState>()
                .api
                .dispatch(request, window.label())
                .await;
        }
        if browse::COMMANDS.contains(&command) {
            let state = app.state::<AppState>();
            return state
                .browse
                .dispatch(&state.library, request, window.label())
                .await;
        }
        if imports::COMMANDS.contains(&command) {
            let state = app.state::<AppState>();
            return state
                .imports
                .dispatch(&state.library, request, window.label(), |event| {
                    app.emit_to("main", "freeshow://ipc", event)
                        .map_err(|error| error.to_string())
                })
                .await;
        }
        if system::COMMANDS.contains(&command) {
            let state = app.state::<AppState>();
            return system::dispatch(&state.library, &state.system, request).await;
        }
        if files::COMMANDS.contains(&command) {
            let state = app.state::<AppState>();
            return files::dispatch(&state.library, &state.files, request, window.label()).await;
        }
        if command == "EMIT_OSC" {
            let state = app.state::<AppState>();
            state
                .library
                .contextualize(&request, &mut [])
                .map_err(WireError::from)?;
            state
                .osc
                .emit(&request.data["data"])
                .await
                .map_err(|error| {
                    WireError::new("osc_error", format!("{:?}: {}", error.kind, error.message))
                })?;
            return Ok(vec![]);
        }
        if freeshow_media::COMMANDS.contains(&command) {
            let state = app.state::<AppState>();
            state
                .library
                .contextualize(&request, &mut [])
                .map_err(WireError::from)?;
            let result = state
                .media
                .dispatch(command, request.data["data"].clone())
                .await
                .map_err(|error| WireError::new(error.code(), error.to_string()))?;
            for diagnostic in &result.diagnostics {
                eprintln!("{}: {}", diagnostic.code, diagnostic.message);
            }
            if command != "FFMPEG_CHECK" && !result.diagnostics.is_empty() {
                return Err(WireError::new(
                    "media_capability_error",
                    result
                        .diagnostics
                        .iter()
                        .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
                        .collect::<Vec<_>>()
                        .join("; "),
                ));
            }
            return contextual_events(
                app,
                &request,
                vec![Event::reply(
                    "MAIN",
                    command,
                    result.data,
                    request.listener_id.clone(),
                )],
            );
        }
        if freeshow_audio::COMMANDS.contains(&command) {
            let state = app.state::<AppState>();
            state
                .library
                .contextualize(&request, &mut [])
                .map_err(WireError::from)?;
            let result = state
                .audio
                .dispatch(command, request.data["data"].clone())
                .await
                .map_err(|error| WireError::new(error.code(), error.to_string()))?;
            if !result.diagnostics.is_empty() {
                return Err(WireError::new(
                    "audio_capability_error",
                    result
                        .diagnostics
                        .iter()
                        .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
                        .collect::<Vec<_>>()
                        .join("; "),
                ));
            }
            return contextual_events(
                app,
                &request,
                vec![Event::reply(
                    "MAIN",
                    command,
                    result.data,
                    request.listener_id.clone(),
                )],
            );
        }
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
            "LANGUAGE" => {
                menu::set_language(app, &request.data["data"]["strings"])?;
                return Ok(vec![]);
            }
            "MAXIMIZED" => return Ok(reply(json!(window.is_maximized().map_err(native_error)?))),
            "MINIMIZE" => {
                window.minimize().map_err(native_error)?;
                return Ok(vec![]);
            }
            "MAXIMIZE" => {
                let was_maximized = window.is_maximized().map_err(native_error)?;
                if was_maximized {
                    window.unmaximize().map_err(native_error)?;
                } else {
                    window.maximize().map_err(native_error)?;
                }
                // Electron announces the requested transition on MAXIMIZED.
                // Window-manager state changes may complete asynchronously.
                return Ok(vec![Event::reply(
                    "MAIN",
                    "MAXIMIZED",
                    json!(!was_maximized),
                    request.listener_id.clone(),
                )]);
            }
            "FULLSCREEN" => {
                window
                    .set_fullscreen(!window.is_fullscreen().map_err(native_error)?)
                    .map_err(native_error)?;
                return Ok(vec![]);
            }
            "CLOSE" => {
                let state = app.state::<AppState>();
                let discard = request.data["data"]["discardUnsaved"].as_bool() == Some(true);
                let drained = state.library.finish_writes().await;
                if !discard {
                    drained.map_err(WireError::from)?;
                }
                let (
                    _,
                    _,
                    _,
                    files_result,
                    system_result,
                    api_result,
                    imports_result,
                    browse_result,
                ) = tokio::join!(
                    state.media.shutdown(),
                    state.audio.shutdown(),
                    state.osc.shutdown(),
                    state.files.shutdown(),
                    state.system.shutdown(),
                    state.api.shutdown(),
                    state.imports.shutdown(),
                    state.browse.shutdown()
                );
                files_result.map_err(|error| WireError::new("files_worker", error.to_string()))?;
                system_result
                    .map_err(|error| WireError::new("system_worker", error.to_string()))?;
                api_result?;
                imports_result?;
                browse_result?;
                state.allow_exit.store(true, Ordering::Release);
                app.exit(0);
                return Ok(vec![]);
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
    let mut events = match prepared {
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
    if command == "UPDATE_DATA_PATH" {
        let context = library.context().map_err(WireError::from)?;
        app.state::<AppState>().files.context_changed(&context);
        app.state::<AppState>().imports.context_changed(&context);
        app.state::<AppState>().browse.context_changed(&context);
        if let Err(error) = app.state::<AppState>().api.context_changed().await {
            let mut alert = Event::reply(
                "MAIN",
                "ALERT",
                json!(format!(
                    "The library changed, but API restart failed: {}",
                    error.message
                )),
                None,
            );
            alert.context = Some(context.clone());
            events.push(alert);
        }
        match app
            .state::<AppState>()
            .outputs
            .clone()
            .reset(app.clone(), context.clone())
            .await
        {
            Ok(reset) => events.extend(reset),
            Err(error) => {
                // The library commit already succeeded. Deliver its new path
                // even if a failed output worker cannot clean up old windows.
                let mut alert = Event::reply(
                    "MAIN",
                    "ALERT",
                    json!(format!(
                        "The library changed, but output cleanup failed: {}",
                        error.message
                    )),
                    None,
                );
                alert.context = Some(context);
                events.push(alert);
            }
        }
    }
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
            let outputs = output::OutputService::start(runtime.context())
                .map_err(|error| std::io::Error::other(error.message))?;
            let media = MediaService::new(MediaConfig {
                local_bin_dir: Some(app_data.join("bin")),
                ffmpeg_candidates: std::env::var_os("FREESHOW_RUST_FFMPEG")
                    .map(PathBuf::from)
                    .into_iter()
                    .collect(),
                ffprobe_candidates: std::env::var_os("FREESHOW_RUST_FFPROBE")
                    .map(PathBuf::from)
                    .into_iter()
                    .collect(),
                ..MediaConfig::default()
            })?;
            let osc = tauri::async_runtime::block_on(async {
                OscSender::start(OscSenderConfig::default())
            })?;
            let audio = AudioService::new(AudioConfig::default())?;
            let files = files::NativeFiles::new(FilesService::start(FilesLimits::default())?);
            let system = SystemService::start(SystemLimits::default())?;
            let imports = imports::NativeImports::new(freeshow_imports::ImportService::new(
                freeshow_imports::ImportConfig::default(),
            )?);
            let browse = browse::NativeBrowse::new(freeshow_browse::BrowseService::new(
                freeshow_browse::BrowseConfig::default(),
            )?);
            let library = LibraryService::start(runtime)?;
            let mut api_config = freeshow_api::ApiConfig::default();
            if let Ok(ip) = std::env::var("FREESHOW_RUST_API_BIND_IP") {
                api_config.bind_ip = ip.parse()?;
            }
            let api_app = app.handle().clone();
            let api = tauri::async_runtime::block_on(async {
                api::NativeApi::new(library.clone(), api_config, move |event| {
                    api_app
                        .emit_to("main", "freeshow://ipc", event)
                        .map_err(|error| error.to_string())
                })
            })
            .map_err(|error| std::io::Error::other(error.message))?;
            app.manage(AppState {
                library,
                outputs,
                media,
                audio,
                files,
                imports,
                browse,
                system,
                api,
                osc,
                app_data,
                error_log,
                allow_exit: AtomicBool::new(false),
            });
            menu::install(app.handle()).map_err(|error| std::io::Error::other(error.message))?;
            let main = app
                .get_webview_window("main")
                .ok_or_else(|| std::io::Error::other("Main WebView is unavailable"))?;
            drop::install(&main, app.state::<AppState>().library.clone())
                .map_err(|error| std::io::Error::other(error.message))?;
            Ok(())
        })
        .on_menu_event(menu::on_menu_event)
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.emit_to(
                        "main",
                        "freeshow://ipc",
                        Event::reply("MAIN", "CLOSE", json!(false), None),
                    );
                }
            }
            if matches!(event, tauri::WindowEvent::Destroyed) {
                if let Some(state) = window.app_handle().try_state::<AppState>() {
                    state.outputs.destroyed(window.app_handle(), window.label());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            ipc_dispatch,
            runtime_context,
            library_status,
            output_status,
            api_status,
            imports_status
        ])
        .build(tauri::generate_context!())
        .expect("FreeShow Rust could not start")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if let Some(state) = app.try_state::<AppState>() {
                    if !state.allow_exit.load(Ordering::Acquire) {
                        api.prevent_exit();
                        let _ = app.emit_to(
                            "main",
                            "freeshow://ipc",
                            Event::reply("MAIN", "CLOSE", json!(false), None),
                        );
                    }
                }
            }
        });
}
