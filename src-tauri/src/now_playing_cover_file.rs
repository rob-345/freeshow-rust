//! Native fixed-cover IO on the existing ordered owner. No route, image decode,
//! request-selected path, callback emission, task or thread is created here.
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    service::{LibraryPhase, PhaseTicket},
};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

pub const MAX_FILE: u64 = 4 * 1024 * 1024;
pub const MAX_CHUNK: usize = 32 * 1024;
const CAPACITY: usize = 4;
static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
pub type Result<T> = std::result::Result<T, Failure>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Busy,
    Cancelled,
    Deadline,
    Unsupported,
    Changed,
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "The explicitly unavailable anchored backend constructs this variant only on Linux"
        )
    )]
    Io,
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "The explicitly unavailable anchored backend constructs this variant only on Linux"
        )
    )]
    Resource,
    Phase,
}
#[derive(Debug, Clone)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
    pub os_code: Option<i32>,
}
impl Failure {
    fn new(kind: FailureKind, message: impl ToString) -> Self {
        let mut message = message.to_string();
        let mut end = message.len().min(1024);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        Self {
            kind,
            message,
            os_code: None,
        }
    }
    #[cfg(target_os = "linux")]
    fn os(stage: &str, error: io::Error) -> Self {
        Self {
            os_code: error.raw_os_error(),
            ..Self::new(FailureKind::Io, format!("{stage}: {error}"))
        }
    }
    pub fn code(&self) -> &'static str {
        match self.kind {
            FailureKind::Busy => "now_playing_cover_busy",
            FailureKind::Cancelled => "now_playing_cover_cancelled",
            FailureKind::Deadline => "now_playing_cover_deadline",
            FailureKind::Unsupported => "now_playing_cover_unsupported",
            FailureKind::Changed => "now_playing_cover_changed",
            FailureKind::Io => "now_playing_cover_io",
            FailureKind::Resource => "now_playing_cover_resource",
            FailureKind::Phase => "now_playing_cover_phase",
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message)
    }
}
impl std::error::Error for Failure {}

#[derive(Clone)]
pub struct Control {
    pub cancel: CancellationToken,
    pub deadline: Instant,
}
impl Default for Control {
    fn default() -> Self {
        Self {
            cancel: CancellationToken::new(),
            deadline: Instant::now() + Duration::from_secs(10),
        }
    }
}
impl Control {
    fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(Failure::new(
                FailureKind::Cancelled,
                "Native preparation cancelled",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(Failure::new(
                FailureKind::Deadline,
                "Native preparation deadline",
            ));
        }
        Ok(())
    }
}
struct Lease {
    _permit: OwnedSemaphorePermit,
}
/// Every instance shares the same process-wide four whole-work/result leases.
/// A future host must retain/drain its own accepted Phase and HTTP owners.
#[derive(Clone, Default)]
pub struct CoverFiles {
    #[cfg(all(test, target_os = "linux"))]
    hook: Arc<std::sync::Mutex<Option<Pause>>>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}
