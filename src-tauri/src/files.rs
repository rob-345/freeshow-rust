//! Native media lookup uses fixed filesystem workers; cache effects return to
//! the ordered library owner before their reply is published.
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
    show::truthy,
    Error, Result, WireError,
};
use freeshow_files::{
    CancelToken, FilesService, JobControl, LocateOutcome, LocateRequest, MediaRoot,
    ThumbnailInvalidation,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::ErrorKind,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_REQUESTS: usize = 10;

#[derive(Clone)]
pub struct NativeFiles {
    service: FilesService,
    slots: Arc<Semaphore>,
    requests: Arc<Mutex<Requests>>,
}

#[derive(Default)]
struct Requests {
    next: u64,
    active: HashMap<u64, (RuntimeContext, CancelToken)>,
}

struct RequestLease {
    id: u64,
    requests: Arc<Mutex<Requests>>,
    control: JobControl,
    _permit: OwnedSemaphorePermit,
}

impl Drop for RequestLease {
    fn drop(&mut self) {
        self.control.cancel.cancel();
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active
            .remove(&self.id);
    }
}

impl NativeFiles {
    pub fn new(service: FilesService) -> Self {
        Self {
            service,
            slots: Arc::new(Semaphore::new(MAX_REQUESTS)),
            requests: Arc::new(Mutex::new(Requests::default())),
        }
    }

    fn begin(&self, request: &Request) -> std::result::Result<RequestLease, WireError> {
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new(
                if self.slots.is_closed() {
                    "files_shutdown"
                } else {
                    "files_busy"
                },
                "Native media requests are closed or at capacity",
            )
        })?;
        let context = request.context.clone().ok_or_else(|| {
            WireError::new("stale_library", "Media request has no library context")
        })?;
        let control = JobControl {
            deadline: Instant::now() + Duration::from_secs(10),
            cancel: CancelToken::default(),
        };
        let mut requests = self.requests.lock().map_err(|_| {
            WireError::new(
                "files_worker",
                "Native media request registry is unavailable",
            )
        })?;
        let id = requests.next;
        requests.next = id.checked_add(1).ok_or_else(|| {
            WireError::new("files_worker", "Native media request sequence exhausted")
        })?;
        requests
            .active
            .insert(id, (context, control.cancel.clone()));
        Ok(RequestLease {
            id,
            requests: self.requests.clone(),
            control,
            _permit: permit,
        })
    }

    pub fn context_changed(&self, context: &RuntimeContext) {
        let requests = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (captured, token) in requests.active.values() {
            if captured != context {
                token.cancel();
            }
        }
    }

    pub async fn shutdown(&self) -> std::result::Result<(), freeshow_files::Error> {
        self.slots.close();
        {
            let requests = self
                .requests
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for (_, token) in requests.active.values() {
                token.cancel();
            }
        }
        self.service.shutdown().await
    }
}

pub const COMMANDS: &[&str] = &[
    "LOCATE_MEDIA_FILE",
    "GET_MEDIA_FOLDER_PATH",
    "SET_MEDIA_FOLDER_PATH",
];

fn service_error(error: freeshow_files::Error) -> WireError {
    for diagnostic in &error.diagnostics {
        eprintln!("media lookup: {diagnostic}");
    }
    let code = match error.code.as_str() {
        "files_invalid" => "files_invalid",
        "files_busy" => "files_busy",
        "files_limit" => "files_limit",
        "files_shutdown" => "files_shutdown",
        "files_cancelled" => "files_cancelled",
        "files_deadline" => "files_deadline",
        "files_encoding" => "files_encoding",
        _ => "files_worker",
    };
    WireError::new(code, error.message)
}

