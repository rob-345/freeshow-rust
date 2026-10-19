//! Single-component Windows opens below retained native directory handles.
//! This child module creates no service, task, thread, URL or renderer authority.
use super::*;
use std::{
    ffi::OsStr,
    mem::{size_of, MaybeUninit},
    os::windows::{
        ffi::OsStrExt,
        fs::FileExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Prefix},
    ptr::{null, null_mut},
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            NtCreateFile, FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
            FILE_OPEN_NO_RECALL, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
        },
    },
    Win32::{
        Foundation::{
            RtlNtStatusToDosError, ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE,
            OBJ_CASE_INSENSITIVE, STATUS_NOT_A_DIRECTORY, STATUS_NO_SUCH_FILE,
            STATUS_OBJECT_NAME_NOT_FOUND, STATUS_OBJECT_PATH_NOT_FOUND, UNICODE_STRING,
        },
        Storage::FileSystem::{
            CreateFileW, FileBasicInfo, FileIdInfo, FileStandardInfo, GetDriveTypeW,
            GetFileInformationByHandleEx, GetFileType, FILE_ATTRIBUTE_DEVICE,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
            FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
            FILE_INFO_BY_HANDLE_CLASS, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_TRAVERSE, FILE_TYPE_DISK,
            OPEN_EXISTING, SYNCHRONIZE,
        },
        System::{WindowsProgramming::DRIVE_FIXED, IO::IO_STATUS_BLOCK},
    },
};

const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const METADATA: u32 = FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const DIRECTORY: u32 = METADATA | FILE_TRAVERSE;
const NO_FOLLOW: u32 = FILE_OPEN_REPARSE_POINT | FILE_OPEN_NO_RECALL | FILE_SYNCHRONOUS_IO_NONALERT;
const AUDIO: &str = "Audio";
const COVER: &str = "NowPlayingCover.png";
const TICKS_PER_SECOND: i64 = 10_000_000;
const EPOCH_SECONDS: i64 = 11_644_473_600;

