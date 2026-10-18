//! Supervised FFmpeg preparation and ordered native installation. No renderer
//! request selects its URL, executable name or application user-data root.
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
    Error as CoreError, WireError,
};
use freeshow_ffmpeg_download::{
    self as download, AcceptedInstall, DownloadOwner, DownloadService, HostCommand,
};
use freeshow_media::MediaService;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
pub const COMMANDS: &[&str] = &["FFMPEG_DOWNLOAD"];
pub type Publish = Arc<dyn Fn(Event) -> Result<(), String> + Send + Sync>;
const CAPACITY: usize = 2;
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
fn fail(code: &'static str, message: impl AsRef<str>) -> WireError {
    let text = message.as_ref();
    let mut end = text.len().min(1024);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    WireError::new(code, &text[..end])
}
fn copy_error(e: &WireError) -> WireError {
    fail(e.code, &e.message)
}
fn native_error(e: download::Error) -> WireError {
    fail(e.code, e.message)
}
fn core_error(e: WireError) -> CoreError {
    CoreError::NativeOperation {
        code: e.code,
        message: e.message,
    }
}
#[derive(Clone)]
pub struct Config {
    pub download: download::Config,
    pub shutdown_timeout: Duration,
    pub tool_timeout: Duration,
    pub controlled_test: Option<ControlledTest>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            download: download::Config::default(),
            shutdown_timeout: Duration::from_secs(5),
            tool_timeout: Duration::from_secs(10),
            controlled_test: None,
        }
    }
}
#[derive(Clone)]
pub struct ControlledTest {
    platform: String,
    arch: String,
    url: download::ControlledTestConfig,
    tools: Option<[PathBuf; 2]>,
}
impl ControlledTest {
    /// Native-only inert fixture override; never derive these values from IPC.
    pub fn loopback(platform: &str, arch: &str, url: &str) -> Result<Self, WireError> {
        if platform.len() > 32 || arch.len() > 32 {
            return Err(fail(
                "download_configuration",
                "Fixture platform/architecture exceeds limit",
            ));
        }
        Ok(Self {
            platform: platform.into(),
            arch: arch.into(),
            url: download::ControlledTestConfig::loopback(url).map_err(native_error)?,
            tools: None,
        })
    }
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "Exercised by the dedicated FFmpeg download integration target; the binary unit harness does not invoke every fixture hook"
    )]
    pub fn mac_tools(mut self, xattr: PathBuf, codesign: PathBuf) -> Result<Self, WireError> {
        if [&xattr, &codesign]
            .iter()
            .any(|p| !p.is_absolute() || p.as_os_str().as_encoded_bytes().len() > 32768)
        {
            return Err(fail(
                "download_configuration",
                "Fixture tool must be an absolute bounded native path",
            ));
        }
        self.tools = Some([xattr, codesign]);
        Ok(self)
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Preparing,
    Accepted,
    Ready,
    Closed,
}
struct Cell {
    context: RuntimeContext,
    token: CancellationToken,
    phase: Mutex<Phase>,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
}
impl Cell {
    fn release(&self) {
        *lock(&self.phase) = Phase::Closed;
        lock(&self.permit).take();
    }
    fn cancel_waiter(&self) {
        if *lock(&self.phase) == Phase::Preparing {
            self.token.cancel();
        }
    }
}
struct Core {
    library: LibraryService,
    media: MediaService,
    root: PathBuf,
    config: Config,
    publish: Publish,
    service: DownloadService,
    closed: AtomicBool,
    cells: Mutex<Vec<Weak<Cell>>>,
    failure: Mutex<Vec<WireError>>,
    pumps: AtomicUsize,
    slots: Arc<Semaphore>,
}
impl Core {
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.slots.close();
        for cell in lock(&self.cells).iter().filter_map(Weak::upgrade) {
            cell.token.cancel();
            let ready = *lock(&cell.phase) == Phase::Ready;
            if ready {
                cell.release();
            }
        }
    }
    fn remember(&self, error: WireError) {
        // A retained-owner timeout is retryable, unlike a completed cleanup
        // receipt reporting a filesystem fault.
        if error.code == "download_shutdown_timeout" {
            return;
        }
        let mut errors = lock(&self.failure);
        if errors.len() < 4
            && !errors
                .iter()
                .any(|e| e.code == error.code && e.message == error.message)
        {
            errors.push(error);
        }
    }
    fn current(&self, cell: &Cell) -> Result<(), WireError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(fail("download_shutdown", "Native downloads are closed"));
        }
        if cell.token.is_cancelled() {
            return Err(fail(
                "download_cancelled",
                "Native download context or waiter cancelled",
            ));
        }
        self.library
            .contextualize(&context_request(&cell.context, None), &mut [])
            .map_err(WireError::from)
    }
    fn emit(&self, cell: &Cell, mut event: Event) -> Result<(), WireError> {
        self.current(cell)?;
        self.library
            .contextualize(
                &context_request(&cell.context, None),
                std::slice::from_mut(&mut event),
            )
            .map_err(WireError::from)?;
        (self.publish)(event).map_err(|e| fail("download_delivery", e))
    }
}
struct Work {
    request: Request,
    cell: Arc<Cell>,
    reply: oneshot::Sender<Result<DownloadResult, WireError>>,
}
struct Inner {
    core: Arc<Core>,
    sender: Mutex<Option<mpsc::Sender<Work>>>,
    tasks: AsyncMutex<Vec<tokio::task::JoinHandle<()>>>,
    shutdown: AsyncMutex<()>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.core.close();
        lock(&self.sender).take();
    }
}
#[derive(Clone)]
pub struct NativeFfmpegDownloads {
    inner: Arc<Inner>,
}
pub struct DownloadTicket {
    cell: Arc<Cell>,
    reply: oneshot::Receiver<Result<DownloadResult, WireError>>,
    armed: bool,
}
impl DownloadTicket {
    pub fn cancel(&self) {
        self.cell.cancel_waiter();
    }
    pub async fn finish(mut self) -> Result<DownloadResult, WireError> {
        let result = (&mut self.reply).await.map_err(|_| {
            fail(
                "download_worker",
                "Native download owner stopped before reply",
            )
        })?;
        self.armed = false;
        result
    }
}
impl Drop for DownloadTicket {
    fn drop(&mut self) {
        if self.armed {
            self.cancel();
        }
    }
}
pub struct DownloadResult {
    events: Vec<Event>,
    pub diagnostics: Vec<WireError>,
    cell: Arc<Cell>,
    core: Arc<Core>,
}
impl DownloadResult {
    /// Consume the ready-result lease only when the host is ready to publish.
    pub fn into_events(mut self) -> Result<Vec<Event>, WireError> {
        self.core.current(&self.cell)?;
        if *lock(&self.cell.phase) != Phase::Ready {
            return Err(fail(
                "download_shutdown",
                "Ready download result was revoked",
            ));
        }
        self.core
            .library
            .contextualize(&context_request(&self.cell.context, None), &mut self.events)
            .map_err(WireError::from)?;
        self.cell.release();
        Ok(self.events)
    }
}
fn context_request(context: &RuntimeContext, listener_id: Option<String>) -> Request {
    Request {
        channel: "MAIN".into(),
        data: json!({"channel":"FFMPEG_DOWNLOAD","data":null}),
        context: Some(context.clone()),
        listener_id,
    }
}
fn owner(context: &RuntimeContext) -> Result<DownloadOwner, WireError> {
    let path = context
        .data_path
        .to_str()
        .ok_or_else(|| fail("download_context", "Non-Unicode library context"))?;
    DownloadOwner::new(&context.runtime_id, context.library_generation, path).map_err(native_error)
}
fn platform() -> (&'static str, &'static str) {
    let os = if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "ia32",
        "aarch64" => "arm64",
        other => other,
    };
    (os, arch)
}
impl NativeFfmpegDownloads {
    pub fn new(
        library: LibraryService,
        media: MediaService,
        app_user_data: PathBuf,
        mut config: Config,
        publish: Publish,
    ) -> Result<Self, WireError> {
        if !app_user_data.is_absolute()
            || app_user_data
                .to_str()
                .is_none_or(|p| p.len() > 32690 || p.contains('\0'))
            || config.shutdown_timeout.is_zero()
            || config.shutdown_timeout > Duration::from_secs(30)
            || config.tool_timeout.is_zero()
            || config.tool_timeout > Duration::from_secs(30)
        {
            return Err(fail(
                "download_configuration",
                "Invalid native download configuration",
            ));
        }
        tokio::runtime::Handle::try_current().map_err(|_| {
            fail(
                "download_runtime",
                "Native downloads require an active runtime",
            )
        })?;
        if let Some(test) = &config.controlled_test {
            config.download.controlled_test = Some(test.url.clone());
        } else if config.download.controlled_test.is_some() {
            return Err(fail(
                "download_configuration",
                "Loopback requires explicit host controlled-test configuration",
            ));
        }
        let service = DownloadService::start(config.download.clone()).map_err(native_error)?;
        let core = Arc::new(Core {
            library,
            media,
            root: app_user_data,
            config,
            publish,
            service,
            closed: AtomicBool::new(false),
            cells: Mutex::new(Vec::with_capacity(CAPACITY)),
            failure: Mutex::new(vec![]),
            pumps: AtomicUsize::new(CAPACITY),
            slots: Arc::new(Semaphore::new(CAPACITY)),
        });
        let (sender, receive) = mpsc::channel::<Work>(CAPACITY);
        let receive = Arc::new(AsyncMutex::new(receive));
        let mut tasks = vec![];
        for _ in 0..CAPACITY {
            let core = core.clone();
            let receive = receive.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    let work = {
                        let mut receiver = receive.lock().await;
                        if core.closed.load(Ordering::Acquire) {
                            receiver.close();
                        }
                        receiver.recv().await
                    };
                    let Some(work) = work else {
                        break;
                    };
                    let result = run(core.clone(), &work.request, work.cell.clone()).await;
                    if result.is_err() {
                        work.cell.release();
                    }
                    let _ = work.reply.send(result);
                }
                if core.pumps.fetch_sub(1, Ordering::AcqRel) == 1 {
                    if let Err(error) = core.service.shutdown().await {
                        core.remember(native_error(error));
                    }
                }
            }));
        }
        Ok(Self {
            inner: Arc::new(Inner {
                core,
                sender: Mutex::new(Some(sender)),
                tasks: AsyncMutex::new(tasks),
                shutdown: AsyncMutex::new(()),
            }),
        })
    }
    pub fn begin(&self, request: Request, window: &str) -> Result<DownloadTicket, WireError> {
        if window != "main"
            || request.channel != "MAIN"
            || request.data["channel"] != "FFMPEG_DOWNLOAD"
        {
            return Err(fail(
                "download_role",
                "Only main FFMPEG_DOWNLOAD is accepted",
            ));
        }
        let core = &self.inner.core;
        if core.closed.load(Ordering::Acquire) {
            return Err(fail("download_shutdown", "Native downloads are closed"));
        }
        core.library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        let context = request
            .context
            .as_ref()
            .ok_or_else(|| fail("stale_library", "Missing download context"))?;
        owner(context)?;
        if request.listener_id.as_ref().is_some_and(|v| v.len() > 4096) {
            return Err(fail("download_limit", "Download listener exceeds limit"));
        }
        let permit = core
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| fail("download_busy", "Native download requests are at capacity"))?;
        let cell = Arc::new(Cell {
            context: context.clone(),
            token: CancellationToken::new(),
            phase: Mutex::new(Phase::Preparing),
            permit: Mutex::new(Some(permit)),
        });
        {
            let mut cells = lock(&core.cells);
            cells.retain(|c| {
                c.upgrade()
                    .is_some_and(|c| *lock(&c.phase) != Phase::Closed)
            });
            if core.closed.load(Ordering::Acquire) {
                return Err(fail("download_shutdown", "Native downloads are closed"));
            }
            cells.push(Arc::downgrade(&cell));
        }
        let request = context_request(context, request.listener_id);
        let (reply, response) = oneshot::channel();
        lock(&self.inner.sender)
            .as_ref()
            .ok_or_else(|| fail("download_shutdown", "Native download queue is closed"))?
            .try_send(Work {
                request,
                cell: cell.clone(),
                reply,
            })
            .map_err(|_| fail("download_busy", "Native download queue is unavailable"))?;
        Ok(DownloadTicket {
            cell,
            reply: response,
            armed: true,
        })
    }
    pub fn context_changed(&self, context: &RuntimeContext) {
        for cell in lock(&self.inner.core.cells)
            .iter()
            .filter_map(Weak::upgrade)
        {
            if &cell.context != context {
                cell.token.cancel();
                let ready = *lock(&cell.phase) == Phase::Ready;
                if ready {
                    cell.release();
                }
            }
        }
    }
    pub fn status(&self) -> Value {
        let core = &self.inner.core;
        let ready = lock(&core.cells)
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|c| *lock(&c.phase) == Phase::Ready)
            .count();
        json!({"closing":core.closed.load(Ordering::Acquire),"admittedRequests":CAPACITY-core.slots.available_permits(),"readyUncollected":ready,"foundation":core.service.status(),"failures":*lock(&core.failure)})
    }
    pub async fn shutdown(&self) -> Result<(), WireError> {
        self.inner.core.close();
        lock(&self.inner.sender).take();
        let deadline = Instant::now() + self.inner.core.config.shutdown_timeout;
        let drain = async {
            let _serial = self.inner.shutdown.lock().await;
            let mut tasks = self.inner.tasks.lock().await;
            while !tasks.is_empty() {
                if let Err(error) = (&mut tasks[0]).await {
                    self.inner
                        .core
                        .remember(fail("download_worker", error.to_string()));
                }
                drop(tasks.remove(0));
            }
            drop(tasks);
            if let Err(error) = self.inner.core.service.shutdown().await {
                self.inner.core.remember(native_error(error));
            }
            // A timeout/fault is not a cleanup receipt. In particular, a
            // panicked pump's accepted guard may still be queued for bounded
            // filesystem cleanup. Keep every remaining host lease until the
            // real foundation owners have exited; a later shutdown retries.
            let status = self.inner.core.service.status();
            if status.admitted != 0 || status.active != 0 || status.workers != 0 {
                return Err(fail(
                    "download_shutdown_timeout",
                    "Download filesystem owners still retain actual work; retry required",
                ));
            }
            self.inner.core.close();
            for cell in lock(&self.inner.core.cells)
                .iter()
                .filter_map(Weak::upgrade)
            {
                cell.release();
            }
            let errors = lock(&self.inner.core.failure);
            if let Some(error) = errors.first() {
                Err(copy_error(error))
            } else {
                Ok(())
            }
        };
        tokio::time::timeout_at(deadline.into(), drain)
            .await
            .map_err(|_| {
                fail(
                    "download_shutdown_timeout",
                    "Native download owners have not drained actual work; retry required",
                )
            })?
    }
}

