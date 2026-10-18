//! Native-authorized download staging. No executable is installed or executed.
mod gzip;
mod http;
mod io;
mod service;
use serde::Serialize;
pub use service::{DownloadService, DownloadTicket, Status};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
pub use tokio_util::sync::CancellationToken;
pub const MAX_JOBS: usize = 2;
pub(crate) const CHUNK: usize = 32768;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > 1024 {
            let mut end = 1024;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        Self { code, message }
    }
    pub(crate) fn limit() -> Self {
        Self::new("download_limit", "FFmpeg download resource limit exceeded")
    }
    pub(crate) fn cancelled() -> Self {
        Self::new("download_cancelled", "FFmpeg download cancelled")
    }
    pub(crate) fn shutdown() -> Self {
        Self::new("download_shutdown", "FFmpeg download service is closing")
    }
    pub(crate) fn source(message: &str) -> Self {
        Self::new("download_source", message)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
#[derive(Debug, Clone)]
pub struct Config {
    pub compressed_bytes: u64,
    pub uncompressed_bytes: u64,
    pub gzip_header_bytes: usize,
    pub gzip_members: usize,
    pub redirects: usize,
    pub gzip_steps: u64,
    pub timeout: Duration,
    pub idle_timeout: Duration,
    pub shutdown_timeout: Duration,
    /// Explicit native-only fixture mode. Never derive from a renderer request.
    pub controlled_test: Option<ControlledTestConfig>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            compressed_bytes: 128 * 1024 * 1024,
            uncompressed_bytes: 512 * 1024 * 1024,
            gzip_header_bytes: 8192,
            gzip_members: 16,
            redirects: 20,
            gzip_steps: 4000000,
            timeout: Duration::from_secs(120),
            idle_timeout: Duration::from_secs(10),
            shutdown_timeout: Duration::from_secs(5),
            controlled_test: None,
        }
    }
}
impl Config {
    fn validate(&self) -> Result<()> {
        if self.compressed_bytes == 0
            || self.compressed_bytes > 256 * 1024 * 1024
            || self.uncompressed_bytes == 0
            || self.uncompressed_bytes > 1024 * 1024 * 1024
            || !(10..=65536).contains(&self.gzip_header_bytes)
            || !(1..=64).contains(&self.gzip_members)
            || self.redirects > 20
            || self.gzip_steps == 0
            || self.gzip_steps > 16000000
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(600)
            || self.idle_timeout.is_zero()
            || self.idle_timeout > Duration::from_secs(30)
            || self.shutdown_timeout.is_zero()
            || self.shutdown_timeout > Duration::from_secs(30)
        {
            return Err(Error::new(
                "download_configuration",
                "Invalid download limits",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct ControlledTestConfig {
    url: String,
}
impl ControlledTestConfig {
    pub fn loopback(url: &str) -> Result<Self> {
        if url.len() > 8192 {
            return Err(Error::limit());
        }
        let uri = url
            .parse::<::http::Uri>()
            .map_err(|_| Error::new("download_test_configuration", "Invalid loopback URL"))?;
        let host = uri.host().unwrap_or("").trim_matches(['[', ']']);
        let ip = host.parse::<std::net::IpAddr>().map_err(|_| {
            Error::new(
                "download_test_configuration",
                "Fixture host must be a numeric loopback address",
            )
        })?;
        if uri.scheme_str() != Some("http")
            || !ip.is_loopback()
            || uri.authority().is_none_or(|v| v.as_str().contains('@'))
        {
            return Err(Error::new(
                "download_test_configuration",
                "Fixture URL must use loopback HTTP without credentials",
            ));
        }
        Ok(Self { url: url.into() })
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadOwner {
    runtime: String,
    generation: u64,
    data_path: String,
}
impl DownloadOwner {
    pub fn new(runtime: &str, generation: u64, data_path: &str) -> Result<Self> {
        if runtime.is_empty()
            || runtime.len() > 256
            || runtime.contains('\0')
            || data_path.len() > 32768
            || data_path.contains('\0')
            || generation > 9007199254740991
        {
            return Err(Error::new(
                "download_owner",
                "Invalid trusted download context",
            ));
        }
        Ok(Self {
            runtime: runtime.into(),
            generation,
            data_path: data_path.into(),
        })
    }
}
#[derive(Debug, Clone)]
pub struct HostDownloadPlan {
    root: PathBuf,
    owner: DownloadOwner,
    platform: String,
    arch: String,
    url: &'static str,
    name: &'static str,
}
pub fn plan_for_host(
    platform: &str,
    arch: &str,
    trusted_bin_root: impl AsRef<Path>,
    owner: DownloadOwner,
) -> Result<HostDownloadPlan> {
    if platform.len() > 32 || arch.len() > 32 {
        return Err(Error::limit());
    }
    let url = download_url(platform, arch)
        .ok_or_else(|| Error::source(&format!("Unsupported platform: {platform}/{arch}")))?;
    let root = trusted_bin_root.as_ref();
    let spelling = root
        .to_str()
        .ok_or_else(|| Error::new("download_path", "Non-Unicode bin root"))?;
    if !root.is_absolute() || spelling.len() > 32700 || spelling.contains('\0') {
        return Err(Error::new(
            "download_path",
            "Trusted bin root must be absolute and bounded",
        ));
    }
    Ok(HostDownloadPlan {
        root: root.into(),
        owner,
        platform: platform.into(),
        arch: arch.into(),
        url,
        name: if platform == "win32" {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        },
    })
}
impl HostDownloadPlan {
    pub fn url(&self) -> &str {
        self.url
    }
    pub fn destination(&self) -> PathBuf {
        self.root.join(self.name)
    }
    pub fn platform(&self) -> &str {
        &self.platform
    }
    pub fn arch(&self) -> &str {
        &self.arch
    }
}
pub fn download_url(platform: &str, arch: &str) -> Option<&'static str> {
    match(platform,arch){
    ("darwin","arm64")=>Some("https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1/ffmpeg-darwin-arm64.gz"),
    ("darwin","x64")=>Some("https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1/ffmpeg-darwin-x64.gz"),
    ("win32","x64")=>Some("https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1/ffmpeg-win32-x64.gz"),
    ("win32","ia32")=>Some("https://github.com/eugeneware/ffmpeg-static/releases/download/b6.0/ffmpeg-win32-ia32.gz"),
    ("linux","x64")=>Some("https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1/ffmpeg-linux-x64.gz"),
    ("linux","arm64")=>Some("https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1/ffmpeg-linux-arm64.gz"),
    ("linux","arm")=>Some("https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1/ffmpeg-linux-arm.gz"),_=>None}
}
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub url: &'static str,
    pub name: &'static str,
    pub progress: f64,
    pub total: u16,
    pub status: &'static str,
}
pub fn progress_for_bytes(received: u64, total: f64) -> Option<Progress> {
    if total == 0.0 || total.is_nan() {
        None
    } else {
        let value = (received as f64 / total) * 100.0;
        let floor = value.floor();
        let rounded = if value - floor >= 0.5 {
            floor + 1.0
        } else {
            floor
        };
        let rounded = if rounded == 0.0 && value.is_sign_negative() {
            -0.0
        } else {
            rounded
        };
        Some(Progress {
            url: "ffmpeg",
            name: "FFmpeg",
            progress: rounded,
            total: 100,
            status: "downloading",
        })
    }
}
#[derive(Debug, Serialize)]
pub struct CompletionPlan {
    pub reply: serde_json::Value,
    pub progress: Progress,
}
/// Data-only original handler effects. The host supplies success only after its
/// real install/post-install/cache-reset obligations have completed.
pub fn source_completion(installed: bool, error: Option<&str>) -> CompletionPlan {
    if installed {
        CompletionPlan {
            reply: serde_json::json!({"success":true}),
            progress: Progress {
                url: "ffmpeg",
                name: "FFmpeg",
                progress: 100.0,
                total: 100,
                status: "complete",
            },
        }
    } else {
        let message = error
            .filter(|value| !value.is_empty())
            .unwrap_or("Unknown download error");
        let mut end = message.len().min(1024);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        let message = &message[..end];
        CompletionPlan {
            reply: serde_json::json!({"success":false,"error":message}),
            progress: Progress {
                url: "ffmpeg",
                name: "FFmpeg",
                progress: 0.0,
                total: 0,
                status: "error",
            },
        }
    }
}
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferStats {
    pub compressed_bytes: u64,
    pub uncompressed_bytes: u64,
    pub members: usize,
    pub redirects: usize,
    pub progress_updates: u64,
}
#[derive(Clone)]
pub(crate) struct Control {
    service: CancellationToken,
    caller: CancellationToken,
    cancel: CancellationToken,
    deadline: Instant,
}
impl Control {
    pub(crate) fn check(&self) -> Result<()> {
        if self.service.is_cancelled() {
            return Err(Error::shutdown());
        }
        if self.caller.is_cancelled() || self.cancel.is_cancelled() {
            return Err(Error::cancelled());
        }
        if Instant::now() >= self.deadline {
            return Err(Error::new(
                "download_deadline",
                "FFmpeg download deadline elapsed",
            ));
        }
        Ok(())
    }
}
pub struct PreparedDownload {
    pub(crate) job: Arc<service::Job>,
    pub stats: TransferStats,
}
pub struct AcceptedInstall {
    job: Arc<service::Job>,
}
/// Private artifact paths become available only after the native host accepts
/// ownership. Keep this guard on the actual ordered IO owner through rename.
impl PreparedDownload {
    pub fn accept(self, current: &DownloadOwner) -> Result<AcceptedInstall> {
        self.job.control.check()?;
        if &self.job.plan.owner != current {
            return Err(Error::new(
                "stale_generation",
                "Download belongs to an obsolete owner",
            ));
        }
        let mut phase = self.job.phase.lock().unwrap_or_else(|e| e.into_inner());
        if *phase != service::Phase::Prepared {
            return Err(Error::new(
                "download_state",
                "Download is not pending install",
            ));
        }
        *phase = service::Phase::Accepted;
        drop(phase);
        Ok(AcceptedInstall { job: self.job })
    }
    pub async fn discard(self) -> Result<()> {
        self.job.cleanup().await
    }
}
#[derive(Debug)]
pub struct InstallView<'a> {
    pub staged_path: &'a Path,
    pub destination: PathBuf,
    pub mode: Option<u32>,
    pub mac_post_install: bool,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCommand {
    pub program: &'static str,
    pub args: Vec<String>,
    pub on_failure: &'static str,
}
impl InstallView<'_> {
    pub fn post_install_commands(&self) -> Vec<HostCommand> {
        if !self.mac_post_install {
            return vec![];
        }
        let path = self.destination.to_string_lossy().into_owned();
        vec![
            HostCommand {
                program: "xattr",
                args: vec!["-cr".into(), path.clone()],
                on_failure: "ignore",
            },
            HostCommand {
                program: "codesign",
                args: vec!["-s".into(), "-".into(), "--force".into(), path],
                on_failure: "warn",
            },
        ]
    }
}
impl AcceptedInstall {
    /// Synchronous read-only verification for the trusted host IO owner, not GTK.
    /// Worker never performs the final rename, xattr/codesign or cache clear.
    pub fn verify_for_install(&self, current: &DownloadOwner) -> Result<InstallView<'_>> {
        if &self.job.plan.owner != current {
            return Err(Error::new(
                "stale_generation",
                "Accepted download owner changed",
            ));
        }
        io::verify(&self.job)?;
        Ok(InstallView {
            staged_path: &self.job.staged,
            destination: self.job.plan.destination(),
            mode: if self.job.plan.platform == "win32" {
                None
            } else {
                Some(0o755)
            },
            mac_post_install: self.job.plan.platform == "darwin",
        })
    }
    /// Call after the real host install succeeds or fails; cleanup only touches
    /// this private staging child. This is not a success acknowledgement to UI.
    pub async fn finish(self) -> Result<()> {
        self.job.cleanup().await
    }
}
pub(crate) fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
#[cfg(test)]
mod tests;
