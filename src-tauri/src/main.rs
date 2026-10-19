#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod assets;
mod audio_ingress;
mod audio_input;
mod audio_pipeline;
mod browse;
mod context_changes;
mod discovery;
#[cfg(target_os = "linux")]
mod documents_path;
mod drop;
mod encoder_inputs;
mod encoders;
mod ffmpeg_download;
mod file_reads;
mod files;
mod imports;
mod media_copy;
mod menu;
mod midi;
mod network_queries;
mod network_snapshot;
mod output;
mod remotes;
mod scriptures;
mod system;
mod thumbnails;
mod timecode;
mod timecode_input;

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
    context_changes: context_changes::ContextChanges,
    outputs: output::OutputService,
    media: MediaService,
    ffmpeg_downloads: ffmpeg_download::NativeFfmpegDownloads,
    encoders: encoders::NativeEncoders,
    scriptures: scriptures::NativeScriptures,
    media_copy: media_copy::NativeMediaCopy,
    audio: AudioService,
    audio_pipeline: audio_pipeline::NativeAudioPipeline,
    audio_ingress: audio_ingress::AudioIngress,
    audio_last_receipt: std::sync::Mutex<serde_json::Value>,
    midi: midi::NativeMidi,
    timecode: timecode::NativeTimecode,
    timecode_observers: tokio::sync::Semaphore,
    files: files::NativeFiles,
    file_reads: file_reads::NativeFileReads,
    imports: imports::NativeImports,
    browse: browse::NativeBrowse,
    system: SystemService,
    network_snapshots: network_snapshot::NetworkSnapshots,
    network_queries: network_queries::NativeNetworkQueries,
    api: api::NativeApi,
    remotes: remotes::NativeRemotes,
    assets: assets::NativeAssets,
    thumbnails: thumbnails::NativeThumbnails,
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
fn assets_status(app: tauri::AppHandle) -> serde_json::Value {
    app.state::<AppState>().assets.status()
}

#[tauri::command]
fn thumbnails_status(app: tauri::AppHandle) -> serde_json::Value {
    app.state::<AppState>().thumbnails.status()
}

#[tauri::command]
fn file_reads_status(app: tauri::AppHandle) -> serde_json::Value {
    app.state::<AppState>().file_reads.status()
}

#[tauri::command]
fn scriptures_status(app: tauri::AppHandle) -> serde_json::Value {
    app.state::<AppState>().scriptures.status()
}

#[tauri::command]
fn media_copy_status(app: tauri::AppHandle) -> serde_json::Value {
    app.state::<AppState>().media_copy.status()
}

#[tauri::command]
fn encoders_status(app: tauri::AppHandle) -> serde_json::Value {
    app.state::<AppState>().encoders.status()
}

#[tauri::command]
async fn thumbnail_complete(
    window: tauri::WebviewWindow,
    request: tauri::ipc::Request<'_>,
) -> Result<(), WireError> {
    let id = request
        .headers()
        .get("x-freeshow-capture")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| WireError::new("invalid_capture_completion", "Missing native capture ID"))?;
    let failed = match request
        .headers()
        .get("x-freeshow-capture-failed")
        .and_then(|value| value.to_str().ok())
    {
        Some("0") => false,
        Some("1") => true,
        _ => {
            return Err(WireError::new(
                "invalid_capture_completion",
                "Invalid native capture state",
            ))
        }
    };
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err(WireError::new(
            "invalid_capture_completion",
            "Native capture requires raw bytes",
        ));
    };
    let state = window.app_handle().state::<AppState>();
    state
        .thumbnails
        .complete(&state.library, window.label(), id, bytes, failed)
}

#[tauri::command]
fn api_status(app: tauri::AppHandle) -> serde_json::Value {
    app.state::<AppState>().api.status()
}

#[tauri::command]
fn remotes_status(app: tauri::AppHandle) -> serde_json::Value {
    app.state::<AppState>().remotes.status()
}

#[tauri::command]
fn network_queries_status(app: tauri::AppHandle) -> serde_json::Value {
    app.state::<AppState>().network_queries.status()
}

#[tauri::command]
fn ffmpeg_download_status(app: tauri::AppHandle) -> serde_json::Value {
    app.state::<AppState>().ffmpeg_downloads.status()
}