fn unsupported(message: &'static str) -> Failure {
    Failure::new(FailureKind::Unsupported, message)
}
fn changed() -> Failure {
    Failure::new(
        FailureKind::Changed,
        "Fixed cover root, Audio, leaf identity or version changed",
    )
}
fn os(stage: &str, error: io::Error) -> Failure {
    Failure {
        os_code: error.raw_os_error(),
        ..Failure::new(FailureKind::Io, format!("{stage}: {error}"))
    }
}
#[derive(Debug)]
struct OpenError {
    status: i32,
    win32: u32,
}
impl OpenError {
    fn missing(&self) -> bool {
        matches!(
            self.status,
            STATUS_OBJECT_NAME_NOT_FOUND
                | STATUS_OBJECT_PATH_NOT_FOUND
                | STATUS_NOT_A_DIRECTORY
                | STATUS_NO_SUCH_FILE
        )
    }
    fn failure(self, stage: &str) -> Failure {
        // os_code remains a real Win32 code. Keep the original NTSTATUS in the
        // bounded diagnostic; a lossy NTSTATUS->Win32 conversion is not hidden.
        Failure {
            os_code: Some(self.win32 as i32),
            ..Failure::new(
                FailureKind::Io,
                format!(
                    "{stage}: NTSTATUS 0x{:08X}, Win32 {} ({})",
                    self.status as u32,
                    self.win32,
                    io::Error::from_raw_os_error(self.win32 as i32)
                ),
            )
        }
    }
}
#[derive(PartialEq, Eq)]
struct Identity {
    volume: u64,
    file: [u8; 16],
}
struct Info {
    id: Identity,
    basic: FILE_BASIC_INFO,
    standard: FILE_STANDARD_INFO,
}
fn info_value<T>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    let mut result = MaybeUninit::<T>::zeroed();
    // SAFETY: each private call pairs its exact Windows ABI struct with the
    // matching FILE_INFO_BY_HANDLE_CLASS and a live owned File handle. The fixed
    // size output is zeroed, and initialization is used only after success.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            class,
            result.as_mut_ptr().cast(),
            size_of::<T>() as u32,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { result.assume_init() })
    }
}
fn info(file: &File) -> Result<Info> {
    // SAFETY: a live File handle, observed without ownership transfer.
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK {
        return Err(unsupported(
            "Only ordinary local disk objects are supported",
        ));
    }
    let basic: FILE_BASIC_INFO =
        info_value(file, FileBasicInfo).map_err(|e| os("Handle basic information", e))?;
    let prohibited = FILE_ATTRIBUTE_REPARSE_POINT
        | FILE_ATTRIBUTE_DEVICE
        | FILE_ATTRIBUTE_OFFLINE
        | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
        | FILE_ATTRIBUTE_RECALL_ON_OPEN;
    if basic.FileAttributes & prohibited != 0 {
        return Err(unsupported(
            "Reparse, device, offline and recall-backed objects are unsupported",
        ));
    }
    let standard: FILE_STANDARD_INFO =
        info_value(file, FileStandardInfo).map_err(|e| os("Handle standard information", e))?;
    if standard.DeletePending {
        return Err(changed());
    }
    if standard.Directory != (basic.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) {
        return Err(unsupported("Inconsistent native object type"));
    }
    let id: FILE_ID_INFO = info_value(file, FileIdInfo).map_err(|e| {
        // Do not fall back to a truncated 64-bit ID on an unsupported volume.
        Failure {
            os_code: e.raw_os_error(),
            ..unsupported("Volume does not provide required 128-bit FileIdInfo")
        }
    })?;
    Ok(Info {
        id: Identity {
            volume: id.VolumeSerialNumber,
            file: id.FileId.Identifier,
        },
        basic,
        standard,
    })
}
fn identity(file: &File) -> Result<Identity> {
    Ok(info(file)?.id)
}
fn seconds(ticks: i64) -> (i64, i64) {
    (
        ticks.div_euclid(TICKS_PER_SECOND) - EPOCH_SECONDS,
        ticks.rem_euclid(TICKS_PER_SECOND) * 100,
    )
}
fn stat(file: &File) -> Result<FileStat> {
    let observed = info(file)?;
    if observed.standard.Directory {
        return Err(unsupported("Cover leaf must be an ordinary file"));
    }
    let length = u64::try_from(observed.standard.EndOfFile)
        .map_err(|_| unsupported("Negative native file length"))?;
    if length > MAX_FILE {
        return Err(Failure::new(FailureKind::Resource, "Cover exceeds4MiB"));
    }
    if observed.id.file[8..].iter().any(|b| *b != 0) {
        return Err(unsupported(
            "128-bit leaf identity is not representable by existing FileStat",
        ));
    }
    let (modified_seconds, modified_nanoseconds) = seconds(observed.basic.LastWriteTime);
    let (changed_seconds, changed_nanoseconds) = seconds(observed.basic.ChangeTime);
    Ok(FileStat {
        device: observed.id.volume,
        inode: u64::from_le_bytes(observed.id.file[..8].try_into().unwrap()),
        length,
        modified_seconds,
        modified_nanoseconds,
        changed_seconds,
        changed_nanoseconds,
    })
}
fn component(name: &OsStr) -> Result<Vec<u16>> {
    let value: Vec<u16> = name.encode_wide().take(256).collect();
    if value.is_empty()
        || value.len() > 255
        || value.iter().any(|c| {
            *c <= 31
                || [
                    b'/' as u16,
                    b'\\' as u16,
                    b':' as u16,
                    b'"' as u16,
                    b'<' as u16,
                    b'>' as u16,
                    b'|' as u16,
                    b'?' as u16,
                    b'*' as u16,
                ]
                .contains(c)
        })
        || value
            .last()
            .is_some_and(|c| *c == b'.' as u16 || *c == b' ' as u16)
    {
        return Err(unsupported("Ambiguous or oversized Windows path component"));
    }
    let stem = name
        .to_string_lossy()
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "CONIN$"
            | "CONOUT$"
            | "COM¹"
            | "COM²"
            | "COM³"
            | "LPT¹"
            | "LPT²"
            | "LPT³"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        return Err(unsupported("Windows device-name component is unsupported"));
    }
    Ok(value)
}
fn open_at(
    parent: &File,
    name: &[u16],
    access: u32,
    disposition: u32,
    options: u32,
) -> std::result::Result<File, OpenError> {
    debug_assert!(!name.is_empty() && name.len() <= 255);
    let unicode = UNICODE_STRING {
        Length: (name.len() * 2) as u16,
        MaximumLength: (name.len() * 2) as u16,
        Buffer: name.as_ptr().cast_mut(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &unicode,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: null(),
        SecurityQualityOfService: null(),
    };
    let mut handle: HANDLE = null_mut();
    let mut completion = MaybeUninit::<IO_STATUS_BLOCK>::zeroed();
    // SAFETY: retained live parent, one validated relative component (no path
    // separators/ADS/NUL), initialized exact public ABI structs, synchronous IO,
    // and no borrowed buffers escape this call. Reparse processing is bypassed
    // for this single child; inspect that same returned handle before use.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &attributes,
            completion.as_mut_ptr(),
            null(),
            0,
            SHARE,
            disposition,
            NO_FOLLOW | options,
            null(),
            0,
        )
    };
    if status < 0 {
        return Err(OpenError {
            status,
            // SAFETY: documented conversion of the actual returned NTSTATUS.
            win32: unsafe { RtlNtStatusToDosError(status) },
        });
    }
    // Successful synchronous NtCreateFile transfers precisely one owned handle.
    // The API contract guarantees a valid FileHandle on successful return.
    Ok(unsafe { File::from_raw_handle(handle) })
}
fn directory_at(parent: &File, name: &[u16]) -> Result<File> {
    let file = open_at(parent, name, DIRECTORY, FILE_OPEN, 0)
        .map_err(|e| e.failure("Anchored directory open"))?;
    if !info(&file)?.standard.Directory {
        return Err(unsupported("Root components must be directories"));
    }
    Ok(file)
}
fn root(path: &Path, control: &Control) -> Result<File> {
    control.check()?;
    if path.as_os_str().encode_wide().take(16385).count() > 16384 {
        return Err(Failure::new(
            FailureKind::Resource,
            "Root exceeds32768UTF16bytes",
        ));
    }
    let mut parts = path.components();
    let drive = match parts.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) if drive.is_ascii_alphabetic() => {
                drive
            }
            _ => {
                return Err(unsupported(
                    "Only local absolute drive-letter roots are supported",
                ))
            }
        },
        _ => return Err(unsupported("Absolute Windows drive root required")),
    };
    if parts.next() != Some(Component::RootDir) {
        return Err(unsupported("Drive-relative Windows root is unsupported"));
    }
    let short = [u16::from(drive), b':' as u16, b'\\' as u16, 0];
    // SAFETY: fixed NUL-terminated drive-root spelling, never a renderer path.
    if unsafe { GetDriveTypeW(short.as_ptr()) } != DRIVE_FIXED {
        return Err(unsupported("Only fixed local drive roots are supported"));
    }
    let drive_path = [
        b'\\' as u16,
        b'\\' as u16,
        b'?' as u16,
        b'\\' as u16,
        u16::from(drive),
        b':' as u16,
        b'\\' as u16,
        0,
    ];
    // SAFETY: fixed absolute drive root, exact public API, no inheritance or raw
    // handle escapes. All remaining components are opened relative to it.
    let handle = unsafe {
        CreateFileW(
            drive_path.as_ptr(),
            DIRECTORY,
            SHARE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(os("Drive root open", io::Error::last_os_error()));
    }
    let mut current = unsafe { File::from_raw_handle(handle) };
    if !info(&current)?.standard.Directory {
        return Err(unsupported("Drive root is not a directory"));
    }
    for (index, part) in parts.enumerate() {
        control.check()?;
        if index >= 128 {
            return Err(Failure::new(FailureKind::Resource, "Root component cap128"));
        }
        let Component::Normal(name) = part else {
            return Err(unsupported("Unnormalized Windows root component"));
        };
        current = directory_at(&current, &component(name)?)?;
    }
    control.check()?;
    Ok(current)
}
pub(super) fn prepare(
    observation: &mut Observation,
    lease: Arc<Lease>,
    control: &Control,
) -> Result<Outcome> {
    let held_root = root(&observation.context.data_path, control)?;
    #[cfg(test)]
    pause(Stage::Root);
    if identity(&root(&observation.context.data_path, control)?)? != identity(&held_root)? {
        return Err(changed());
    }
    let audio_name = component(OsStr::new(AUDIO))?;
    let leaf_name = component(OsStr::new(COVER))?;
    let initial = open_at(&held_root, &audio_name, METADATA, FILE_OPEN, 0);
    let make = match initial {
        Ok(file) => !info(&file)?.standard.Directory,
        Err(error) if error.missing() => true,
        // Opening a metadata handle can fail when a by-name exists test would
        // succeed (ACL/sharing/filter differences). Do not invent exists-false.
        Err(error) => return Err(error.failure("Audio metadata probe failed")),
    };
    if make {
        control.check()?;
        if let Err(error) = open_at(
            &held_root,
            &audio_name,
            DIRECTORY,
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
        ) {
            // Match the existing source-derived race handling: an already-created
            // actual directory is not reported as a failed folder creation.
            let raced_directory = error.win32 == ERROR_ALREADY_EXISTS
                && open_at(&held_root, &audio_name, METADATA, FILE_OPEN, 0)
                    .ok()
                    .is_some_and(|file| info(&file).is_ok_and(|i| i.standard.Directory));
            if !raced_directory {
                observation.folder_failure = Some(FolderFailure {
                    folder: observation.context.data_path.join(AUDIO),
                    diagnostic: error.failure("Audio mkdir"),
                });
            }
        }
    }
    control.check()?;
    let audio_probe = match open_at(&held_root, &audio_name, METADATA, FILE_OPEN, 0) {
        Ok(file) => file,
        Err(error) if error.missing() => {
            observation
                .diagnostics
                .push(error.failure("Cover existence below absent/nondirectory Audio"));
            return missing(observation, held_root, None, lease, control);
        }
        Err(error) => return Err(error.failure("Audio metadata probe after mkdir failed")),
    };
    let audio_info = info(&audio_probe)?;
    let audio = if audio_info.standard.Directory {
        let directory = directory_at(&held_root, &audio_name)?;
        if identity(&directory)? != audio_info.id {
            return Err(changed());
        }
        directory
    } else {
        audio_probe
    };
    #[cfg(test)]
    pause(Stage::Audio);
    let probe = match open_at(&audio, &leaf_name, METADATA, FILE_OPEN, 0) {
        Ok(file) => file,
        Err(error) if error.missing() => {
            observation
                .diagnostics
                .push(error.failure("Source cover exists-false"));
            return missing(observation, held_root, Some(audio), lease, control);
        }
        Err(error) => return Err(error.failure("Cover metadata probe failed")),
    };
    let expected_identity = info(&probe)?;
    if expected_identity.standard.Directory {
        return Err(unsupported("Directory cover leaf is unsupported"));
    }
    control.check()?;
    #[cfg(test)]
    pause(Stage::BeforeLeaf);
    let file = open_at(
        &audio,
        &leaf_name,
        METADATA | FILE_READ_DATA,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
    )
    .map_err(|e| e.failure("Cover open after exists-true"))?;
    if identity(&file)? != expected_identity.id {
        return Err(changed());
    }
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
    pause(Stage::Leaf);
    revalidate(&authority, control)?;
    observation.authority = Some(authority.clone());
    Ok(Outcome::File(HeldCover { authority }))
}
fn missing(
    observation: &mut Observation,
    root: File,
    audio: Option<File>,
    lease: Arc<Lease>,
    control: &Control,
) -> Result<Outcome> {
    let authority = Arc::new(Authority {
        context: observation.context.clone(),
        root,
        audio,
        leaf: None,
        validating: AtomicBool::new(false),
        _lease: lease,
    });
    revalidate(&authority, control)?;
    observation.authority = Some(authority);
    Ok(Outcome::Missing)
}
pub(super) fn revalidate(authority: &Authority, control: &Control) -> Result<Option<FileStat>> {
    control.check()?;
    let current_root = root(&authority.context.data_path, control)?;
    if identity(&current_root)? != identity(&authority.root)? {
        return Err(changed());
    }
    let Some(audio) = &authority.audio else {
        return Ok(None);
    };
    let audio_name = component(OsStr::new(AUDIO))?;
    let current_audio =
        open_at(&current_root, &audio_name, METADATA, FILE_OPEN, 0).map_err(|_| changed())?;
    let held = info(audio)?;
    if info(&current_audio)?.id != held.id {
        return Err(changed());
    }
    let Some((file, expected)) = &authority.leaf else {
        control.check()?;
        return Ok(None);
    };
    if stat(file)? != *expected {
        return Err(changed());
    }
    let directory = directory_at(&current_root, &audio_name)?;
    if identity(&directory)? != held.id {
        return Err(changed());
    }
    let current_file = open_at(
        &directory,
        &component(OsStr::new(COVER))?,
        METADATA | FILE_READ_DATA,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
    )
    .map_err(|_| changed())?;
    if stat(&current_file)? != *expected || stat(file)? != *expected {
        return Err(changed());
    }
    control.check()?;
    Ok(Some(expected.clone()))
}
pub(super) fn read_at(file: &File, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
    // Explicit offsets determine each read. Windows seek_read also changes the
    // cursor; the sealed HTTP owner performs sequential reads on this File and
    // validators perform metadata-only operations. No handle clone is exposed.
    file.seek_read(buffer, offset)
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Root,
    Audio,
    BeforeLeaf,
    Leaf,
}
#[cfg(test)]
struct Pause {
    stage: Stage,
    entered: oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}
