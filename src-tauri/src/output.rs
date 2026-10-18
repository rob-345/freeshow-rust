//! Native output windows have their own lifecycle queue. Cue/state forwarding
//! only takes a short registry snapshot and never joins either IO queue.
use crate::AppState;
use freeshow_core::{
    output::{self, Display, Endpoint, Rect},
    protocol::{Event, Request, RuntimeContext},
    Error, WireError,
};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use tauri::{Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindow};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone)]
struct Entry {
    endpoint: Endpoint,
    config: Value,
}

struct Registry {
    context: RuntimeContext,
    entries: Vec<Entry>,
    move_enabled: bool,
}

enum Job {
    Dispatch {
        app: tauri::AppHandle,
        request: Request,
        reply: oneshot::Sender<Result<Vec<Event>, WireError>>,
    },
    Reset {
        app: tauri::AppHandle,
        context: RuntimeContext,
        reply: oneshot::Sender<Result<Vec<Event>, WireError>>,
    },
}

#[derive(Clone)]
pub struct OutputService {
    jobs: mpsc::Sender<Job>,
    registry: Arc<RwLock<Registry>>,
}

fn failed(error: impl std::fmt::Display) -> WireError {
    WireError::new("output_error", error.to_string())
}

fn stale() -> WireError {
    WireError::from(Error::StaleLibrary {
        recovery_path: None,
        recovery_error: None,
    })
}

fn current(app: &tauri::AppHandle, request: &Request) -> Result<RuntimeContext, WireError> {
    let context = app
        .state::<AppState>()
        .library
        .context()
        .map_err(WireError::from)?;
    if request.context.as_ref() != Some(&context) {
        return Err(stale());
    }
    Ok(context)
}

impl OutputService {
    pub fn status(&self) -> Result<Value, WireError> {
        let state = self.registry.read().map_err(failed)?;
        Ok(
            json!({"context":state.context,"outputs":state.entries.iter().map(|entry|json!({"id":entry.endpoint.id,"label":entry.endpoint.label})).collect::<Vec<_>>()}),
        )
    }

    pub fn start(context: RuntimeContext) -> Result<Self, WireError> {
        let registry = Arc::new(RwLock::new(Registry {
            context,
            entries: Vec::new(),
            move_enabled: false,
        }));
        let shared = registry.clone();
        let (jobs, mut receiver) = mpsc::channel(8);
        // AppHandle is held only by admitted jobs, not by the idle worker. This
        // avoids an AppState -> sender -> permanent AppHandle ownership cycle.
        std::thread::Builder::new()
            .name("freeshow-outputs".into())
            .spawn(move || {
                while let Some(job) = receiver.blocking_recv() {
                    match job {
                        Job::Dispatch {
                            app,
                            request,
                            reply,
                        } => {
                            let _ = reply.send(lifecycle(&app, &shared, request));
                        }
                        Job::Reset {
                            app,
                            context,
                            reply,
                        } => {
                            let _ = reply.send(reset_windows(&app, &shared, context));
                        }
                    }
                }
            })
            .map_err(failed)?;
        Ok(Self { jobs, registry })
    }

    pub fn is_output(&self, label: &str) -> Result<bool, WireError> {
        Ok(self
            .registry
            .read()
            .map_err(failed)?
            .entries
            .iter()
            .any(|entry| entry.endpoint.label == label))
    }

    pub async fn dispatch(
        &self,
        app: tauri::AppHandle,
        request: Request,
    ) -> Result<Vec<Event>, WireError> {
        let command = request.data["channel"]
            .as_str()
            .ok_or_else(|| failed("output channel must be a string"))?;
        if matches!(
            command,
            "CREATE"
                | "REMOVE"
                | "TOGGLE_OUTPUTS"
                | "UPDATE_BOUNDS"
                | "SET_VALUE"
                | "FOCUS"
                | "TO_FRONT"
                | "MOVE"
        ) {
            let (reply, result) = oneshot::channel();
            self.jobs
                .send(Job::Dispatch {
                    app,
                    request,
                    reply,
                })
                .await
                .map_err(|_| failed("output lifecycle worker stopped"))?;
            return result
                .await
                .map_err(|_| failed("output lifecycle worker stopped before replying"))?;
        }
        if matches!(
            command,
            "ALIGN_WITH_SCREEN" | "REQUEST_PREVIEW" | "CAPTURE" | "IDENTIFY_SCREENS"
        ) {
            return Err(WireError::from(Error::Unsupported(format!(
                "OUTPUT.{command}"
            ))));
        }
        let context = current(&app, &request)?;
        let mut endpoints = {
            let state = self.registry.read().map_err(failed)?;
            if state.context != context {
                return Err(stale());
            }
            state
                .entries
                .iter()
                .map(|entry| entry.endpoint.clone())
                .collect::<Vec<_>>()
        };
        for endpoint in &mut endpoints {
            endpoint.available = app.get_webview_window(&endpoint.label).is_some();
        }
        for delivery in output::route(&request.data, &endpoints).map_err(WireError::from)? {
            app.emit_to(
                delivery.target,
                "freeshow://ipc",
                json!({"channel":"OUTPUT","message":delivery.message,"context":context}),
            )
            .map_err(failed)?;
        }
        Ok(Vec::new())
    }