impl FileStat {
    #[cfg(test)]
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "Dedicated cover integration targets exercise this observation/test hook"
        )
    )]
    pub fn device(&self) -> u64 {
        self.device
    }
    #[cfg(test)]
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "Dedicated cover integration targets exercise this observation/test hook"
        )
    )]
    pub fn inode(&self) -> u64 {
        self.inode
    }
    pub fn length(&self) -> u64 {
        self.length
    }
    pub fn modified_seconds(&self) -> i64 {
        self.modified_seconds
    }
    pub fn modified_nanoseconds(&self) -> i64 {
        self.modified_nanoseconds
    }
    #[cfg(test)]
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "Dedicated cover integration targets exercise this observation/test hook"
        )
    )]
    pub fn changed_seconds(&self) -> i64 {
        self.changed_seconds
    }
    #[cfg(test)]
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "Dedicated cover integration targets exercise this observation/test hook"
        )
    )]
    pub fn changed_nanoseconds(&self) -> i64 {
        self.changed_nanoseconds
    }
}
#[derive(Debug)]
pub struct FolderFailure {
    folder: PathBuf,
    diagnostic: Failure,
}
impl FolderFailure {
    pub fn folder(&self) -> &Path {
        &self.folder
    }
    pub fn diagnostic(&self) -> &Failure {
        &self.diagnostic
    }
}
/// Holds diagnostic/result admission even for Missing or Failed. No notification
/// has been emitted and no renderer delivery has been verified.
pub struct Observation {
    context: RuntimeContext,
    folder_failure: Option<FolderFailure>,
    diagnostics: Vec<Failure>,
    authority: Option<Arc<Authority>>,
    _lease: Arc<Lease>,
}
impl Observation {
    pub fn context(&self) -> &RuntimeContext {
        &self.context
    }
    pub fn folder_failure(&self) -> Option<&FolderFailure> {
        self.folder_failure.as_ref()
    }
    pub fn diagnostics(&self) -> &[Failure] {
        &self.diagnostics
    }
    /// Missing is an already observed exists-false result, not a promise it
    /// remains absent. Revalidation checks held namespace guards, not absence.
    pub fn revalidate(&self, phase: &LibraryPhase, control: Control) -> Result<ValidationTicket> {
        let authority = self.authority.clone().ok_or_else(|| {
            Failure::new(
                FailureKind::Unsupported,
                "No anchored authority for failed preparation",
            )
        })?;
        validate_submit(authority, phase, control)
    }
}
pub enum Outcome {
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Only the verified Linux backend constructs a successful outcome"
        )
    )]
    Missing,
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Only the verified Linux backend constructs held cover authority"
        )
    )]
    File(HeldCover),
    Failed(Failure),
}
pub struct Prepared {
    observation: Observation,
    outcome: Outcome,
}
impl Prepared {
    #[cfg(test)]
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "Dedicated cover integration targets exercise this observation/test hook"
        )
    )]
    pub fn observation(&self) -> &Observation {
        &self.observation
    }
    #[cfg(test)]
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "Dedicated cover integration targets exercise this observation/test hook"
        )
    )]
    pub fn outcome(&self) -> &Outcome {
        &self.outcome
    }
    /// The diagnostic observation and file share admission. Keep Observation
    /// through any independent notification; HeldCover always retains FD quota.
    pub fn into_parts(self) -> (Observation, Outcome) {
        (self.observation, self.outcome)
    }
}
/// Opaque held descriptor: no File extraction/clone that can outlive its lease.
pub struct HeldCover {
    authority: Arc<Authority>,
}
impl HeldCover {
    pub fn context(&self) -> &RuntimeContext {
        &self.authority.context
    }
    pub fn stat(&self) -> &FileStat {
        &self.authority.leaf.as_ref().expect("opened cover").1
    }
    pub fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.len() > MAX_CHUNK
            || offset > MAX_FILE
            || (buffer.len() as u64) > MAX_FILE - offset
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Bounded cover read range required",
            ));
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::FileExt;
            self.authority
                .leaf
                .as_ref()
                .expect("opened cover")
                .0
                .read_at(buffer, offset)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = buffer;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Anchored backend unavailable",
            ))
        }
    }
    pub fn revalidate(&self, phase: &LibraryPhase, control: Control) -> Result<ValidationTicket> {
        validate_submit(self.authority.clone(), phase, control)
    }
}
struct Authority {
    context: RuntimeContext,
    #[cfg(target_os = "linux")]
    root: File,
    #[cfg(target_os = "linux")]
    audio: Option<File>,
    leaf: Option<(File, FileStat)>,
    validating: AtomicBool,
    _lease: Arc<Lease>,
}
pub struct Ticket {
    ordered: PhaseTicket,
    reply: oneshot::Receiver<Prepared>,
}
impl Ticket {
    pub async fn finish(self) -> Result<Prepared> {
        self.ordered
            .finish()
            .await
            .map_err(|e| Failure::new(FailureKind::Phase, e))?;
        self.reply
            .await
            .map_err(|_| Failure::new(FailureKind::Phase, "Ordered cover receipt lost"))
    }
}
pub struct Validation {
    guard: ValidationGuard,
    result: Result<Option<FileStat>>,
}
// One live continuation/result per held authority. It shares the original
// whole-operation slot and cannot accumulate unbounded ready observations.
struct ValidationGuard {
    authority: Arc<Authority>,
}
impl Drop for ValidationGuard {
    fn drop(&mut self) {
        self.authority.validating.store(false, Ordering::Release);
    }
}
impl Validation {
    pub fn context(&self) -> &RuntimeContext {
        &self.guard.authority.context
    }
    pub fn stat(&self) -> std::result::Result<Option<&FileStat>, &Failure> {
        self.result.as_ref().map(Option::as_ref)
    }
}
pub struct ValidationTicket {
    ordered: PhaseTicket,
    reply: oneshot::Receiver<Validation>,
}
impl ValidationTicket {
    pub async fn finish(self) -> Result<Validation> {
        self.ordered
            .finish()
            .await
            .map_err(|e| Failure::new(FailureKind::Phase, e))?;
        self.reply
            .await
            .map_err(|_| Failure::new(FailureKind::Phase, "Ordered revalidation receipt lost"))
    }
}
fn request(context: &RuntimeContext) -> Request {
    Request {
        channel: "MAIN".into(),
        data: serde_json::Value::Null,
        listener_id: None,
        context: Some(context.clone()),
    }
}
impl CoverFiles {
    pub fn retained(&self) -> usize {
        CAPACITY
            - SLOTS
                .get_or_init(|| Arc::new(Semaphore::new(CAPACITY)))
                .available_permits()
    }
    pub fn submit(&self, phase: &LibraryPhase, control: Control) -> Result<Ticket> {
        control.check()?;
        let permit = SLOTS
            .get_or_init(|| Arc::new(Semaphore::new(CAPACITY)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                Failure::new(
                    FailureKind::Busy,
                    "Four cover work/descriptors/results remain owned",
                )
            })?;
        let lease = Arc::new(Lease { _permit: permit });
        let context = phase.context().clone();
        let (send, reply) = oneshot::channel();
        #[cfg(all(test, target_os = "linux"))]
        let hooks = self.clone();
        let ordered = phase
            .submit(request(&context), move |runtime| {
                let observation = Observation {
                    context,
                    folder_failure: None,
                    diagnostics: Vec::new(),
                    authority: None,
                    _lease: lease.clone(),
                };
                #[cfg(target_os = "linux")]
                let mut observation = observation;
                let result = (|| {
                    control.check()?;
                    if runtime.context() != observation.context {
                        return Err(Failure::new(
                            FailureKind::Changed,
                            "Ordered context changed",
                        ));
                    }
                    #[cfg(target_os = "linux")]
                    {
                        backend::prepare(
                            &mut observation,
                            lease,
                            &control,
                            #[cfg(test)]
                            &hooks,
                        )
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        let _ = lease;
                        Err(Failure::new(
                            FailureKind::Unsupported,
                            "Anchored cover backend verified only on Linux",
                        ))
                    }
                })();
                let outcome = result.unwrap_or_else(Outcome::Failed);
                let _ = send.send(Prepared {
                    observation,
                    outcome,
                });
                Ok(vec![])
            })
            .map_err(|e| Failure::new(FailureKind::Phase, e))?;
        Ok(Ticket { ordered, reply })
    }
}
fn validate_submit(
    authority: Arc<Authority>,
    phase: &LibraryPhase,
    control: Control,
) -> Result<ValidationTicket> {
    control.check()?;
    if phase.context() != &authority.context {
        return Err(Failure::new(
            FailureKind::Changed,
            "Foreign revalidation context",
        ));
    }
    authority
        .validating
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .map_err(|_| {
            Failure::new(
                FailureKind::Busy,
                "One revalidation continuation/result remains owned",
            )
        })?;
    let guard = ValidationGuard { authority };
    let (send, reply) = oneshot::channel();
    let ordered = phase
        .submit(request(&guard.authority.context), move |_| {
            let result = {
                #[cfg(target_os = "linux")]
                {
                    backend::revalidate(&guard.authority, &control)
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = control;
                    Err(Failure::new(
                        FailureKind::Unsupported,
                        "Anchored cover backend verified only on Linux",
                    ))
                }
            };
            let _ = send.send(Validation { guard, result });
            Ok(vec![])
        })
        .map_err(|e| Failure::new(FailureKind::Phase, e))?;
    Ok(ValidationTicket { ordered, reply })
}

