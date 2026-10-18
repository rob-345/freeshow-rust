//! Trusted explicit static fixtures/assets, fixed synchronous file workers.
use crate::{state::Run, RemoteError, Result, ServerKind};
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::{mpsc as std_mpsc, Arc, Mutex},
    time::{Duration, Instant, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit};

type Chunk = std::result::Result<Vec<u8>, io::Error>;
pub(crate) type PoolSender = Arc<Mutex<Option<std_mpsc::SyncSender<Work>>>>;
pub(crate) struct Work {
    started: Instant,
    root: PathBuf,
    path: String,
    query: Option<String>,
    head: bool,
    headers: HeaderMap,
    reply: oneshot::Sender<Response>,
    run: Arc<Run>,
    _permit: OwnedSemaphorePermit,
}
pub(crate) struct FilePool {
    sender: PoolSender,
    threads: Vec<std::thread::JoinHandle<()>>,
    done: Vec<oneshot::Receiver<()>>,
}
impl FilePool {
    pub fn new(run: Arc<Run>) -> Result<Self> {
        let (sender, receiver) = std_mpsc::sync_channel::<Work>(run.config.static_queue);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut pool = Self {
            sender: Arc::new(Mutex::new(Some(sender))),
            threads: vec![],
            done: vec![],
        };
        if run.config.static_roots.is_empty() {
            pool.sender.lock().unwrap().take();
            return Ok(pool);
        }
        for index in 0..run.config.static_workers {
            let receiver = receiver.clone();
            let (done, finished) = oneshot::channel();
            let thread = std::thread::Builder::new()
                .name(format!("freeshow-static-{index}"))
                .spawn(move || {
                    loop {
                        let work = receiver.lock().unwrap().recv();
                        let Ok(work) = work else { break };
                        handle(work);
                    }
                    let _ = done.send(());
                })
                .map_err(|error| RemoteError::new("file_worker_unavailable", error.to_string()))?;
            pool.threads.push(thread);
            pool.done.push(finished);
        }
        Ok(pool)
    }
    pub fn sender(&self) -> PoolSender {
        self.sender.clone()
    }
    pub async fn stop(mut self) -> Result<()> {
        self.sender.lock().unwrap().take();
        for done in self.done.drain(..) {
            done.await.map_err(|_| {
                RemoteError::new("file_worker_unavailable", "Static file worker exited")
            })?;
        }
        for thread in self.threads.drain(..) {
            thread.join().map_err(|_| {
                RemoteError::new("file_worker_panicked", "Static file worker panicked")
            })?;
        }
        Ok(())
    }
}
impl Drop for FilePool {
    fn drop(&mut self) {
        self.sender.lock().unwrap().take();
        // Includes partial worker creation and error unwinding. Normal stop has
        // already awaited completion and drained these handles.
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
pub(crate) fn validate_root(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|v| matches!(v, Component::CurDir | Component::ParentDir))
    {
        return Err(RemoteError::new(
            "invalid_static_root",
            "Static root must be a trusted normalized absolute directory",
        ));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| RemoteError::new("invalid_static_root", error.to_string()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(RemoteError::new(
            "invalid_static_root",
            "Static root must be a non-symlink directory",
        ));
    }
    Ok(())
}
pub(crate) async fn serve(
    request: Request,
    kind: ServerKind,
    run: Arc<Run>,
    sender: PoolSender,
) -> Response {
    if request.uri().path() == "/nowplayingcover" {
        return error(
            StatusCode::NOT_IMPLEMENTED,
            "Now-playing cover capability is not implemented",
        );
    }
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return error(StatusCode::NOT_FOUND, "Not Found");
    }
    let Some(root) = run.config.static_roots.get(&kind).cloned() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Trusted client asset root is not configured",
        );
    };
    let permit = match run.static_requests.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Static file capacity reached",
            )
        }
    };
    let (reply, receive) = oneshot::channel();
    let work = Work {
        started: Instant::now(),
        root,
        path: request.uri().path().into(),
        query: request.uri().query().map(str::to_owned),
        head: request.method() == Method::HEAD,
        headers: request.headers().clone(),
        reply,
        run: run.clone(),
        _permit: permit,
    };
    let admitted = sender
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|sender| sender.try_send(work).is_ok());
    if !admitted {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Static file queue unavailable",
        );
    }
    tokio::select! {result=tokio::time::timeout(run.config.body_timeout,receive)=>result.ok().and_then(|value|value.ok()).unwrap_or_else(||error(StatusCode::SERVICE_UNAVAILABLE,"Static worker unavailable")),_=run.cancel.cancelled()=>error(StatusCode::SERVICE_UNAVAILABLE,"Remote service stopped")}
}
fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        [("content-type", "text/plain; charset=UTF-8")],
        message.to_owned(),
    )
        .into_response()
}
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}
fn handle(work: Work) {
    if work.run.cancel.is_cancelled()
        || work.started.elapsed() >= work.run.config.body_timeout
        || work.reply.is_closed()
    {
        let _ = work.reply.send(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Remote service stopped",
        ));
        return;
    }
    let raw_path = work.path.as_bytes();
    if raw_path.iter().enumerate().any(|(index, byte)| {
        *byte == b'%'
            && (index + 2 >= raw_path.len()
                || !raw_path[index + 1].is_ascii_hexdigit()
                || !raw_path[index + 2].is_ascii_hexdigit())
    }) {
        let _ = work
            .reply
            .send(error(StatusCode::BAD_REQUEST, "Invalid URL encoding"));
        return;
    }
    let decoded = match percent_encoding::percent_decode_str(&work.path).decode_utf8() {
        Ok(path) => path,
        Err(_) => {
            let _ = work
                .reply
                .send(error(StatusCode::BAD_REQUEST, "Invalid URL encoding"));
            return;
        }
    };
    if decoded.contains('\0') || decoded.contains('\\') {
        let _ = work
            .reply
            .send(error(StatusCode::BAD_REQUEST, "Invalid asset path"));
        return;
    }
    let parts = decoded
        .trim_start_matches('/')
        .split('/')
        .filter(|v| !v.is_empty() && *v != ".")
        .collect::<Vec<_>>();
    if parts.iter().any(|v| *v == ".." || v.contains(':')) {
        let _ = work
            .reply
            .send(error(StatusCode::FORBIDDEN, "Path traversal forbidden"));
        return;
    }
    if parts.iter().any(|v| v.starts_with('.')) || parts.len() > 32 {
        let _ = work.reply.send(error(StatusCode::NOT_FOUND, "Not Found"));
        return;
    }
    let mut path = work.root.clone();
    for part in &parts {
        path.push(part);
    }
    if !safe_parents(&work.root, &path) {
        let _ = work.reply.send(error(
            StatusCode::FORBIDDEN,
            "Symlink asset paths are unsupported",
        ));
        return;
    }
    if path.is_dir() {
        if !work.path.ends_with('/') {
            let location = format!(
                "{}/{}",
                work.path,
                work.query
                    .as_ref()
                    .map(|q| format!("?{q}"))
                    .unwrap_or_default()
            );
            let response = Response::builder()
                .status(StatusCode::MOVED_PERMANENTLY)
                .header("location", location)
                .body(Body::empty())
                .unwrap();
            let _ = work.reply.send(response);
            return;
        }
        path.push("index.html");
    }
    if !safe_parents(&work.root, &path) {
        let _ = work.reply.send(error(
            StatusCode::FORBIDDEN,
            "Symlink asset paths are unsupported",
        ));
        return;
    }
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
        _ => {
            let _ = work.reply.send(error(StatusCode::NOT_FOUND, "Not Found"));
            return;
        }
    };
    if metadata.len() > work.run.config.max_static_bytes {
        let _ = work.reply.send(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Static file size limit",
        ));
        return;
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(_) => {
            let _ = work.reply.send(error(StatusCode::NOT_FOUND, "Not Found"));
            return;
        }
    };
    let opened = match file.metadata() {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => {
            let _ = work
                .reply
                .send(error(StatusCode::FORBIDDEN, "Expected regular static file"));
            return;
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.dev() != opened.dev() || metadata.ino() != opened.ino() {
            let _ = work
                .reply
                .send(error(StatusCode::CONFLICT, "Asset changed while opening"));
            return;
        }
    }
    if metadata.len() != opened.len() || metadata.modified().ok() != opened.modified().ok() {
        let _ = work
            .reply
            .send(error(StatusCode::CONFLICT, "Asset changed while opening"));
        return;
    }
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    let duration = modified.duration_since(UNIX_EPOCH).unwrap_or_default();
    let millis = (duration.as_secs() as f64 * 1000.0
        + f64::from(duration.subsec_nanos()) / 1_000_000.0)
        .round() as u128;
    let etag = format!("W/\"{:x}-{:x}\"", metadata.len(), millis);
    let last_modified = httpdate::fmt_http_date(modified);
    let date = httpdate::parse_http_date(&last_modified).unwrap();
    let matches_etag = |value: &str| {
        value.split(',').any(|value| {
            let value = value.trim_matches(' ');
            value == etag || value == format!("W/{etag}") || format!("W/{value}") == etag
        })
    };
    let precondition_failed =
        if let Some(value) = header(&work.headers, "if-match").filter(|v| !v.is_empty()) {
            value != "*" && !matches_etag(value)
        } else {
            header(&work.headers, "if-unmodified-since")
                .and_then(|value| httpdate::parse_http_date(value).ok())
                .is_some_and(|value| date > value)
        };
    if precondition_failed {
        let _ = work.reply.send(error(
            StatusCode::PRECONDITION_FAILED,
            "Precondition Failed",
        ));
        return;
    }
    let none_match = header(&work.headers, "if-none-match").filter(|v| !v.is_empty());
    let since = header(&work.headers, "if-modified-since").filter(|v| !v.is_empty());
    let no_cache = header(&work.headers, "cache-control")
        .is_some_and(|value| value.split(',').any(|v| v.trim() == "no-cache"));
    let not_modified = (none_match.is_some() || since.is_some())
        && !no_cache
        && none_match.is_none_or(|value| value == "*" || matches_etag(value))
        && since
            .is_none_or(|value| httpdate::parse_http_date(value).is_ok_and(|value| date <= value));
    let mut response = Response::builder()
        .header("accept-ranges", "bytes")
        .header("cache-control", "public, max-age=0")
        .header("etag", &etag)
        .header("last-modified", &last_modified);
    if not_modified {
        let _ = work.reply.send(
            response
                .status(StatusCode::NOT_MODIFIED)
                .body(Body::empty())
                .unwrap(),
        );
        return;
    }
    let mut start = 0;
    let mut length = metadata.len();
    let mut status = StatusCode::OK;
    let range_fresh = header(&work.headers, "if-range").is_none_or(|v| {
        if v.contains('"') {
            v.contains(&etag)
        } else {
            httpdate::parse_http_date(v).is_ok_and(|value| date <= value)
        }
    });
    if range_fresh {
        if let Some(range) = header(&work.headers, "range")
            .and_then(|range| range.strip_prefix("bytes="))
            .filter(|range| !range.contains(','))
        {
            if let Some((from, to)) = range.split_once('-') {
                let selected = if from.is_empty() {
                    to.parse::<u64>().ok().filter(|n| *n > 0).map(|n| {
                        (
                            metadata.len().saturating_sub(n),
                            metadata.len().saturating_sub(1),
                        )
                    })
                } else {
                    from.parse::<u64>().ok().and_then(|start| {
                        if to.is_empty() {
                            Some((start, metadata.len().saturating_sub(1)))
                        } else {
                            to.parse::<u64>()
                                .ok()
                                .map(|end| (start, end.min(metadata.len().saturating_sub(1))))
                        }
                    })
                };
                if let Some((from, to)) = selected {
                    if from >= metadata.len() || to < from {
                        let _ = work.reply.send(
                            response
                                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                                .header("content-range", format!("bytes */{}", metadata.len()))
                                .body(Body::empty())
                                .unwrap(),
                        );
                        return;
                    }
                    start = from;
                    length = to - from + 1;
                    status = StatusCode::PARTIAL_CONTENT;
                    response = response.header(
                        "content-range",
                        format!("bytes {from}-{to}/{}", metadata.len()),
                    );
                }
            }
        }
    }
    response = response
        .status(status)
        .header("content-type", mime(&path))
        .header("content-length", length);
    if work.head {
        let _ = work.reply.send(response.body(Body::empty()).unwrap());
        return;
    }
    {
        use std::io::{Seek, SeekFrom};
        if file.seek(SeekFrom::Start(start)).is_err() {
            let _ = work.reply.send(error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Asset seek failed",
            ));
            return;
        }
    }
    let (sender, receiver) = mpsc::channel::<Chunk>(2);
    let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(receiver));
    if work.reply.send(response.body(body).unwrap()).is_err() {
        return;
    }
    let end = work.started + work.run.config.body_timeout;
    let mut remaining = length;
    while remaining > 0 {
        if work.run.cancel.is_cancelled() || Instant::now() >= end {
            return;
        }
        let mut buffer = vec![0; remaining.min(32768) as usize];
        let read = match file.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        buffer.truncate(read);
        remaining -= read as u64;
        let mut item = Ok(buffer);
        loop {
            if work.run.cancel.is_cancelled() || Instant::now() >= end {
                return;
            }
            match sender.try_send(item) {
                Ok(()) => break,
                Err(mpsc::error::TrySendError::Closed(_)) => return,
                Err(mpsc::error::TrySendError::Full(value)) => {
                    item = value;
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
    }
}
fn safe_parents(root: &Path, path: &Path) -> bool {
    if validate_root(root).is_err() {
        return false;
    }
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut cursor = root.to_owned();
    let parts = relative.components().collect::<Vec<_>>();
    for (index, part) in parts.iter().enumerate() {
        if !matches!(part, Component::Normal(_)) {
            return false;
        }
        cursor.push(part.as_os_str());
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || (index + 1 < parts.len() && !metadata.is_dir())
                {
                    return false;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
            Err(_) => return false,
        }
    }
    true
}
fn mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=UTF-8",
        "js" | "mjs" => "application/javascript; charset=UTF-8",
        "css" => "text/css; charset=UTF-8",
        "json" | "map" => "application/json; charset=UTF-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=UTF-8",
        _ => "application/octet-stream",
    }
}
