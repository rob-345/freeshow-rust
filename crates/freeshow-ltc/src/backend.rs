use crate::*;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod linux {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        ffi::{c_void, CString},
        fs::{File, OpenOptions},
        io::{Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::OpenOptionsExt,
        },
    };
    #[repr(C)]
    #[derive(Default)]
    struct Frame {
        drop: i32,
        days: i32,
        months: i32,
        years: i32,
        hours: i32,
        minutes: i32,
        seconds: i32,
        frames: i32,
        reverse: i32,
        offset_start: i64,
        volume: f64,
        timezone: [u8; 6],
    }
    const _: () = assert!(std::mem::size_of::<Frame>() == 64);
    extern "C" {
        fn fs_ltc_open(path: *const std::ffi::c_char) -> *mut c_void;
        fn fs_ltc_close(api: *mut c_void);
        fn fs_ltc_encoder(api: *mut c_void, fps: f64) -> i32;
        fn fs_ltc_decoder(api: *mut c_void, apv: i32) -> i32;
        fn fs_ltc_stop_encoder(api: *mut c_void);
        fn fs_ltc_stop_decoder(api: *mut c_void) -> i32;
        fn fs_ltc_encode(api: *mut c_void, h: i32, m: i32, s: i32, out: *mut u8, cap: usize)
            -> i32;
        fn fs_ltc_decode(
            api: *mut c_void,
            bytes: *mut u8,
            len: usize,
            out: *mut Frame,
            backlog: *mut i32,
        ) -> i32;
    }
    pub struct Native {
        api: *mut c_void,
        _file: File,
        encoder: bool,
        decoder: bool,
        samples: u64,
        stats: Arc<Shared>,
    }
    impl Native {
        pub fn open(path: &TrustedLibrary, stats: Arc<Shared>) -> Result<Self> {
            let mut file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&path.path)
                .map_err(|e| {
                    Error::unavailable(format!("Verified library open failed ({:?})", e.kind()))
                })?;
            let metadata = file
                .metadata()
                .map_err(|e| Error::unavailable(e.to_string()))?;
            if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 {
                return Err(Error::unavailable("Library is not a bounded regular file"));
            }
            let name = CString::new("freeshow-verified-ltc").unwrap();
            let descriptor = unsafe {
                libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING)
            };
            if descriptor < 0 {
                return Err(Error::unavailable(
                    "A sealed library snapshot cannot be created",
                ));
            }
            let mut sealed = unsafe { File::from_raw_fd(descriptor) };
            let mut hash = Sha256::new();
            let mut bytes = [0; 8192];
            let mut total = 0;
            loop {
                let n = file
                    .read(&mut bytes)
                    .map_err(|e| Error::unavailable(e.to_string()))?;
                if n == 0 {
                    break;
                }
                total += n;
                if total > 2 * 1024 * 1024 {
                    return Err(Error::unavailable("Library grew beyond its bound"));
                }
                hash.update(&bytes[..n]);
                sealed
                    .write_all(&bytes[..n])
                    .map_err(|e| Error::unavailable(e.to_string()))?;
                if stats.closing.load(Ordering::Acquire) {
                    return Err(Error::closing());
                }
            }
            if format!("{:x}", hash.finalize()) != LIBRARY_SHA256 {
                return Err(Error::unavailable(
                    "Library checksum is not the pinned Linux x64 ABI",
                ));
            }
            let seals =
                libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
            if unsafe { libc::fcntl(sealed.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 {
                return Err(Error::unavailable("Library snapshot cannot be sealed"));
            }
            let path = CString::new(format!("/proc/self/fd/{}", sealed.as_raw_fd())).unwrap();
            // SAFETY: the held, sealed descriptor is an immutable verified ELF; the
            // C bridge resolves public functions from its pinned public header.
            let api = unsafe { fs_ltc_open(path.as_ptr()) };
            if api.is_null() {
                return Err(Error::unavailable(
                    "Verified library or a required public symbol could not load",
                ));
            }
            stats.loaded.store(true, Ordering::Release);
            Ok(Self {
                api,
                _file: sealed,
                encoder: false,
                decoder: false,
                samples: 0,
                stats,
            })
        }
        pub fn run(&mut self, operation: Operation, limit: u64) -> Result<Reply> {
            match operation {
                Operation::Capability => Ok(Reply::Capability(Capability::available())),
                Operation::CreateEncoder { fps } => {
                    supported_fps(fps)?;
                    let r = unsafe { fs_ltc_encoder(self.api, fps) };
                    if r != 0 {
                        return Err(Error::new(
                            "ltc_allocation",
                            "Bounded encoder allocation failed",
                        ));
                    }
                    if self.encoder {
                        self.stats.encoder_freed.fetch_add(1, Ordering::AcqRel);
                    }
                    self.encoder = true;
                    self.stats.encoder_created.fetch_add(1, Ordering::AcqRel);
                    self.stats.encoder.store(true, Ordering::Release);
                    Ok(Reply::Done)
                }
                Operation::CreateDecoder { fps } => {
                    supported_fps(fps)?;
                    let r = unsafe { fs_ltc_decoder(self.api, (48000.0 / fps) as i32) };
                    if r != 0 {
                        return Err(Error::new(
                            "ltc_allocation",
                            "Bounded decoder allocation failed",
                        ));
                    }
                    if self.decoder {
                        self.stats.decoder_freed.fetch_add(1, Ordering::AcqRel);
                    }
                    self.decoder = true;
                    self.samples = 0;
                    self.stats.samples.store(0, Ordering::Release);
                    self.stats.backlog.store(0, Ordering::Release);
                    self.stats.decoder_created.fetch_add(1, Ordering::AcqRel);
                    self.stats.decoder.store(true, Ordering::Release);
                    Ok(Reply::Done)
                }
                Operation::Encode {
                    hours,
                    minutes,
                    seconds,
                    frames: _,
                } => {
                    let h = field(hours, 23)?;
                    let m = field(minutes, 59)?;
                    let s = field(seconds, 59)?;
                    if !self.encoder {
                        return Err(Error::new(
                            "ltc_not_initialized",
                            "Encoder is not initialized",
                        ));
                    }
                    let mut bytes = vec![0; 4096];
                    let n = unsafe {
                        fs_ltc_encode(self.api, h, m, s, bytes.as_mut_ptr(), bytes.len())
                    };
                    if n < 0 {
                        return Err(Error::new(
                            "ltc_native",
                            "Native encoder rejected its bounded output",
                        ));
                    }
                    bytes.truncate(n as usize);
                    Ok(Reply::Audio(bytes))
                }
                Operation::Decode { mut bytes } => {
                    if !self.decoder {
                        return Err(Error::new(
                            "ltc_not_initialized",
                            "Decoder is not initialized",
                        ));
                    }
                    let samples = self
                        .samples
                        .checked_add(bytes.len() as u64)
                        .filter(|n| *n <= limit)
                        .ok_or_else(Error::limit)?;
                    let mut frame = Frame::default();
                    let mut backlog = 0;
                    let n = unsafe {
                        fs_ltc_decode(
                            self.api,
                            bytes.as_mut_ptr(),
                            bytes.len(),
                            &mut frame,
                            &mut backlog,
                        )
                    };
                    self.samples = samples;
                    self.stats.samples.store(samples, Ordering::Release);
                    if n < 0 || !(0..=32).contains(&backlog) {
                        return Err(Error::new("ltc_native", "Decoder returned invalid state"));
                    }
                    self.stats
                        .backlog
                        .store(backlog as usize, Ordering::Release);
                    Ok(Reply::Frame(if n == 0 {
                        None
                    } else {
                        Some(Decoded {
                            drop_frame_format: frame.drop != 0,
                            days: frame.days,
                            months: frame.months,
                            years: frame.years,
                            hours: frame.hours,
                            minutes: frame.minutes,
                            seconds: frame.seconds,
                            frames: frame.frames,
                            offset_start: frame.offset_start,
                            reverse: frame.reverse != 0,
                            volume: frame.volume,
                            timezone: String::from_utf8_lossy(&frame.timezone[..5]).into_owned(),
                        })
                    }))
                }
                Operation::StopEncoder => {
                    if self.encoder {
                        unsafe { fs_ltc_stop_encoder(self.api) };
                        self.encoder = false;
                        self.stats.encoder.store(false, Ordering::Release);
                        self.stats.encoder_freed.fetch_add(1, Ordering::AcqRel);
                    }
                    Ok(Reply::Done)
                }
                Operation::StopDecoder => {
                    if self.decoder {
                        let r = unsafe { fs_ltc_stop_decoder(self.api) };
                        if r != 0 {
                            return Err(Error::new("ltc_native", "Native decoder free failed"));
                        }
                        self.decoder = false;
                        self.stats.decoder.store(false, Ordering::Release);
                        self.stats.decoder_freed.fetch_add(1, Ordering::AcqRel);
                    }
                    self.samples = 0;
                    self.stats.samples.store(0, Ordering::Release);
                    self.stats.backlog.store(0, Ordering::Release);
                    Ok(Reply::Done)
                }
            }
        }
    }
    impl Drop for Native {
        fn drop(&mut self) {
            let _ = self.run(Operation::StopEncoder, 0);
            let _ = self.run(Operation::StopDecoder, 0);
            unsafe { fs_ltc_close(self.api) };
            self.stats.loaded.store(false, Ordering::Release);
        }
    }
    fn field(n: f64, max: i32) -> Result<i32> {
        if !n.is_finite() || n.fract() != 0.0 || n < 0.0 || n > f64::from(max) {
            Err(Error::new(
                "ltc_unsupported",
                "Only bounded source clock fields are supported",
            ))
        } else {
            Ok(n as i32)
        }
    }
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub use linux::Native;
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
pub struct Native;
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
impl Native {
    pub fn open(library: &TrustedLibrary, _: Arc<Shared>) -> Result<Self> {
        let _ = &library.path;
        Err(Error::unavailable(
            "Native LTC ABI has not been verified on this platform",
        ))
    }
    pub fn run(&mut self, _: Operation, _: u64) -> Result<Reply> {
        Err(Error::unavailable("Native LTC is unavailable"))
    }
}
