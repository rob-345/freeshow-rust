use crate::*;
#[derive(Clone)]
pub(crate) struct Stat {
    pub directory: bool,
    pub json: Value,
}
#[cfg(target_os = "linux")]
fn ms(seconds: i64, nanos: i64) -> f64 {
    seconds as f64 * 1000.0 + nanos as f64 / 1_000_000.0
}
#[cfg(target_os = "linux")]
pub(crate) fn stat(path: &str, budget: &mut Budget) -> Result<Option<Stat>> {
    use std::{
        ffi::CString,
        mem::MaybeUninit,
        sync::atomic::{AtomicBool, Ordering},
    };
    static NO_STATX: AtomicBool = AtomicBool::new(false);
    budget.check()?;
    let c_path = CString::new(path)
        .map_err(|_| BrowseError::new("invalid_request", "Stat path contains NUL"))?;
    let mut result = MaybeUninit::<libc::statx>::zeroed();
    if !NO_STATX.load(Ordering::Relaxed) {
        // SAFETY: CString is NUL terminated; statx writes a correctly sized,
        // zero-initialized struct; no pointer escapes this synchronous syscall.
        // Use the kernel syscall directly, like libuv, so libc cannot silently
        // emulate statx with unverified fallback metadata.
        let status = unsafe {
            libc::syscall(
                libc::SYS_statx,
                libc::AT_FDCWD,
                c_path.as_ptr(),
                0,
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
        if status == 0 {
            let value = unsafe { result.assume_init() };
            return Ok(Some(Stat {
                directory: value.stx_mode as u32 & libc::S_IFMT == libc::S_IFDIR,
                json: serde_json::json!({"dev":libc::makedev(value.stx_dev_major,value.stx_dev_minor) as f64,"mode":value.stx_mode as f64,"nlink":value.stx_nlink as f64,"uid":value.stx_uid as f64,"gid":value.stx_gid as f64,"rdev":libc::makedev(value.stx_rdev_major,value.stx_rdev_minor) as f64,"blksize":value.stx_blksize as f64,"ino":value.stx_ino as f64,"size":value.stx_size as f64,"blocks":value.stx_blocks as f64,"atimeMs":ms(value.stx_atime.tv_sec,value.stx_atime.tv_nsec as i64),"mtimeMs":ms(value.stx_mtime.tv_sec,value.stx_mtime.tv_nsec as i64),"ctimeMs":ms(value.stx_ctime.tv_sec,value.stx_ctime.tv_nsec as i64),"birthtimeMs":ms(value.stx_btime.tv_sec,value.stx_btime.tv_nsec as i64)}),
            }));
        }
        if ![libc::EINVAL, libc::EPERM, libc::ENOSYS, libc::EOPNOTSUPP].contains(&error) {
            return Ok(None);
        }
        NO_STATX.store(true, Ordering::Relaxed);
    }
    Err(BrowseError::new(
        "unsupported_stat_backend",
        "Linux statx unavailable; fallback stat contract is not yet verified",
    ))
}
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(crate) fn stat(_: &str, _: &mut Budget) -> Result<Option<Stat>> {
    Err(BrowseError::new(
        "unsupported_platform",
        "Native stat backend is unavailable on this platform",
    ))
}

#[cfg(target_os = "macos")]
#[path = "stats/macos.rs"]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::stat;

#[cfg(target_os = "windows")]
#[path = "stats/windows.rs"]
mod windows;
#[cfg(target_os = "windows")]
pub(crate) fn stat(path: &str, budget: &mut Budget) -> Result<Option<Stat>> {
    windows::stat(path, budget)
}
