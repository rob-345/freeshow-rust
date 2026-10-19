//! Windows Node22 ordinary-disk Stats projection on the existing browse worker.
//! No worker, path grant, descriptor escape, content read, or filesystem write.
use super::*;
use std::{
    ffi::OsStr,
    fs::File,
    mem::{size_of, MaybeUninit},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        CreateFileW, FileBasicInfo, FileIdInfo, FileStandardInfo, GetDriveTypeW,
        GetFileInformationByHandle, GetFileInformationByHandleEx, GetFileType,
        GetFinalPathNameByHandleW, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DEVICE,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_READONLY,
        FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO,
        FILE_INFO_BY_HANDLE_CLASS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_TYPE_DISK, OPEN_EXISTING,
    },
    System::WindowsProgramming::DRIVE_FIXED,
};

const TICKS_PER_SECOND: i64 = 10_000_000;
const EPOCH_TICKS: i64 = 116_444_736_000_000_000;

fn unsupported(message: impl Into<String>) -> BrowseError {
    BrowseError::new("unsupported_stat_backend", message)
}
fn api_error(stage: &'static str) -> BrowseError {
    let error = std::io::Error::last_os_error();
    unsupported(format!(
        "{stage} failed: {error}; Node by-name/fallback outcome is uncertified"
    ))
}

fn device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(
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
}

// Narrow certified path spellings avoid substituting Win32 normalization for
// Node's native path.toNamespacedPath. The untouched caller spelling remains
// the engine's output path; this allocation is only the native metadata open.
fn path_wide(path: &str, budget: &mut Budget) -> Result<Vec<u16>> {
    budget.check()?;
    if path.len() > budget.config.path_bytes || path.contains('\0') {
        return Err(BrowseError::resource("Browse path length/NUL limit"));
    }
    let verbatim = path.starts_with(r"\\?\");
    let ordinary = if verbatim { &path[4..] } else { path };
    let bytes = ordinary.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'\\' | b'/')
    {
        return Err(unsupported(
            "Only absolute local drive-letter stat paths are certified",
        ));
    }
    if verbatim && ordinary.contains('/') {
        return Err(unsupported(
            "Verbatim forward-slash path semantics are uncertified",
        ));
    }
    let body = &ordinary[3..];
    let body = body.strip_suffix(['/', '\\']).unwrap_or(body);
    for name in body.split(['/', '\\']) {
        if name.is_empty() && body.is_empty() {
            continue;
        }
        if name.is_empty()
            || name == "."
            || name == ".."
            || device_name(name)
            || name.ends_with(['.', ' '])
            || name
                .chars()
                .any(|c| c <= '\u{1f}' || ":\"<>|?*".contains(c))
        {
            return Err(unsupported(
                "Ambiguous/device/unnormalized Windows stat component",
            ));
        }
        if name.encode_utf16().take(256).count() > 255 {
            return Err(unsupported(
                "Windows stat component exceeds certified255UTF16units",
            ));
        }
        budget.check()?;
    }
    let mut wide: Vec<u16> = r"\\?\".encode_utf16().collect();
    wide.extend(OsStr::new(&ordinary[..3]).encode_wide().map(|c| {
        if c == b'/' as u16 {
            b'\\' as u16
        } else {
            c
        }
    }));
    wide.extend(OsStr::new(body).encode_wide().map(|c| {
        if c == b'/' as u16 {
            b'\\' as u16
        } else {
            c
        }
    }));
    if wide.len() >= 32767 {
        return Err(BrowseError::resource("Windows native stat path limit"));
    }
    wide.push(0);
    Ok(wide)
}

