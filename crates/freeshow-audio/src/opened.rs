//! Native descriptor ownership. None of these inputs is a path grant or deserializable.
use crate::{js::Js, AudioConfig, AudioError, Inner};
use serde::Serialize;
use std::{fs::File, sync::Arc};
use tokio::sync::{oneshot, OwnedSemaphorePermit};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFormat {
    Mpeg,
    Flac,
    Ogg,
    Mp4,
    Wav,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatHint {
    /// Content detection only, independent of the diagnostic source name.
    Detect,
    /// Check a supported source extension against content; unknown extensions detect.
    /// Unsupported source parser families and mismatches are explicit errors.
    FromSourceName,
    Require(AudioFormat),
}
impl FormatHint {
    pub(crate) fn expected(self, name: &str) -> Result<Option<AudioFormat>, AudioError> {
        Ok(match self {
            Self::Detect => None,
            Self::Require(format) => Some(format),
            Self::FromSourceName => {
                match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()) {
                    Some(ext) => match ext.as_str() {
                        "mp2" | "mp3" | "m2a" => Some(AudioFormat::Mpeg),
                        "flac" => Some(AudioFormat::Flac),
                        "ogg" | "ogv" | "oga" | "ogm" | "ogx" | "opus" => Some(AudioFormat::Ogg),
                        "mp4" | "m4a" | "m4b" | "m4pa" | "m4v" | "m4r" | "3gp" => {
                            Some(AudioFormat::Mp4)
                        }
                        "wav" | "bwf" => Some(AudioFormat::Wav),
                        "aac" | "ape" | "wma" | "wmv" | "asf" | "spx" | "aif" | "aiff" | "aifc"
                        | "wv" | "wvp" | "mpc" | "dsf" | "dff" | "mka" | "mkv" | "mk3d" | "mks"
                        | "webm" => {
                            return Err(AudioError::UnsupportedVariant(
                                "source extension parser family".into(),
                            ))
                        }
                        _ => None,
                    },
                    None => None,
                }
            }
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverKnowledge {
    /// Common parsing skips covers. No absence or completeness is certified.
    Unknown,
}
#[derive(Debug)]
pub struct NativeCommon {
    value: Js,
}
impl NativeCommon {
    pub fn value(&self) -> &crate::CommonValue {
        &self.value
    }
    /// Move the native value into the host without a second common-data allocation.
    pub fn into_value(self) -> crate::CommonValue {
        self.value
    }
    /// Compatibility projection only: undefined keys disappear; nonfinite numbers become null.
    pub fn json_projection(&self) -> serde_json::Value {
        self.value.json()
    }
    pub fn covers(&self) -> CoverKnowledge {
        CoverKnowledge::Unknown
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum FileIdentity {
    Unix { device: u64, inode: u64 },
    Windows { volume: u64, index: u64 },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum NativeFileTime {
    Unix { seconds: i64, nanoseconds: i64 },
    Windows { ticks_100ns_since_1601: i64 },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileVersion {
    pub identity: FileIdentity,
    pub length: u64,
    pub modified: NativeFileTime,
    /// Native Unix ctime or Windows ChangeTime, never birth/creation time.
    pub changed: NativeFileTime,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct FileObservations {
    pub before: Option<FileVersion>,
    pub after: Option<FileVersion>,
}
impl FileObservations {
    /// Equality of two observed versions is not an immutable snapshot guarantee.
    pub fn unchanged(&self) -> Option<bool> {
        Some(self.before.as_ref()? == self.after.as_ref()?)
    }
}

/// Reserving this capacity performs no IO and copies no source-name buffer.
pub struct OpenedAdmission {
    inner: Arc<Inner>,
    permit: OwnedSemaphorePermit,
}
impl OpenedAdmission {
    pub(crate) fn new(inner: Arc<Inner>, permit: OwnedSemaphorePermit) -> Self {
        Self { inner, permit }
    }
    /// Caller already owns/authorizes the regular File. The source name is a bounded
    /// parser hint/diagnostic only: it is never opened, canonicalized or granted.
    /// Caller must not concurrently seek/read an OS-offset-sharing descriptor clone.
    pub fn submit(
        self,
        file: File,
        source_name: &str,
        hint: FormatHint,
        cancel: CancellationToken,
    ) -> Result<OpenedTicket, AudioError> {
        if source_name.is_empty() || source_name.len() > 32768 || source_name.contains('\0') {
            return Err(AudioError::Invalid);
        }
        let mut sender = self
            .inner
            .sender
            .lock()
            .map_err(|_| AudioError::Worker("queue lock poisoned".into()))?;
        if self.inner.cancel.is_cancelled() {
            return Err(AudioError::Shutdown);
        }
        if cancel.is_cancelled() {
            return Err(AudioError::Cancelled);
        }
        let (response, receive) = oneshot::channel();
        let work = Work {
            file,
            source_name: source_name.into(),
            hint,
            cancel,
            shutdown: self.inner.cancel.clone(),
            response,
            permit: self.permit,
            #[cfg(test)]
            gate: None,
            #[cfg(test)]
            after_read_gate: None,
        };
        sender
            .as_mut()
            .ok_or(AudioError::Shutdown)?
            .try_send(crate::Work::Opened(work))
            .map_err(|_| AudioError::Busy)?;
        Ok(OpenedTicket { receive })
    }
}
/// Dropping a ticket/waiter does not cancel accepted native parsing. The worker
/// retains its File/permit until real return, then disposes an undeliverable result.
pub struct OpenedTicket {
    receive: oneshot::Receiver<OpenedRead>,
}
impl OpenedTicket {
    pub async fn finish(self) -> Result<OpenedRead, AudioError> {
        self.receive
            .await
            .map_err(|_| AudioError::Worker("opened worker disconnected".into()))
    }
}
/// Shared admission remains held through ready-but-uncollected results and this
/// owned observation. No descriptor is cloned by the service.
#[derive(Debug)]
pub struct OpenedRead {
    common: Result<NativeCommon, AudioError>,
    file: File,
    observations: FileObservations,
    _permit: OwnedSemaphorePermit,
}
impl OpenedRead {
    pub fn common(&self) -> Result<&NativeCommon, &AudioError> {
        self.common.as_ref()
    }
    pub fn observations(&self) -> &FileObservations {
        &self.observations
    }
    /// Actual consuming handoff, including on parse error. Audio quota ends here;
    /// the host/assets whole-operation lease must bound the returned data/File and
    /// all subsequent sequential cover work. This does not claim the File closed.
    pub fn into_parts(self) -> (Result<NativeCommon, AudioError>, File, FileObservations) {
        (self.common, self.file, self.observations)
    }
}

pub(crate) struct Work {
    file: File,
    source_name: String,
    hint: FormatHint,
    cancel: CancellationToken,
    shutdown: CancellationToken,
    response: oneshot::Sender<OpenedRead>,
    permit: OwnedSemaphorePermit,
    #[cfg(test)]
    gate: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    #[cfg(test)]
    after_read_gate: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
}
pub(crate) fn execute(mut work: Work, config: &AudioConfig) {
    #[cfg(test)]
    if let Some((entered, release)) = work.gate.take() {
        let _ = entered.send(());
        let _ = release.recv();
    }
    let mut observations = FileObservations::default();
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        observations.before = Some(observe(&work.file)?);
        let result = crate::parser::parse_opened(
            &mut work.file,
            &work.source_name,
            work.hint,
            config,
            work.shutdown.clone(),
            work.cancel.clone(),
        );
        #[cfg(test)]
        if let Some((entered, release)) = work.after_read_gate.take() {
            let _ = entered.send(());
            let _ = release.recv();
        }
        observations.after = Some(observe(&work.file)?);
        if observations.unchanged() == Some(false) {
            return Err(AudioError::Changed);
        }
        result
    }))
    .unwrap_or_else(|_| Err(AudioError::Worker("native opened parser panicked".into())));
    let common = parsed.and_then(|value| {
        if raw_within_limit(
            &value,
            config.output_limit,
            config.tag_limit.saturating_mul(16),
        ) {
            Ok(NativeCommon { value })
        } else {
            Err(AudioError::Resource("native common output".into()))
        }
    });
    let common = if work.shutdown.is_cancelled() || work.cancel.is_cancelled() {
        Err(AudioError::Cancelled)
    } else {
        common
    };
    let ready = OpenedRead {
        common,
        file: work.file,
        observations,
        _permit: work.permit,
    };
    let _ = work.response.send(ready);
}

// Bound native values before producing another allocated projection. Undefined
// fields count too. Parser-created structures have fixed depth; retain an explicit
// bound here so a future collector cannot silently introduce unbounded recursion.
fn raw_within_limit(value: &Js, mut bytes: usize, mut nodes: usize) -> bool {
    fn visit(v: &Js, bytes: &mut usize, nodes: &mut usize, depth: usize) -> Option<()> {
        if depth > 16 {
            return None;
        }
        *nodes = nodes.checked_sub(1)?;
        *bytes = bytes.checked_sub(24)?;
        match v {
            Js::String(s) => {
                *bytes = bytes.checked_sub(s.len().checked_mul(6)?)?;
            }
            Js::Array(a) => {
                for v in a {
                    visit(v, bytes, nodes, depth + 1)?;
                }
            }
            Js::Object(o) => {
                for (k, v) in o {
                    *bytes = bytes.checked_sub(k.len().checked_mul(6)?)?;
                    visit(v, bytes, nodes, depth + 1)?;
                }
            }
            _ => {}
        }
        Some(())
    }
    visit(value, &mut bytes, &mut nodes, 0).is_some()
}

fn observe(file: &File) -> Result<FileVersion, AudioError> {
    let metadata = file.metadata().map_err(|_| AudioError::Input)?;
    if !metadata.is_file() {
        return Err(AudioError::Input);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(FileVersion {
            identity: FileIdentity::Unix {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            length: metadata.len(),
            modified: NativeFileTime::Unix {
                seconds: metadata.mtime(),
                nanoseconds: metadata.mtime_nsec(),
            },
            changed: NativeFileTime::Unix {
                seconds: metadata.ctime(),
                nanoseconds: metadata.ctime_nsec(),
            },
        })
    }
    #[cfg(windows)]
    {
        use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            FileBasicInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
            BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO,
        };
        let mut id = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        let mut basic = MaybeUninit::<FILE_BASIC_INFO>::uninit();
        // SAFETY: held File is live; exact windows-sys public output layouts and lengths.
        let success = unsafe {
            GetFileInformationByHandle(file.as_raw_handle().cast(), id.as_mut_ptr()) != 0
                && GetFileInformationByHandleEx(
                    file.as_raw_handle().cast(),
                    FileBasicInfo,
                    basic.as_mut_ptr().cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                ) != 0
        };
        if !success {
            return Err(AudioError::UnsupportedVariant(
                "native file identity/change-time unavailable".into(),
            ));
        }
        // SAFETY: both successful calls initialized their exact output structs.
        let (id, basic) = unsafe { (id.assume_init(), basic.assume_init()) };
        Ok(FileVersion {
            identity: FileIdentity::Windows {
                volume: id.dwVolumeSerialNumber as u64,
                index: ((id.nFileIndexHigh as u64) << 32) | id.nFileIndexLow as u64,
            },
            length: metadata.len(),
            modified: NativeFileTime::Windows {
                ticks_100ns_since_1601: basic.LastWriteTime,
            },
            changed: NativeFileTime::Windows {
                ticks_100ns_since_1601: basic.ChangeTime,
            },
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(AudioError::UnsupportedVariant(
            "native descriptor identity/version platform".into(),
        ))
    }
}

#[cfg(test)]
mod tests;