    pub async fn reset(
        &self,
        app: tauri::AppHandle,
        context: RuntimeContext,
    ) -> Result<Vec<Event>, WireError> {
        let (reply, result) = oneshot::channel();
        self.jobs
            .send(Job::Reset {
                app,
                context,
                reply,
            })
            .await
            .map_err(|_| failed("output lifecycle worker stopped"))?;
        result
            .await
            .map_err(|_| failed("output lifecycle worker stopped before reset"))?
    }

    pub fn destroyed(&self, app: &tauri::AppHandle, label: &str) {
        let removed = self.registry.write().ok().and_then(|mut registry| {
            let index = registry
                .entries
                .iter()
                .position(|entry| entry.endpoint.label == label)?;
            let entry = registry.entries.remove(index);
            Some((entry.endpoint.id, registry.context.clone()))
        });
        if let Some((id, context)) = removed {
            let _ = app.emit_to(
                "main",
                "freeshow://ipc",
                state_event(vec![json!({"id":id,"active":false})], &context),
            );
        }
    }
}

fn state_event(states: Vec<Value>, context: &RuntimeContext) -> Event {
    let mut event = Event::reply("OUTPUT", "OUTPUT_STATE", json!(states), None);
    event.context = Some(context.clone());
    event
}

fn reset_windows(
    app: &tauri::AppHandle,
    registry: &RwLock<Registry>,
    context: RuntimeContext,
) -> Result<Vec<Event>, WireError> {
    let entries = {
        let mut state = registry.write().map_err(failed)?;
        if state.context == context {
            return Ok(Vec::new());
        }
        state.context = context.clone();
        std::mem::take(&mut state.entries)
    };
    let mut states = Vec::new();
    let mut failures = Vec::new();
    for entry in entries {
        states.push(json!({"id":entry.endpoint.id,"active":false}));
        if let Some(window) = app.get_webview_window(&entry.endpoint.label) {
            if let Err(error) = window.destroy() {
                failures.push(error.to_string());
            }
        }
    }
    let mut events = vec![state_event(states, &context)];
    if !failures.is_empty() {
        let mut event = Event::reply("MAIN", "ALERT", json!("The data library changed, but an old output window could not close. Its messages are blocked. Close that window before presenting."), None);
        event.context = Some(context);
        events.push(event);
        eprintln!("Output reset: {}", failures.join("; "));
    }
    Ok(events)
}

fn entry(registry: &RwLock<Registry>, id: &str) -> Result<Option<Entry>, WireError> {
    Ok(registry
        .read()
        .map_err(failed)?
        .entries
        .iter()
        .find(|entry| entry.endpoint.id == id)
        .cloned())
}

fn remove(app: &tauri::AppHandle, registry: &RwLock<Registry>, id: &str) -> Result<(), WireError> {
    let previous = {
        let mut state = registry.write().map_err(failed)?;
        state
            .entries
            .iter()
            .position(|entry| entry.endpoint.id == id)
            .map(|index| state.entries.remove(index))
    };
    if let Some(previous) = previous {
        if let Some(window) = app.get_webview_window(&previous.endpoint.label) {
            window.destroy().map_err(failed)?;
        }
    }
    Ok(())
}

fn id(data: &Value) -> Result<&str, WireError> {
    data["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| failed("output ID must be a nonempty string"))
}

fn desktop(app: &tauri::AppHandle) -> Result<(Vec<Display>, Rect), WireError> {
    let displays = app
        .available_monitors()
        .map_err(failed)?
        .into_iter()
        .enumerate()
        .map(|(index, monitor)| {
            let scale = monitor.scale_factor();
            let position = monitor.position().to_logical::<f64>(scale);
            let size = monitor.size().to_logical::<f64>(scale);
            Display {
                id: monitor
                    .name()
                    .cloned()
                    .unwrap_or_else(|| (index + 1).to_string()),
                bounds: Rect {
                    x: position.x,
                    y: position.y,
                    width: size.width,
                    height: size.height,
                },
            }
        })
        .collect();
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| failed("main window unavailable"))?;
    let scale = main.scale_factor().map_err(failed)?;
    let position = main
        .outer_position()
        .map_err(failed)?
        .to_logical::<f64>(scale);
    let size = main.outer_size().map_err(failed)?.to_logical::<f64>(scale);
    Ok((
        displays,
        Rect {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        },
    ))
}

