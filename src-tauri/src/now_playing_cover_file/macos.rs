//! Darwin fixed-cover descriptor backend. Selected only by the native parent;
//! no path selection, canonicalization, callback, worker or quota lives here.
#![cfg(target_os = "macos")]
use super::*;
use std::{
    ffi::{CStr, CString},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
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
        "Symlink, nonregular or unsupported Darwin fixed-path variant",
    )
}
fn mode(value: &libc::stat) -> libc::mode_t {
    value.st_mode & libc::S_IFMT
}
fn fd_stat(file: &File) -> Result<libc::stat> {
    let mut value = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: borrowed live FD and the public Darwin libc stat ABI. No field is
    // read until fstat confirms that it initialized the destination.
    if unsafe { libc::fstat(file.as_raw_fd(), value.as_mut_ptr()) } != 0 {
        return Err(Failure::os("Held Darwin stat", io::Error::last_os_error()));
    }
    // SAFETY: successful fstat initialized this exact public libc layout.
    Ok(unsafe { value.assume_init() })
}
fn id(file: &File) -> Result<(libc::dev_t, libc::ino_t)> {
    let value = fd_stat(file)?;
    Ok((value.st_dev, value.st_ino))
}
fn stat(file: &File) -> Result<FileStat> {
    let value = fd_stat(file)?;
    if mode(&value) != libc::S_IFREG {
        return Err(unsupported());
    }
    let length = u64::try_from(value.st_size)
        .ok()
        .filter(|length| *length <= MAX_FILE)
        .ok_or_else(|| Failure::new(FailureKind::Resource, "Invalid or over 4MiB cover length"))?;
    Ok(FileStat {
        // dev_t is signed on Darwin. Internal identity comparisons above retain
        // its native signed type; this observation uses the parent's u64 field.
        device: value.st_dev as u64,
        inode: value.st_ino,
        length,
        // Preserve raw signed Darwin fields, as libuv's Apple adapter does.
        // A host needing canonical POSIX nanoseconds must validate separately;
        // normalization here could change the source's binary64 projection.
        modified_seconds: value.st_mtime,
        modified_nanoseconds: value.st_mtime_nsec,
        changed_seconds: value.st_ctime,
        changed_nanoseconds: value.st_ctime_nsec,
    })
}
fn at(parent: &File, name: &CStr) -> io::Result<libc::stat> {
    let mut value = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: live parent and a single bounded NUL-terminated component. The
    // flag observes a link itself; it never follows a replaced fixed leaf.
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
    // SAFETY: initialization was confirmed by the successful syscall.
    Ok(unsafe { value.assume_init() })
}
fn open(parent: &File, name: &CStr, flags: i32) -> io::Result<File> {
    // SAFETY: borrowed parent and single component; no O_CREAT argument, no
    // request-selected name, and only public Darwin flags are passed.
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
    // SAFETY: the new descriptor is transferred exactly once to its RAII owner.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn directory(parent: &File, name: &CStr) -> Result<File> {
    open(
        parent,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NONBLOCK,
    )
    .map_err(|error| match error.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR) => unsupported(),
        _ => Failure::os("Anchored Darwin directory open", error),
    })
}
fn root(path: &Path, control: &Control) -> Result<File> {
    control.check()?;
    if !path.is_absolute() || path.as_os_str().as_bytes().len() > 32768 {
        return Err(unsupported());
    }
    // O_EVTONLY is for monitoring and does not prevent unmount. O_PATH is not
    // a Darwin API. Real readable directory descriptors are intentionally used.
    // SAFETY: constant absolute root, no create, and public open flags.
    let fd = unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NONBLOCK
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(Failure::os(
            "Darwin filesystem root anchor",
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: newly opened and exclusively owned descriptor.
    let mut current = unsafe { File::from_raw_fd(fd) };
    let mut count = 0;
    for part in path.components() {
        control.check()?;
        match part {
            Component::RootDir => {}
            Component::Normal(name) => {
                count += 1;
                if count > 128 {
                    return Err(Failure::new(
                        FailureKind::Resource,
                        "Root component cap 128",
                    ));
                }
                let name = CString::new(name.as_bytes()).map_err(|_| unsupported())?;
                current = directory(&current, &name)?;
            }
            _ => return Err(unsupported()),
        }
    }
    control.check()?;
    Ok(current)
}
fn matches(file: &File, expected: &libc::stat) -> Result<()> {
    if id(file)? != (expected.st_dev, expected.st_ino) {
        return Err(changed());
    }
    Ok(())
}
fn audio_open(parent: &File, observed: &libc::stat) -> io::Result<File> {
    let flags = if mode(observed) == libc::S_IFDIR {
        libc::O_DIRECTORY
    } else {
        0
    };
    open(parent, AUDIO, libc::O_RDONLY | libc::O_NONBLOCK | flags)
}
pub(super) fn prepare(
    observation: &mut Observation,
    lease: Arc<Lease>,
    control: &Control,
) -> Result<Outcome> {
    let held_root = root(&observation.context.data_path, control)?;
    #[cfg(test)]
    pause(MacTestStage::RootAnchored);
    if id(&root(&observation.context.data_path, control)?)? != id(&held_root)? {
        return Err(changed());
    }
    let initial = at(&held_root, AUDIO);
    let make = match &initial {
        Ok(value) if mode(value) == libc::S_IFDIR => false,
        Ok(value) if mode(value) == libc::S_IFREG => true,
        Ok(_) => return Err(unsupported()),
        Err(_) => true,
    };
    if make {
        control.check()?;
        // SAFETY: fixed single native component below the held root. Mode has
        // only permission bits and remains subject to the process's umask.
        if unsafe { libc::mkdirat(held_root.as_raw_fd(), AUDIO.as_ptr(), 0o777) } != 0 {
            let error = io::Error::last_os_error();
            let raced_directory = error.raw_os_error() == Some(libc::EEXIST)
                && at(&held_root, AUDIO).is_ok_and(|value| mode(&value) == libc::S_IFDIR);
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
        Ok(value) => value,
        Err(error) => {
            // The parent does not exist/is inaccessible at the actual probe;
            // Node existsSync(fixed child) reports false for this branch.
            observation.diagnostics.push(Failure::os(
                "Cover existence below absent/inaccessible Audio",
                error,
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
    // A failed open after the parent stat is never relabeled exists-false.
    let audio = audio_open(&held_root, &audio_stat)
        .map_err(|error| Failure::os("Darwin Audio anchor", error))?;
    matches(&audio, &audio_stat)?;
    #[cfg(test)]
    pause(MacTestStage::AudioAnchored);
    let cover_stat = match at(&audio, COVER) {
        Ok(value) => value,
        Err(error) => {
            observation
                .diagnostics
                .push(Failure::os("Source cover exists-false", error));
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
    pause(MacTestStage::BeforeLeafOpen);
    let file = open(&audio, COVER, libc::O_RDONLY | libc::O_NONBLOCK)
        .map_err(|error| Failure::os("Cover open after exists-true", error))?;
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
    pause(MacTestStage::LeafOpened);
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
    let current_audio = audio_open(&current_root, &observed).map_err(|_| changed())?;
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
    let path_file =
        open(&current_audio, COVER, libc::O_RDONLY | libc::O_NONBLOCK).map_err(|_| changed())?;
    matches(&path_file, &path_stat)?;
    if stat(&path_file)? != *expected || stat(file)? != *expected {
        return Err(changed());
    }
    control.check()?;
    Ok(Some(expected.clone()))
}

// One process-wide hook only in test builds, on the existing ordered callback.
// The native backend creates no test or production worker/thread of its own.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "Dedicated macOS integration target uses these hooks"
)]
pub enum MacTestStage {
    RootAnchored,
    AudioAnchored,
    BeforeLeafOpen,
    LeafOpened,
}
#[cfg(test)]
struct Pause {
    stage: MacTestStage,
    entered: oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}
#[cfg(test)]
static PAUSE: std::sync::Mutex<Option<Pause>> = std::sync::Mutex::new(None);
#[cfg(test)]
impl CoverFiles {
    #[allow(
        dead_code,
        reason = "Dedicated macOS integration target uses this hook"
    )]
    pub fn test_pause_macos(
        &self,
        stage: MacTestStage,
    ) -> (oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (entered, ready) = oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        assert!(PAUSE
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
}
#[cfg(test)]
fn pause(stage: MacTestStage) {
    let pause = {
        let mut held = PAUSE.lock().unwrap();
        if held.as_ref().is_some_and(|pause| pause.stage == stage) {
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
