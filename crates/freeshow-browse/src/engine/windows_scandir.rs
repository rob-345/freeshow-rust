//! Node22/libuv1.51 Windows fs__scandir shape, with a bounded checked decoder.
//! The OS owns enumeration order. This is browsing, never a path authorization.
use super::*;
use std::{
    fs::File,
    mem::offset_of,
    os::windows::io::{AsRawHandle, FromRawHandle},
    ptr::{null, null_mut},
};
use windows_sys::{
    Wdk::Storage::FileSystem::{
        FileDirectoryInformation, NtQueryDirectoryFile, FILE_DIRECTORY_INFORMATION,
    },
    Win32::{
        Foundation::{
            ERROR_ACCESS_DENIED, ERROR_DELETE_PENDING, ERROR_DIRECTORY, ERROR_FILE_NOT_FOUND,
            ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, INVALID_HANDLE_VALUE, NTSTATUS,
            STATUS_ACCESS_DENIED, STATUS_DELETE_PENDING, STATUS_FILE_DELETED,
            STATUS_NOT_A_DIRECTORY, STATUS_NO_MORE_FILES, STATUS_NO_SUCH_FILE,
            STATUS_OBJECT_NAME_NOT_FOUND, STATUS_OBJECT_PATH_NOT_FOUND, STATUS_SHARING_VIOLATION,
            STATUS_SUCCESS,
        },
        Storage::FileSystem::{
            CreateFileW, GetDriveTypeW, GetFileType, GetFinalPathNameByHandleW,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FILE_TYPE_DISK, OPEN_EXISTING, SYNCHRONIZE, VOLUME_NAME_GUID,
        },
        System::{WindowsProgramming::DRIVE_FIXED, IO::IO_STATUS_BLOCK},
    },
};
const BUFFER_BYTES: usize = 8192;
const NAME_OFFSET: usize = offset_of!(FILE_DIRECTORY_INFORMATION, FileName);
const LENGTH_OFFSET: usize = offset_of!(FILE_DIRECTORY_INFORMATION, FileNameLength);
const NEXT_OFFSET: usize = offset_of!(FILE_DIRECTORY_INFORMATION, NextEntryOffset);

