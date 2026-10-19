//! Native-only registered-file access. No path in this API creates a grant.
use crate::{io, AssetServer, Context, Control, Entry, Error, Shared};
use std::{
    fs::File,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Weak,
    },
};
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};

pub const NATIVE_READ_LIMIT: usize = 4;

/// A lookup into the current registry, never a path authorization request.
#[derive(Clone, Copy)]
pub enum RegisteredMedia<'a> {
    SourceKey(&'a str),
    GrantId(&'a str),
}

pub(crate) struct ReadState {
    slots: Arc<Semaphore>,
    count: AtomicUsize,
    changed: watch::Sender<()>,
}
impl ReadState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            slots: Arc::new(Semaphore::new(NATIVE_READ_LIMIT)),
            count: AtomicUsize::new(0),
            changed: watch::channel(()).0,
        })
    }
    fn admit(self: &Arc<Self>) -> Result<Arc<ReadLease>, Error> {
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            Error::new(
                if self.slots.is_closed() {
                    "assets_shutdown"
                } else {
                    "assets_busy"
                },
                "Native media descriptors/results are closed or at capacity",
            )
        })?;
        self.count.fetch_add(1, Ordering::AcqRel);
        Ok(Arc::new(ReadLease {
            permit: Some(permit),
            state: self.clone(),
        }))
    }
    pub(crate) fn close(&self) {
        self.slots.close();
    }
    pub(crate) fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
    pub(crate) async fn drain(&self) -> Result<(), Error> {
        let mut changed = self.changed.subscribe();
        while self.count() != 0 {
            changed.changed().await.map_err(|_| {
                Error::new("assets_worker", "Native descriptor drain channel ended")
            })?;
        }
        Ok(())
    }
}
pub(crate) struct ReadLease {
    permit: Option<OwnedSemaphorePermit>,
    state: Arc<ReadState>,
}
impl Drop for ReadLease {
    fn drop(&mut self) {
        self.permit.take();
        self.state.count.fetch_sub(1, Ordering::AcqRel);
        self.state.changed.send_replace(());
    }
}

/// An actual held-file version, not a content hash or immutable snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub identity: io::Identity,
    pub length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    fn read(file: &File) -> Result<Self, Error> {
        let metadata = file.metadata().map_err(Error::os)?;
        if !metadata.is_file() {
            return Err(Error::new(
                "assets_invalid",
                "Native media access requires a regular file",
            ));
        }
        #[cfg(unix)]
        let (modified, changed) = {
            use std::os::unix::fs::MetadataExt;
            (
                (metadata.mtime(), metadata.mtime_nsec()),
                (metadata.ctime(), metadata.ctime_nsec()),
            )
        };
        #[cfg(windows)]
        let (modified, changed) = {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                FileBasicInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO,
            };
            let mut info = std::mem::MaybeUninit::<FILE_BASIC_INFO>::uninit();
            // SAFETY: the live File and correctly sized native output outlive the call.
            if unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle().cast(),
                    FileBasicInfo,
                    info.as_mut_ptr().cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            } == 0
            {
                return Err(Error::os(std::io::Error::last_os_error()));
            }
            let info = unsafe { info.assume_init() };
            ((info.LastWriteTime, 0), (info.ChangeTime, 0))
        };
        #[cfg(not(any(unix, windows)))]
        let (modified, changed) = return Err(Error::new(
            "assets_unsupported",
            "Native file times are unsupported",
        ));
        Ok(Self {
            identity: io::identity(file, &metadata)?,
            length: metadata.len(),
            modified,
            changed,
        })
    }
}

pub(crate) struct Registration {
    shared: Weak<Shared>,
    entry: Entry,
}
impl Registration {
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.entry.path
    }
    pub(crate) fn check(&self, control: &Control) -> Result<Arc<Shared>, Error> {
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| Error::new("assets_shutdown", "Native media owner ended"))?;
        control.check(&shared.closing)?;
        {
            let registry = shared
                .registry
                .lock()
                .map_err(|_| Error::new("assets_worker", "Asset registry lock poisoned"))?;
            if registry.context != self.entry.public.context {
                return Err(Error::new(
                    "assets_stale_context",
                    "Registered media belongs to an obsolete full context",
                ));
            }
            if !registry
                .grants
                .get(&self.entry.public.id)
                .is_some_and(|entry| {
                    entry.path == self.entry.path
                        && entry.identity == self.entry.identity
                        && entry.public.source_path == self.entry.public.source_path
                })
            {
                return Err(Error::new(
                    "assets_revoked",
                    "Native media grant was revoked or replaced",
                ));
            }
        }
        Ok(shared)
    }
    fn canonical(&self, shared: &Shared) -> Result<(), Error> {
        let path = std::fs::canonicalize(&self.entry.path).map_err(Error::os)?;
        if shared
            .config
            .protected_roots
            .iter()
            .any(|root| path.starts_with(root))
        {
            return Err(Error::new(
                "assets_protected",
                "Protected media requires separate decryption/license authority",
            ));
        }
        if path != self.entry.path {
            return Err(Error::new(
                "assets_changed",
                "Registered canonical media path changed",
            ));
        }
        Ok(())
    }
    fn open_path(&self, shared: &Shared) -> Result<(File, Stamp), Error> {
        self.canonical(shared)?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
        }
        let file = options.open(&self.entry.path).map_err(Error::os)?;
        let stamp = Stamp::read(&file)?;
        if stamp.identity != self.entry.identity {
            return Err(Error::new(
                "assets_changed",
                "Registered media identity was replaced",
            ));
        }
        if stamp.length > shared.config.max_file_bytes {
            return Err(Error::new(
                "assets_limit",
                "Registered media exceeds the file limit",
            ));
        }
        self.canonical(shared)?;
        Ok((file, stamp))
    }
    pub(crate) fn open(&self, control: &Control) -> Result<(File, Stamp), Error> {
        let shared = self.check(control)?;
        let (file, stamp) = self.open_path(&shared)?;
        self.revalidate(&file, &stamp, control)?;
        Ok((file, stamp))
    }
    pub(crate) fn revalidate(
        &self,
        anchor: &File,
        expected: &Stamp,
        control: &Control,
    ) -> Result<Stamp, Error> {
        let shared = self.check(control)?;
        let before = Stamp::read(anchor)?;
        let (_path_file, path_stamp) = self.open_path(&shared)?;
        let after = Stamp::read(anchor)?;
        if &before != expected || &after != expected || &path_stamp != expected {
            return Err(Error::new(
                "assets_changed",
                "Held or current media identity/length/native times changed",
            ));
        }
        self.check(control)?;
        Ok(after)
    }
}

