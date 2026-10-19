//! Real codecs on one fixed owner; trusted pinned library only. No audio device.
mod backend;
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Mutex, Weak,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
pub const LIBRARY_SHA256: &str = "f02c75f982fe01d16f345875d2b09404b8b326bb847f2b5456616f6d0227d09f";
pub const MAX_LEASES: usize = 8;
pub const MAX_AUDIO_BYTES: usize = 65536;
pub const MAX_SESSION_SAMPLES: u64 = 48000 * 3600 * 24;
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    fn new(code: &'static str, message: impl AsRef<str>) -> Self {
        let text = message.as_ref();
        let mut n = text.len().min(1024);
        while !text.is_char_boundary(n) {
            n -= 1;
        }
        Self {
            code,
            message: text[..n].into(),
        }
    }
    fn unavailable(message: impl AsRef<str>) -> Self {
        Self::new("ltc_backend_unavailable", message)
    }
    fn limit() -> Self {
        Self::new("ltc_resource_limit", "LTC resource limit exceeded")
    }
    fn closing() -> Self {
        Self::new("ltc_shutdown", "LTC owner is retired")
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
#[derive(Clone)]
pub struct TrustedLibrary {
    path: PathBuf,
}
impl TrustedLibrary {
    /// Native application resource path only; never expose as renderer IPC.
    pub fn from_host(path: PathBuf) -> Result<Self> {
        if !path.is_absolute()
            || path.as_os_str().as_encoded_bytes().len() > 32768
            || path.as_os_str().as_encoded_bytes().contains(&0)
        {
            return Err(Error::limit());
        }
        Ok(Self { path })
    }
}
struct Run {
    live: AtomicBool,
}
#[derive(Clone)]
pub struct Owner {
    runtime: String,
    generation: u64,
    data_path: String,
    run: Arc<Run>,
}
impl Owner {
    pub fn new(runtime: &str, generation: u64, data_path: &str) -> Result<Self> {
        if runtime.is_empty()
            || runtime.len() > 256
            || runtime.contains('\0')
            || data_path.len() > 32768
            || data_path.contains('\0')
            || !std::path::Path::new(data_path).is_absolute()
        {
            return Err(Error::limit());
        }
        Ok(Self {
            runtime: runtime.into(),
            generation,
            data_path: data_path.into(),
            run: Arc::new(Run {
                live: AtomicBool::new(true),
            }),
        })
    }
    pub fn retire(&self) {
        self.run.live.store(false, Ordering::Release);
    }
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.run, &other.run)
            && self.runtime == other.runtime
            && self.generation == other.generation
            && self.data_path == other.data_path
    }
    fn live(&self) -> bool {
        self.run.live.load(Ordering::Acquire)
    }
}
#[derive(Clone)]
pub struct Config {
    pub max_audio_bytes: usize,
    pub max_session_samples: u64,
    pub max_operations: u64,
    pub shutdown_timeout: Duration,
    #[cfg(test)]
    gate: Option<Arc<tests::Gate>>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            max_audio_bytes: MAX_AUDIO_BYTES,
            max_session_samples: MAX_SESSION_SAMPLES,
            max_operations: 8_000_000,
            shutdown_timeout: Duration::from_secs(5),
            #[cfg(test)]
            gate: None,
        }
    }
}
#[derive(Debug)]
pub enum Operation {
    Capability,
    CreateEncoder {
        fps: f64,
    },
    CreateDecoder {
        fps: f64,
    },
    Encode {
        hours: f64,
        minutes: f64,
        seconds: f64,
        frames: f64,
    },
    Decode {
        bytes: Vec<u8>,
    },
    StopEncoder,
    StopDecoder,
}
#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
fn supported_fps(fps: f64) -> Result<()> {
    if [24.0, 25.0, 29.97, 30.0].contains(&fps) {
        Ok(())
    } else {
        Err(Error::new(
            "ltc_unsupported",
            "Only verified numeric 24/25/29.97/30 fps is supported",
        ))
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub available: bool,
    pub platform: &'static str,
    pub architecture: &'static str,
    pub header_version: Option<&'static str>,
    pub library_sha256: Option<&'static str>,
    pub error: Option<Error>,
}
impl Capability {
    fn available() -> Self {
        Self {
            available: true,
            platform: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            header_version: Some("1.3.1"),
            library_sha256: Some(LIBRARY_SHA256),
            error: None,
        }
    }
    fn unavailable(error: Error) -> Self {
        Self {
            available: false,
            platform: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            header_version: None,
            library_sha256: None,
            error: Some(error),
        }
    }
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Decoded {
    pub drop_frame_format: bool,
    pub days: i32,
    pub months: i32,
    pub years: i32,
    pub hours: i32,
    pub minutes: i32,
    pub seconds: i32,
    pub frames: i32,
    pub offset_start: i64,
    pub reverse: bool,
    pub volume: f64,
    pub timezone: String,
}
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum Reply {
    Capability(Capability),
    Done,
    Audio(Vec<u8>),
    Frame(Option<Decoded>),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Queued,
    Accepted,
    Ready,
    Released,
}
struct Cell {
    shared: Arc<Shared>,
    phase: Mutex<Phase>,
    cancel: AtomicBool,
    leased: AtomicBool,
}
impl Cell {
    fn release(&self) {
        *lock(&self.phase) = Phase::Released;
        if self.leased.swap(false, Ordering::AcqRel) {
            self.shared.admitted.fetch_sub(1, Ordering::AcqRel);
        }
    }
    fn cancel(&self) {
        if *lock(&self.phase) == Phase::Queued {
            self.cancel.store(true, Ordering::Release);
        }
    }
}
impl Drop for Cell {
    fn drop(&mut self) {
        if self.leased.swap(false, Ordering::AcqRel) {
            self.shared.admitted.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
struct Shared {
    owner: Owner,
    closing: AtomicBool,
    admitted: AtomicUsize,
    cells: Mutex<Vec<Weak<Cell>>>,
    failure: Mutex<Option<Error>>,
    loaded: AtomicBool,
    encoder: AtomicBool,
    decoder: AtomicBool,
    encoder_created: AtomicU64,
    encoder_freed: AtomicU64,
    decoder_created: AtomicU64,
    decoder_freed: AtomicU64,
    samples: AtomicU64,
    backlog: AtomicUsize,
}
impl Shared {
    fn close(&self) {
        self.closing.store(true, Ordering::Release);
        self.owner.retire();
        for cell in lock(&self.cells).iter().filter_map(Weak::upgrade) {
            cell.cancel();
            let ready = *lock(&cell.phase) == Phase::Ready;
            if ready {
                cell.release();
            }
        }
    }
}
struct Work {
    operation: Operation,
    deadline: Instant,
    cell: Arc<Cell>,
    reply: oneshot::Sender<Ready>,
}
struct Inner {
    shared: Arc<Shared>,
    sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    config: Config,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.close();
        lock(&self.sender).take();
        if let Some(thread) = lock(&self.thread).take() {
            let _ = thread.join();
        }
    }
}
#[derive(Clone)]
pub struct LtcService {
    inner: Arc<Inner>,
}
pub struct Ticket {
    reply: oneshot::Receiver<Ready>,
    cell: Arc<Cell>,
    armed: bool,
}
impl Ticket {
    pub fn cancel(&self) {
        self.cell.cancel();
    }
    pub async fn finish(mut self) -> Result<Ready> {
        let ready = (&mut self.reply)
            .await
            .map_err(|_| Error::new("ltc_worker", "Codec owner stopped before replying"))?;
        self.armed = false;
        Ok(ready)
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if self.armed {
            self.cell.cancel();
        }
    }
}
pub struct Ready {
    outcome: Result<Reply>,
    cell: Arc<Cell>,
}
impl Ready {
    pub fn into_reply(self) -> Result<Reply> {
        if self.cell.shared.closing.load(Ordering::Acquire) || !self.cell.shared.owner.live() {
            return Err(Error::closing());
        }
        self.cell.release();
        self.outcome
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub closing: bool,
    pub admitted: usize,
    pub worker_finished: bool,
    pub loaded: bool,
    pub encoder: bool,
    pub decoder: bool,
    pub encoder_created: u64,
    pub encoder_freed: u64,
    pub decoder_created: u64,
    pub decoder_freed: u64,
    pub samples: u64,
    pub backlog: usize,
    pub failure: Option<Error>,
}
impl LtcService {
    pub fn start(owner: Owner, library: TrustedLibrary, config: Config) -> Result<Self> {
        if !owner.live()
            || config.max_audio_bytes == 0
            || config.max_audio_bytes > MAX_AUDIO_BYTES
            || config.max_session_samples == 0
            || config.max_session_samples > MAX_SESSION_SAMPLES
            || config.max_operations == 0
            || config.max_operations > 8_000_000
            || config.shutdown_timeout.is_zero()
            || config.shutdown_timeout > Duration::from_secs(30)
        {
            return Err(Error::limit());
        }
        let shared = Arc::new(Shared {
            owner,
            closing: AtomicBool::new(false),
            admitted: AtomicUsize::new(0),
            cells: Mutex::new(vec![]),
            failure: Mutex::new(None),
            loaded: AtomicBool::new(false),
            encoder: AtomicBool::new(false),
            decoder: AtomicBool::new(false),
            encoder_created: AtomicU64::new(0),
            encoder_freed: AtomicU64::new(0),
            decoder_created: AtomicU64::new(0),
            decoder_freed: AtomicU64::new(0),
            samples: AtomicU64::new(0),
            backlog: AtomicUsize::new(0),
        });
        let (send, receive) = mpsc::sync_channel::<Work>(MAX_LEASES);
        let state = shared.clone();
        let limits = config.clone();
        let thread = std::thread::Builder::new()
            .name("freeshow-ltc-owner".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut backend = backend::Native::open(&library, state.clone());
                    let capability = match &backend {
                        Ok(_) => Capability::available(),
                        Err(error) => Capability::unavailable(error.clone()),
                    };
                    let mut count = 0;
                    loop {
                        let work = match receive.recv_timeout(Duration::from_millis(5)) {
                            Ok(work) => work,
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                if state.closing.load(Ordering::Acquire) || !state.owner.live() {
                                    break;
                                }
                                continue;
                            }
                            Err(_) => break,
                        };
                        let cancelled = {
                            let mut phase = lock(&work.cell.phase);
                            let cancelled = state.closing.load(Ordering::Acquire)
                                || !state.owner.live()
                                || work.cell.cancel.load(Ordering::Acquire)
                                || Instant::now() >= work.deadline;
                            if !cancelled {
                                *phase = Phase::Accepted;
                            }
                            cancelled
                        };
                        let outcome = if cancelled {
                            Err(Error::new(
                                "ltc_cancelled",
                                "Codec work cancelled before acceptance",
                            ))
                        } else if count >= limits.max_operations {
                            Err(Error::limit())
                        } else {
                            count += 1;
                            #[cfg(test)]
                            if matches!(work.operation, Operation::CreateEncoder { .. }) {
                                if let Some(gate) = &limits.gate {
                                    gate.wait();
                                }
                            }
                            match work.operation {
                                Operation::Capability => Ok(Reply::Capability(capability.clone())),
                                op => match &mut backend {
                                    Ok(backend) => backend.run(op, limits.max_session_samples),
                                    Err(error) => Err(error.clone()),
                                },
                            }
                        };
                        {
                            let mut phase = lock(&work.cell.phase);
                            *phase = Phase::Ready;
                        }
                        if state.closing.load(Ordering::Acquire) || !state.owner.live() {
                            work.cell.release();
                        }
                        let _ = work.reply.send(Ready {
                            outcome,
                            cell: work.cell,
                        });
                    }
                    state.close();
                    drop(backend);
                }));
                if result.is_err() {
                    *lock(&state.failure) = Some(Error::new(
                        "ltc_worker",
                        "Codec owner panicked after dropping its native guards",
                    ));
                }
                state.close();
                // Every native guard has exited before revoking cells whose
                // receiver/ticket outlived a worker failure.
                for cell in lock(&state.cells).iter().filter_map(Weak::upgrade) {
                    cell.release();
                }
            })
            .map_err(|e| Error::new("ltc_worker", e.to_string()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                shared,
                sender: Mutex::new(Some(send)),
                thread: Mutex::new(Some(thread)),
                config,
            }),
        })
    }
    pub fn begin(&self, owner: &Owner, operation: Operation, deadline: Instant) -> Result<Ticket> {
        let shared = &self.inner.shared;
        if !shared.owner.same(owner) {
            return Err(Error::new(
                "ltc_stale_owner",
                "Wrong runtime/library/run owner",
            ));
        }
        if shared.closing.load(Ordering::Acquire) || !owner.live() {
            return Err(Error::closing());
        }
        if deadline > Instant::now() + Duration::from_secs(30) {
            return Err(Error::limit());
        }
        if let Operation::Decode { bytes } = &operation {
            if bytes.len() > self.inner.config.max_audio_bytes {
                return Err(Error::limit());
            }
        }
        let mut admitted = shared.admitted.load(Ordering::Acquire);
        loop {
            if admitted >= MAX_LEASES {
                return Err(Error::new("ltc_busy", "Codec result/work leases are full"));
            }
            match shared.admitted.compare_exchange_weak(
                admitted,
                admitted + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(n) => admitted = n,
            }
        }

        let cell = Arc::new(Cell {
            shared: shared.clone(),
            phase: Mutex::new(Phase::Queued),
            cancel: AtomicBool::new(false),
            leased: AtomicBool::new(true),
        });
        {
            let mut cells = lock(&shared.cells);
            cells.retain(|c| {
                c.upgrade()
                    .is_some_and(|c| *lock(&c.phase) != Phase::Released)
            });
            if shared.closing.load(Ordering::Acquire) {
                return Err(Error::closing());
            }
            cells.push(Arc::downgrade(&cell));
        }
        let (reply, receive) = oneshot::channel();
        lock(&self.inner.sender)
            .as_ref()
            .ok_or_else(Error::closing)?
            .try_send(Work {
                operation,
                deadline,
                cell: cell.clone(),
                reply,
            })
            .map_err(|_| Error::new("ltc_busy", "Codec queue is unavailable"))?;
        Ok(Ticket {
            reply: receive,
            cell,
            armed: true,
        })
    }
    pub fn status(&self) -> Status {
        let s = &self.inner.shared;
        Status {
            closing: s.closing.load(Ordering::Acquire),
            admitted: s.admitted.load(Ordering::Acquire),
            worker_finished: lock(&self.inner.thread)
                .as_ref()
                .is_none_or(JoinHandle::is_finished),
            loaded: s.loaded.load(Ordering::Acquire),
            encoder: s.encoder.load(Ordering::Acquire),
            decoder: s.decoder.load(Ordering::Acquire),
            encoder_created: s.encoder_created.load(Ordering::Acquire),
            encoder_freed: s.encoder_freed.load(Ordering::Acquire),
            decoder_created: s.decoder_created.load(Ordering::Acquire),
            decoder_freed: s.decoder_freed.load(Ordering::Acquire),
            samples: s.samples.load(Ordering::Acquire),
            backlog: s.backlog.load(Ordering::Acquire),
            failure: lock(&s.failure).clone(),
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.shared.close();
        lock(&self.inner.sender).take();
        let deadline = Instant::now() + self.inner.config.shutdown_timeout;
        loop {
            if lock(&self.inner.thread)
                .as_ref()
                .is_none_or(JoinHandle::is_finished)
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err(Error::new(
                    "ltc_shutdown_timeout",
                    "Actual codec owner has not exited; retain service and retry",
                ));
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        if let Some(thread) = lock(&self.inner.thread).take() {
            thread
                .join()
                .map_err(|_| Error::new("ltc_worker", "Codec owner panicked"))?;
        }
        if let Some(error) = lock(&self.inner.shared.failure).clone() {
            return Err(error);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;