fn query<T>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS, budget: &mut Budget) -> Result<T> {
    budget.check()?;
    let mut value = MaybeUninit::<T>::zeroed();
    // SAFETY: each private caller pairs the exact ABI struct and class. A live
    // owned handle and correctly sized initialized output storage stay local.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            class,
            value.as_mut_ptr().cast(),
            size_of::<T>() as u32,
        )
    };
    let error = if ok == 0 {
        Some(api_error("Handle metadata query"))
    } else {
        None
    };
    budget.check()?;
    if let Some(error) = error {
        return Err(error);
    }
    Ok(unsafe { value.assume_init() })
}

fn resolved_local_disk(file: &File, budget: &mut Budget) -> Result<()> {
    // A local-looking symlink may resolve to another volume or a remote share.
    // Certify the actual opened object, not just the initial drive spelling.
    let mut path = vec![0u16; 32768];
    budget.check()?;
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            path.as_mut_ptr(),
            path.len() as u32,
            0,
        )
    };
    let error = if length == 0 {
        Some(api_error("Final handle path"))
    } else {
        None
    };
    budget.check()?;
    if let Some(error) = error {
        return Err(error);
    }
    if length as usize >= path.len() {
        return Err(BrowseError::resource("Resolved stat path length limit"));
    }
    let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    if length < 7
        || path[..4] != prefix
        || path[5] != b':' as u16
        || path[6] != b'\\' as u16
        || !(path[4] <= 0x7f && (path[4] as u8).is_ascii_alphabetic())
    {
        return Err(unsupported(
            "Resolved non-drive stat namespace is uncertified",
        ));
    }
    let drive = [path[4], b':' as u16, b'\\' as u16, 0];
    let kind = unsafe { GetDriveTypeW(drive.as_ptr()) };
    budget.check()?;
    if kind != DRIVE_FIXED {
        return Err(unsupported(
            "Resolved stat object is not a certified fixed local disk",
        ));
    }
    Ok(())
}

// Retain native identity before any lossy JS Number conversion. The legacy
// index is certified only when its complete FileIdInfo representation agrees.
struct Identity {
    volume: u64,
    file: [u8; 16],
}
fn certify_identity(id: &Identity, legacy: &BY_HANDLE_FILE_INFORMATION) -> Result<u64> {
    let index = (u64::from(legacy.nFileIndexHigh) << 32) | u64::from(legacy.nFileIndexLow);
    if id.file[8..].iter().any(|b| *b != 0)
        || u64::from_le_bytes(id.file[..8].try_into().unwrap()) != index
        || id.volume as u32 != legacy.dwVolumeSerialNumber
    {
        return Err(unsupported(
            "Volume/file identity is not certified by Node's64bit stat index",
        ));
    }
    Ok(index)
}

fn milliseconds(ticks: i64) -> Result<f64> {
    if ticks < 0 {
        return Err(unsupported("Negative native FILETIME is uncertified"));
    }
    let relative = ticks - EPOCH_TICKS;
    let seconds = relative.div_euclid(TICKS_PER_SECOND);
    let nanos = relative.rem_euclid(TICKS_PER_SECOND) * 100;
    // Pinned Node casts Windows long time fields through unsigned long before
    // Number conversion. This interval survives that source32bit conversion;
    // exceptional dates require actual libuv/Node evidence before expansion.
    if !(0..=u32::MAX as i64).contains(&seconds) {
        return Err(unsupported(
            "Windows Node stat time outside certified unsigned32seconds interval",
        ));
    }
    Ok(seconds as f64 * 1000.0 + nanos as f64 / 1_000_000.0)
}

