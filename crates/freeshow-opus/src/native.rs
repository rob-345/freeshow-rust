use crate::*;
use libloading::Library;
use std::{
    ffi::{c_char, c_int, c_void},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct LibrarySource {
    path: Option<PathBuf>,
}
impl LibrarySource {
    pub fn system() -> Self {
        Self { path: None }
    }
    /// Select a host-trusted native executable library. Never accept this from IPC.
    ///
    /// # Safety
    /// Any executable loaded from this path must have safe process constructors/
    /// destructors and implement the genuine public Opus C ABI for its reported
    /// verified version. A version string alone does not authenticate executable code.
    pub unsafe fn trusted_path(path: &Path) -> Result<Self> {
        let bytes = path.as_os_str().as_encoded_bytes();
        if !path.is_absolute() || bytes.len() > 32768 || bytes.contains(&0) {
            return Err(Error::new(
                "invalid_library_path",
                "Require bounded absolute trusted library path",
            ));
        }
        Ok(Self {
            path: Some(path.into()),
        })
    }
    fn candidate(&self) -> Result<PathBuf> {
        if let Some(path) = &self.path {
            return Ok(path.clone());
        }
        #[cfg(target_os = "linux")]
        {
            Ok("libopus.so.0".into())
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(Error::new("unsupported_platform","Default libopus discovery verified on Linux only; host may select explicit trusted library"))
        }
    }
}
// Exact stable public signatures in @discordjs/opus0.10.0 bundled Opus1.3.1
// include/opus.h and opus_defines.h. No guessed private symbol/struct layout.
type Version = unsafe extern "C" fn() -> *const c_char;
type Size = unsafe extern "C" fn(c_int) -> c_int;
type EncoderInit = unsafe extern "C" fn(*mut c_void, i32, c_int, c_int) -> c_int;
type Encode = unsafe extern "C" fn(*mut c_void, *const i16, c_int, *mut u8, i32) -> i32;
type EncoderCtl = unsafe extern "C" fn(*mut c_void, c_int, ...) -> c_int;
#[cfg(any(test, feature = "test-harness"))]
type DecoderInit = unsafe extern "C" fn(*mut c_void, i32, c_int) -> c_int;
#[cfg(any(test, feature = "test-harness"))]
type Decode = unsafe extern "C" fn(*mut c_void, *const u8, i32, *mut i16, c_int, c_int) -> c_int;
const APPLICATION_AUDIO: c_int = 2049;
const GET_BITRATE: c_int = 4003;
const GET_LOOKAHEAD: c_int = 4027;
const MAX_STATE: usize = 512 * 1024;
#[repr(C, align(16))]
#[derive(Clone)]
struct Block([u8; 16]);
struct State {
    blocks: Box<[Block]>,
}
impl State {
    fn allocate(size: c_int) -> Result<Self> {
        if size <= 0 || size as usize > MAX_STATE {
            return Err(Error::new(
                "unsupported_state_size",
                "libopus state size outside bounded allocation",
            ));
        }
        Ok(Self {
            blocks: vec![Block([0; 16]); (size as usize).div_ceil(16)].into_boxed_slice(),
        })
    }
    fn ptr(&mut self) -> *mut c_void {
        self.blocks.as_mut_ptr().cast()
    }
}
struct Api {
    library: Library,
    version: String,
    label: String,
}
impl Api {
    fn load(source: LibrarySource) -> Result<Self> {
        if !cfg!(target_endian = "little") {
            return Err(Error::new(
                "unsupported_platform",
                "Original PCM reinterpretation verified on little-endian targets",
            ));
        }
        let path = source.candidate()?;
        // Safety: host selects a trusted executable library; dlopen may run its code.
        let library =
            unsafe { Library::new(&path) }.map_err(|e| Error::new("library_unavailable", e))?;
        // Safety: declarations are the official public ABI; retain Library for every
        // function/state lifetime. Version string is documented immutable NUL text.
        let version_fn: Version = *unsafe { library.get(b"opus_get_version_string\0") }
            .map_err(|e| Error::new("missing_symbol", e))?;
        let pointer = unsafe { version_fn() };
        if pointer.is_null() {
            return Err(Error::new("unsupported_version", "Null version string"));
        }
        let mut bytes = Vec::new();
        for i in 0..128 {
            let byte = unsafe { pointer.add(i).read() } as u8;
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
        let version = String::from_utf8(bytes)
            .map_err(|_| Error::new("unsupported_version", "Non UTF-8 version"))?;
        if !matches!(version.as_str(), "libopus 1.3.1" | "libopus 1.4") {
            return Err(Error::new(
                "unsupported_version",
                format!("Unverified {version}"),
            ));
        }
        let label = path.to_string_lossy().into_owned();
        Ok(Self {
            library,
            version,
            label,
        })
    }
    unsafe fn symbol<T: Copy>(&self, name: &[u8]) -> Result<T> {
        // Safety: each caller requests an exact public function signature above.
        unsafe { self.library.get::<T>(name) }
            .map(|s| *s)
            .map_err(|e| Error::new("missing_symbol", e))
    }
}
pub(crate) struct Encoder {
    api: Api,
    state: State,
    encode_fn: Encode,
    info: NativeInfo,
}
impl Encoder {
    pub(crate) fn open(source: LibrarySource) -> Result<Self> {
        let api = Api::load(source)?;
        // Safety: all names/types come from the pinned public Opus header ABI.
        let (size_fn, init, encode_fn, ctl): (Size, EncoderInit, Encode, EncoderCtl) = unsafe {
            (
                api.symbol(b"opus_encoder_get_size\0")?,
                api.symbol(b"opus_encoder_init\0")?,
                api.symbol(b"opus_encode\0")?,
                api.symbol(b"opus_encoder_ctl\0")?,
            )
        };
        let size = unsafe { size_fn(CHANNELS) };
        let mut state = State::allocate(size)?;
        // Safety: allocation is16-byte aligned and at least get_size bytes; fixed valid
        // rate/channels/application. init owns no external pointer needing destroy.
        let result = unsafe { init(state.ptr(), RATE, CHANNELS, APPLICATION_AUDIO) };
        if result != 0 {
            return Err(Error::native("encoder_init_failed", result));
        }
        let mut lookahead = 0;
        let mut bitrate = 0;
        for (request, out) in [(GET_LOOKAHEAD, &mut lookahead), (GET_BITRATE, &mut bitrate)] {
            let result = unsafe { ctl(state.ptr(), request, out as *mut c_int) };
            if result != 0 {
                return Err(Error::native("encoder_query_failed", result));
            }
        }
        let info = NativeInfo {
            version: api.version.clone(),
            library: api.label.clone(),
            state_bytes: size as usize,
            lookahead,
            default_bitrate: bitrate,
            rate: RATE,
            channels: CHANNELS,
            frame_samples: SAMPLES,
            max_packet_bytes: MAX_PACKET,
        };
        Ok(Self {
            api,
            state,
            encode_fn,
            info,
        })
    }
}
impl Codec for Encoder {
    fn info(&self) -> NativeInfo {
        self.info.clone()
    }
    fn encode(&mut self, pcm: &[u8; PCM_BYTES]) -> Result<Vec<u8>> {
        let mut samples = [0i16; SAMPLES * 2];
        for (out, bytes) in samples.iter_mut().zip(pcm.as_chunks::<2>().0) {
            *out = i16::from_le_bytes([bytes[0], bytes[1]]);
        }
        let mut packet = [0u8; MAX_PACKET];
        // Safety: initialized state/library retained, exactly960 stereo samples, capped
        // output buffer. No user CTLs or frame-size override can reach the native call.
        let result = unsafe {
            (self.encode_fn)(
                self.state.ptr(),
                samples.as_ptr(),
                SAMPLES as c_int,
                packet.as_mut_ptr(),
                MAX_PACKET as i32,
            )
        };
        if result <= 0 {
            return Err(Error::native("encode_failed", result));
        }
        if result as usize > MAX_PACKET {
            return Err(Error::new(
                "codec_contract",
                "Native output exceeded provided packet buffer",
            ));
        }
        let _keep_library_alive = &self.api;
        Ok(packet[..result as usize].to_vec())
    }
    fn dispose(self: Box<Self>) -> Result<()> {
        let Self { state, api, .. } = *self;
        drop(state);
        api.library
            .close()
            .map_err(|e| Error::new("library_close_failed", e))
    }
}
// State is allocated/init'ed/freed by Rust, as explicitly supported by the Opus
// API. No *_destroy is called on an init'ed caller allocation (that would free it).

#[cfg(any(test, feature = "test-harness"))]
pub(crate) struct Decoder {
    api: Api,
    state: State,
    decode_fn: Decode,
}
#[cfg(any(test, feature = "test-harness"))]
impl Decoder {
    pub(crate) fn open(source: LibrarySource) -> Result<Self> {
        let api = Api::load(source)?;
        let (size, init, decode_fn): (Size, DecoderInit, Decode) = unsafe {
            (
                api.symbol(b"opus_decoder_get_size\0")?,
                api.symbol(b"opus_decoder_init\0")?,
                api.symbol(b"opus_decode\0")?,
            )
        };
        let mut state = State::allocate(unsafe { size(CHANNELS) })?;
        let result = unsafe { init(state.ptr(), RATE, CHANNELS) };
        if result != 0 {
            return Err(Error::native("decoder_init_failed", result));
        }
        Ok(Self {
            api,
            state,
            decode_fn,
        })
    }
    pub(crate) fn version(&self) -> &str {
        &self.api.version
    }
    pub(crate) fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>> {
        if packet.is_empty() || packet.len() > MAX_PACKET {
            return Err(Error::new(
                "invalid_packet",
                "Evidence decoder requires bounded nonempty packet",
            ));
        }
        let mut pcm = vec![0i16; 5760 * 2];
        let n = unsafe {
            (self.decode_fn)(
                self.state.ptr(),
                packet.as_ptr(),
                packet.len() as i32,
                pcm.as_mut_ptr(),
                5760,
                0,
            )
        };
        if n < 0 {
            return Err(Error::native("decode_failed", n));
        }
        if n as usize > 5760 {
            return Err(Error::new(
                "codec_contract",
                "Decoder frame count exceeded bounded buffer",
            ));
        }
        pcm.truncate(n as usize * 2);
        Ok(pcm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocation_and_trusted_path_bounds() {
        assert!(State::allocate(0).is_err());
        assert!(State::allocate(MAX_STATE as i32 + 1).is_err());
        let mut state = State::allocate(100).unwrap();
        assert_eq!(state.ptr() as usize % 16, 0);
        assert!(unsafe { LibrarySource::trusted_path(Path::new("relative.so")) }.is_err());
    }
}