#[tauri::command]
fn midi_status(app: tauri::AppHandle) -> serde_json::Value {
    app.state::<AppState>().midi.status()
}

fn audio_window(window: &tauri::WebviewWindow) -> Result<(), WireError> {
    if window.label() != "main" {
        return Err(WireError::new(
            "window_error",
            "AUDIO input requires the main capture owner",
        ));
    }
    Ok(())
}

#[tauri::command]
async fn audio_pcm(
    window: tauri::WebviewWindow,
    request: tauri::ipc::Request<'_>,
) -> Result<(), WireError> {
    audio_window(&window)?;
    let (ticket, origin) = {
        let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
            return Err(WireError::new(
                "audio_raw_body",
                "AUDIO requires its bounded raw frame",
            ));
        };
        let input = audio_input::parse(bytes).map_err(|e| WireError::new(e.code, e.message))?;
        // Framing provenance is private evidence, never hardware/codec authority.
        let ticket = window
            .app_handle()
            .state::<AppState>()
            .audio_ingress
            .submit(
                &input.context,
                input.pcm,
                &input.metadata,
                &audio_pipeline::SinkAvailability::default(),
                tokio_util::sync::CancellationToken::new(),
            )
            .map_err(WireError::from)?;
        (ticket, input.origin)
    };
    let observation = ticket.finish().await.map_err(WireError::from)?;
    let evidence = observation.inspect(|receipt| json!({"origin":origin,"context":receipt.context(),"sequence":receipt.sequence(),"effects":receipt.effects(),"diagnostics":receipt.diagnostics(),"failure":receipt.failure()})).map_err(WireError::from)?;
    *window
        .app_handle()
        .state::<AppState>()
        .audio_last_receipt
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = evidence;
    observation.into_void().map_err(WireError::from)
}

#[tauri::command]
async fn audio_disconnect(
    window: tauri::WebviewWindow,
    context: RuntimeContext,
) -> Result<(), WireError> {
    audio_window(&window)?;
    let ticket = window
        .app_handle()
        .state::<AppState>()
        .audio_ingress
        .disconnect_icecast(&context, tokio_util::sync::CancellationToken::new())
        .map_err(WireError::from)?;
    ticket
        .finish()
        .await
        .map_err(WireError::from)?
        .into_void()
        .map_err(WireError::from)
}

#[tauri::command]
async fn audio_close_context(
    window: tauri::WebviewWindow,
    context: RuntimeContext,
) -> Result<(), WireError> {
    audio_window(&window)?;
    let drain = window
        .app_handle()
        .state::<AppState>()
        .audio_ingress
        .close_context(&context);
    drain.await.map_err(WireError::from)
}

#[tauri::command]
async fn audio_capability(app: tauri::AppHandle) -> Result<freeshow_opus::Capability, WireError> {
    app.state::<AppState>()
        .audio_pipeline
        .capability()
        .await
        .map_err(WireError::from)
}

#[tauri::command]
fn audio_status(app: tauri::AppHandle) -> serde_json::Value {
    let state = app.state::<AppState>();
    json!({"host":state.audio_pipeline.status(),"ingress":state.audio_ingress.status(),"lastReceipt":*state.audio_last_receipt.lock().unwrap_or_else(|e|e.into_inner())})
}

#[tauri::command]
fn audio_status_complete(
    window: tauri::WebviewWindow,
    context: RuntimeContext,
    id: String,
    accepted: bool,
) -> Result<(), WireError> {
    window
        .app_handle()
        .state::<AppState>()
        .audio_pipeline
        .complete_status(window.label(), &context, &id, accepted)
        .map_err(WireError::from)
}

fn consume_timecode_receipt(receipt: timecode::TimecodeReceipt) {
    // Source TIMECODE_STATUS only logs. These private observations never become
    // retained source ACKs, and the whole result lease lives through consumption.
    for observation in receipt.observations() {
        match observation {
            timecode::Observation::LogStatus { data } => {
                println!("Timecode status [{:?}]: {data:?}", receipt.context());
            }
            timecode::Observation::Diagnostic { code, message } => {
                eprintln!("Timecode [{:?}]: {code}: {message}", receipt.context());
            }
            timecode::Observation::Scheduled { .. } => (),
        }
    }
}

