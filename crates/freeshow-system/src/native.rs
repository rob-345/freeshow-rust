use crate::{Error, MemorySample};

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn hostname() -> Result<String, Error> {
    // Matches libuv's MAXHOSTNAMELEN + 1 buffers on the supported Unix OSes.
    #[cfg(target_os = "linux")]
    let mut buffer = [0u8; 65];
    #[cfg(target_os = "macos")]
    let mut buffer = [0u8; 257];
    // SAFETY: buffer is writable and its passed length is its actual capacity.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return Err(Error::os("gethostname", std::io::Error::last_os_error()));
    }
    *buffer.last_mut().unwrap() = 0;
    let end = buffer.iter().position(|&byte| byte == 0).unwrap();
    Ok(String::from_utf8_lossy(&buffer[..end]).into_owned())
}

#[cfg(target_os = "linux")]
pub(crate) fn memory() -> Result<MemorySample, Error> {
    use std::{fs::File, io::Read};
    // libuv 1.51 samples total and free separately. Each bounded read consumes
    // at most 4095 bytes; MemAvailable is deliberately distinct from MemFree.
    fn proc_value(key: &[u8]) -> u64 {
        let mut buffer = [0u8; 4095];
        let Ok(mut file) = File::open("/proc/meminfo") else {
            return 0;
        };
        loop {
            match file.read(&mut buffer) {
                Ok(count) => return meminfo_field(&buffer[..count], key),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return 0,
            }
        }
    }
    fn fallback(free: bool) -> Result<u64, Error> {
        let mut info = std::mem::MaybeUninit::<libc::sysinfo>::uninit();
        // SAFETY: the kernel writes a valid sysinfo to the provided storage
        // on success; it is only assumed initialized on that success branch.
        if unsafe { libc::sysinfo(info.as_mut_ptr()) } != 0 {
            return Err(Error::os("sysinfo", std::io::Error::last_os_error()));
        }
        let info = unsafe { info.assume_init() };
        let units = if free { info.freeram } else { info.totalram };
        #[cfg(target_pointer_width = "32")]
        let units = u64::from(units);
        Ok(units.wrapping_mul(u64::from(info.mem_unit)))
    }
    let total = proc_value(b"MemTotal:");
    let total_source = if total == 0 {
        "sysinfo.totalram*mem_unit"
    } else {
        "/proc/meminfo:MemTotal"
    };
    let total = if total == 0 { fallback(false)? } else { total };
    let free = proc_value(b"MemAvailable:");
    let free_source = if free == 0 {
        "sysinfo.freeram*mem_unit"
    } else {
        "/proc/meminfo:MemAvailable"
    };
    let free = if free == 0 { fallback(true)? } else { free };
    checked_sample(total, free, total_source, free_source)
}

/// The libuv meminfo helper uses strstr + unsigned sscanf then KiB->bytes.
/// Limit the input exactly as its bounded proc read does. This helper does no IO.
pub fn meminfo_field(input: &[u8], key: &[u8]) -> u64 {
    let input = &input[..input.len().min(4095)];
    let input = &input[..input
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(input.len())];
    if key.is_empty() {
        return 0;
    }
    let Some(start) = input.windows(key.len()).position(|part| part == key) else {
        return 0;
    };
    let value = &input[start + key.len()..];
    let mut index = value
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
        .unwrap_or(value.len());
    let negative = value.get(index) == Some(&b'-');
    if negative || value.get(index) == Some(&b'+') {
        index += 1;
    }
    let mut parsed = 0u64;
    let mut overflow = false;
    while let Some(byte @ b'0'..=b'9') = value.get(index) {
        let next = parsed
            .checked_mul(10)
            .and_then(|n| n.checked_add((byte - b'0') as u64));
        match next {
            Some(next) => parsed = next,
            None => overflow = true,
        }
        index += 1;
    }
    if overflow {
        parsed = u64::MAX;
    } else if negative {
        parsed = parsed.wrapping_neg();
    }
    parsed.wrapping_mul(1024)
}