#[derive(Clone)]
struct Identity {
    #[cfg(not(unix))]
    created: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}
impl Identity {
    fn capture(path: &Path) -> Result<Self, WireError> {
        let metadata = fs::symlink_metadata(path).map_err(fs_error)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(fail(
                "download_root_identity",
                "Native download root is not a regular directory",
            ));
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            #[cfg(not(unix))]
            created: metadata.created().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
    fn verify(&self, path: &Path) -> Result<(), WireError> {
        let current = Self::capture(path)?;
        #[cfg(unix)]
        let same = self.device == current.device && self.inode == current.inode;
        #[cfg(not(unix))]
        let same = self.created.is_some() && self.created == current.created;
        if same {
            Ok(())
        } else {
            Err(fail(
                "download_root_identity",
                "Native download root identity changed",
            ))
        }
    }
}
#[derive(Clone)]
struct Authority {
    app: Identity,
    bin: Identity,
}
fn fs_error(error: std::io::Error) -> WireError {
    fail(
        "download_filesystem",
        format!(
            "Native download filesystem operation failed ({:?})",
            error.kind()
        ),
    )
}
fn root_matches(runtime: &Runtime, root: &Path) -> Result<(), WireError> {
    if runtime.stores.app_data != root {
        return Err(fail(
            "download_root_identity",
            "Native application user-data root does not match the library owner",
        ));
    }
    Ok(())
}
async fn prepare_root(core: &Core, cell: Arc<Cell>) -> Result<Authority, WireError> {
    let root = core.root.clone();
    let output = Arc::new(Mutex::new(None));
    let result = output.clone();
    core.library
        .commit(
            context_request(&cell.context, None),
            "main",
            move |runtime| {
                if cell.token.is_cancelled() {
                    return Err(core_error(fail(
                        "download_cancelled",
                        "Download root preparation cancelled",
                    )));
                }
                root_matches(runtime, &root).map_err(core_error)?;
                let app = Identity::capture(&root).map_err(core_error)?;
                let bin = root.join("bin");
                fs::create_dir_all(&bin).map_err(|e| core_error(fs_error(e)))?;
                let bin = Identity::capture(&bin).map_err(core_error)?;
                *lock(&result) = Some(Authority { app, bin });
                Ok(vec![])
            },
        )
        .await
        .map_err(WireError::from)?;
    let authority = lock(&output).take().ok_or_else(|| {
        fail(
            "download_worker",
            "Ordered root preparation returned no authority",
        )
    })?;
    Ok(authority)
}
struct Install {
    guard: Option<AcceptedInstall>,
    renamed: bool,
    commands: Vec<HostCommand>,
}
async fn install(
    core: &Core,
    cell: Arc<Cell>,
    authority: Authority,
    guard: AcceptedInstall,
) -> (
    Result<(), WireError>,
    AcceptedInstall,
    Vec<HostCommand>,
    bool,
) {
    let resources = Arc::new(Mutex::new(Install {
        guard: Some(guard),
        renamed: false,
        commands: vec![],
    }));
    let operation = resources.clone();
    let root = core.root.clone();
    let context = cell.context.clone();
    let result = core
        .library
        .commit(context_request(&context, None), "main", move |runtime| {
            root_matches(runtime, &root).map_err(core_error)?;
            authority.app.verify(&root).map_err(core_error)?;
            authority
                .bin
                .verify(&root.join("bin"))
                .map_err(core_error)?;
            let current = owner(&runtime.context()).map_err(core_error)?;
            let mut resources = lock(&operation);
            let view = resources
                .guard
                .as_ref()
                .expect("owned accepted install")
                .verify_for_install(&current)
                .map_err(|e| core_error(native_error(e)))?;
            let staged = view.staged_path.to_path_buf();
            let destination = view.destination.clone();
            let mode = view.mode;
            let commands = view.post_install_commands();
            if let Some(mode) = mode {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&staged, fs::Permissions::from_mode(mode))
                        .map_err(|e| core_error(fs_error(e)))?;
                }
                #[cfg(not(unix))]
                {
                    let _ = mode;
                    return Err(core_error(fail(
                        "download_mode",
                        "Unix permissions are unsupported on this host",
                    )));
                }
            }
            fs::rename(&staged, &destination).map_err(|e| core_error(fs_error(e)))?;
            resources.renamed = true;
            resources.commands = commands;
            Ok(vec![])
        })
        .await
        .map(|_| ())
        .map_err(WireError::from);
    let mut resources = lock(&resources);
    let renamed = resources.renamed;
    let guard = resources
        .guard
        .take()
        .expect("host owns accepted install after ordered result");
    (
        result,
        guard,
        std::mem::take(&mut resources.commands),
        renamed,
    )
}
async fn mac_tool(
    core: &Core,
    cell: &Cell,
    index: usize,
    command: &HostCommand,
) -> Result<Option<WireError>, WireError> {
    let executable = core
        .config
        .controlled_test
        .as_ref()
        .and_then(|t| t.tools.as_ref())
        .map(|p| p[index].as_os_str())
        .unwrap_or_else(|| std::ffi::OsStr::new(command.program));
    if cell.token.is_cancelled() {
        return Err(fail("download_cancelled", "Mac post-install cancelled"));
    }
    let mut command_builder = tokio::process::Command::new(executable);
    command_builder
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = match command_builder.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Ok(Some(fail(
                "download_mac_tool",
                format!("{} failed to start ({:?})", command.program, error.kind()),
            )))
        }
    };
    let result = tokio::select! {
        biased;
        _ = cell.token.cancelled() => Err(fail("download_cancelled", "Mac post-install cancelled")),
        result = tokio::time::timeout(core.config.tool_timeout, child.wait()) => match result {
            Ok(Ok(status)) if status.success() => Ok(None),
            Ok(Ok(status)) => Ok(Some(fail("download_mac_tool", format!("{} exited with {status}", command.program)))),
            Ok(Err(error)) => Err(fail("download_mac_wait", error.to_string())),
            Err(_) => Err(fail("download_mac_timeout", "Mac post-install child exceeded timeout")),
        }
    };
    if result.is_err() {
        let _ = child.start_kill();
        // Even an OS wait error is not evidence that the child exited. Keep
        // this fixed pump/guard/lease and retry real reaping. An outer host
        // deadline can report retained ownership, never detach this child.
        while let Err(error) = child.wait().await {
            core.remember(fail("download_mac_reap", error.to_string()));
            let _ = child.start_kill();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    result
}
fn diagnostic(errors: &mut Vec<WireError>, error: WireError) {
    if errors.len() < 8
        && !errors
            .iter()
            .any(|e| e.code == error.code && e.message == error.message)
    {
        errors.push(error);
    }
}
async fn prepare_and_install(
    core: &Core,
    cell: Arc<Cell>,
    plan: download::HostDownloadPlan,
    owner: &DownloadOwner,
    diagnostics: &mut Vec<WireError>,
) -> Result<(), WireError> {
    let authority = prepare_root(core, cell.clone()).await?;
    core.current(&cell)?;
    let ticket = core
        .service
        .begin(
            plan,
            cell.token.clone(),
            Instant::now() + core.config.download.timeout,
        )
        .map_err(native_error)?;
    let mut progress = ticket.progress();
    let finish = ticket.finish();
    tokio::pin!(finish);
    let mut progress_open = true;
    let prepared = loop {
        tokio::select! {
            biased;
            result = &mut finish => break result.map_err(native_error)?,
            changed = progress.changed(), if progress_open => {
                progress_open = changed.is_ok();
                if progress_open {
                    let latest = progress.borrow_and_update().clone();
                    if let Some(progress) = latest {
                        let event = Event::reply("MAIN", "MEDIA_DOWNLOAD_PROGRESS",
                            serde_json::to_value(progress).expect("finite real progress"), None);
                        if let Err(error) = core.emit(&cell, event) {
                            diagnostic(diagnostics, error);
                        }
                    }
                }
            }
        }
    };
    // Synchronize caller Drop with the transition to host-owned effects.
    // Context/CLOSE cancellation remains active after this transition.
    let cancelled = {
        let mut phase = lock(&cell.phase);
        if cell.token.is_cancelled() {
            true
        } else {
            *phase = Phase::Accepted;
            false
        }
    };
    if cancelled {
        prepared.discard().await.map_err(native_error)?;
        return Err(fail(
            "download_cancelled",
            "Prepared native download cancelled",
        ));
    }
    let accepted = prepared
        .accept_or_discard(owner)
        .await
        .map_err(native_error)?;
    let (mut outcome, accepted, commands, renamed) =
        install(core, cell.clone(), authority, accepted).await;
    if renamed {
        for (index, command) in commands.iter().enumerate() {
            match mac_tool(core, &cell, index, command).await {
                Ok(Some(error)) if command.on_failure == "warn" => diagnostic(diagnostics, error),
                Ok(_) => (),
                Err(error) => {
                    diagnostic(diagnostics, copy_error(&error));
                    if outcome.is_ok() {
                        outcome = Err(error);
                    }
                }
            }
        }
        // A successful replacement invalidates even if mac cancellation,
        // context change or an earlier owner error prevents source success.
        core.media.clear_ffmpeg_path_cache().await;
    }
    if let Err(error) = accepted.finish().await {
        let error = native_error(error);
        core.remember(copy_error(&error));
        if outcome.is_ok() {
            outcome = Err(copy_error(&error));
        }
        diagnostic(diagnostics, error);
    }
    outcome
}
async fn run(
    core: Arc<Core>,
    request: &Request,
    cell: Arc<Cell>,
) -> Result<DownloadResult, WireError> {
    core.current(&cell)?;
    let owner = owner(&cell.context)?;
    let (os, arch) = platform();
    let (os, arch) = core
        .config
        .controlled_test
        .as_ref()
        .map_or((os, arch), |t| (t.platform.as_str(), t.arch.as_str()));
    let mut diagnostics = vec![];
    let result = match download::plan_for_host(os, arch, core.root.join("bin"), owner.clone()) {
        Ok(plan) => prepare_and_install(&core, cell.clone(), plan, &owner, &mut diagnostics).await,
        Err(error) => Err(native_error(error)),
    };
    if core.closed.load(Ordering::Acquire) || cell.token.is_cancelled() {
        return Err(fail(
            "download_cancelled",
            "Native download was cancelled or closed after draining its effects",
        ));
    }
    core.library
        .contextualize(request, &mut [])
        .map_err(WireError::from)?;
    let completion = match &result {
        Ok(()) => download::source_completion(true, None),
        Err(error) => {
            diagnostic(&mut diagnostics, copy_error(error));
            download::source_completion(false, Some(&error.message))
        }
    };
    if let Err(error) = core.emit(
        &cell,
        Event::reply(
            "MAIN",
            "MEDIA_DOWNLOAD_PROGRESS",
            serde_json::to_value(completion.progress).expect("finite completion"),
            None,
        ),
    ) {
        diagnostic(&mut diagnostics, error);
    }
    if core.closed.load(Ordering::Acquire) || cell.token.is_cancelled() {
        return Err(fail("download_cancelled", "Native completion revoked"));
    }
    {
        let mut phase = lock(&cell.phase);
        if core.closed.load(Ordering::Acquire) || cell.token.is_cancelled() {
            return Err(fail("download_cancelled", "Ready completion revoked"));
        }
        *phase = Phase::Ready;
    }
    Ok(DownloadResult {
        events: vec![Event::reply(
            "MAIN",
            "FFMPEG_DOWNLOAD",
            completion.reply,
            request.listener_id.clone(),
        )],
        diagnostics,
        cell,
        core,
    })
}