fn api_failure(stage: &str) -> BrowseError {
    unsupported(format!(
        "{stage}: {}; enumeration capability is uncertified",
        std::io::Error::last_os_error()
    ))
}
// These are real filesystem failures which the source readFolder{Async}
// converts to []. Unknown NT/API failures are not a successful empty listing.
fn empty_status(status: NTSTATUS) -> bool {
    matches!(
        status,
        STATUS_ACCESS_DENIED
            | STATUS_DELETE_PENDING
            | STATUS_FILE_DELETED
            | STATUS_NOT_A_DIRECTORY
            | STATUS_NO_SUCH_FILE
            | STATUS_OBJECT_NAME_NOT_FOUND
            | STATUS_OBJECT_PATH_NOT_FOUND
            | STATUS_SHARING_VIOLATION
    )
}
fn empty_open_error(code: Option<i32>) -> bool {
    matches!(
        code.map(|code| code as u32),
        Some(
            ERROR_ACCESS_DENIED
                | ERROR_DELETE_PENDING
                | ERROR_DIRECTORY
                | ERROR_FILE_NOT_FOUND
                | ERROR_PATH_NOT_FOUND
                | ERROR_SHARING_VIOLATION
        )
    )
}
fn held_fixed_disk(file: &File, budget: &mut Budget) -> Result<()> {
    budget.check()?;
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK {
        return Err(unsupported("Held enumeration object is not a disk file"));
    }
    // A fixed-looking input can traverse a junction. Verify the actual held
    // enumeration object before NtQueryDirectoryFile, not an earlier stat.
    let mut path = vec![0u16; 32768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            path.as_mut_ptr(),
            path.len() as u32,
            0,
        )
    };
    if length == 0 {
        return Err(api_failure("Held directory final path"));
    }
    budget.check()?;
    if length as usize >= path.len() {
        return Err(BrowseError::resource("Resolved listing path length limit"));
    }
    if length < 7
        || path[..4] != [92, 92, 63, 92]
        || path[5] != 58
        || path[6] != 92
        || !(path[4] <= 127 && (path[4] as u8).is_ascii_alphabetic())
    {
        return Err(unsupported(
            "Held directory resolves outside a certified drive namespace",
        ));
    }
    let drive = [path[4], 58, 92, 0];
    let kind = unsafe { GetDriveTypeW(drive.as_ptr()) };
    budget.check()?;
    if kind != DRIVE_FIXED {
        return Err(unsupported(
            "Held enumeration directory is not on a fixed local disk",
        ));
    }
    // Drive letters can themselves be mounted-folder aliases. Ask the same
    // live handle for its actual volume GUID, then classify that volume root.
    // Failure is an unsupported backend, never a guessed fixed-local result.
    budget.check()?;
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            path.as_mut_ptr(),
            path.len() as u32,
            VOLUME_NAME_GUID,
        )
    };
    if length == 0 {
        return Err(api_failure("Held directory volume GUID"));
    }
    budget.check()?;
    if length as usize >= path.len() {
        return Err(BrowseError::resource("Held volume path length limit"));
    }
    let root = volume_root(&path[..length as usize])?;
    let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
    budget.check()?;
    if kind != DRIVE_FIXED {
        return Err(unsupported(
            "Actual held directory volume is not a certified fixed local disk",
        ));
    }
    Ok(())
}
fn volume_root(path: &[u16]) -> Result<[u16; 50]> {
    // Public GetFinalPathNameByHandleW VOLUME_NAME_GUID format:
    // \\?\Volume{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}\
    let prefix = [92, 92, 63, 92, 86, 111, 108, 117, 109, 101, 123];
    if path.len() < 49 || path[..11] != prefix || path[47] != 125 || path[48] != 92 {
        return Err(unsupported("Held volume GUID namespace is uncertified"));
    }
    for (index, unit) in path[11..47].iter().copied().enumerate() {
        let valid = if [8, 13, 18, 23].contains(&index) {
            unit == 45
        } else {
            unit <= 127 && (unit as u8).is_ascii_hexdigit()
        };
        if !valid {
            return Err(unsupported("Malformed held volume GUID"));
        }
    }
    let mut root = [0u16; 50];
    root[..49].copy_from_slice(&path[..49]);
    Ok(root)
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or_else(|| unsupported("Truncated native directory record header"))?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}
fn decode_records(bytes: &[u8], names: &mut Vec<String>, budget: &mut Budget) -> Result<()> {
    let mut offset = 0usize;
    loop {
        budget.check()?;
        let record = bytes
            .get(offset..)
            .ok_or_else(|| unsupported("Invalid native record offset"))?;
        let next = u32_at(record, NEXT_OFFSET)? as usize;
        let length = u32_at(record, LENGTH_OFFSET)? as usize;
        // The certified component subset is at most255 UTF16 units. Reserve
        // entry/string budgets before allocating a decoded name or joined path.
        if !length.is_multiple_of(2) || length > 510 {
            return Err(unsupported(
                "Native filename exceeds certified paired UTF16 component shape",
            ));
        }
        let end = NAME_OFFSET
            .checked_add(length)
            .ok_or_else(|| unsupported("Native filename offset overflow"))?;
        let span = if next == 0 { record.len() } else { next };
        if span < end
            || span > record.len()
            || (next != 0 && (!next.is_multiple_of(8) || next < NAME_OFFSET))
        {
            return Err(unsupported("Invalid native directory record span"));
        }
        let encoded = record
            .get(NAME_OFFSET..end)
            .ok_or_else(|| unsupported("Truncated native filename"))?;
        let mut wide = [0u16; 255];
        let mut count = length / 2;
        for (index, pair) in encoded.as_chunks::<2>().0.iter().enumerate() {
            wide[index] = u16::from_le_bytes(*pair);
        }
        // libuv trims terminal NULs for a native SharePoint enumeration quirk.
        while count != 0 && wide[count - 1] == 0 {
            count -= 1;
        }
        let wide = &wide[..count];
        if !wide.is_empty() && wide != [46] && wide != [46, 46] {
            budget.entry()?;
            let mut length = 0usize;
            for scalar in char::decode_utf16(wide.iter().copied()) {
                let scalar = scalar.map_err(|_| unsupported("Unpaired native UTF16 filename is not representable by the JSON path contract"))?;
                if scalar == '\0' {
                    return Err(unsupported("Interior NUL in native filename"));
                }
                length += scalar.len_utf8();
            }
            if length > budget.config.path_bytes
                || budget.strings.saturating_add(length) > budget.config.output_bytes
            {
                return Err(BrowseError::resource("Aggregate retained filename bytes"));
            }
            let name = String::from_utf16(wide)
                .map_err(|_| unsupported("Unpaired native UTF16 filename"))?;
            // Reject native names which Win32 would normalize into another
            // spelling. stat applies the same existing certified component gate.
            if reserved(&name)
                || name.ends_with(['.', ' '])
                || name
                    .chars()
                    .any(|c| c <= '\u{1f}' || ":\"<>|?*/\\".contains(c))
            {
                return Err(unsupported(
                    "Ambiguous native Windows filename is uncertified",
                ));
            }
            budget.string(&name)?;
            names.push(name);
        }
        if next == 0 {
            break;
        }
        offset = offset
            .checked_add(next)
            .ok_or_else(|| unsupported("Native record chain overflow"))?;
        if offset >= bytes.len() {
            return Err(unsupported("Native record chain lacks final entry"));
        }
    }
    Ok(())
}

