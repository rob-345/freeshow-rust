//! Bounded native implementation of the pinned Electron media metadata handlers.
//! Replies retain MP4Box's format gate and fallback behavior. Operational failures
//! are reported separately as diagnostics; callers must not discard these.
mod metadata;
mod process;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

pub const COMMANDS: &[&str] = &["FFMPEG_CHECK", "MEDIA_CODEC", "MEDIA_TRACKS"];

#[derive(Clone, Debug)]
pub struct MediaConfig {
    /// Explicit candidates precede PATH/system/local candidates. Empty lists use discovery.
    pub ffmpeg_candidates: Vec<PathBuf>,
    pub ffprobe_candidates: Vec<PathBuf>,
    pub discover_system: bool,
    /// Optional application user-data/bin directory (the original resolver's last candidate).
    pub local_bin_dir: Option<PathBuf>,
    pub max_jobs: usize,
    pub process_timeout: Duration,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
}
impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            ffmpeg_candidates: vec![],
            ffprobe_candidates: vec![],
            discover_system: true,
            local_bin_dir: None,
            max_jobs: 2,
            process_timeout: Duration::from_secs(10),
            stdout_limit: 8 * 1024 * 1024,
            stderr_limit: 64 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("unsupported media command: {0}")]
    Unsupported(String),
    #[error("media payload must be an object")]
    InvalidPayload,
    #[error("invalid media supervisor configuration")]
    InvalidConfig,
    #[error("all media jobs are occupied")]
    Busy,
    #[error("media operation cancelled")]
    Cancelled,
    #[error("media service is shut down")]
    Shutdown,
    #[error("media process timed out")]
    Timeout,
    #[error("media process {0} exceeded its byte limit")]
    OutputLimit(&'static str),
    #[error("media process failed: {0}")]
    Process(String),
    #[error("media process exited with {code:?}: {stderr}")]
    Exit { code: Option<i32>, stderr: String },
    #[error("media process returned invalid JSON: {0}")]
    InvalidOutput(String),
    #[error("media variant is unsupported: {0}")]
    UnsupportedVariant(String),
    #[error("usable {0} executable is unavailable")]
    Unavailable(&'static str),
}
impl MediaError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsupported(_) => "unsupported_command",
            Self::InvalidPayload | Self::InvalidConfig => "invalid_request",
            Self::Busy => "media_busy",
            Self::Cancelled => "cancelled",
            Self::Shutdown => "media_shutdown",
            Self::Timeout => "media_timeout",
            Self::OutputLimit(_) => "media_output_limit",
            Self::Process(_) => "media_process_error",
            Self::Exit { code: Some(1), .. } => "media_input_rejected",
            Self::Exit { .. } => "media_process_error",
            Self::InvalidOutput(_) => "media_invalid_output",
            Self::UnsupportedVariant(_) => "media_unsupported_variant",
            Self::Unavailable(_) => "media_unavailable",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaDiagnostic {
    pub code: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaReply {
    pub data: Value,
    pub diagnostics: Vec<MediaDiagnostic>,
}
impl MediaReply {
    fn plain(data: Value) -> Self {
        Self {
            data,
            diagnostics: vec![],
        }
    }
    fn fallback(data: Value, error: MediaError) -> Self {
        // Invalid/missing/corrupt inputs are ordinary original-parser fallbacks.
        if matches!(error, MediaError::Exit { code: Some(1), .. }) {
            return Self::plain(data);
        }
        Self {
            data,
            diagnostics: vec![MediaDiagnostic {
                code: error.code().into(),
                message: error.to_string(),
            }],
        }
    }
}

struct Inner {
    config: MediaConfig,
    permits: Arc<Semaphore>,
    cancel: CancellationToken,
    ffmpeg: Mutex<Option<PathBuf>>,
    ffprobe: Mutex<Option<PathBuf>>,
}
#[derive(Clone)]
pub struct MediaService {
    inner: Arc<Inner>,
}
impl MediaService {
    pub fn new(config: MediaConfig) -> Result<Self, MediaError> {
        if !(1..=32).contains(&config.max_jobs)
            || config.process_timeout.is_zero()
            || config.process_timeout > Duration::from_secs(120)
            || config.stdout_limit == 0
            || config.stdout_limit > 64 * 1024 * 1024
            || config.stderr_limit == 0
            || config.stderr_limit > 1024 * 1024
        {
            return Err(MediaError::InvalidConfig);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                permits: Arc::new(Semaphore::new(config.max_jobs)),
                config,
                cancel: CancellationToken::new(),
                ffmpeg: Mutex::new(None),
                ffprobe: Mutex::new(None),
            }),
        })
    }

    /// At most max_jobs tasks are admitted; excess requests return Busy immediately.
    /// Dropping this future cancels its supervised task; cancellation kills and reaps
    /// its child before the task releases its job permit.
    pub async fn dispatch(&self, command: &str, payload: Value) -> Result<MediaReply, MediaError> {
        if !COMMANDS.contains(&command) {
            return Err(MediaError::Unsupported(command.into()));
        }
        if command != "FFMPEG_CHECK" && !payload.is_object() {
            return Err(MediaError::InvalidPayload);
        }
        if self.inner.cancel.is_cancelled() {
            return Err(MediaError::Shutdown);
        }
        let permit = self.inner.acquire_job()?;
        let cancel = self.inner.cancel.child_token();
        let guard = CancelOnDrop(cancel.clone());
        let inner = self.inner.clone();
        let command = command.to_owned();
        let task = tokio::spawn(async move {
            let _permit = permit;
            inner.handle(&command, payload, &cancel).await
        });
        let result = task
            .await
            .map_err(|error| MediaError::Process(error.to_string()))?;
        drop(guard);
        result
    }

    /// Cancel admitted requests, kill/reap their processes, then wait for all permits.
    /// Call while the application's Tokio runtime is still alive.
    pub async fn shutdown(&self) {
        self.inner.cancel.cancel();
        let _ = self
            .inner
            .permits
            .acquire_many(self.inner.config.max_jobs as u32)
            .await;
    }
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl Inner {
    fn acquire_job(&self) -> Result<OwnedSemaphorePermit, MediaError> {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| MediaError::Busy)?;
        // A different runtime thread may complete shutdown between dispatch's
        // precheck and this acquisition. Once held, shutdown must wait for us;
        // if shutdown already happened, do not admit any new worker.
        if self.cancel.is_cancelled() {
            return Err(MediaError::Shutdown);
        }
        Ok(permit)
    }

    async fn handle(
        &self,
        command: &str,
        payload: Value,
        cancel: &CancellationToken,
    ) -> Result<MediaReply, MediaError> {
        if command == "FFMPEG_CHECK" {
            return match self.resolve("ffmpeg", cancel).await {
                Ok(path) => Ok(MediaReply::plain(json!({"installed": true, "path": path}))),
                Err(MediaError::Cancelled) => Err(MediaError::Cancelled),
                Err(error) => Ok(MediaReply::fallback(json!({"installed": false}), error)),
            };
        }
        let fallback = metadata::fallback(command, &payload);
        let Some(path) = payload.get("path").and_then(Value::as_str) else {
            return Ok(MediaReply::plain(fallback));
        };
        if !metadata::has_mp4_header(path).await {
            return Ok(MediaReply::plain(fallback));
        }
        // Always pass a native absolute file path; ffprobe must not reinterpret a
        // relative file named "-" or a colon-containing name as stdin/a protocol.
        let Ok(path) = tokio::fs::canonicalize(path).await else {
            return Ok(MediaReply::plain(fallback));
        };
        let result = async {
            let binary = self.resolve("ffprobe", cancel).await?;
            let mut args: Vec<std::ffi::OsString> = [
                "-v",
                "error",
                "-protocol_whitelist",
                "file",
                "-ignore_editlist",
                "1",
                "-show_streams",
                "-show_data",
            ]
            .iter()
            .map(Into::into)
            .collect();
            if command == "MEDIA_TRACKS" {
                args.extend(
                    ["-select_streams", "s", "-show_packets"]
                        .iter()
                        .map(std::ffi::OsString::from),
                );
            }
            args.extend([
                std::ffi::OsString::from("-of"),
                "json".into(),
                "-i".into(),
                path.into_os_string(),
            ]);
            let output = process::run(&binary, &args, &self.config, cancel).await?;
            let probe: Value = serde_json::from_slice(&output)
                .map_err(|error| MediaError::InvalidOutput(error.to_string()))?;
            metadata::project(command, payload, &probe)
        }
        .await;
        match result {
            Ok(data) => Ok(MediaReply::plain(data)),
            Err(MediaError::Cancelled) => Err(MediaError::Cancelled),
            Err(error) => Ok(MediaReply::fallback(fallback, error)),
        }
    }

    async fn resolve(
        &self,
        name: &'static str,
        cancel: &CancellationToken,
    ) -> Result<PathBuf, MediaError> {
        let cache = if name == "ffmpeg" {
            &self.ffmpeg
        } else {
            &self.ffprobe
        };
        let mut cache = tokio::select! { _ = cancel.cancelled() => return Err(MediaError::Cancelled), cache = cache.lock() => cache };
        if let Some(path) = cache.as_ref() {
            return Ok(path.clone());
        }
        let mut candidates = if name == "ffmpeg" {
            self.config.ffmpeg_candidates.clone()
        } else {
            self.config.ffprobe_candidates.clone()
        };
        if self.config.discover_system {
            candidates.push(name.into());
            #[cfg(target_os = "linux")]
            for directory in ["/usr/bin", "/usr/local/bin", "/snap/bin"] {
                candidates.push(PathBuf::from(directory).join(name));
            }
            #[cfg(target_os = "macos")]
            for directory in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"] {
                candidates.push(PathBuf::from(directory).join(name));
            }
        }
        if let Some(directory) = &self.config.local_bin_dir {
            candidates.push(directory.join(if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.into()
            }));
        }
        let mut diagnostic = None;
        for candidate in candidates {
            match process::run(&candidate, &["-version".into()], &self.config, cancel).await {
                Ok(_) => {
                    *cache = Some(candidate.clone());
                    return Ok(candidate);
                }
                Err(MediaError::Cancelled) => return Err(MediaError::Cancelled),
                Err(error @ (MediaError::Timeout | MediaError::OutputLimit(_))) => {
                    diagnostic = Some(error);
                }
                Err(_) => {}
            }
        }
        Err(diagnostic.unwrap_or(MediaError::Unavailable(name)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn isolated() -> MediaService {
        MediaService::new(MediaConfig {
            discover_system: false,
            ..MediaConfig::default()
        })
        .unwrap()
    }
    #[tokio::test]
    async fn default_replies_do_not_require_an_installed_media_binary() {
        let service = isolated();
        let check = service
            .dispatch("FFMPEG_CHECK", json!({"ignored":true}))
            .await
            .unwrap();
        assert_eq!(check.data, json!({"installed":false}));
        assert_eq!(check.diagnostics[0].code, "media_unavailable");
        let metadata = service
            .dispatch("MEDIA_CODEC", json!({"future":true}))
            .await
            .unwrap();
        assert_eq!(
            metadata.data,
            json!({"future":true,"codecs":[],"mimeType":"application/octet-stream","mimeCodec":""})
        );
        assert!(metadata.diagnostics.is_empty());
        let tracks = service
            .dispatch("MEDIA_TRACKS", json!({"path":false,"tracks":["stale"]}))
            .await
            .unwrap();
        assert_eq!(tracks.data, json!({"path":false,"tracks":[]}));
        assert!(tracks.diagnostics.is_empty());
        assert!(matches!(
            service.dispatch("UNKNOWN", Value::Null).await,
            Err(MediaError::Unsupported(_))
        ));
        assert!(matches!(
            service.dispatch("MEDIA_CODEC", Value::Null).await,
            Err(MediaError::InvalidPayload)
        ));
        service.shutdown().await;
        assert!(matches!(
            service.dispatch("FFMPEG_CHECK", Value::Null).await,
            Err(MediaError::Shutdown)
        ));
    }
    #[tokio::test]
    async fn job_admission_is_bounded_and_shutdown_drains_permits() {
        let service = MediaService::new(MediaConfig {
            max_jobs: 1,
            discover_system: false,
            ..MediaConfig::default()
        })
        .unwrap();
        // join polls both dispatch futures before the admitted spawned worker can run.
        let (first, second) = tokio::join!(
            service.dispatch("FFMPEG_CHECK", Value::Null),
            service.dispatch("FFMPEG_CHECK", Value::Null)
        );
        assert!(first.is_ok());
        assert!(matches!(second, Err(MediaError::Busy)));
        service.shutdown().await;
        assert_eq!(service.inner.permits.available_permits(), 1);
    }
    #[tokio::test]
    async fn shutdown_cannot_be_overtaken_after_a_dispatch_precheck() {
        let service = isolated();
        let (checked_tx, checked_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        let late_admission = async {
            // Pause exactly between dispatch's precheck and its production
            // permit-admission helper; shutdown completes during that pause.
            assert!(!service.inner.cancel.is_cancelled());
            checked_tx.send(()).unwrap();
            resume_rx.await.unwrap();
            service.inner.acquire_job()
        };
        let shutdown = async {
            checked_rx.await.unwrap();
            service.shutdown().await;
            resume_tx.send(()).unwrap();
        };
        let (admission, ()) = tokio::join!(late_admission, shutdown);
        assert!(matches!(admission, Err(MediaError::Shutdown)));
        assert_eq!(
            service.inner.permits.available_permits(),
            service.inner.config.max_jobs
        );
    }

    #[tokio::test]
    async fn shutdown_waits_for_a_job_that_already_holds_its_permit() {
        let service = isolated();
        let admitted = service.inner.acquire_job().unwrap();
        let mut shutdown = Box::pin(service.shutdown());
        tokio::select! {
            biased;
            _ = &mut shutdown => panic!("shutdown returned before admitted work released its permit"),
            _ = tokio::task::yield_now() => {},
        }
        assert!(service.inner.cancel.is_cancelled());
        drop(admitted);
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .unwrap();
    }

    #[test]
    fn invalid_limits_cannot_disable_resource_bounds() {
        for max_jobs in [0, 33] {
            assert!(matches!(
                MediaService::new(MediaConfig {
                    max_jobs,
                    ..MediaConfig::default()
                }),
                Err(MediaError::InvalidConfig)
            ));
        }
        assert!(matches!(
            MediaService::new(MediaConfig {
                stdout_limit: 0,
                ..MediaConfig::default()
            }),
            Err(MediaError::InvalidConfig)
        ));
    }
}
