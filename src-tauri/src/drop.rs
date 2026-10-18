//! Wry drag coordinates originate at its WebView widget, including when Tauri
//! forwards them as synthesized WindowEvent::DragDrop events.
#[path = "drop_spec.rs"]
mod spec;

use freeshow_core::{protocol::RuntimeContext, service::LibraryService, WireError};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tauri::{DragDropEvent, Emitter, WebviewWindow, WindowEvent};
use tokio::sync::Semaphore;

fn failed(error: impl std::fmt::Display) -> WireError {
    WireError::new("native_drop_error", error.to_string())
}

fn emit(window: &WebviewWindow, payload: Value) {
    if let Err(error) = window.emit_to("main", "freeshow://drop", payload) {
        eprintln!("Native file-drop delivery failed: {error}");
    }
}

/// Install once after the main WebviewWindow and LibraryService exist. Native
/// callbacks only validate bounded strings/read context and dispatch events.
/// Filesystem metadata uses at most two admitted blocking workers, never GTK.
pub fn install(
    window: &WebviewWindow,
    library: LibraryService,
    assets: crate::assets::NativeAssets,
) -> Result<(), WireError> {
    if window.label() != "main" {
        return Err(failed(
            "Native file drops are supported only in the main window",
        ));
    }
    let window_handle = window.clone();
    let state = Arc::new(Mutex::new(spec::DragSession::<RuntimeContext>::new()));
    let workers = Arc::new(Semaphore::new(2));
    window.on_window_event(move |event| {
        let WindowEvent::DragDrop(event) = event else { return; };
        let Ok(mut state_guard) = state.lock() else {
            eprintln!("Native file-drop state was poisoned");
            return;
        };
        let result = (|| -> Result<Option<Value>, WireError> {
            if matches!(event, DragDropEvent::Enter { .. }) {
                state_guard.enter(library.context().map_err(WireError::from)?);
            }
            let Some(gesture) = state_guard.current() else { return Ok(None); };
            let mut payload = json!({"sequence":gesture.sequence,"context":gesture.context});
            if library.context().map_err(WireError::from)? != gesture.context {
                payload["phase"] = json!("leave");
                state_guard.leave();
                return Ok(Some(payload));
            }
            let (phase, paths, position) = match event {
                DragDropEvent::Enter { paths, position } => ("enter", Some(paths), Some(position)),
                DragDropEvent::Over { position } => ("over", None, Some(position)),
                DragDropEvent::Drop { paths, position } => ("drop", Some(paths), Some(position)),
                DragDropEvent::Leave => ("leave", None, None),
                _ => return Ok(None),
            };
            payload["phase"] = json!(phase);
            if let Some(paths) = paths { payload["files"] = json!(spec::path_descriptors(paths).map_err(failed)?); }
            if let Some(position) = position {
                payload["position"] = json!({"x":position.x,"y":position.y,"units":if cfg!(target_os="windows") {"physical"} else {"logical"},"scaleFactor":window_handle.scale_factor().map_err(failed)?});
            }
            if phase == "drop" {
                let permit = workers.clone().try_acquire_owned().map_err(|_| failed("Native file-drop metadata workers are busy; try the drop again"))?;
                let paths = paths.unwrap().clone();
                let window = window_handle.clone();
                let library = library.clone();
                let assets = assets.clone();
                let state = state.clone();
                state_guard.accept_drop();
                tauri::async_runtime::spawn(async move {
                    let metadata = tauri::async_runtime::spawn_blocking(move || {
                        let _permit = permit;
                        spec::file_descriptors(&paths)
                    }).await;
                    // The library and native gesture may have changed while
                    // filesystem metadata was pending. Never retag old paths.
                    let current = library.context().ok();
                    let admitted = state.lock().ok().is_some_and(|state| state.accepts_completion(gesture.sequence));
                    if current.as_ref() != Some(&gesture.context) || !admitted {
                        payload["phase"] = json!("leave");
                        emit(&window, payload);
                        return;
                    }
                    match metadata {
                        Ok(Ok(files)) => {
                            payload["files"] = json!(files);
                            match assets.drop_grants(&library, &gesture.context, &payload["files"]).await {
                                Ok(grants) => payload["nativeAssets"] = grants,
                                Err(error) => { payload["phase"] = json!("error"); payload["error"] = json!(error); }
                            }
                        },
                        error => {
                            payload["phase"] = json!("error");
                            payload["error"] = json!(failed(format!("Unable to read dropped-file metadata: {error:?}")));
                        }
                    }
                    if library.context().ok().as_ref() != Some(&gesture.context) || !state.lock().ok().is_some_and(|state| state.accepts_completion(gesture.sequence)) {
                        payload["phase"] = json!("leave");
                        payload.as_object_mut().unwrap().remove("nativeAssets");
                    }
                    emit(&window, payload);
                });
                return Ok(None);
            }
            if phase == "leave" { state_guard.leave(); }
            Ok(Some(payload))
        })();
        match result {
            Ok(Some(payload)) => emit(&window_handle, payload),
            Ok(None) => {}
            Err(error) => {
                let context = state_guard.current().map(|gesture| gesture.context);
                let payload = json!({"phase":"error","sequence":state_guard.sequence,"context":context,"error":error});
                state_guard.leave();
                emit(&window_handle, payload);
            }
        }
    });
    Ok(())
}