fn locate_request(value: &Value) -> std::result::Result<LocateRequest, WireError> {
    let invalid = || {
        WireError::new(
            "files_invalid",
            "Expected filePath string and folders array of strings",
        )
    };
    let file_path = value["filePath"].as_str().ok_or_else(invalid)?;
    let folders = value["folders"].as_array().ok_or_else(invalid)?;
    // Validate before copying request strings onto the native worker queue.
    if folders.len() > 128 || file_path.len() > 32768 {
        return Err(WireError::new(
            "files_limit",
            "Media lookup path/root limit",
        ));
    }
    let mut bytes = file_path.len();
    for folder in folders {
        let folder = folder.as_str().ok_or_else(invalid)?;
        bytes = bytes.saturating_add(folder.len());
        if folder.len() > 32768 || bytes > 1024 * 1024 {
            return Err(WireError::new(
                "files_limit",
                "Media lookup input byte limit",
            ));
        }
    }
    Ok(LocateRequest {
        file_path: file_path.into(),
        folders: folders
            .iter()
            .map(|folder| folder.as_str().unwrap().into())
            .collect(),
    })
}

pub async fn dispatch(
    library: &LibraryService,
    files: &NativeFiles,
    mut request: Request,
    window: &str,
) -> std::result::Result<Vec<Event>, WireError> {
    let command = request.data["channel"].as_str().unwrap_or("").to_owned();
    // A lease covers scanning AND any later ordered effect commit. Completed
    // scans waiting for the library cannot accumulate unbounded IPC futures.
    let lease = files.begin(&request)?;
    if command == "SET_MEDIA_FOLDER_PATH" {
        let value = request.data["data"]
            .as_str()
            .ok_or_else(|| WireError::new("files_invalid", "Media folder must be a string"))?;
        if value.len() > 32768 || value.contains('\0') {
            return Err(WireError::new(
                "files_limit",
                "Media folder path length/NUL limit",
            ));
        }
        request.data = json!({"channel":"SET_STORE_VALUE","data":{"file":"config","key":"mediaFolderPath","value":value}});
        return library.dispatch(request, window).await.map_err(Into::into);
    }
    let settings = library.media_settings(&request).map_err(WireError::from)?;
    let configured = &settings.configured_media_folder;
    let configured = if truthy(configured) {
        Some(configured.as_str().ok_or_else(|| {
            WireError::new("files_invalid", "Configured media folder must be a string")
        })?)
    } else {
        None
    };
    let data_root = settings.context.data_path.to_str().ok_or_else(|| {
        WireError::new(
            "files_encoding",
            "Media lookup requires a Unicode library path",
        )
    })?;
    let root = MediaRoot::from_host(configured, data_root);
    if command == "GET_MEDIA_FOLDER_PATH" {
        let folder = files
            .service
            .media_folder_path(root, lease.control.clone())
            .await
            .map_err(service_error)?;
        for diagnostic in folder.diagnostics {
            eprintln!("media folder: {diagnostic}");
        }
        let mut events = vec![Event::reply(
            "MAIN",
            &command,
            json!(folder.reply),
            request.listener_id.clone(),
        )];
        library
            .contextualize(&request, &mut events)
            .map_err(WireError::from)?;
        return Ok(events);
    }
    let input = locate_request(&request.data["data"])?;
    let outcome = match files
        .service
        .locate(input, root, lease.control.clone())
        .await
    {
        Ok(outcome) => outcome,
        Err(mut error) => {
            let invalidations = std::mem::take(&mut error.thumbnail_invalidations);
            if !invalidations.is_empty() {
                library
                    .commit(request, window, move |runtime| {
                        invalidate_thumbnails(runtime, &invalidations)?;
                        Ok(vec![])
                    })
                    .await
                    .map_err(WireError::from)?;
            } else {
                library
                    .contextualize(&request, &mut [])
                    .map_err(WireError::from)?;
            }
            return Err(service_error(error));
        }
    };
    for diagnostic in &outcome.diagnostics {
        eprintln!("media lookup: {diagnostic}");
    }
    let listener_id = request.listener_id.clone();
    if outcome.replacement.is_none() && outcome.thumbnail_invalidations.is_empty() {
        let mut events = reply(outcome, listener_id).map_err(WireError::from)?;
        library
            .contextualize(&request, &mut events)
            .map_err(WireError::from)?;
        return Ok(events);
    }
    library
        .commit(request, window, move |runtime| {
            apply_lookup(runtime, &outcome)?;
            reply(outcome, listener_id)
        })
        .await
        .map_err(Into::into)
}