fn bounds(app: &tauri::AppHandle, config: &Value, auto: bool) -> Result<Rect, WireError> {
    let (displays, main) = desktop(app)?;
    let result = output::resolve_bounds(config, &displays, main, auto, cfg!(target_os = "macos"))
        .map_err(WireError::from)?;
    result.validate().map_err(WireError::from)?;
    Ok(result)
}

fn set_bounds(window: &WebviewWindow, bounds: Rect) -> Result<(), WireError> {
    bounds.validate().map_err(WireError::from)?;
    window
        .set_position(LogicalPosition::new(bounds.x, bounds.y))
        .map_err(failed)?;
    window
        .set_size(LogicalSize::new(bounds.width, bounds.height))
        .map_err(failed)
}

fn create(
    app: &tauri::AppHandle,
    registry: &RwLock<Registry>,
    request: &Request,
    config: &Value,
) -> Result<Entry, WireError> {
    let id = id(config)?;
    // These variants require capture/native workers that have not been ported.
    if ["ndi", "omt", "blackmagic"]
        .iter()
        .any(|key| config[*key] == true)
        || config["webrtcData"]["streaming"] == true
        || config["rtmpData"]["streaming"] == true
    {
        return Err(WireError::from(Error::Unsupported(
            "OUTPUT.CREATE capture/streaming variant".into(),
        )));
    }
    let context = current(app, request)?;
    let rect = bounds(app, config, false)?;
    remove(app, registry, id)?;
    let item = Entry {
        endpoint: Endpoint {
            id: id.into(),
            label: output::new_label(),
            follower: false,
            available: true,
        },
        config: config.clone(),
    };
    {
        let mut state = registry.write().map_err(failed)?;
        if state.context != context {
            return Err(stale());
        }
        state.entries.push(item.clone());
    }
    let result = tauri::WebviewWindowBuilder::new(
        app,
        &item.endpoint.label,
        WebviewUrl::App("index.html".into()),
    )
    .title(format!(
        "FreeShow Output — {}",
        config["name"].as_str().unwrap_or(id)
    ))
    .position(rect.x, rect.y)
    .inner_size(rect.width, rect.height)
    .visible(false)
    .focused(false)
    .decorations(false)
    .shadow(false)
    .resizable(config["alwaysOnTop"] == false && config["boundsLocked"] != true)
    .always_on_top(config["alwaysOnTop"] != false)
    // Upstream makes non-topmost outputs ordinary taskbar windows.
    .skip_taskbar(config["alwaysOnTop"] != false)
    .transparent(config["transparent"] == true)
    .background_color(tauri::webview::Color(
        0,
        0,
        0,
        if config["transparent"] == true {
            0
        } else {
            255
        },
    ))
    .build();
    match result {
        Ok(window) => {
            if let Err(error) = current(app, request) {
                let _ = remove(app, registry, id);
                let _ = window.destroy();
                return Err(error);
            }
            Ok(item)
        }
        Err(error) => {
            let _ = remove(app, registry, id);
            Err(failed(error))
        }
    }
}

