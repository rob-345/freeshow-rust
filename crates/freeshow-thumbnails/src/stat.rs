use crate::*;
#[derive(Clone)]
pub(crate) struct Stat {
    pub identity: FileIdentity,
    pub mtime_ms: f64,
    pub ctime_ms: f64,
}
#[cfg(target_os = "linux")]
pub(crate) fn access(path: &Path, budget: &Budget) -> Result<bool> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    budget.check()?;
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| ThumbnailError::new("invalid_request", "Path contains NUL"))?;
    // SAFETY: CString is valid for this synchronous F_OK access call.
    let exists = unsafe { libc::access(path.as_ptr(), libc::F_OK) } == 0;
    budget.check()?;
    Ok(exists)
}
#[cfg(target_os = "linux")]
pub(crate) fn read(path: &Path, nofollow: bool, budget: &Budget) -> Result<Option<Stat>> {
    use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};
    budget.check()?;
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| ThumbnailError::new("invalid_request", "Path contains NUL"))?;
    let mut result = MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: correctly sized zeroed statx and a NUL-terminated path; nothing escapes.
    let status = unsafe {
        libc::syscall(
            libc::SYS_statx,
            libc::AT_FDCWD,
            path.as_ptr(),
            if nofollow {
                libc::AT_SYMLINK_NOFOLLOW
            } else {
                0
            },
            libc::STATX_BASIC_STATS | libc::STATX_BTIME,
            result.as_mut_ptr(),
        )
    };
    let error = if status == 0 {
        0
    } else {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    };
    budget.check()?;
    if status != 0 {
        return if [libc::EINVAL, libc::EPERM, libc::ENOSYS, libc::EOPNOTSUPP].contains(&error) {
            Err(ThumbnailError::new(
                "unsupported_stat_backend",
                "Linux statx unavailable; fallback contract unverified",
            ))
        } else {
            Ok(None)
        };
    }
    let s = unsafe { result.assume_init() };
    Ok(Some(project(s)))
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn access(_: &Path, _: &Budget) -> Result<bool> {
    Err(ThumbnailError::new(
        "unsupported_platform",
        "Thumbnail filesystem contract verified on Linux only",
    ))
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn read(_: &Path, _: bool, _: &Budget) -> Result<Option<Stat>> {
    Err(ThumbnailError::new(
        "unsupported_platform",
        "Thumbnail stat contract verified on Linux only",
    ))
}
pub(crate) fn regular(stat: &Stat) -> bool {
    stat.identity.mode & 0o170000 == 0o100000
}

#[cfg(target_os = "linux")]
fn project(s: libc::statx) -> Stat {
    Stat {
        identity: FileIdentity {
            device: libc::makedev(s.stx_dev_major, s.stx_dev_minor),
            inode: s.stx_ino,
            length: s.stx_size,
            mode: s.stx_mode as u32,
            mtime_seconds: s.stx_mtime.tv_sec,
            mtime_nanos: s.stx_mtime.tv_nsec,
            ctime_seconds: s.stx_ctime.tv_sec,
            ctime_nanos: s.stx_ctime.tv_nsec,
        },
        mtime_ms: s.stx_mtime.tv_sec as f64 * 1000.0 + s.stx_mtime.tv_nsec as f64 / 1_000_000.0,
        ctime_ms: s.stx_ctime.tv_sec as f64 * 1000.0 + s.stx_ctime.tv_nsec as f64 / 1_000_000.0,
    }
}
#[cfg(target_os = "linux")]
pub(crate) fn identity_from_file(file: &std::fs::File) -> Result<FileIdentity> {
    use std::{mem::MaybeUninit, os::fd::AsRawFd};
    let mut result = MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: file owns its fd; the empty path and statx output remain valid for
    // this synchronous AT_EMPTY_PATH syscall. No pointer or descriptor escapes.
    let status = unsafe {
        libc::syscall(
            libc::SYS_statx,
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_BASIC_STATS | libc::STATX_BTIME,
            result.as_mut_ptr(),
        )
    };
    if status != 0 {
        let error = std::io::Error::last_os_error();
        let code = error.raw_os_error().unwrap_or(0);
        return Err(ThumbnailError::new(
            if [libc::EINVAL, libc::EPERM, libc::ENOSYS, libc::EOPNOTSUPP].contains(&code) {
                "unsupported_stat_backend"
            } else {
                "thumbnail_io_error"
            },
            error.to_string(),
        ));
    }
    Ok(project(unsafe { result.assume_init() }).identity)
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn identity_from_file(_: &std::fs::File) -> Result<FileIdentity> {
    Err(ThumbnailError::new(
        "unsupported_platform",
        "Thumbnail identity verified on Linux only",
    ))
}