/// Sealed observation and resource lease. Retaining it conservatively retains admission.
pub struct MediaObservation {
    pub(crate) stamp: Stamp,
    pub(crate) lease: Arc<ReadLease>,
}
impl std::fmt::Debug for MediaObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaObservation")
            .field("stamp", &self.stamp)
            .finish_non_exhaustive()
    }
}
impl MediaObservation {
    pub fn length(&self) -> u64 {
        self.stamp.length
    }
    pub fn same_file(&self, other: &Self) -> bool {
        self.stamp.identity == other.stamp.identity
    }
    pub fn same_version(&self, other: &Self) -> bool {
        self.stamp == other.stamp
    }
}

/// Native-only authority. Keep it alive through every use/copy of its transferred File.
/// Revalidation performs real worker-side IO; it does not certify immutable bytes.
pub struct MediaAuthority {
    registration: Arc<Registration>,
    handle: io::Handle,
    observation: MediaObservation,
}
impl std::fmt::Debug for MediaAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaAuthority")
            .field("source_path", &self.source_path())
            .field("observation", &self.observation)
            .finish_non_exhaustive()
    }
}
impl MediaAuthority {
    pub fn source_path(&self) -> &str {
        &self.registration.entry.public.source_path
    }
    pub fn canonical_path(&self) -> &Path {
        &self.registration.entry.path
    }
    pub fn context(&self) -> &Context {
        &self.registration.entry.public.context
    }
    pub fn observation(&self) -> &MediaObservation {
        &self.observation
    }
    pub async fn revalidate(&self, mut control: Control) -> Result<MediaObservation, Error> {
        control.deadline = control
            .deadline
            .min(std::time::Instant::now() + std::time::Duration::from_secs(10));
        let shared = self.registration.check(&control)?;
        {
            let registry = shared
                .registry
                .lock()
                .map_err(|_| Error::new("assets_worker", "Asset registry lock poisoned"))?;
            control.epoch = Some(registry.epoch.clone());
        }
        shared
            .pool
            .native_check(
                self.registration.clone(),
                self.handle.clone(),
                self.observation.stamp.clone(),
                self.observation.lease.clone(),
                control,
            )
            .await
    }
}

/// Preferred whole guarded handoff. No Deserialize, path constructor or File cloning API.
#[derive(Debug)]
pub struct AuthorizedMediaRead {
    file: File,
    authority: MediaAuthority,
}
impl AuthorizedMediaRead {
    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }
    pub fn authority(&self) -> &MediaAuthority {
        &self.authority
    }
    /// Native consumer MUST retain authority until File and all trusted copies finish.
    /// Duplicated Files may share the seek cursor: use sequentially and rewind.
    pub fn into_parts(self) -> (File, MediaAuthority) {
        (self.file, self.authority)
    }
}

impl AssetServer {
    pub async fn open_registered_media(
        &self,
        context: &Context,
        source: RegisteredMedia<'_>,
        mut control: Control,
    ) -> Result<AuthorizedMediaRead, Error> {
        control.deadline = control
            .deadline
            .min(std::time::Instant::now() + std::time::Duration::from_secs(10));
        self.context_control(context, &control)?;
        let text = match source {
            RegisteredMedia::SourceKey(key) | RegisteredMedia::GrantId(key) => key,
        };
        if text.is_empty() || text.len() > 32768 || text.contains('\0') {
            return Err(Error::new(
                "assets_invalid",
                "Registered media lookup must be bounded and nonempty",
            ));
        }
        let lease = self.0.shared.native_reads.admit()?;
        let registration = {
            let registry = self
                .0
                .shared
                .registry
                .lock()
                .map_err(|_| Error::new("assets_worker", "Asset registry lock poisoned"))?;
            if &registry.context != context {
                return Err(Error::new(
                    "assets_stale_context",
                    "Media lookup context changed",
                ));
            }
            control.epoch = Some(registry.epoch.clone());
            let entry = match source {
                RegisteredMedia::SourceKey(key) => registry
                    .grants
                    .values()
                    .find(|entry| entry.public.source_path == key),
                RegisteredMedia::GrantId(key) => registry.grants.get(key),
            }
            .ok_or_else(|| {
                Error::new(
                    "assets_unregistered",
                    "No existing current registered media source",
                )
            })?;
            Arc::new(Registration {
                shared: Arc::downgrade(&self.0.shared),
                entry: entry.clone(),
            })
        };
        let opened = self
            .0
            .shared
            .pool
            .native_open(registration.clone(), lease, control.clone())
            .await?;
        registration.check(&control)?;
        Ok(AuthorizedMediaRead {
            file: opened.file,
            authority: MediaAuthority {
                registration,
                handle: opened.handle,
                observation: opened.observation,
            },
        })
    }
}
