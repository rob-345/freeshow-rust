#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use freeshow_core::{
    protocol::{Event, Request},
    runtime::Runtime,
    WireError,
};
use serde_json::json;
use std::{path::PathBuf, sync::Mutex};
use tauri::{Emitter, Manager};

struct AppState {
    runtime: Mutex<Runtime>,
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
        let native_error = |error: tauri::Error| WireError {
            code: "window_error",
            message: error.to_string(),
        };
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
    let state = window.app_handle().clone();
    // Filesystem and JSON work run off the GUI event loop. One mutex serializes
    // writes across windows so overlapping saves cannot race.
    tauri::async_runtime::spawn_blocking(move || {
        let state = state.state::<AppState>();
        let mut runtime = state.runtime.lock().map_err(|_| WireError {
            code: "runtime_error",
            message: "Runtime lock poisoned".into(),
        })?;
        runtime.dispatch(request, &label).map_err(WireError::from)
    })
    .await
    .map_err(|error| WireError {
        code: "runtime_error",
        message: error.to_string(),
    })?
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let app_data = std::env::var_os("FREESHOW_RUST_APP_DATA")
                .map(PathBuf::from)
                .unwrap_or(app.path().app_data_dir()?);
            let data = std::env::var_os("FREESHOW_RUST_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| app_data.join("Data"));
            app.manage(AppState {
                runtime: Mutex::new(Runtime::new(app_data, data)?),
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
        .invoke_handler(tauri::generate_handler![ipc_dispatch])
        .run(tauri::generate_context!())
        .expect("FreeShow Rust could not start");
}