#[cfg(test)]
static PAUSE: std::sync::Mutex<Option<Pause>> = std::sync::Mutex::new(None);
#[cfg(test)]
fn pause(stage: Stage) {
    let value = {
        let mut current = PAUSE.lock().unwrap();
        if current.as_ref().is_some_and(|p| p.stage == stage) {
            current.take()
        } else {
            None
        }
    };
    if let Some(value) = value {
        let _ = value.entered.send(());
        let _ = value.release.recv();
    }
}
#[cfg(test)]
fn install_pause(stage: Stage) -> (oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
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
#[cfg(test)]
impl CoverFiles {
    #[allow(
        dead_code,
        reason = "Dedicated Windows fixtures exercise this bounded actual-IO hook"
    )]
    pub fn test_pause_windows_root(&self) -> (oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
        install_pause(Stage::Root)
    }
    #[allow(
        dead_code,
        reason = "Dedicated Windows fixtures exercise this bounded actual-IO hook"
    )]
    pub fn test_pause_windows_audio(&self) -> (oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
        install_pause(Stage::Audio)
    }
    #[allow(
        dead_code,
        reason = "Dedicated Windows fixtures exercise this bounded actual-IO hook"
    )]
    pub fn test_pause_windows_before_leaf(
        &self,
    ) -> (oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
        install_pause(Stage::BeforeLeaf)
    }
    #[allow(
        dead_code,
        reason = "Dedicated Windows fixtures exercise this bounded actual-IO hook"
    )]
    pub fn test_pause_windows_leaf(&self) -> (oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
        install_pause(Stage::Leaf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_epoch_conversion_keeps_signed_subseconds() {
        assert_eq!(seconds(0), (-11_644_473_600, 0));
        assert_eq!(seconds(EPOCH_SECONDS * TICKS_PER_SECOND), (0, 0));
        assert_eq!(
            seconds(EPOCH_SECONDS * TICKS_PER_SECOND - 1),
            (-1, 999_999_900)
        );
        assert_eq!(seconds(EPOCH_SECONDS * TICKS_PER_SECOND + 1), (0, 100));
    }
    #[test]
    fn ambiguous_relative_components_are_explicitly_unsupported() {
        for name in [
            "",
            ".",
            "..",
            "name.",
            "name ",
            "a:b",
            "a/b",
            "a\\b",
            "CON",
            "nul.png",
            "LPT9",
            "COM1.txt",
            "COM¹.txt",
            "LPT²",
            "a\0b",
        ] {
            assert_eq!(
                component(OsStr::new(name)).err().unwrap().kind,
                FailureKind::Unsupported,
                "{name:?}"
            );
        }
        assert!(component(OsStr::new("日本語 — 👋.png")).is_ok());
        assert_eq!(
            component(OsStr::new(&"x".repeat(256))).err().unwrap().kind,
            FailureKind::Unsupported
        );
    }
    #[test]
    fn unsupported_namespaces_fail_before_attempting_remote_or_device_opens() {
        for path in [
            r"\\server\share\library",
            r"\\?\UNC\server\share\library",
            r"\\.\pipe\name",
            r"C:relative",
            r"\relative",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1",
        ] {
            assert_eq!(
                root(Path::new(path), &Control::default())
                    .err()
                    .unwrap()
                    .kind,
                FailureKind::Unsupported,
                "{path}"
            );
        }
    }
    #[test]
    fn only_proven_namespace_failure_can_become_missing() {
        for status in [
            STATUS_OBJECT_NAME_NOT_FOUND,
            STATUS_OBJECT_PATH_NOT_FOUND,
            STATUS_NO_SUCH_FILE,
            STATUS_NOT_A_DIRECTORY,
        ] {
            assert!(OpenError { status, win32: 2 }.missing());
        }
        for status in [
            windows_sys::Win32::Foundation::STATUS_ACCESS_DENIED,
            windows_sys::Win32::Foundation::STATUS_SHARING_VIOLATION,
        ] {
            assert!(!OpenError { status, win32: 5 }.missing());
        }
    }
}