fn lifecycle(
    app: &tauri::AppHandle,
    registry: &RwLock<Registry>,
    request: Request,
) -> Result<Vec<Event>, WireError> {
    let context = current(app, &request)?;
    if registry.read().map_err(failed)?.context != context {
        return Err(stale());
    }
    let command = request.data["channel"].as_str().unwrap_or("");
    let data = &request.data["data"];
    match command {
        "CREATE" => {
            create(app, registry, &request, data)?;
        }
        "REMOVE" => {
            remove(app, registry, id(data)?)?;
        }
        "TOGGLE_OUTPUTS" => {
            let outputs = data["outputs"]
                .as_array()
                .ok_or_else(|| failed("outputs must be an array"))?;
            let mut states = Vec::new();
            for config in outputs {
                let output_id = id(config)?;
                let output = match entry(registry, output_id)? {
                    Some(output) if app.get_webview_window(&output.endpoint.label).is_some() => {
                        output
                    }
                    _ => create(app, registry, &request, config)?,
                };
                let window = app
                    .get_webview_window(&output.endpoint.label)
                    .ok_or_else(|| failed("output window unavailable"))?;
                let (displays, main) = desktop(app)?;
                let auto = data["autoPosition"] == true && config["boundsLocked"] != true;
                let rect = bounds(app, config, auto)?;
                let single_screen_startup = data["autoStartup"] == true && displays.len() <= 1;
                // Automatic startup on one screen leaves an already visible
                // window alone, matching the upstream early return.
                if single_screen_startup && config["invisible"] != true {
                    states.push(json!({"id":output_id,"active":false}));
                    continue;
                }
                let overlaps_main = output::amount_covered(rect, main) > 0.5
                    && auto
                    && window.is_always_on_top().map_err(failed)?;
                let active = data["state"] == true && !single_screen_startup && !overlaps_main;
                let invisible = config["invisible"] == true;
                if active && !invisible {
                    set_bounds(&window, rect)?;
                    window
                        .set_always_on_top(config["alwaysOnTop"] != false)
                        .map_err(failed)?;
                    window.show().map_err(failed)?;
                } else {
                    window.hide().map_err(failed)?;
                    set_bounds(&window, bounds(app, config, false)?)?;
                }
                states.push(json!({"id":output_id,"active":if invisible { json!("invisible") } else { json!(active) }}));
                if data["state"] == true && !active && !invisible && data["autoStartup"] != true {
                    let mut alert = Event::reply("MAIN", "ALERT", json!("error.display"), None);
                    alert.context = Some(context.clone());
                    app.emit_to("main", "freeshow://ipc", alert)
                        .map_err(failed)?;
                }
            }
            app.emit_to("main", "freeshow://ipc", state_event(states, &context))
                .map_err(failed)?;
        }
        "UPDATE_BOUNDS" => {
            if let Some(entry) = entry(registry, id(data)?)? {
                if let Some(window) = app.get_webview_window(&entry.endpoint.label) {
                    let rect: Rect =
                        serde_json::from_value(data["bounds"].clone()).map_err(failed)?;
                    set_bounds(&window, rect)?;
                    if let Some(entry) = registry
                        .write()
                        .map_err(failed)?
                        .entries
                        .iter_mut()
                        .find(|entry| entry.endpoint.id == id(data).unwrap_or_default())
                    {
                        entry.config["bounds"] = json!(rect);
                    }
                }
            }
        }
        "SET_VALUE" => {
            let output_id = id(data)?;
            let key = data["key"].as_str().unwrap_or("");
            if !matches!(key, "alwaysOnTop" | "boundsLocked") {
                return Err(WireError::from(Error::Unsupported(format!(
                    "OUTPUT.SET_VALUE {key}"
                ))));
            }
            if let Some(mut entry) = entry(registry, output_id)? {
                entry.config[key] = data["value"].clone();
                if let Some(window) = app.get_webview_window(&entry.endpoint.label) {
                    if key == "alwaysOnTop" {
                        window
                            .set_always_on_top(data["value"] == true)
                            .map_err(failed)?;
                        window
                            .set_skip_taskbar(data["value"] == true)
                            .map_err(failed)?;
                    }
                    window
                        .set_resizable(
                            entry.config["alwaysOnTop"] == false
                                && entry.config["boundsLocked"] != true,
                        )
                        .map_err(failed)?;
                }
                if let Some(stored) = registry
                    .write()
                    .map_err(failed)?
                    .entries
                    .iter_mut()
                    .find(|entry| entry.endpoint.id == output_id)
                {
                    stored.config = entry.config;
                }
            }
        }
        "FOCUS" | "TO_FRONT" => {
            let output_id = if command == "TO_FRONT" {
                data.as_str()
                    .ok_or_else(|| failed("output ID must be a string"))?
            } else {
                id(data)?
            };
            if let Some(entry) = entry(registry, output_id)? {
                if let Some(window) = app.get_webview_window(&entry.endpoint.label) {
                    window.set_focus().map_err(failed)?;
                }
            }
        }
        "MOVE" => {
            let mut state = registry.write().map_err(failed)?;
            state.move_enabled = data["enabled"] == true;
            let move_enabled = state.move_enabled;
            let entries = state.entries.clone();
            drop(state);
            for entry in entries {
                if let Some(window) = app.get_webview_window(&entry.endpoint.label) {
                    window
                        .set_resizable(
                            move_enabled
                                && entry.config["boundsLocked"] != true
                                && entry.config["invisible"] != true,
                        )
                        .map_err(failed)?;
                }
            }
        }
        _ => return Err(failed("invalid output lifecycle command")),
    }
    Ok(Vec::new())
}