pub(super) fn stat(path: &str, budget: &mut Budget) -> Result<Option<Stat>> {
    budget.check()?;
    if path.is_empty() {
        return Ok(None);
    } // actual source fs.statSync("") failure
    let wide = path_wide(path, budget)?;
    let drive = [wide[4], b':' as u16, b'\\' as u16, 0];
    // SAFETY: fixed NUL-terminated local drive root; never a remote/device name.
    let drive_type = unsafe { GetDriveTypeW(drive.as_ptr()) };
    budget.check()?;
    if drive_type != DRIVE_FIXED {
        return Err(unsupported(
            "Only fixed local disk stat behavior is certified",
        ));
    }
    // Follow source stat's symlink/junction behavior. This is read-only browse
    // metadata, not a protected-root or media-descriptor authorization grant.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = std::io::Error::last_os_error();
        budget.check()?;
        return if matches!(
            error.raw_os_error().map(|v| v as u32),
            Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
        ) {
            Ok(None)
        } else {
            Err(unsupported(format!(
                "Stat metadata open failed: {error}; Node fast/directory fallback is uncertified"
            )))
        };
    }
    // SAFETY: one successful native open transfers one owned handle to File.
    let file = unsafe { File::from_raw_handle(handle) };
    budget.check()?;
    let file_type = unsafe { GetFileType(file.as_raw_handle()) };
    budget.check()?;
    if file_type != FILE_TYPE_DISK {
        return Err(unsupported("Device/pipe stat is uncertified"));
    }
    resolved_local_disk(&file, budget)?;
    let basic: FILE_BASIC_INFO = query(&file, FileBasicInfo, budget)?;
    let prohibited = FILE_ATTRIBUTE_REPARSE_POINT
        | FILE_ATTRIBUTE_DEVICE
        | FILE_ATTRIBUTE_OFFLINE
        | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
        | FILE_ATTRIBUTE_RECALL_ON_OPEN;
    if basic.FileAttributes & prohibited != 0 {
        return Err(unsupported(
            "Unresolved reparse/offline/recall stat variant is uncertified",
        ));
    }
    let standard: FILE_STANDARD_INFO = query(&file, FileStandardInfo, budget)?;
    let id: FILE_ID_INFO = query(&file, FileIdInfo, budget)?;
    let mut legacy = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), legacy.as_mut_ptr()) };
    let error = if ok == 0 {
        Some(api_error("Legacy handle identity"))
    } else {
        None
    };
    budget.check()?;
    if let Some(error) = error {
        return Err(error);
    }
    let legacy = unsafe { legacy.assume_init() };
    let identity = Identity {
        volume: id.VolumeSerialNumber,
        file: id.FileId.Identifier,
    };
    let index = certify_identity(&identity, &legacy)?;
    let directory = standard.Directory;
    if standard.DeletePending
        || directory != (basic.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0)
        || basic.FileAttributes != legacy.dwFileAttributes
        || standard.NumberOfLinks != legacy.nNumberOfLinks
        || standard.EndOfFile < 0
        || standard.AllocationSize < 0
    {
        return Err(unsupported(
            "Changing/inconsistent native stat observations are uncertified",
        ));
    }
    let permissions = if basic.FileAttributes & FILE_ATTRIBUTE_READONLY != 0 {
        0o444
    } else {
        0o666
    };
    let mode = if directory { 0o040000 } else { 0o100000 } | permissions;
    let mut json = serde_json::Map::with_capacity(14);
    for (name, value) in [
        ("dev", f64::from(legacy.dwVolumeSerialNumber)),
        ("mode", f64::from(mode)),
        ("nlink", f64::from(standard.NumberOfLinks)),
        ("uid", 0.0),
        ("gid", 0.0),
        ("rdev", 0.0),
        ("blksize", 4096.0),
        ("ino", index as f64),
        (
            "size",
            if directory {
                0.0
            } else {
                standard.EndOfFile as f64
            },
        ),
        ("blocks", (standard.AllocationSize as u64 >> 9) as f64),
        ("atimeMs", milliseconds(basic.LastAccessTime)?),
        ("mtimeMs", milliseconds(basic.LastWriteTime)?),
        ("ctimeMs", milliseconds(basic.ChangeTime)?),
        ("birthtimeMs", milliseconds(basic.CreationTime)?),
    ] {
        json.insert(name.into(), Value::from(value));
    }
    budget.check()?;
    Ok(Some(Stat {
        directory,
        json: Value::Object(json),
    }))
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