pub(super) fn readdir(path: &str, budget: &mut Budget) -> Result<Vec<String>> {
    ordinary(path, budget)?;
    // Reuse the existing narrow Windows stat path/volume/metadata contract.
    // It is not the held enumeration-handle verification performed below.
    if !stats::stat(path, budget)?.is_some_and(|stat| stat.directory) {
        return Ok(vec![]);
    }
    let mut wide: Vec<u16> = r"\\?\".encode_utf16().collect();
    wide.extend(path.encode_utf16().map(|c| if c == 47 { 92 } else { c }));
    if wide.len() >= 32767 {
        return Err(BrowseError::resource("Native listing path length limit"));
    }
    wide.push(0);
    budget.check()?;
    // Same synchronous directory open/access/share modes as pinned libuv.
    // FILE_FLAG_OVERLAPPED is absent: NtQueryDirectoryFile completes before it
    // returns, so its stack output/IO_STATUS_BLOCK never outlive this call.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY | SYNCHRONIZE,
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
        return if empty_open_error(error.raw_os_error()) {
            Ok(vec![])
        } else {
            Err(unsupported(format!(
                "Directory open failed: {error}; source outcome is uncertified"
            )))
        };
    }
    // SAFETY: one successful owned CreateFile handle, never escaped/cloned.
    let file = unsafe { File::from_raw_handle(handle) };
    held_fixed_disk(&file, budget)?;
    #[cfg(test)]
    test_hook(path);
    let mut names = Vec::new();
    // u64 provides the documented8byte ABI alignment. The initialized buffer
    // and IO_STATUS_BLOCK live until the synchronous native call returns.
    let mut storage = [0u64; BUFFER_BYTES / 8];
    let mut restart = true;
    loop {
        budget.check()?;
        let mut status_block = IO_STATUS_BLOCK::default();
        let status = unsafe {
            NtQueryDirectoryFile(
                file.as_raw_handle(),
                null_mut(),
                None,
                null(),
                &mut status_block,
                storage.as_mut_ptr().cast(),
                BUFFER_BYTES as u32,
                FileDirectoryInformation,
                false,
                null(),
                restart,
            )
        };
        budget.check()?;
        restart = false;
        if status == STATUS_NO_MORE_FILES {
            break;
        }
        if status != STATUS_SUCCESS {
            return if empty_status(status) {
                Ok(vec![])
            } else {
                Err(unsupported(format!(
                    "Directory query NTSTATUS0x{:08x} is uncertified",
                    status as u32
                )))
            };
        }
        let length = status_block.Information;
        if length == 0 || length > BUFFER_BYTES {
            return Err(unsupported(
                "Native directory response has uncertified zero/oversized information length",
            ));
        }
        // SAFETY: u64 initialized storage is byte-addressable, and checked
        // Information bounds keep this slice inside it. No typed record read.
        let bytes = unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), length) };
        decode_records(bytes, &mut names, budget)?;
    }
    names
        .into_iter()
        .map(|name| {
            let joined = lexical(paths::join(path, &name))?;
            budget.string(&joined)?;
            Ok(joined)
        })
        .collect()
}