#[tauri::command]
async fn timecode_command(
    window: tauri::WebviewWindow,
    request: tauri::ipc::Request<'_>,
) -> Result<(), WireError> {
    audio_window(&window)?;
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err(WireError::new(
            "timecode_raw_body",
            "Timecode requires its bounded typed frame",
        ));
    };
    let input =
        timecode_input::parse_command(bytes).map_err(|e| WireError::new(e.code, e.message))?;
    let state = window.app_handle().state::<AppState>();
    let cancel = tokio_util::sync::CancellationToken::new();
    let receipt = match input.command {
        freeshow_timecode::Command::Value(value) => {
            state.timecode.value(&input.context, value, cancel).await?
        }
        command => {
            state
                .timecode
                .command(&input.context, command, cancel)
                .await?
        }
    };
    consume_timecode_receipt(receipt);
    Ok(())
}

#[tauri::command]
async fn timecode_audio(
    window: tauri::WebviewWindow,
    request: tauri::ipc::Request<'_>,
) -> Result<(), WireError> {
    audio_window(&window)?;
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err(WireError::new(
            "timecode_raw_body",
            "LTC requires its bounded raw frame",
        ));
    };
    let input = timecode_input::parse_ltc(bytes).map_err(|e| WireError::new(e.code, e.message))?;
    let receipt = window
        .app_handle()
        .state::<AppState>()
        .timecode
        .audio(
            &input.context,
            input.mode,
            input.bytes,
            input.bytes.len(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await?;
    consume_timecode_receipt(receipt);
    Ok(())
}

#[tauri::command]
fn timecode_complete(
    window: tauri::WebviewWindow,
    context: RuntimeContext,
    id: String,
    accepted: bool,
) -> Result<(), WireError> {
    window.app_handle().state::<AppState>().timecode.complete(
        window.label(),
        &context,
        &id,
        accepted,
    )
}

#[tauri::command]
fn timecode_status(window: tauri::WebviewWindow) -> Result<serde_json::Value, WireError> {
    audio_window(&window)?;
    Ok(window.app_handle().state::<AppState>().timecode.status())
}

#[tauri::command]
async fn timecode_retire(
    window: tauri::WebviewWindow,
    context: RuntimeContext,
    terminal: bool,
) -> Result<(), WireError> {
    audio_window(&window)?;
    let state = window.app_handle().state::<AppState>();
    let current = state.library.context().map_err(WireError::from)?;
    // A nonterminal hint cannot select a native run: native current-context
    // checks choose retirement. Terminal close deliberately drains the process.
    if context.runtime_id != current.runtime_id
        || context.library_generation > current.library_generation
        || (context.library_generation == current.library_generation && context != current)
        || context.data_path.as_os_str().as_encoded_bytes().len() > 32768
        || context
            .data_path
            .as_os_str()
            .as_encoded_bytes()
            .contains(&0)
    {
        return Err(WireError::new(
            "stale_library",
            "Foreign timecode retirement context",
        ));
    }
    let _observer = state.timecode_observers.try_acquire().map_err(|_| {
        WireError::new(
            "timecode_control_busy",
            "Timecode lifecycle observers are full",
        )
    })?;
    if terminal {
        state.timecode.shutdown().await
    } else {
        state.timecode.context_changed().await
    }
}

fn timecode_event(publication: timecode::NativeTimecodePublication) -> Event {
    let mut event = publication.event;
    let mut metadata = json!({"hydration":publication.hydration,"delivery":publication.delivery});
    if let Some(bytes) = publication.bytes {
        metadata["bytes"] = json!(bytes);
    }
    event.native_timecode = Some(metadata);
    event
}

#[tauri::command]
fn midi_delivery_complete(
    window: tauri::WebviewWindow,
    id: String,
    accepted: bool,
) -> Result<(), WireError> {
    window
        .app_handle()
        .state::<AppState>()
        .midi
        .complete(window.label(), &id, accepted)
}

fn midi_event(publication: midi::NativeMidiPublication) -> Event {
    let mut event = publication.event;
    let mut metadata = json!({"hydration":publication.hydration});
    if let Some(delivery) = publication.delivery {
        metadata["delivery"] = json!({"id":delivery.id,"context":delivery.context,"run":delivery.run.to_string(),"listener":delivery.listener.to_string()});
    }
    event.native_midi = Some(metadata);
    event
}

#[tauri::command]
fn remote_delivery_complete(
    window: tauri::WebviewWindow,
    id: String,
    accepted: bool,
) -> Result<(), WireError> {
    window
        .app_handle()
        .state::<AppState>()
        .remotes
        .complete(window.label(), &id, accepted)
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
    let app = window.app_handle().clone();
    let assets = app.state::<AppState>().assets.clone();
    let lease = assets.begin(&app.state::<AppState>().library, &request)?;
    let mut events = if request.channel == "MAIN" && request.data["channel"] == "UPDATE_DATA_PATH" {
        app.state::<AppState>()
            .context_changes
            .run(async move { dispatch_native(window, request).await })
            .await?
    } else {
        dispatch_native(window, request).await?
    };
    if let Some(lease) = lease {
        assets
            .complete(&app.state::<AppState>().library, lease, &mut events)
            .await?;
    }
    // The bridge installs listeners before STARTUP. Its first role response
    // carries the current bounded replay, including for newly created outputs.
    for event in &mut events {
        if event.channel == "STARTUP" && event.message.channel == "TYPE" {
            if let Some(current) = &event.context {
                event.native_assets = Some(assets.replay(current)?);
            }
        }
    }
    Ok(events)
}

async fn dispatch_native(
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
    if freeshow_remotes::ServerKind::ALL
        .iter()
        .any(|kind| kind.channel() == request.channel)
        || request.channel == "MAIN"
            && remotes::COMMANDS.contains(&request.data["channel"].as_str().unwrap_or(""))
    {
        return app
            .state::<AppState>()
            .remotes
            .dispatch(request, window.label())
            .await;
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
        if timecode::COMMANDS.contains(&command) {
            if command == "TIMECODE_AUDIO_DATA" {
                return Err(WireError::new(
                    "timecode_raw_body",
                    "LTC input requires the main-window raw adapter",
                ));
            }
            let receipt = app
                .state::<AppState>()
                .timecode
                .dispatch(
                    &request,
                    window.label(),
                    tokio_util::sync::CancellationToken::new(),
                )
                .await?;
            consume_timecode_receipt(receipt);
            return Ok(vec![]);
        }
        if midi::COMMANDS.contains(&command) {
            return app
                .state::<AppState>()
                .midi
                .dispatch(request, window.label())
                .await
                .map(|publications| publications.into_iter().map(midi_event).collect());
        }
        if ffmpeg_download::COMMANDS.contains(&command) {
            let result = app
                .state::<AppState>()
                .ffmpeg_downloads
                .begin(request, window.label())?
                .finish()
                .await?;
            for diagnostic in &result.diagnostics {
                eprintln!("{}: {}", diagnostic.code, diagnostic.message);
            }
            return result.into_events();
        }
        if network_queries::COMMANDS.contains(&command) {
            return app
                .state::<AppState>()
                .network_queries
                .dispatch(request, window.label())
                .await;
        }
        if media_copy::COMMANDS.contains(&command) {
            if window.label() != "main" {
                return Err(WireError::new(
                    "window_error",
                    "Media copying requires the main window",
                ));
            }
            return app.state::<AppState>().media_copy.dispatch(request).await;
        }
        if scriptures::COMMANDS.contains(&command) {
            return app.state::<AppState>().scriptures.dispatch(request).await;
        }
        if encoders::COMMANDS.contains(&command) {
            return app
                .state::<AppState>()
                .encoders
                .dispatch(request, window.label())
                .await;
        }
        if file_reads::COMMANDS.contains(&command) {
            let state = app.state::<AppState>();
            return state.file_reads.dispatch(&state.library, request).await;
        }
        if thumbnails::COMMANDS.contains(&command) {
            let state = app.state::<AppState>();
            let publish = |event| {
                app.emit_to("main", "freeshow://ipc", event)
                    .map_err(|error| error.to_string())
            };
            return state
                .thumbnails
                .dispatch(
                    &state.library,
                    &state.assets,
                    request,
                    window.label(),
                    &publish,
                )
                .await;
        }
        if api::COMMANDS.contains(&command) {
            return app
                .state::<AppState>()
                .api
                .dispatch(request, window.label())
                .await;
        }
        if browse::COMMANDS.contains(&command) {
            let state = app.state::<AppState>();
            if command == "READ_FOLDER" {
                let publish = |event| {
                    app.emit_to("main", "freeshow://ipc", event)
                        .map_err(|error| error.to_string())
                };
                return state
                    .browse
                    .dispatch_with_thumbnails(
                        &state.library,
                        request,
                        window.label(),
                        &state.thumbnails,
                        &state.assets,
                        &publish,
                    )
                    .await;
            }
            return state
                .browse
                .dispatch(&state.library, request, window.label())
                .await;
        }
        if imports::COMMANDS.contains(&command) {
            let state = app.state::<AppState>();
            let mut imported = state
                .imports
                .dispatch(&state.library, request, window.label(), |event| {
                    app.emit_to("main", "freeshow://ipc", event)
                        .map_err(|error| error.to_string())
                })
                .await?;
            match state
                .assets
                .import_grants(&state.library, &imported.context, &imported.media)
                .await
            {
                Ok(grants) => {
                    if let Some(event) = imported
                        .events
                        .iter_mut()
                        .find(|event| event.message.channel == "IMPORT2")
                    {
                        event.native_assets = Some(grants);
                    }
                }
                Err(error) => eprintln!("Import media publication unavailable: {}", error.message),
            }
            if state.library.context().map_err(WireError::from)? != imported.context {
                return Err(WireError::new(
                    "stale_library",
                    "Imported media belongs to an obsolete library",
                ));
            }
            return Ok(imported.events);
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
                state.context_changes.close_and_drain().await?;
                let (
                    _,
                    _,
                    _,
                    files_result,
                    system_result,
                    api_result,
                    imports_result,
                    browse_result,
                    assets_result,
                    thumbnails_result,
                    file_reads_result,
                    remotes_result,
                    encoders_result,
                    scriptures_result,
                    media_copy_result,
                    network_queries_result,
                    ffmpeg_downloads_result,
                    midi_result,
                    timecode_result,
                    audio_runtime_result,
                ) = tokio::join!(
                    state.media.shutdown(),
                    state.audio.shutdown(),
                    state.osc.shutdown(),
                    state.files.shutdown(),
                    state.system.shutdown(),
                    state.api.shutdown(),
                    state.imports.shutdown(),
                    state.browse.shutdown(),
                    state.assets.shutdown(),
                    state.thumbnails.shutdown(),
                    state.file_reads.shutdown(),
                    state.remotes.shutdown(),
                    state.encoders.shutdown(),
                    state.scriptures.shutdown(),
                    state.media_copy.shutdown(),
                    state.network_queries.shutdown(),
                    state.ffmpeg_downloads.shutdown(),
                    state.midi.shutdown(),
                    state.timecode.shutdown(),
                    async {
                        let ingress = state.audio_ingress.shutdown().await;
                        let pipeline = state.audio_pipeline.shutdown().await;
                        // A failed pump retains its active guard until the real
                        // native owner joins. Always attempt both independent drains.
                        let retry = if ingress.is_err() {
                            state.audio_ingress.shutdown().await
                        } else {
                            Ok(())
                        };
                        (ingress, pipeline, retry)
                    }
                );
                // Shared OS ownership outlives every consumer, including discovery
                // when its remote owner is installed. Always attempt its real drain.
                let network_snapshots_result = state.network_snapshots.shutdown().await;
                files_result.map_err(|error| WireError::new("files_worker", error.to_string()))?;
                system_result
                    .map_err(|error| WireError::new("system_worker", error.to_string()))?;
                api_result?;
                imports_result?;
                browse_result?;
                assets_result?;
                thumbnails_result?;
                file_reads_result?;
                remotes_result?;
                encoders_result?;
                scriptures_result?;
                media_copy_result?;
                network_queries_result?;
                network_snapshots_result?;
                ffmpeg_downloads_result?;
                midi_result?;
                timecode_result?;
                audio_runtime_result.0.map_err(WireError::from)?;
                audio_runtime_result.1.map_err(WireError::from)?;
                audio_runtime_result.2.map_err(WireError::from)?;
                // Accepted native effects may still need the ordered library owner.
                // A timed-out owner retains write admission for its real drain retry.
                let drained = state.library.finish_writes().await;
                if !discard {
                    drained.map_err(WireError::from)?;
                }
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
        let state = app.state::<AppState>();
        if let Err(error) = state.audio_pipeline.context_changed() {
            let mut alert = Event::reply(
                "MAIN",
                "ALERT",
                json!(format!(
                    "The library changed, but audio status cleanup failed: {}",
                    error.message
                )),
                None,
            );
            alert.context = Some(context.clone());
            events.push(alert);
        }
        if let Err(error) = state.audio_ingress.context_changed().await {
            let mut alert = Event::reply(
                "MAIN",
                "ALERT",
                json!(format!(
                    "The library changed, but audio input cleanup failed: {}",
                    error.message
                )),
                None,
            );
            alert.context = Some(context.clone());
            events.push(alert);
        }
        if let Err(error) = state.timecode.context_changed().await {
            let mut alert = Event::reply(
                "MAIN",
                "ALERT",
                json!(format!(
                    "The library changed, but timecode cleanup failed: {}",
                    error.message
                )),
                None,
            );
            alert.context = Some(context.clone());
            events.push(alert);
        }
        app.state::<AppState>().files.context_changed(&context);
        app.state::<AppState>().imports.context_changed(&context);
        app.state::<AppState>().browse.context_changed(&context);
        app.state::<AppState>().thumbnails.context_changed(&context);
        app.state::<AppState>().file_reads.context_changed(&context);
        app.state::<AppState>().scriptures.context_changed(&context);
        app.state::<AppState>().media_copy.context_changed(&context);
        app.state::<AppState>()
            .network_queries
            .context_changed(&context);
        app.state::<AppState>()
            .ffmpeg_downloads
            .context_changed(&context);
        if let Err(error) = app.state::<AppState>().assets.context_changed(&context) {
            let mut alert = Event::reply(
                "MAIN",
                "ALERT",
                json!(format!(
                    "The library changed, but media revocation failed: {}",
                    error.message
                )),
                None,
            );
            alert.context = Some(context.clone());
            events.push(alert);
        }
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
        if let Err(error) = app.state::<AppState>().midi.context_changed().await {
            let mut alert = Event::reply(
                "MAIN",
                "ALERT",
                json!(format!(
                    "The library changed, but MIDI cleanup failed: {}",
                    error.message
                )),
                None,
            );
            alert.context = Some(context.clone());
            events.push(alert);
        }
        if let Err(error) = app.state::<AppState>().remotes.context_changed().await {
            let mut alert = Event::reply(
                "MAIN",
                "ALERT",
                json!(format!(
                    "The library changed, but remote restart failed: {}",
                    error.message
                )),
                None,
            );
            alert.context = Some(context.clone());
            events.push(alert);
        }
        if let Err(error) = app.state::<AppState>().encoders.context_changed().await {
            let mut alert = Event::reply(
                "MAIN",
                "ALERT",
                json!(format!(
                    "The library changed, but encoder detection restart failed: {}",
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
                        "OPEN_CACHE" => state.thumbnails.cache_root().to_path_buf(),
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
            let network_snapshots = network_snapshot::NetworkSnapshots::start(
                network_snapshot::SnapshotLimits::default(),
            )
            .map_err(|error| std::io::Error::other(error.message))?;
            let network_queries = network_queries::NativeNetworkQueries::new(
                library.clone(),
                network_snapshots.clone(),
            );
            let assets = tauri::async_runtime::block_on(assets::NativeAssets::start(
                &library.context()?,
                &app_data,
            ))
            .map_err(|error| std::io::Error::other(error.message))?;
            let thumbnails = thumbnails::NativeThumbnails::new(app_data.join("media-cache"))
                .map_err(|error| std::io::Error::other(error.message))?;
            let file_read_app = app.handle().clone();
            let file_reads = file_reads::NativeFileReads::new(std::sync::Arc::new(move |event| {
                file_read_app
                    .emit_to("main", "freeshow://ipc", event)
                    .map_err(|error| error.to_string())
            }))
            .map_err(|error| std::io::Error::other(error.message))?;
            let mut api_config = freeshow_api::ApiConfig::default();
            let download_app = app.handle().clone();
            let ffmpeg_downloads = tauri::async_runtime::block_on(async {
                let mut config = ffmpeg_download::Config::default();
                if let Some(url) = std::env::var_os("FREESHOW_RUST_FFMPEG_DOWNLOAD_LOOPBACK_URL") {
                    config.controlled_test = Some(ffmpeg_download::ControlledTest::loopback(
                        "linux",
                        "x64",
                        &url.to_string_lossy(),
                    )?);
                }
                ffmpeg_download::NativeFfmpegDownloads::new(
                    library.clone(),
                    media.clone(),
                    app_data.clone(),
                    config,
                    std::sync::Arc::new(move |event| {
                        download_app
                            .emit_to("main", "freeshow://ipc", event)
                            .map_err(|error| error.to_string())
                    }),
                )
            })
            .map_err(|error| std::io::Error::other(error.message))?;
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
            let audio_app = app.handle().clone();
            let audio_pipeline = audio_pipeline::NativeAudioPipeline::start(
                library.clone(), freeshow_opus::LibrarySource::system(), audio_pipeline::AudioLimits::default(),
                move |delivery| {
                    let mut event = delivery.event().clone();
                    event.native_audio = Some(json!({"id":delivery.id(),"run":delivery.run().to_string(),"ownUndefinedError":delivery.own_undefined_error()}));
                    if let Some(error) = delivery.diagnostic() { eprintln!("Icecast: {}: {}", error.code, error.message); }
                    audio_app.emit_to("main", "freeshow://ipc", event).map_err(|e| e.to_string())
                },
            ).map_err(std::io::Error::other)?;
            let audio_ingress = tauri::async_runtime::block_on(async {
                audio_ingress::AudioIngress::new(library.clone(), audio_pipeline.clone())
            }).map_err(std::io::Error::other)?;
            // Launch environment and packaged resources are native authority.
            // The verified Linux codec is not yet a distributed application resource.
            let ltc_path = std::env::var_os("FREESHOW_RUST_LTC_LIBRARY").map(PathBuf::from)
                .unwrap_or(app.path().resource_dir()?.join("codecs/libltc.so.11"));
            let ltc_source = freeshow_ltc::TrustedLibrary::from_host(ltc_path)?;
            let timecode_app = app.handle().clone();
            let timecode = tauri::async_runtime::block_on(async {
                timecode::NativeTimecode::new(library.clone(), timecode::TimecodeHostConfig::default(), ltc_source,
                    || Ok(Box::new(freeshow_midi_ports::NativeBackend)),
                    move |publication| timecode_app.emit_to("main", "freeshow://ipc", timecode_event(publication)).map_err(|e|e.to_string()))
            }).map_err(|error| std::io::Error::other(error.message))?;
            let midi_app = app.handle().clone();
            let midi = tauri::async_runtime::block_on(async {
                midi::NativeMidi::new(
                    library.clone(),
                    midi::MidiHostConfig::default(),
                    || Ok(Box::new(freeshow_midi_ports::NativeBackend)),
                    move |publication| {
                        midi_app
                            .emit_to("main", "freeshow://ipc", midi_event(publication))
                            .map_err(|error| error.to_string())
                    },
                )
            })
            .map_err(|error| std::io::Error::other(error.message))?;
            let mut remote_config = freeshow_remotes::RemoteConfig::default();
            if let Ok(ip) = std::env::var("FREESHOW_RUST_REMOTES_BIND_IP") {
                remote_config.bind_ip = ip.parse()?;
            }
            let resources = app.path().resource_dir()?.join("remotes");
            for kind in freeshow_remotes::ServerKind::ALL {
                remote_config
                    .static_roots
                    .insert(kind, resources.join(kind.key()));
            }
            let remote_app = app.handle().clone();
            let remotes = tauri::async_runtime::block_on(async {
                let mut discovery_config = discovery::NativeDiscovery::production_config();
                // This process-only fixture authority is never derived from IPC.
                if let Some(target) = std::env::var_os("FREESHOW_RUST_DISCOVERY_LOOPBACK_TARGET") {
                    discovery_config.transport = freeshow_discovery::DiscoveryTransport::Loopback {
                        bind: std::net::SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, 0),
                        target: target.to_string_lossy().parse().map_err(|error| {
                            WireError::new(
                                "discovery_config",
                                format!("Invalid native discovery fixture target: {error}"),
                            )
                        })?,
                    };
                }
                let discovered = match discovery::FrozenDiscoveryInputs::capture(&network_snapshots)
                    .await
                {
                    Ok(frozen) => {
                        discovery::NativeDiscovery::new(library.clone(), discovery_config, frozen)
                    }
                    Err(error) => Err(error),
                };
                let publish = move |event| {
                    remote_app
                        .emit_to("main", "freeshow://ipc", event)
                        .map_err(|error| error.to_string())
                };
                match discovered {
                    Ok((discovery, events)) => remotes::NativeRemotes::new_with_discovery(
                        library.clone(),
                        remote_config,
                        discovery,
                        events,
                        publish,
                    ),
                    Err(error) => {
                        eprintln!(
                            "Native discovery setup failed: {}: {}",
                            error.code, error.message
                        );
                        remotes::NativeRemotes::new(library.clone(), remote_config, publish)
                    }
                }
            })
            .map_err(|error| std::io::Error::other(error.message))?;
            #[cfg(target_os = "linux")]
            let documents_root = documents_path::resolve();
            #[cfg(not(target_os = "linux"))]
            let documents_root = app.path().document_dir()?;
            let scripture_app = app.handle().clone();
            let scriptures = scriptures::NativeScriptures::new(
                library.clone(),
                documents_root,
                std::sync::Arc::new(move |event| {
                    scripture_app
                        .emit_to("main", "freeshow://ipc", event)
                        .map_err(|error| error.to_string())
                }),
            )
            .map_err(|error| std::io::Error::other(error.message))?;
            let encoders =
                encoders::NativeEncoders::new(library.clone(), media.clone(), app_data.clone())
                    .map_err(|error| std::io::Error::other(error.message))?;
            let copy_events = app.handle().clone();
            let copy_opener = app.handle().clone();
            let media_copy = media_copy::NativeMediaCopy::new(
                library.clone(),
                std::sync::Arc::new(move |event| {
                    copy_events
                        .emit_to("main", "freeshow://ipc", event)
                        .map_err(|error| error.to_string())
                }),
                std::sync::Arc::new(move |path| {
                    if copy_opener
                        .opener()
                        .open_path(path.to_string_lossy(), None::<&str>)
                        .is_err()
                    {
                        copy_opener
                            .opener()
                            .reveal_item_in_dir(path)
                            .map_err(|error| error.to_string())?;
                    }
                    Ok(())
                }),
            )
            .map_err(|error| std::io::Error::other(error.message))?;
            app.manage(AppState {
                library,
                context_changes: context_changes::ContextChanges::new(),
                outputs,
                media,
                ffmpeg_downloads,
                encoders,
                scriptures,
                media_copy,
                audio,
                audio_pipeline,
                audio_ingress,
                audio_last_receipt: std::sync::Mutex::new(serde_json::Value::Null),
                midi,
                timecode,
                timecode_observers: tokio::sync::Semaphore::new(4),
                files,
                imports,
                browse,
                system,
                network_snapshots,
                network_queries,
                api,
                remotes,
                assets,
                thumbnails,
                file_reads,
                osc,
                app_data,
                error_log,
                allow_exit: AtomicBool::new(false),
            });
            menu::install(app.handle()).map_err(|error| std::io::Error::other(error.message))?;
            let main = app
                .get_webview_window("main")
                .ok_or_else(|| std::io::Error::other("Main WebView is unavailable"))?;
            drop::install(
                &main,
                app.state::<AppState>().library.clone(),
                app.state::<AppState>().assets.clone(),
            )
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
            remotes_status,
            network_queries_status,
            ffmpeg_download_status,
            midi_status,
            midi_delivery_complete,
            audio_pcm,
            audio_disconnect,
            audio_close_context,
            audio_capability,
            audio_status,
            audio_status_complete,
            timecode_command,
            timecode_audio,
            timecode_complete,
            timecode_retire,
            timecode_status,
            remote_delivery_complete,
            imports_status,
            assets_status,
            thumbnails_status,
            file_reads_status,
            encoders_status,
            scriptures_status,
            media_copy_status,
            thumbnail_complete
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