#[cfg(target_os = "macos")]
pub(crate) fn memory() -> Result<MemorySample, Error> {
    let mut total = 0u64;
    let mut size = std::mem::size_of_val(&total);
    let mut mib = [libc::CTL_HW, libc::HW_MEMSIZE];
    // SAFETY: all pointers refer to writable, correctly sized storage; this
    // read-only sysctl has no replacement-value pointer.
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            (&mut total as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(Error::os(
            "sysctl(HW_MEMSIZE)",
            std::io::Error::last_os_error(),
        ));
    }
    let mut statistics = std::mem::MaybeUninit::<libc::vm_statistics_data_t>::uninit();
    let mut count = (std::mem::size_of::<libc::vm_statistics_data_t>()
        / std::mem::size_of::<libc::integer_t>())
        as libc::mach_msg_type_number_t;
    // SAFETY: storage/count match HOST_VM_INFO's vm_statistics_data_t layout.
    let result = unsafe {
        libc::host_statistics(
            mach2::mach_init::mach_host_self(),
            libc::HOST_VM_INFO,
            statistics.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS {
        return Err(Error::new(
            "system_os",
            format!("host_statistics(HOST_VM_INFO) failed: {result}"),
        ));
    }
    let statistics = unsafe { statistics.assume_init() };
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 {
        return Err(Error::new(
            "system_os",
            "sysconf(_SC_PAGESIZE) returned no valid page size",
        ));
    }
    checked_sample(
        total,
        (statistics.free_count as u64).wrapping_mul(page_size as u64),
        "sysctl:HW_MEMSIZE",
        "host_statistics:free_count*page_size",
    )
}

#[cfg(target_os = "windows")]
pub(crate) fn hostname() -> Result<String, Error> {
    use windows_sys::Win32::Networking::WinSock::{
        GetHostNameW, WSACleanup, WSAGetLastError, WSAStartup, WSADATA,
    };
    let mut data = std::mem::MaybeUninit::<WSADATA>::uninit();
    // Each successful startup holds its own Winsock reference; cleanup balances
    // it without tearing down other components' independently acquired use.
    let result = unsafe { WSAStartup(0x0202, data.as_mut_ptr()) };
    if result != 0 {
        return Err(Error::new(
            "system_os",
            format!("WSAStartup failed: {result}"),
        ));
    }
    struct Winsock;
    impl Drop for Winsock {
        fn drop(&mut self) {
            unsafe {
                WSACleanup();
            }
        }
    }
    let _winsock = Winsock;
    let mut buffer = [0u16; 256];
    if unsafe { GetHostNameW(buffer.as_mut_ptr(), buffer.len() as i32) } != 0 {
        return Err(Error::new(
            "system_os",
            format!("GetHostNameW failed: {}", unsafe { WSAGetLastError() }),
        ));
    }
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .ok_or_else(|| Error::new("system_os", "GetHostNameW result was not terminated"))?;
    String::from_utf16(&buffer[..end])
        .map_err(|_| Error::new("system_encoding", "GetHostNameW returned invalid UTF-16"))
}

#[cfg(target_os = "windows")]
pub(crate) fn memory() -> Result<MemorySample, Error> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    fn status() -> Result<MEMORYSTATUSEX, Error> {
        let mut status = std::mem::MaybeUninit::<MEMORYSTATUSEX>::zeroed();
        // SAFETY: zeroed storage is valid for MEMORYSTATUSEX and dwLength is
        // initialized as required before the API writes the remaining fields.
        unsafe {
            (*status.as_mut_ptr()).dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        }
        if unsafe { GlobalMemoryStatusEx(status.as_mut_ptr()) } == 0 {
            return Err(Error::os(
                "GlobalMemoryStatusEx",
                std::io::Error::last_os_error(),
            ));
        }
        Ok(unsafe { status.assume_init() })
    }
    // Source os.totalmem() and os.freemem() are separate libuv API calls.
    let total = status()?.ullTotalPhys;
    let free = status()?.ullAvailPhys;
    checked_sample(
        total,
        free,
        "GlobalMemoryStatusEx:ullTotalPhys",
        "GlobalMemoryStatusEx:ullAvailPhys",
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(crate) fn hostname() -> Result<String, Error> {
    Err(Error::new(
        "system_unsupported",
        "Native hostname is not implemented for this OS",
    ))
}
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(crate) fn memory() -> Result<MemorySample, Error> {
    Err(Error::new(
        "system_unsupported",
        "Native memory is not implemented for this OS",
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn checked_sample(
    total: u64,
    free: u64,
    total_source: &'static str,
    free_source: &'static str,
) -> Result<MemorySample, Error> {
    if total == 0 {
        return Err(Error::new(
            "system_os",
            "OS reported zero total physical memory",
        ));
    }
    Ok(MemorySample {
        total,
        free,
        total_source: total_source.into(),
        free_source: free_source.into(),
    })
}

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "macos", target_os = "windows")
))]
mod tests {
    #[test]
    fn invalid_native_total_is_an_explicit_error_and_zero_free_is_valid() {
        assert_eq!(
            super::checked_sample(0, 0, "fixture", "fixture")
                .unwrap_err()
                .code,
            "system_os"
        );
        let memory =
            super::checked_sample(16 * 1024 * 1024 * 1024, 0, "fixture", "fixture").unwrap();
        assert!(memory.usage().performance_mode);
    }
}