#[cfg(test)]
type Hook = (String, Box<dyn FnOnce() + Send>);
#[cfg(test)]
pub(super) static AFTER_OPEN: std::sync::Mutex<Option<Hook>> = std::sync::Mutex::new(None);
#[cfg(test)]
fn test_hook(path: &str) {
    let hook = {
        let mut hook = AFTER_OPEN.lock().unwrap();
        if hook.as_ref().is_some_and(|(expected, _)| expected == path) {
            hook.take()
        } else {
            None
        }
    };
    if let Some((_, callback)) = hook {
        callback();
    }
}

#[cfg(test)]
mod record_tests {
    use super::*;
    fn record(name: &[u16], next: u32) -> Vec<u8> {
        let mut bytes = vec![0; NAME_OFFSET + name.len() * 2];
        bytes[NEXT_OFFSET..NEXT_OFFSET + 4].copy_from_slice(&next.to_le_bytes());
        bytes[LENGTH_OFFSET..LENGTH_OFFSET + 4]
            .copy_from_slice(&(name.len() as u32 * 2).to_le_bytes());
        for (pair, unit) in bytes[NAME_OFFSET..]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(name)
        {
            pair.copy_from_slice(&unit.to_le_bytes());
        }
        bytes
    }
    #[test]
    fn checked_public_abi_record_chain_rejects_truncation_offsets_and_utf16() {
        let mut names = Vec::new();
        let valid = record(&"音楽🙌.txt".encode_utf16().collect::<Vec<_>>(), 0);
        decode_records(
            &valid,
            &mut names,
            &mut super::super::tests::budget(BrowseConfig::default()),
        )
        .unwrap();
        assert_eq!(names, ["音楽🙌.txt"]);
        for length in 0..valid.len() {
            assert!(decode_records(
                &valid[..length],
                &mut Vec::new(),
                &mut super::super::tests::budget(BrowseConfig::default())
            )
            .is_err());
        }
        for next in [1, 4, 8, NAME_OFFSET as u32, u32::MAX] {
            let invalid = record(&[65], next);
            assert!(decode_records(
                &invalid,
                &mut Vec::new(),
                &mut super::super::tests::budget(BrowseConfig::default())
            )
            .is_err());
        }
        for name in [
            vec![0xd800],
            vec![65, 0, 66],
            vec![65; 256],
            vec![67, 79, 78],
        ] {
            assert!(decode_records(
                &record(&name, 0),
                &mut Vec::new(),
                &mut super::super::tests::budget(BrowseConfig::default())
            )
            .is_err());
        }
        let mut odd = valid.clone();
        odd[LENGTH_OFFSET..LENGTH_OFFSET + 4].copy_from_slice(&1u32.to_le_bytes());
        assert!(decode_records(
            &odd,
            &mut Vec::new(),
            &mut super::super::tests::budget(BrowseConfig::default())
        )
        .is_err());
    }
    #[test]
    fn source_dot_terminal_nul_and_known_error_projection_are_explicit() {
        for name in [vec![], vec![46], vec![46, 46]] {
            let mut names = Vec::new();
            decode_records(
                &record(&name, 0),
                &mut names,
                &mut super::super::tests::budget(BrowseConfig::default()),
            )
            .unwrap();
            assert!(names.is_empty());
        }
        let mut names = Vec::new();
        decode_records(
            &record(&[65, 0, 0], 0),
            &mut names,
            &mut super::super::tests::budget(BrowseConfig::default()),
        )
        .unwrap();
        assert_eq!(names, ["A"]);
        assert!(empty_status(STATUS_ACCESS_DENIED));
        assert!(!empty_status(
            windows_sys::Win32::Foundation::STATUS_INVALID_INFO_CLASS
        ));
        assert!(!empty_status(
            windows_sys::Win32::Foundation::STATUS_PENDING
        ));
        assert!(!empty_open_error(Some(
            windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER as i32
        )));
        let guid: Vec<_> = r"\\?\Volume{26a21bda-a627-11d7-9931-806e6f6e6963}\folder"
            .encode_utf16()
            .collect();
        let root = volume_root(&guid).unwrap();
        assert_eq!(
            String::from_utf16(&root[..49]).unwrap(),
            r"\\?\Volume{26a21bda-a627-11d7-9931-806e6f6e6963}\"
        );
        assert_eq!(root[49], 0);
        assert!(volume_root(&r"\\?\UNC\server\share\".encode_utf16().collect::<Vec<_>>()).is_err());
        let mut malformed = guid;
        malformed[11] = b'Z' as u16;
        assert!(volume_root(&malformed).is_err());
    }
}
