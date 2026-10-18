//! Trusted native provenance produces capabilities before the retained payload
//! is delivered. File opens/bytes use the independent fixed asset workers.
use freeshow_asset_plans::{
    AcceptedNative, AcceptedPath, BrowsedFile, Committed, DroppedFile, Plan, Planner,
};
use freeshow_assets::{AssetServer, Config, Context, Control, Protection};
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::LibraryService,
    WireError,
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

fn failed(error: impl std::fmt::Display) -> WireError {
    WireError::new("assets_error", error.to_string())
}
pub fn context(value: &RuntimeContext) -> Result<Context, WireError> {
    Ok(Context {
        runtime_id: value.runtime_id.clone(),
        library_generation: value.library_generation,
        data_path: value
            .data_path
            .to_str()
            .ok_or_else(|| failed("Non-Unicode library path"))?
            .into(),
    })
}

#[derive(Clone)]
pub struct NativeAssets {
    server: AssetServer,
    roots: Arc<Vec<PathBuf>>,
    slots: Arc<Semaphore>,
}
pub struct Lease {
    _permit: OwnedSemaphorePermit,
    captured: RuntimeContext,
    control: Control,
    saved: Option<Plan>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.control.cancel.cancel();
    }
}

impl NativeAssets {
    /// Called only during trusted native startup; no renderer path is accepted.
    pub async fn start(current: &RuntimeContext, app_data: &Path) -> Result<Self, WireError> {
        let mut roots = Vec::new();
        for root in [
            app_data.join("protected-cache"),
            std::env::temp_dir().join("freeshow-protected"),
        ] {
            if let Ok(canonical) = std::fs::canonicalize(&root) {
                roots.push(canonical);
            }
            // Include the spelling below the canonical parent even when the
            // protected cache does not exist yet.
            if let (Some(parent), Some(name)) = (root.parent(), root.file_name()) {
                if let Ok(parent) = std::fs::canonicalize(parent) {
                    roots.push(parent.join(name));
                }
            }
        }
        roots.sort();
        roots.dedup();
        let server = AssetServer::start(
            Config {
                allowed_origins: vec![
                    "tauri://localhost".into(),
                    "http://tauri.localhost".into(),
                    "https://tauri.localhost".into(),
                    "http://127.0.0.1:1420".into(),
                ],
                protected_roots: roots.clone(),
                ..Config::default()
            },
            context(current)?,
        )
        .await
        .map_err(failed)?;
        Ok(Self {
            server,
            roots: Arc::new(roots),
            slots: Arc::new(Semaphore::new(4)),
        })
    }
    fn classify(&self, source: &str) -> Option<Protection> {
        let path = PathBuf::from(freeshow_files::paths::normalize_local_path(source));
        Some(
            if source.starts_with("freeshow-protected://")
                || self.roots.iter().any(|root| path.starts_with(root))
            {
                Protection::Protected
            } else {
                Protection::Plain
            },
        )
    }
    fn accepted<'a>(&self, path: &'a str) -> AcceptedPath<'a> {
        AcceptedPath::new(path, Path::new(path), self.classify(path))
    }
    fn admit(&self, captured: &RuntimeContext) -> Result<Lease, WireError> {
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new(
                if self.slots.is_closed() {
                    "assets_shutdown"
                } else {
                    "assets_busy"
                },
                "Native media publication is closed or at capacity",
            )
        })?;
        Ok(Lease {
            _permit: permit,
            captured: captured.clone(),
            control: Control::new(Instant::now() + Duration::from_secs(10), Default::default()),
            saved: None,
        })
    }
    pub fn begin(
        &self,
        library: &LibraryService,
        request: &Request,
    ) -> Result<Option<Lease>, WireError> {
        if request.channel != "MAIN"
            || !matches!(
                request.data["channel"].as_str(),
                Some("SHOW" | "SAVE" | "OPEN_FILE" | "LOCATE_MEDIA_FILE" | "READ_FOLDER")
            )
        {
            return Ok(None);
        }
        if let Err(error) = library.contextualize(request, &mut []) {
            // The IO owner preserves obsolete SAVE snapshots as recovery
            // files. Optional media preparation must not intercept that path.
            if request.data["channel"] == "SAVE" {
                return Ok(None);
            }
            return Err(WireError::from(error));
        }
        if request.data["channel"] == "SAVE" {
            let data = &request.data["data"];
            let triggers = &data["customTriggers"];
            let backup_only = triggers["backup"].as_bool() == Some(true)
                && triggers["isAutoBackup"].as_bool() == Some(true)
                && triggers["autosave"].as_bool() != Some(true)
                && data["closeWhenFinished"].as_bool() != Some(true);
            if backup_only
                || data["showsCache"]
                    .as_object()
                    .is_none_or(|shows| shows.is_empty())
            {
                return Ok(None);
            }
        }
        let captured = request
            .context
            .as_ref()
            .ok_or_else(|| failed("Missing media context"))?;
        let mut lease = match self.admit(captured) {
            Ok(lease) => lease,
            Err(error) if request.data["channel"] == "SAVE" => {
                eprintln!("SAVE media publication deferred: {}", error.message);
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if request.data["channel"] == "SAVE" {
            // Pure bounded preparation only. Authorization occurs after the
            // library owner accepts and successfully commits this snapshot.
            let current = context(captured)?;
            match Planner::default().plan(Committed::save_snapshot(
                &current,
                &request.data["data"],
                &|path| self.classify(path),
            )) {
                Ok(plan) => lease.saved = Some(plan),
                Err(error) => eprintln!("SAVE media publication deferred: {error}"),
            }
        }
        Ok(Some(lease))
    }
    pub fn context_changed(&self, current: &RuntimeContext) -> Result<(), WireError> {
        self.server
            .activate_context(context(current)?)
            .map_err(failed)
    }
    pub fn status(&self) -> Value {
        json!(self.server.status())
    }
    pub fn replay(&self, current: &RuntimeContext) -> Result<Value, WireError> {
        Ok(json!(self
            .server
            .snapshot(&context(current)?)
            .map_err(failed)?))
    }
    pub fn replay_since(
        &self,
        current: &RuntimeContext,
        previous: Option<&freeshow_assets::Revision>,
    ) -> Result<(freeshow_assets::Revision, Option<Value>), WireError> {
        let (revision, grants) = self
            .server
            .snapshot_since(&context(current)?, previous)
            .map_err(failed)?;
        Ok((revision, grants.map(|grants| json!(grants))))
    }
    fn plan_event(&self, current: &Context, event: &Event) -> Result<Option<Plan>, WireError> {
        if event.channel != "MAIN" {
            return Ok(None);
        }
        let Some(data) = event.message.data.as_ref() else {
            return Ok(None);
        };
        let planner = Planner::default();
        let plan = match event.message.channel.as_str() {
            "SHOW" => planner.plan(Committed::show_reply(current, data, &|path| {
                self.classify(path)
            })),
            "OPEN_FILE2" => {
                let paths = data["files"]
                    .as_array()
                    .ok_or_else(|| failed("Invalid native selection result"))?;
                if paths.len() > 1024 {
                    return Err(failed("Native selection exceeds asset path limit"));
                }
                let paths = paths
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|path| self.accepted(path))
                    .collect::<Vec<_>>();
                planner.plan(AcceptedNative::selection(current, &paths))
            }
            "LOCATE_MEDIA_FILE" => planner.plan(AcceptedNative::located(
                current,
                data["path"].as_str().map(|path| self.accepted(path)),
            )),
            "READ_FOLDER" => {
                let entries = data
                    .as_object()
                    .ok_or_else(|| failed("Invalid native folder listing"))?;
                if entries.len() > 10000 {
                    return Err(failed("Native listing exceeds entry limit"));
                }
                let files = entries
                    .values()
                    .filter_map(|entry| {
                        if entry["isFolder"].as_bool() != Some(false) {
                            return None;
                        }
                        Some(BrowsedFile {
                            path: self.accepted(entry["path"].as_str()?),
                            is_folder: false,
                            thumbnail: entry["thumbnailPath"]
                                .as_str()
                                .filter(|path| !path.is_empty())
                                .map(|path| self.accepted(path)),
                        })
                    })
                    .collect::<Vec<_>>();
                planner.plan(AcceptedNative::browse(current, &files))
            }
            _ => return Ok(None),
        };
        plan.map(Some).map_err(failed)
    }
    async fn authorize(&self, plan: Plan, lease: &Lease) -> Result<Value, WireError> {
        for diagnostic in plan.diagnostics {
            eprintln!(
                "{}: {} ({})",
                diagnostic.code, diagnostic.source_path, diagnostic.field
            );
        }
        let mut grants = Vec::new();
        let mut candidates = plan.grants.into_iter().peekable();
        loop {
            let mut batch = Vec::new();
            let mut bytes = 0;
            while let Some(next) = candidates.peek() {
                let next_bytes = next.source_path.len() + next.path.as_os_str().len();
                if batch.len() == 128 || (!batch.is_empty() && bytes + next_bytes > 1024 * 1024) {
                    break;
                }
                bytes += next_bytes;
                batch.push(candidates.next().unwrap());
            }
            if batch.is_empty() {
                break;
            }
            let batch = self
                .server
                .authorize_batch(&plan.context, batch, lease.control.clone())
                .await
                .map_err(failed)?;
            for rejected in batch.rejected {
                eprintln!(
                    "{}: {} ({})",
                    rejected.error.code, rejected.error.message, rejected.source_path
                );
            }
            grants.extend(batch.grants);
        }
        Ok(json!(grants))
    }
    pub async fn complete(
        &self,
        library: &LibraryService,
        mut lease: Lease,
        events: &mut [Event],
    ) -> Result<(), WireError> {
        let current = context(&lease.captured)?;
        lease.control.deadline = Instant::now() + Duration::from_secs(10);
        for event in events.iter_mut() {
            if event.context.as_ref() != Some(&lease.captured) {
                return Err(failed("Media result has an obsolete context"));
            }
            // Successful reset saves return SETTINGS/SYNCED_SETTINGS instead
            // of SAVE2. The prepared snapshot still follows the completed write.
            let saved = lease.saved.take();
            let plan = match self.plan_event(&current, event) {
                Ok(plan) => saved.or(plan),
                Err(error) => {
                    eprintln!("Native media publication unavailable: {}", error.message);
                    saved
                }
            };
            if let Some(plan) = plan {
                match self.authorize(plan, &lease).await {
                    Ok(grants) => event.native_assets = Some(grants),
                    Err(error) => {
                        eprintln!("Native media publication unavailable: {}", error.message)
                    }
                }
            }
        }
        if library.context().map_err(WireError::from)? != lease.captured {
            return Err(WireError::new(
                "stale_library",
                "Media publication belongs to an obsolete library",
            ));
        }
        Ok(())
    }
    pub async fn drop_grants(
        &self,
        library: &LibraryService,
        captured: &RuntimeContext,
        files: &Value,
    ) -> Result<Value, WireError> {
        let lease = self.admit(captured)?;
        if library.context().map_err(WireError::from)? != *captured {
            return Err(failed("Dropped media belongs to an obsolete library"));
        }
        let files = files
            .as_array()
            .ok_or_else(|| failed("Invalid accepted native drop"))?;
        if files.len() > 256 {
            return Err(failed("Native drop exceeds path limit"));
        }
        let files = files
            .iter()
            .filter_map(|file| {
                let source = file["path"].as_str()?;
                Some(DroppedFile {
                    path: AcceptedPath::new(source, Path::new(source), self.classify(source)),
                    is_directory: file["directory"].as_bool().unwrap_or(false),
                })
            })
            .collect::<Vec<_>>();
        let current = context(captured)?;
        let plan = Planner::default()
            .plan(AcceptedNative::drop_files(&current, &files))
            .map_err(failed)?;
        let grants = self.authorize(plan, &lease).await?;
        if library.context().map_err(WireError::from)? != *captured {
            return Err(failed("Dropped media belongs to an obsolete library"));
        }
        Ok(grants)
    }
    pub async fn import_grants(
        &self,
        library: &LibraryService,
        captured: &RuntimeContext,
        paths: &[PathBuf],
    ) -> Result<Value, WireError> {
        let lease = self.admit(captured)?;
        if paths.len() > 2048 {
            return Err(failed("Committed import media exceeds path limit"));
        }
        if library.context().map_err(WireError::from)? != *captured {
            return Err(failed("Imported media belongs to an obsolete library"));
        }
        let mut sources = Vec::new();
        for path in paths {
            let key = path
                .to_str()
                .ok_or_else(|| failed("Non-Unicode committed media path"))?;
            sources.push((key.to_string(), path));
            let unix = freeshow_files::paths::to_unix(key);
            if unix != key {
                sources.push((unix, path));
            }
        }
        let accepted = sources
            .iter()
            .map(|(source, path)| AcceptedPath::new(source, path, self.classify(source)))
            .collect::<Vec<_>>();
        let current = context(captured)?;
        let plan = Planner::default()
            .plan(AcceptedNative::committed_imports(&current, &accepted))
            .map_err(failed)?;
        let grants = self.authorize(plan, &lease).await?;
        if library.context().map_err(WireError::from)? != *captured {
            return Err(failed("Imported media belongs to an obsolete library"));
        }
        Ok(grants)
    }
    pub async fn shutdown(&self) -> Result<(), WireError> {
        self.slots.close();
        self.server.shutdown().await.map_err(failed)
    }
}
