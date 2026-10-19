//! Native-only DEVICE_ID preparation. Run on the host's retained, bounded,
//! ordered CONFIG owner; this module starts no worker and performs no writes.
use crate::JobControl;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fmt,
    io::Write,
    time::{Duration, Instant},
};

const INPUT_BYTES: usize = 8192;
const PROVIDER_BYTES: usize = 16384;
const FALLBACK: &str = "fallback-machine-id";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    fn new(code: &'static str, message: &str) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn resource() -> Self {
        Self::new(
            "device_id_resource_limit",
            "Device identity exceeds bounded work/representation limits",
        )
    }
    fn unsupported() -> Self {
        Self::new(
            "device_id_unsupported",
            "Native legacy identity representation is not certified",
        )
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// A native snapshot of the actual global CONFIG getter result. It is never a
/// renderer input or a library-data identity. Missing getter/value uses None.
pub enum ConfigSnapshot {
    Read { value: Option<Value>, can_set: bool },
    ReadFailed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Stored,
    Legacy,
    RandomUuid,
    OuterFallback,
}
/// Native guarded persistence obligation. No Deserialize, Debug or raw path.
pub struct WriteIntent {
    value: String,
}
impl WriteIntent {
    pub fn key(&self) -> &'static str {
        "machineId"
    }
    pub fn value(&self) -> &str {
        &self.value
    }
}
pub enum WriteResult {
    NotRequired,
    Committed,
    Failed,
}
/// The host must retain its whole-operation lease and serial CONFIG authority
/// through actual commit and final publication. This is not an IO receipt.
pub struct PreparedIdentity {
    reply: Value,
    write: Option<WriteIntent>,
    origin: Origin,
}
impl PreparedIdentity {
    pub fn origin(&self) -> Origin {
        self.origin
    }
    pub fn write_intent(&self) -> Option<&WriteIntent> {
        self.write.as_ref()
    }
    pub fn finish(self, result: WriteResult) -> Result<Value> {
        match (self.write.is_some(), result) {
            (false, WriteResult::NotRequired) | (true, WriteResult::Committed) => Ok(self.reply),
            (true, WriteResult::Failed) => Ok(Value::String(FALLBACK.into())),
            _ => Err(Error::new(
                "device_id_commit",
                "Device identity persistence result does not match its native intent",
            )),
        }
    }
}
fn check(control: &JobControl) -> Result<()> {
    if control.cancel.is_cancelled() {
        return Err(Error::new(
            "device_id_cancelled",
            "Device identity preparation cancelled",
        ));
    }
    if Instant::now() >= control.deadline {
        return Err(Error::new(
            "device_id_deadline",
            "Device identity preparation deadline elapsed",
        ));
    }
    Ok(())
}
fn bounded(value: &Value, depth: usize, nodes: &mut usize) -> Result<()> {
    *nodes += 1;
    if depth > 8 || *nodes > 256 {
        return Err(Error::resource());
    }
    match value {
        Value::String(text) if text.len() > INPUT_BYTES => return Err(Error::resource()),
        Value::Array(values) => {
            if values.len() > 256 {
                return Err(Error::resource());
            }
            for value in values {
                bounded(value, depth + 1, nodes)?;
            }
        }
        Value::Object(values) => {
            if values.len() > 256 {
                return Err(Error::resource());
            }
            for (key, value) in values {
                if key.len() > INPUT_BYTES {
                    return Err(Error::resource());
                }
                *nodes += 1;
                bounded(value, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}
struct Count(usize);
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        if self.0 > INPUT_BYTES {
            return Err(std::io::Error::other("Device identity byte limit"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}
#[derive(Debug)]
enum Failure {
    // Linux's original pipeline forces success after cat/hostname failures;
    // the shared source-failure/UUID branch is exercised by fixtures and the
    // actual Windows/macOS providers, not invented for Linux IO errors.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Legacy,
    Capability(Error),
}
type ProviderResult<T> = std::result::Result<T, Failure>;
impl From<Error> for Failure {
    fn from(value: Error) -> Self {
        Self::Capability(value)
    }
}
fn fallback() -> PreparedIdentity {
    PreparedIdentity {
        reply: Value::String(FALLBACK.into()),
        write: None,
        origin: Origin::OuterFallback,
    }
}
fn uuid(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let mut value = String::with_capacity(36);
    for (index, byte) in bytes.into_iter().enumerate() {
        if [4, 6, 8, 10].contains(&index) {
            value.push('-');
        }
        use fmt::Write;
        write!(value, "{byte:02x}").expect("String formatting");
    }
    value
}
fn prepare_with(
    snapshot: ConfigSnapshot,
    control: &JobControl,
    legacy: impl FnOnce(&JobControl) -> ProviderResult<String>,
    random: impl FnOnce() -> std::result::Result<[u8; 16], ()>,
) -> Result<PreparedIdentity> {
    let mut bounded_control = control.clone();
    bounded_control.deadline = bounded_control
        .deadline
        .min(Instant::now() + Duration::from_secs(2));
    let control = &bounded_control;
    check(control)?;
    let ConfigSnapshot::Read { value, can_set } = snapshot else {
        return Ok(fallback());
    };
    if let Some(value) = &value {
        bounded(value, 0, &mut 0)?;
        serde_json::to_writer(&mut Count(0), value).map_err(|_| Error::resource())?;
        if truthy(value) {
            check(control)?;
            return Ok(PreparedIdentity {
                reply: value.clone(),
                write: None,
                origin: Origin::Stored,
            });
        }
    }
    let observed = legacy(control);
    check(control)?;
    let (id, origin) = match observed {
        Ok(id) => (id, Origin::Legacy),
        Err(Failure::Legacy) => {
            check(control)?;
            let observed = random();
            check(control)?;
            let Ok(bytes) = observed else {
                return Ok(fallback());
            };
            (uuid(bytes), Origin::RandomUuid)
        }
        Err(Failure::Capability(error)) => return Err(error),
    };
    check(control)?;
    Ok(PreparedIdentity {
        reply: Value::String(id.clone()),
        write: can_set.then_some(WriteIntent { value: id }),
        origin,
    })
}
/// Synchronous OS work: call only inside an existing bounded retained native
/// owner. No child processes, path inputs, storage writes or hidden workers.
pub fn prepare(snapshot: ConfigSnapshot, control: &JobControl) -> Result<PreparedIdentity> {
    prepare_with(snapshot, control, native, || {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|_| ())?;
        Ok(bytes)
    })
}
#[derive(Clone, Copy)]
enum Platform {
    #[cfg(any(test, target_os = "linux"))]
    Linux,
    #[cfg(any(test, target_os = "windows"))]
    Windows,
    #[cfg(any(test, target_os = "macos"))]
    Mac,
}
fn whitespace(ch: char) -> bool {
    matches!(ch, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
fn digest(platform: Platform, output: &[u8]) -> ProviderResult<String> {
    if output.len() > PROVIDER_BYTES {
        return Err(Error::resource().into());
    }
    let output =
        std::str::from_utf8(output).map_err(|_| Failure::Capability(Error::unsupported()))?;
    let selected = match platform {
        #[cfg(any(test, target_os = "linux"))]
        Platform::Linux => output,
        #[cfg(any(test, target_os = "windows"))]
        Platform::Windows => output.split("REG_SZ").nth(1).ok_or(Failure::Legacy)?,
        #[cfg(any(test, target_os = "macos"))]
        Platform::Mac => output
            .split("IOPlatformUUID")
            .nth(1)
            .ok_or(Failure::Legacy)?
            .split('\n')
            .next()
            .unwrap_or(""),
    };
    let quotes = match platform {
        #[cfg(any(test, target_os = "macos"))]
        Platform::Mac => true,
        #[cfg(any(test, target_os = "linux"))]
        Platform::Linux => false,
        #[cfg(any(test, target_os = "windows"))]
        Platform::Windows => false,
    };
    let mut token = String::with_capacity(selected.len());
    for ch in selected.chars() {
        if whitespace(ch) || (quotes && matches!(ch, '=' | '"')) {
            continue;
        }
        // Normal native UUID/hostname tokens are ASCII. Unicode/ill-formed
        // provider output is an explicit capability gap, never a new identity.
        if !ch.is_ascii() {
            return Err(Error::unsupported().into());
        }
        token.push(ch.to_ascii_lowercase());
    }
    Ok(format!("{:x}", Sha256::digest(token.as_bytes())))
}

#[cfg(target_os = "linux")]
fn linux_bytes(
    paths: [&std::path::Path; 2],
    control: &JobControl,
    hostname: impl FnOnce() -> Result<Vec<u8>>,
) -> ProviderResult<Vec<u8>> {
    use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt};
    let mut output = Vec::with_capacity(128);
    let mut failed = false;
    for path in paths {
        check(control)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path);
        let Ok(mut file) = file else {
            failed = true;
            continue;
        };
        let metadata = file.metadata().map_err(|_| {
            Failure::Capability(Error::new(
                "device_id_os",
                "Cannot observe native identity file",
            ))
        })?;
        if metadata.is_dir() {
            failed = true;
            continue;
        }
        if !metadata.is_file() {
            return Err(Error::unsupported().into());
        }
        let mut ended = false;
        for _ in 0..64 {
            check(control)?;
            let mut bytes = [0; 512];
            match file.read(&mut bytes) {
                Ok(0) => {
                    ended = true;
                    break;
                }
                Ok(count) => {
                    let line = bytes[..count].iter().position(|byte| *byte == b'\n');
                    let count = line.unwrap_or(count);
                    if output.len() + count > PROVIDER_BYTES {
                        return Err(Error::resource().into());
                    }
                    output.extend_from_slice(&bytes[..count]);
                    if line.is_some() {
                        return Ok(output);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    failed = true;
                    ended = true;
                    break;
                }
            }
        }
        if !ended {
            return Err(Error::resource().into());
        }
    }
    if failed {
        check(control)?;
        let name = hostname()?;
        if output.len() + name.len() > PROVIDER_BYTES {
            return Err(Error::resource().into());
        }
        output.extend_from_slice(&name);
    }
    Ok(output)
}
#[cfg(target_os = "linux")]
fn native(control: &JobControl) -> ProviderResult<String> {
    let bytes = linux_bytes(
        [
            std::path::Path::new("/var/lib/dbus/machine-id"),
            std::path::Path::new("/etc/machine-id"),
        ],
        control,
        || {
            // Reuse the existing bounded native hostname query; an actual failure
            // is never converted into invented empty provider success.
            crate::native::hostname()
                .map(String::into_bytes)
                .map_err(|_| Error::new("device_id_os", "Native hostname fallback failed"))
        },
    )?;
    digest(Platform::Linux, &bytes)
}

#[cfg(target_os = "windows")]
fn native(control: &JobControl) -> ProviderResult<String> {
    use windows_sys::Win32::{
        Foundation::{
            ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND,
            ERROR_SUCCESS, ERROR_UNSUPPORTED_TYPE,
        },
        System::Registry::*,
    };
    check(control)?;
    let key: Vec<u16> = "SOFTWARE\\Microsoft\\Cryptography\0"
        .encode_utf16()
        .collect();
    let name: Vec<u16> = "MachineGuid\0".encode_utf16().collect();
    let mut value = [0u16; 4096];
    let mut bytes = std::mem::size_of_val(&value) as u32;
    let mut kind = 0;
    let mut flags = RRF_RT_REG_SZ | RRF_NOEXPAND;
    // Matches node-machine-id's Sysnative REG.exe choice for a 32-bit process
    // with PROCESSOR_ARCHITEW6432 present; otherwise use its native view.
    static MIXED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *MIXED.get_or_init(|| {
        cfg!(target_pointer_width = "32") && std::env::var_os("PROCESSOR_ARCHITEW6432").is_some()
    }) {
        flags |= RRF_SUBKEY_WOW6464KEY;
    }
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            flags,
            &mut kind,
            value.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    check(control)?;
    if status == ERROR_MORE_DATA {
        return Err(Error::resource().into());
    }
    if [
        ERROR_FILE_NOT_FOUND,
        ERROR_PATH_NOT_FOUND,
        ERROR_ACCESS_DENIED,
        ERROR_UNSUPPORTED_TYPE,
    ]
    .contains(&status)
    {
        return Err(Failure::Legacy);
    }
    if status != ERROR_SUCCESS {
        return Err(Error::new(
            "device_id_os",
            &format!("Native MachineGuid registry query failed (Win32 {status})"),
        )
        .into());
    }
    if kind != REG_SZ {
        return Err(Error::unsupported().into());
    }
    if bytes < 2 || bytes as usize > std::mem::size_of_val(&value) || !bytes.is_multiple_of(2) {
        return Err(Error::unsupported().into());
    }
    let units = &value[..bytes as usize / 2];
    let Some((&0, units)) = units.split_last() else {
        return Err(Error::unsupported().into());
    };
    if units.contains(&0) {
        return Err(Error::unsupported().into());
    }
    let value = String::from_utf16(units).map_err(|_| Failure::Capability(Error::unsupported()))?;
    let output = format!("MachineGuid REG_SZ {value}\r\n");
    digest(Platform::Windows, output.as_bytes())
}

#[cfg(target_os = "macos")]
fn native(control: &JobControl) -> ProviderResult<String> {
    use std::{
        ffi::{c_char, c_void},
        ptr,
    };
    // Apple XNU mach/{arm,i386}/boolean.h: ARM boolean_t is int;
    // x86_64 user-space boolean_t is unsigned int. CF Boolean is unsigned char.
    #[cfg(target_arch = "x86_64")]
    type MachBoolean = u32;
    #[cfg(not(target_arch = "x86_64"))]
    type MachBoolean = i32;
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingServices(
            port: u32,
            matching: *mut c_void,
            iterator: *mut u32,
        ) -> i32;
        fn IOIteratorNext(iterator: u32) -> u32;
        fn IOIteratorIsValid(iterator: u32) -> MachBoolean;
        fn IORegistryEntryCreateCFProperty(
            entry: u32,
            key: *const c_void,
            allocator: *const c_void,
            options: u32,
        ) -> *const c_void;
        fn IOObjectRelease(object: u32) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFStringGetTypeID() -> usize;
        fn CFGetTypeID(value: *const c_void) -> usize;
        fn CFStringGetLength(value: *const c_void) -> isize;
        fn CFStringGetCString(
            value: *const c_void,
            buffer: *mut c_char,
            capacity: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(value: *const c_void);
    }
    struct Object(u32);
    impl Drop for Object {
        fn drop(&mut self) {
            unsafe {
                IOObjectRelease(self.0);
            }
        }
    }
    struct Cf(*const c_void);
    impl Drop for Cf {
        fn drop(&mut self) {
            unsafe {
                CFRelease(self.0);
            }
        }
    }
    const UTF8: u32 = 0x0800_0100;
    check(control)?;
    unsafe {
        let matching = IOServiceMatching(c"IOPlatformExpertDevice".as_ptr());
        if matching.is_null() {
            return Err(Failure::Legacy);
        }
        // GetMatchingServices consumes the matching dictionary on all results.
        // Inspect at most two entries: source ioreg ordering for multiple
        // platform experts is not certified, so never persist an arbitrary one.
        let mut iterator = 0;
        let status = IOServiceGetMatchingServices(0, matching, &mut iterator);
        if status != 0 || iterator == 0 {
            if iterator != 0 {
                IOObjectRelease(iterator);
            }
            return Err(Failure::Legacy);
        }
        let iterator = Object(iterator);
        let service = IOIteratorNext(iterator.0);
        if service == 0 {
            return Err(Failure::Legacy);
        }
        let service = Object(service);
        let second = IOIteratorNext(iterator.0);
        if second != 0 {
            let _second = Object(second);
            return Err(Error::unsupported().into());
        }
        if IOIteratorIsValid(iterator.0) == 0 {
            return Err(
                Error::new("device_id_os", "Native platform expert iterator changed").into(),
            );
        }
        let key = CFStringCreateWithCString(ptr::null(), c"IOPlatformUUID".as_ptr(), UTF8);
        if key.is_null() {
            return Err(Error::new(
                "device_id_os",
                "Native identity property key allocation failed",
            )
            .into());
        }
        let key = Cf(key);
        let property = IORegistryEntryCreateCFProperty(service.0, key.0, ptr::null(), 0);
        if property.is_null() {
            return Err(Failure::Legacy);
        }
        let property = Cf(property);
        if CFGetTypeID(property.0) != CFStringGetTypeID() {
            return Err(Error::unsupported().into());
        }
        let units = CFStringGetLength(property.0);
        if !(0..=4096).contains(&units) {
            return Err(Error::resource().into());
        }
        let mut buffer = [0u8; PROVIDER_BYTES + 1];
        if CFStringGetCString(
            property.0,
            buffer.as_mut_ptr().cast(),
            buffer.len() as isize,
            UTF8,
        ) == 0
        {
            return Err(Error::unsupported().into());
        }
        let end = buffer
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(Error::unsupported)?;
        let token = std::str::from_utf8(&buffer[..end])
            .map_err(|_| Failure::Capability(Error::unsupported()))?;
        if token.len() as isize != units
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            return Err(Error::unsupported().into());
        }
        check(control)?;
        let output = format!("IOPlatformUUID = \"{token}\"\n");
        digest(Platform::Mac, output.as_bytes())
    }
}
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn native(_: &JobControl) -> ProviderResult<String> {
    Err(Error::unsupported().into())
}

#[cfg(test)]
mod tests;