#[cfg(target_os = "linux")]
mod backend {
    use super::*;
    use std::{
        ffi::{CStr, CString},
        mem::MaybeUninit,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
        path::Component,
    };
    const AUDIO: &CStr = c"Audio";
    const COVER: &CStr = c"NowPlayingCover.png";
    fn changed() -> Failure {
        Failure::new(
            FailureKind::Changed,
            "Fixed cover root, parent, leaf identity or version changed",
        )
    }
    fn unsupported() -> Failure {
        Failure::new(
            FailureKind::Unsupported,
            "Symlink, nonregular or unsupported fixed-path variant",
        )
    }
    fn id(file: &File) -> Result<(u64, u64)> {
        let m = file
            .metadata()
            .map_err(|e| Failure::os("Held identity", e))?;
        Ok((m.dev(), m.ino()))
    }
    fn stat(file: &File) -> Result<FileStat> {
        let m = file
            .metadata()
            .map_err(|e| Failure::os("Held cover stat", e))?;
        if !m.is_file() {
            return Err(unsupported());
        }
        if m.len() > MAX_FILE {
            return Err(Failure::new(FailureKind::Resource, "Cover exceeds4MiB"));
        }
        Ok(FileStat {
            device: m.dev(),
            inode: m.ino(),
            length: m.len(),
            modified_seconds: m.mtime(),
            modified_nanoseconds: m.mtime_nsec(),
            changed_seconds: m.ctime(),
            changed_nanoseconds: m.ctime_nsec(),
        })
    }
    fn at(parent: &File, name: &CStr) -> io::Result<libc::stat> {
        let mut value = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: live directory descriptor, NUL-terminated bounded literal/name,
        // exact libc public stat layout; initialization checked before reading.
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                value.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { value.assume_init() })
    }
    fn open(parent: &File, name: &CStr, flags: i32) -> io::Result<File> {
        // SAFETY: live anchored parent, valid name and public Linux openat flags.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: newly owned descriptor transferred exactly once into File.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    fn directory(parent: &File, name: &CStr) -> Result<File> {
        open(parent, name, libc::O_PATH | libc::O_DIRECTORY).map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP | libc::ENOTDIR) => unsupported(),
            _ => Failure::os("Anchored directory open", e),
        })
    }
    fn root(path: &Path, control: &Control) -> Result<File> {
        if !path.is_absolute() || path.as_os_str().as_bytes().len() > 32768 {
            return Err(unsupported());
        }
        let mut current = std::fs::OpenOptions::new();
        use std::os::unix::fs::OpenOptionsExt;
        current
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let mut current = current
            .open("/")
            .map_err(|e| Failure::os("Filesystem root anchor", e))?;
        let mut count = 0;
        for part in path.components() {
            control.check()?;
            match part {
                Component::RootDir => {}
                Component::Normal(name) => {
                    count += 1;
                    if count > 128 {
                        return Err(Failure::new(FailureKind::Resource, "Root component cap128"));
                    }
                    let name = CString::new(name.as_bytes()).map_err(|_| unsupported())?;
                    current = directory(&current, &name)?;
                }
                _ => return Err(unsupported()),
            }
        }
        Ok(current)
    }
    fn mode(value: &libc::stat) -> libc::mode_t {
        value.st_mode & libc::S_IFMT
    }
    fn matches(file: &File, expected: &libc::stat) -> Result<()> {
        if id(file)? != (expected.st_dev, expected.st_ino) {
            return Err(changed());
        }
        Ok(())
    }
    pub(super) fn prepare(
        observation: &mut Observation,
        lease: Arc<Lease>,
        control: &Control,
        #[cfg(test)] hooks: &CoverFiles,
    ) -> Result<Outcome> {
        let held_root = root(&observation.context.data_path, control)?;
        #[cfg(test)]
        hooks.pause(TestStage::RootAnchored);
        if id(&root(&observation.context.data_path, control)?)? != id(&held_root)? {
            return Err(changed());
        }
        let initial = at(&held_root, AUDIO);
        let make = match &initial {
            Ok(s) if mode(s) == libc::S_IFDIR => false,
            Ok(s) if mode(s) == libc::S_IFREG => true,
            Ok(_) => return Err(unsupported()),
            // Source existsSync returns false for an inaccessible/missing path;
            // createFolder then still attempts mkdir and observes its failure.
            Err(_) => true,
        };
        if make {
            control.check()?;
            // SAFETY: fixed single native leaf below the live root descriptor.
            if unsafe { libc::mkdirat(held_root.as_raw_fd(), AUDIO.as_ptr(), 0o777) } != 0 {
                let error = io::Error::last_os_error();
                let raced_directory = error.raw_os_error() == Some(libc::EEXIST)
                    && at(&held_root, AUDIO).is_ok_and(|s| mode(&s) == libc::S_IFDIR);
                if !raced_directory {
                    observation.folder_failure = Some(FolderFailure {
                        folder: observation.context.data_path.join("Audio"),
                        diagnostic: Failure::os("Audio mkdir", error),
                    });
                }
            }
        }
        control.check()?;
        let audio_stat = match at(&held_root, AUDIO) {
            Ok(s) => s,
            Err(e) => {
                observation.diagnostics.push(Failure::os(
                    "Cover existence below absent/inaccessible Audio",
                    e,
                ));
                let authority = Arc::new(Authority {
                    context: observation.context.clone(),
                    root: held_root,
                    audio: None,
                    leaf: None,
                    validating: AtomicBool::new(false),
                    _lease: lease,
                });
                revalidate(&authority, control)?;
                observation.authority = Some(authority);
                return Ok(Outcome::Missing);
            }
        };
        if !matches!(mode(&audio_stat), libc::S_IFDIR | libc::S_IFREG) {
            return Err(unsupported());
        }
        let audio =
            open(&held_root, AUDIO, libc::O_PATH).map_err(|e| Failure::os("Audio anchor", e))?;
        matches(&audio, &audio_stat)?;
        #[cfg(test)]
        hooks.pause(TestStage::AudioAnchored);
        let cover_stat = match at(&audio, COVER) {
            Ok(s) => s,
            Err(e) => {
                observation
                    .diagnostics
                    .push(Failure::os("Source cover exists-false", e));
                let authority = Arc::new(Authority {
                    context: observation.context.clone(),
                    root: held_root,
                    audio: Some(audio),
                    leaf: None,
                    validating: AtomicBool::new(false),
                    _lease: lease,
                });
                revalidate(&authority, control)?;
                observation.authority = Some(authority);
                return Ok(Outcome::Missing);
            }
        };
        if mode(&cover_stat) != libc::S_IFREG {
            return Err(unsupported());
        }
        control.check()?;
        #[cfg(test)]
        hooks.pause(TestStage::BeforeLeafOpen);
        let file = open(&audio, COVER, libc::O_RDONLY | libc::O_NONBLOCK)
            .map_err(|e| Failure::os("Cover open after exists-true", e))?;
        matches(&file, &cover_stat)?;
        let expected = stat(&file)?;
        let authority = Arc::new(Authority {
            context: observation.context.clone(),
            root: held_root,
            audio: Some(audio),
            leaf: Some((file, expected)),
            validating: AtomicBool::new(false),
            _lease: lease,
        });
        #[cfg(test)]
        hooks.pause(TestStage::LeafOpened);
        revalidate(&authority, control)?;
        observation.authority = Some(authority.clone());
        Ok(Outcome::File(HeldCover { authority }))
    }
    pub(super) fn revalidate(authority: &Authority, control: &Control) -> Result<Option<FileStat>> {
        control.check()?;
        let current_root = root(&authority.context.data_path, control)?;
        if id(&current_root)? != id(&authority.root)? {
            return Err(changed());
        }
        let Some(audio) = &authority.audio else {
            return Ok(None);
        };
        let observed = at(&current_root, AUDIO).map_err(|_| changed())?;
        if !matches!(mode(&observed), libc::S_IFDIR | libc::S_IFREG) {
            return Err(unsupported());
        }
        matches(audio, &observed)?;
        let current_audio = open(&current_root, AUDIO, libc::O_PATH).map_err(|_| changed())?;
        if id(&current_audio)? != id(audio)? {
            return Err(changed());
        }
        let Some((file, expected)) = &authority.leaf else {
            control.check()?;
            return Ok(None);
        };
        if stat(file)? != *expected {
            return Err(changed());
        }
        let path_stat = at(&current_audio, COVER).map_err(|_| changed())?;
        if mode(&path_stat) != libc::S_IFREG {
            return Err(changed());
        }
        let path_file = open(&current_audio, COVER, libc::O_RDONLY | libc::O_NONBLOCK)
            .map_err(|_| changed())?;
        matches(&path_file, &path_stat)?;
        if stat(&path_file)? != *expected || stat(file)? != *expected {
            return Err(changed());
        }
        control.check()?;
        Ok(Some(expected.clone()))
    }
}

#[cfg(all(test, target_os = "linux"))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TestStage {
    RootAnchored,
    AudioAnchored,
    BeforeLeafOpen,
    LeafOpened,
}
#[cfg(all(test, target_os = "linux"))]
struct Pause {
    stage: TestStage,
    entered: oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}
#[cfg(all(test, target_os = "linux"))]
impl CoverFiles {
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "Dedicated cover integration targets exercise this observation/test hook"
        )
    )]
    pub fn test_pause(
        &self,
        stage: TestStage,
    ) -> (oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (entered, ready) = oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        assert!(self
            .hook
            .lock()
            .unwrap()
            .replace(Pause {
                stage,
                entered,
                release: wait
            })
            .is_none());
        (ready, release)
    }
    fn pause(&self, stage: TestStage) {
        let pause = {
            let mut held = self.hook.lock().unwrap();
            if held.as_ref().is_some_and(|p| p.stage == stage) {
                held.take()
            } else {
                None
            }
        };
        if let Some(pause) = pause {
            let _ = pause.entered.send(());
            let _ = pause.release.recv();
        }
    }
}
