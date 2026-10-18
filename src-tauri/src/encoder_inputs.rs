//! Trusted encoder executable/cache preparation on two fixed filesystem workers.
use freeshow_core::{storage, WireError};
use freeshow_encoders::{CacheWriteIntent, FrozenDetectionInput, FrozenExecutable};
use std::{
    fs::{self, Metadata, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, SystemTime},
};
use tokio::sync::{oneshot, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

const CAPACITY: usize = 8;
const CACHE_LIMIT: u64 = 4 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    length: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    change: (i64, i64),
}
impl Identity {
    fn new(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            change: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}
#[derive(Clone)]
struct Config {
    root: PathBuf,
    root_identity: Identity,
    cwd: PathBuf,
    search: Vec<PathBuf>,
    platform: String,
    arch: String,
    device: String,
}
pub struct PreparedEncoderInput {
    input: FrozenDetectionInput,
    root: PathBuf,
    root_identity: Identity,
    source_stat_path: Option<PathBuf>,
    resolution_paths: Vec<PathBuf>,
    executable_identity: Option<Identity>,
}
impl PreparedEncoderInput {
    pub fn input(&self) -> &FrozenDetectionInput {
        &self.input
    }
    /// Called only on the ordered library owner, with its final frozen context.
    /// Errors warn without suppressing a valid source detection reply.
    pub fn commit_cache(
        &self,
        intent: &CacheWriteIntent,
        root: &Path,
        generation: u64,
    ) -> Result<(), WireError> {
        if root != self.root {
            return Err(failure("Encoder cache root changed"));
        }
        let metadata = fs::symlink_metadata(root).map_err(io_error)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || !same_directory(&self.root_identity, &metadata)
        {
            return Err(failure("Encoder cache directory identity changed"));
        }
        let Some(executable) = self.input.executable.as_ref() else {
            return Err(failure("Missing encoder executable authority"));
        };
        if resolve(&self.resolution_paths, &CancellationToken::new())? != executable.path {
            return Err(failure(
                "Encoder executable resolution changed before cache commit",
            ));
        }
        let metadata = fs::metadata(&executable.path).map_err(io_error)?;
        if !metadata.is_file()
            || self.executable_identity.as_ref() != Some(&Identity::new(&metadata))
        {
            return Err(failure("Encoder executable changed before cache commit"));
        }
        let mtime = self
            .source_stat_path
            .as_ref()
            .and_then(|path| fs::metadata(path).ok())
            .map(|metadata| mtime_ms(&metadata))
            .unwrap_or(0.0);
        let mut current = self.input.clone();
        current.executable.as_mut().unwrap().mtime_ms = mtime;
        let key = current
            .cache_key()
            .map_err(|e| WireError::new(e.code, e.message))?
            .ok_or_else(|| failure("Missing encoder cache key"))?;
        intent
            .ensure_current(generation, &key)
            .map_err(|e| WireError::new(e.code, e.message))?;
        let target = root.join("encoder-detection.json");
        match fs::symlink_metadata(&target) {
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err(failure("Encoder cache target is not a regular file"))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
        storage::atomic_write_bytes(&target, intent.bytes()).map_err(WireError::from)
    }
}
fn same_directory(identity: &Identity, metadata: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        identity.device == metadata.dev() && identity.inode == metadata.ino()
    }
    #[cfg(not(unix))]
    {
        identity.created.is_some() && identity.created == metadata.created().ok()
    }
}
struct Work {
    source: Option<String>,
    generation: u64,
    force: bool,
    reply: oneshot::Sender<Result<PreparedEncoderInput, WireError>>,
    _permit: OwnedSemaphorePermit,
    #[cfg(test)]
    panic_worker: bool,
}
struct Inner {
    sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    slots: Arc<Semaphore>,
    cancel: CancellationToken,
    closing: AtomicBool,
    shutdown: AsyncMutex<()>,
}
#[derive(Clone)]
pub struct EncoderInputs {
    inner: Arc<Inner>,
}
impl EncoderInputs {
    pub fn new(root: PathBuf) -> Result<Self, WireError> {
        if !root.is_absolute() {
            return Err(failure("Encoder cache root must be absolute"));
        }
        let metadata = fs::symlink_metadata(&root).map_err(io_error)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(failure("Encoder cache root must be a regular directory"));
        }
        let search: Vec<_> =
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
        if search.len() > 128
            || search
                .iter()
                .any(|path| path.as_os_str().as_encoded_bytes().len() > 16384)
        {
            return Err(failure(
                "Encoder executable search exceeds its trusted limit",
            ));
        }
        let config = Config {
            root,
            root_identity: Identity::new(&metadata),
            cwd: std::env::current_dir().map_err(io_error)?,
            search,
            platform: if cfg!(windows) {
                "win32"
            } else if cfg!(target_os = "macos") {
                "darwin"
            } else {
                std::env::consts::OS
            }
            .into(),
            arch: match std::env::consts::ARCH {
                "x86_64" => "x64",
                "x86" => "ia32",
                "aarch64" => "arm64",
                other => other,
            }
            .into(),
            device: std::env::var("FREESHOW_VAAPI_DEVICE")
                .unwrap_or_else(|_| "/dev/dri/renderD128".into()),
        };
        if config.device.len() > 4096 {
            return Err(failure("VAAPI device exceeds its trusted limit"));
        }
        let (sender, receive) = mpsc::sync_channel::<Work>(CAPACITY);
        let receive = Arc::new(Mutex::new(receive));
        let cancel = CancellationToken::new();
        let mut workers = Vec::new();
        for index in 0..2 {
            let receive = receive.clone();
            let config = config.clone();
            let cancel = cancel.clone();
            workers.push(
                std::thread::Builder::new()
                    .name(format!("freeshow-encoder-files-{index}"))
                    .spawn(move || loop {
                        let work = receive.lock().unwrap_or_else(|e| e.into_inner()).recv();
                        let Ok(work) = work else {
                            break;
                        };
                        #[cfg(test)]
                        assert!(
                            !work.panic_worker,
                            "controlled encoder input worker failure"
                        );
                        let result =
                            freeze(&config, work.source, work.generation, work.force, &cancel);
                        let _ = work.reply.send(result);
                    })
                    .map_err(io_error)?,
            );
        }
        Ok(Self {
            inner: Arc::new(Inner {
                sender: Mutex::new(Some(sender)),
                workers: Mutex::new(workers),
                slots: Arc::new(Semaphore::new(CAPACITY)),
                cancel,
                closing: AtomicBool::new(false),
                shutdown: AsyncMutex::new(()),
            }),
        })
    }
    pub async fn freeze(
        &self,
        source: Option<String>,
        generation: u64,
        force: bool,
    ) -> Result<PreparedEncoderInput, WireError> {
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(failure("Encoder input preparation is closed"));
        }
        let permit = self.inner.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new(
                "encoder_inputs_busy",
                "Encoder input preparation is at capacity",
            )
        })?;
        let (reply, response) = oneshot::channel();
        self.inner
            .sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .ok_or_else(|| failure("Encoder input worker is closed"))?
            .try_send(Work {
                source,
                generation,
                force,
                reply,
                _permit: permit,
                #[cfg(test)]
                panic_worker: false,
            })
            .map_err(|_| failure("Encoder input queue is unavailable"))?;
        response
            .await
            .map_err(|_| failure("Encoder input worker stopped before replying"))?
    }
    #[cfg(test)]
    pub(crate) async fn panic_worker_for_test(&self) -> Result<(), WireError> {
        let permit = self.inner.slots.clone().try_acquire_owned().unwrap();
        let (reply, response) = oneshot::channel();
        self.inner
            .sender
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .try_send(Work {
                source: None,
                generation: 0,
                force: false,
                reply,
                _permit: permit,
                panic_worker: true,
            })
            .map_err(|_| failure("Controlled worker injection was not admitted"))?;
        response
            .await
            .map_err(|_| failure("Controlled encoder input worker stopped"))?
            .map(|_| ())
    }
    pub async fn shutdown(&self) -> Result<(), WireError> {
        let _serial = self.inner.shutdown.lock().await;
        self.inner.closing.store(true, Ordering::Release);
        self.inner.cancel.cancel();
        self.inner
            .sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        while !self
            .inner
            .workers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .all(JoinHandle::is_finished)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        for worker in self
            .inner
            .workers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            worker
                .join()
                .map_err(|_| failure("Encoder input worker panicked"))?;
        }
        Ok(())
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.sender
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}
fn freeze(
    config: &Config,
    source: Option<String>,
    generation: u64,
    force: bool,
    cancel: &CancellationToken,
) -> Result<PreparedEncoderInput, WireError> {
    if cancel.is_cancelled() {
        return Err(failure("Encoder input preparation was cancelled"));
    }
    let mut prepared = PreparedEncoderInput {
        input: FrozenDetectionInput {
            generation,
            executable: None,
            platform: config.platform.clone(),
            arch: config.arch.clone(),
            vaapi_device: config.device.clone(),
            cache: None,
        },
        root: config.root.clone(),
        root_identity: config.root_identity.clone(),
        source_stat_path: None,
        resolution_paths: Vec::new(),
        executable_identity: None,
    };
    let Some(source) = source else {
        return Ok(prepared);
    };
    if source.is_empty() || source.len() > 16384 || source.contains('\0') {
        return Err(failure("Invalid trusted encoder executable spelling"));
    }
    let source_path = PathBuf::from(&source);
    let stat_path = if source_path.is_absolute() {
        source_path.clone()
    } else {
        config.cwd.join(&source_path)
    };
    let resolution_paths = if source_path.is_absolute() || source_path.components().count() > 1 {
        vec![stat_path.clone()]
    } else {
        let name = if cfg!(windows) && source_path.extension().is_none() {
            format!("{source}.exe")
        } else {
            source.clone()
        };
        config
            .search
            .iter()
            .map(|directory| {
                if directory.is_absolute() {
                    directory.join(&name)
                } else {
                    config.cwd.join(directory).join(&name)
                }
            })
            .collect()
    };
    let authority = resolve(&resolution_paths, cancel)?;
    prepared.resolution_paths = resolution_paths;
    let metadata = fs::metadata(&authority).map_err(io_error)?;
    if !metadata.is_file() {
        return Err(failure("Encoder executable is not a regular file"));
    }
    prepared.executable_identity = Some(Identity::new(&metadata));
    let mtime = fs::metadata(&stat_path)
        .map(|metadata| mtime_ms(&metadata))
        .unwrap_or(0.0);
    prepared.source_stat_path = Some(stat_path);
    prepared.input.executable = Some(FrozenExecutable {
        path: authority,
        source_path: source,
        mtime_ms: mtime,
    });
    if !force {
        validate_root(config)?;
        prepared.input.cache = read_cache(&config.root.join("encoder-detection.json"), cancel)?;
        validate_root(config)?;
    }
    Ok(prepared)
}
fn validate_root(config: &Config) -> Result<(), WireError> {
    let metadata = fs::symlink_metadata(&config.root).map_err(io_error)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || !same_directory(&config.root_identity, &metadata)
    {
        return Err(failure("Encoder cache directory identity changed"));
    }
    Ok(())
}
fn resolve(paths: &[PathBuf], cancel: &CancellationToken) -> Result<PathBuf, WireError> {
    for path in paths {
        if cancel.is_cancelled() {
            return Err(failure("Encoder executable search was cancelled"));
        }
        if let Ok(path) = fs::canonicalize(path) {
            if fs::metadata(&path).is_ok_and(|metadata| metadata.is_file() && executable(&metadata))
            {
                return Ok(path);
            }
        }
    }
    Err(failure(
        "Previously resolved encoder executable is unavailable",
    ))
}
fn read_cache(path: &Path, cancel: &CancellationToken) -> Result<Option<Vec<u8>>, WireError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Ok(None);
    }
    let Ok(mut file) = options.open(path) else {
        return Ok(None);
    };
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() {
        return Ok(None);
    }
    if metadata.len() > CACHE_LIMIT {
        return Err(WireError::new(
            "encoder_cache_limit",
            "Encoder cache exceeds its byte limit",
        ));
    }
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 32 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(failure("Encoder cache read was cancelled"));
        }
        let length = file.read(&mut chunk).map_err(io_error)?;
        if length == 0 {
            break;
        }
        if bytes.len() + length > CACHE_LIMIT as usize {
            return Err(WireError::new(
                "encoder_cache_limit",
                "Encoder cache grew beyond its byte limit",
            ));
        }
        bytes.extend_from_slice(&chunk[..length]);
    }
    Ok(Some(bytes))
}
fn mtime_ms(metadata: &Metadata) -> f64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.mtime() as f64 * 1000.0 + metadata.mtime_nsec() as f64 / 1_000_000.0
    }
    #[cfg(not(unix))]
    {
        match metadata
            .modified()
            .unwrap_or(std::time::UNIX_EPOCH)
            .duration_since(std::time::UNIX_EPOCH)
        {
            Ok(value) => {
                value.as_secs() as f64 * 1000.0 + f64::from(value.subsec_nanos()) / 1_000_000.0
            }
            Err(value) => {
                let value = value.duration();
                -(value.as_secs() as f64 * 1000.0 + f64::from(value.subsec_nanos()) / 1_000_000.0)
            }
        }
    }
}
fn executable(metadata: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}
fn failure(message: &str) -> WireError {
    WireError::new("encoder_inputs", message)
}
fn io_error(error: std::io::Error) -> WireError {
    WireError::new("encoder_inputs_io", error.to_string())
}