fn reply(outcome: LocateOutcome, listener_id: Option<String>) -> Result<Vec<Event>> {
    Ok(vec![Event::reply(
        "MAIN",
        "LOCATE_MEDIA_FILE",
        serde_json::to_value(outcome.reply)?,
        listener_id,
    )])
}

pub fn apply_lookup(runtime: &mut Runtime, outcome: &LocateOutcome) -> Result<()> {
    invalidate_thumbnails(runtime, &outcome.thumbnail_invalidations)?;
    if let Some(replacement) = &outcome.replacement {
        // Merge at commit time. Concurrent completed scans must not overwrite
        // one another's entries or unrelated local sync metadata.
        let mut cache = runtime.stores.get("CACHE_SYNC")?;
        if !truthy(&cache["replacedPaths"]) {
            cache["replacedPaths"] = json!({});
        }
        match &mut cache["replacedPaths"] {
            Value::Object(paths) => {
                paths.insert(replacement.cache_key.clone(), json!(replacement.path));
            }
            // The source's named array property disappears during JSON output.
            Value::Array(_) => {}
            _ => {
                return Err(Error::Invalid(
                    "CACHE_SYNC.replacedPaths must be an object or array".into(),
                ))
            }
        }
        runtime.stores.set("CACHE_SYNC", cache)?;
    }
    Ok(())
}

fn invalidate_thumbnails(runtime: &Runtime, invalidations: &[ThumbnailInvalidation]) -> Result<()> {
    let directory = runtime.stores.app_data.join("Thumbnails");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        _ => {
            return Err(Error::Invalid(
                "The fork's thumbnail cache must be a non-symlink directory".into(),
            ))
        }
    }
    for invalidation in invalidations {
        if invalidation.hash != freeshow_files::paths::file_path_hash(&invalidation.file_path)
            || invalidation.sizes != [900, 500, 250, 100]
        {
            return Err(Error::Invalid(
                "Invalid native thumbnail invalidation".into(),
            ));
        }
        for size in invalidation.sizes {
            let file = directory.join(format!("{}-{size}.png", invalidation.hash));
            if let Err(error) = fs::remove_file(file) {
                if error.kind() != ErrorKind::NotFound {
                    // Original deleteFile reports failure but keeps searching.
                    eprintln!("Could not invalidate thumbnail: {error}");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn request_leases_bound_the_full_pipeline_cancel_old_contexts_and_close_admission() {
        let native =
            NativeFiles::new(FilesService::start(freeshow_files::Limits::default()).unwrap());
        let context = RuntimeContext {
            runtime_id: "host-test".into(),
            library_generation: 0,
            data_path: std::env::temp_dir().join("A"),
        };
        let request =
            serde_json::from_value::<Request>(json!({"channel":"MAIN","context":context})).unwrap();
        let mut leases = (0..MAX_REQUESTS)
            .map(|_| native.begin(&request).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(native.begin(&request).err().unwrap().code, "files_busy");
        native.context_changed(&context);
        assert!(leases
            .iter()
            .all(|lease| !lease.control.cancel.is_cancelled()));
        let newer = RuntimeContext {
            library_generation: 1,
            ..context.clone()
        };
        native.context_changed(&newer);
        assert!(leases
            .iter()
            .all(|lease| lease.control.cancel.is_cancelled()));
        leases.pop();
        let replacement = native.begin(&request).unwrap();
        assert_eq!(native.requests.lock().unwrap().active.len(), MAX_REQUESTS);
        native.shutdown().await.unwrap();
        assert!(replacement.control.cancel.is_cancelled());
        assert_eq!(native.begin(&request).err().unwrap().code, "files_shutdown");
        drop(replacement);
        drop(leases);
        assert!(native.requests.lock().unwrap().active.is_empty());
    }
}
