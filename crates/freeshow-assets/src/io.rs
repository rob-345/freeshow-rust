use crate::access::{MediaObservation, ReadLease, Registration, Stamp};
use crate::{Control, Error, Stats};
use std::{
    collections::HashMap,
    fs::File,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{oneshot, watch};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity(u64, u64);
#[derive(Clone)]
pub(crate) struct Handle(Arc<HandleInner>);
struct HandleInner {
    worker: usize,
    id: u64,
    released: Arc<AtomicBool>,
}
impl Drop for HandleInner {
    fn drop(&mut self) {
        self.released.store(true, Ordering::Release);
    }
}
pub(crate) struct Opened {
    pub handle: Handle,
    pub path: PathBuf,
    pub identity: Identity,
    pub length: u64,
}
pub(crate) struct NativeOpened {
    pub file: File,
    pub handle: Handle,
    pub observation: MediaObservation,
}
enum Operation {
    NativeOpen {
        registration: Arc<Registration>,
        lease: Arc<ReadLease>,
    },
    NativeCheck {
        registration: Arc<Registration>,
        handle: Handle,
        expected: Stamp,
        lease: Arc<ReadLease>,
    },
    Open {
        path: PathBuf,
        expected: Option<Identity>,
    },
    Read {
        handle: Handle,
        offset: u64,
        count: usize,
    },
}
enum Answer {
    NativeOpen(NativeOpened),
    NativeCheck(MediaObservation),
    Open(Opened),
    Read(Vec<u8>),
}
struct Work {
    operation: Operation,
    control: Control,
    answer: oneshot::Sender<Result<Answer, Error>>,
}
#[cfg(test)]
struct TestPause {
    path: PathBuf,
    entered: oneshot::Sender<()>,
    release: mpsc::Receiver<()>,
}
#[cfg(test)]
struct TestExitPause {
    entered: oneshot::Sender<()>,
    release: mpsc::Receiver<()>,
    panic: bool,
}
struct PoolInner {
    #[cfg(test)]
    pause: Arc<Mutex<Option<TestPause>>>,
    #[cfg(test)]
    exit_pause: Arc<Mutex<Option<TestExitPause>>>,
    senders: Mutex<Option<Vec<SyncSender<Work>>>>,
    closing: Arc<AtomicBool>,
    next: AtomicUsize,
    done: watch::Receiver<usize>,
    stats: Arc<Stats>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    failure: Mutex<Option<Error>>,
}
impl PoolInner {
    fn close(&self) {
        self.closing.store(true, Ordering::Release);
        if let Ok(mut senders) = self.senders.lock() {
            senders.take();
        }
    }
}
impl Drop for PoolInner {
    fn drop(&mut self) {
        self.close();
    }
}
#[derive(Clone)]
pub(crate) struct Pool(Arc<PoolInner>);

pub(crate) fn identity(file: &File, metadata: &std::fs::Metadata) -> Result<Identity, Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = file;
        Ok(Identity(metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        let _ = metadata;
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: file remains open and info has the exact native output layout.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), info.as_mut_ptr()) }
            == 0
        {
            return Err(Error::os(std::io::Error::last_os_error()));
        }
        let info = unsafe { info.assume_init() };
        Ok(Identity(
            info.dwVolumeSerialNumber as u64,
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, metadata);
        Err(Error::new(
            "assets_unsupported",
            "File identity is unsupported on this OS",
        ))
    }
}

impl Pool {
    pub(crate) fn start(
        workers: usize,
        capacity: usize,
        handles_per_worker: usize,
        chunk_bytes: usize,
        max_file_bytes: u64,
        stats: Arc<Stats>,
    ) -> Result<Self, Error> {
        let closing = Arc::new(AtomicBool::new(false));
        let (completed, done) = watch::channel(workers);
        let id = Arc::new(AtomicU64::new(1));
        let mut senders = Vec::new();
        let mut threads = Vec::new();
        let startup_stop = closing.clone();
        #[cfg(test)]
        let pause = Arc::new(Mutex::new(None::<TestPause>));
        #[cfg(test)]
        let exit_pause = Arc::new(Mutex::new(None::<TestExitPause>));
        for worker in 0..workers {
            let (sender, receiver) = mpsc::sync_channel::<Work>(capacity);
            senders.push(sender);
            let closing = closing.clone();
            let completed = completed.clone();
            let stats = stats.clone();
            let id = id.clone();
            #[cfg(test)]
            let pause = pause.clone();
            #[cfg(test)]
            let exit_pause = exit_pause.clone();
            let spawned = std::thread::Builder::new().name(format!("freeshow-assets-{worker}")).spawn(move || {
                struct Completed(watch::Sender<usize>);
                impl Drop for Completed { fn drop(&mut self) { self.0.send_modify(|left| *left = left.saturating_sub(1)); } }
                let _completed = Completed(completed);
                // The fixed worker owns HTTP files and native anchor descriptors.
                // Exported native duplicates retain a separate whole-result lease.
                let mut files = HashMap::<u64, (File, Arc<AtomicBool>)>::new();
                loop {
                    files.retain(|_, (_, released)| { if released.load(Ordering::Acquire) { stats.open_files.fetch_sub(1, Ordering::AcqRel); false } else { true } });
                    let work = match receiver.recv_timeout(Duration::from_millis(250)) {
                        Ok(work) => work,
                        Err(mpsc::RecvTimeoutError::Timeout) => { if closing.load(Ordering::Acquire) { break } else { continue } },
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    stats.queued_io.fetch_sub(1, Ordering::AcqRel);
                    stats.active_io.fetch_add(1, Ordering::AcqRel);
                    #[cfg(test)]
                    {
                        let path = match &work.operation { Operation::Open { path, .. } => Some(path.as_path()), Operation::NativeOpen { registration, .. } | Operation::NativeCheck { registration, .. } => Some(registration.path()), _ => None };
                        if let Some(path) = path {
                        let held = { let mut held = pause.lock().unwrap(); if held.as_ref().is_some_and(|held| held.path.as_path() == path) { held.take() } else { None } };
                        if let Some(held) = held { let _ = held.entered.send(()); let _ = held.release.recv(); }
                        }
                    }
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        work.control.check(&closing)?;
                        match work.operation {
                            Operation::NativeOpen { registration, lease } => {
                                if files.len() >= handles_per_worker { return Err(Error::new("assets_busy", "File handle budget exhausted")); }
                                let (file, stamp) = registration.open(&work.control)?;
                                let duplicate = file.try_clone().map_err(Error::os)?;
                                registration.check(&work.control)?;
                                let number = id.fetch_add(1, Ordering::Relaxed);
                                let released = Arc::new(AtomicBool::new(false));
                                let handle = Handle(Arc::new(HandleInner { worker, id: number, released: released.clone() }));
                                files.insert(number, (file, released));
                                stats.open_files.fetch_add(1, Ordering::AcqRel);
                                Ok(Answer::NativeOpen(NativeOpened { file: duplicate, handle, observation: MediaObservation { stamp, lease } }))
                            }
                            Operation::NativeCheck { registration, handle, expected, lease } => {
                                let file = &files.get(&handle.0.id).ok_or_else(|| Error::new("assets_closed", "Native media anchor expired"))?.0;
                                let stamp = registration.revalidate(file, &expected, &work.control)?;
                                Ok(Answer::NativeCheck(MediaObservation { stamp, lease }))
                            }
                            Operation::Open { path, expected } => {
                                if files.len() >= handles_per_worker { return Err(Error::new("assets_busy", "File handle budget exhausted")); }
                                let path = if expected.is_none() { std::fs::canonicalize(path).map_err(Error::os)? } else { path };
                                work.control.check(&closing)?;
                                // Windows ordinary file opens report access denied
                                // for directories. Reject a proven directory with
                                // the same typed outcome as other platforms; the
                                // opened-handle check below still guards races.
                                #[cfg(windows)]
                                if std::fs::metadata(&path).map_err(Error::os)?.is_dir() {
                                    return Err(Error::new("assets_invalid", "Only regular media files may be granted"));
                                }
                                let mut options = std::fs::OpenOptions::new(); options.read(true);
                                #[cfg(unix)]
                                { use std::os::unix::fs::OpenOptionsExt; options.custom_flags(libc::O_NONBLOCK); }
                                let file = options.open(&path).map_err(Error::os)?;
                                let metadata = file.metadata().map_err(Error::os)?;
                                if !metadata.is_file() { return Err(Error::new("assets_invalid", "Only regular media files may be granted")); }
                                if metadata.len() > max_file_bytes { return Err(Error::new("assets_limit", "File exceeds the media size limit")); }
                                let identity = identity(&file, &metadata)?;
                                if expected.as_ref().is_some_and(|expected| expected != &identity) { return Err(Error::new("assets_changed", "Granted file was replaced; host authorization is required")); }
                                work.control.check(&closing)?;
                                let number = id.fetch_add(1, Ordering::Relaxed);
                                let released = Arc::new(AtomicBool::new(false));
                                let handle = Handle(Arc::new(HandleInner { worker, id: number, released: released.clone() }));
                                files.insert(number, (file, released));
                                stats.open_files.fetch_add(1, Ordering::AcqRel);
                                Ok(Answer::Open(Opened { handle, path, identity, length: metadata.len() }))
                            }
                            Operation::Read { handle, offset, count } => {
                                if count > chunk_bytes { return Err(Error::new("assets_limit", "Read chunk exceeds the buffer budget")); }
                                let file = &files.get(&handle.0.id).ok_or_else(|| Error::new("assets_closed", "File ticket has expired"))?.0;
                                let mut buffer = vec![0u8; count];
                                let mut done = 0;
                                while done < count {
                                    work.control.check(&closing)?;
                                    #[cfg(unix)]
                                    let read = { use std::os::unix::fs::FileExt; file.read_at(&mut buffer[done..], offset + done as u64) };
                                    #[cfg(windows)]
                                    let read = { use std::os::windows::fs::FileExt; file.seek_read(&mut buffer[done..], offset + done as u64) };
                                    #[cfg(not(any(unix, windows)))]
                                    let read = Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Native read-at is unavailable"));
                                    match read { Ok(0) => return Err(Error::new("assets_changed", "Media file ended before its declared response length")), Ok(count) => { done += count; stats.read_bytes.fetch_add(count as u64, Ordering::Relaxed); }, Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue, Err(error) => return Err(Error::os(error)) }
                                }
                                work.control.check(&closing)?;
                                Ok(Answer::Read(buffer))
                            }
                        }
                    })).unwrap_or_else(|_| Err(Error::new("assets_worker", "Native media worker panicked")));
                    stats.active_io.fetch_sub(1, Ordering::AcqRel);
                    let _ = work.answer.send(result);
                }
                stats.open_files.fetch_sub(files.len(), Ordering::AcqRel);
                drop(files);
                drop(_completed);
                #[cfg(test)]
                {
                    // A real thread can remain alive after its body announces
                    // logical completion (e.g. final native/TLS destruction).
                    let held = exit_pause.lock().unwrap().take();
                    if let Some(held) = held { let _ = held.entered.send(()); let _ = held.release.recv(); assert!(!held.panic, "controlled actual asset worker exit panic"); }
                }
            });
            match spawned {
                Ok(worker) => threads.push(worker),
                Err(error) => {
                    startup_stop.store(true, Ordering::Release);
                    drop(senders);
                    for worker in threads {
                        let _ = worker.join();
                    }
                    return Err(Error::os(error));
                }
            }
        }
        Ok(Self(Arc::new(PoolInner {
            #[cfg(test)]
            pause,
            #[cfg(test)]
            exit_pause,
            senders: Mutex::new(Some(senders)),
            closing,
            next: AtomicUsize::new(0),
            done,
            stats,
            workers: Mutex::new(threads),
            failure: Mutex::new(None),
        })))
    }
    async fn submit(
        &self,
        worker: usize,
        operation: Operation,
        control: Control,
    ) -> Result<Answer, Error> {
        control.check(&self.0.closing)?;
        let (answer, result) = oneshot::channel();
        let waiting = control.clone();
        {
            let senders = self
                .0
                .senders
                .lock()
                .map_err(|_| Error::new("assets_worker", "File admission lock poisoned"))?;
            let senders = senders
                .as_ref()
                .ok_or_else(|| Error::new("assets_shutdown", "Media IO is shut down"))?;
            self.0.stats.queued_io.fetch_add(1, Ordering::AcqRel);
            if let Err(error) = senders[worker % senders.len()].try_send(Work {
                operation,
                control,
                answer,
            }) {
                self.0.stats.queued_io.fetch_sub(1, Ordering::AcqRel);
                return Err(Error::new(
                    if matches!(error, mpsc::TrySendError::Full(_)) {
                        "assets_busy"
                    } else {
                        "assets_shutdown"
                    },
                    "Media IO request was not admitted",
                ));
            }
        }
        let epoch = waiting.epoch.clone().unwrap_or_default();
        let response = tokio::select! {
            result = result => result.map_err(|_| Error::new("assets_worker", "Native media worker stopped before replying"))?,
            _ = waiting.cancel.cancelled() => Err(Error::new("assets_cancelled", "Asset request cancelled while waiting for IO")),
            _ = epoch.cancelled() => Err(Error::new("assets_stale_context", "Library changed while waiting for IO")),
            _ = tokio::time::sleep_until(waiting.deadline.into()) => Err(Error::new("assets_deadline", "Asset IO deadline elapsed")),
        };
        waiting.check(&self.0.closing)?;
        response
    }
    pub(crate) async fn open(
        &self,
        path: PathBuf,
        expected: Option<Identity>,
        control: Control,
    ) -> Result<Opened, Error> {
        let worker = self.0.next.fetch_add(1, Ordering::Relaxed);
        match self
            .submit(worker, Operation::Open { path, expected }, control)
            .await?
        {
            Answer::Open(opened) => Ok(opened),
            _ => unreachable!(),
        }
    }
    pub(crate) async fn read(
        &self,
        handle: Handle,
        offset: u64,
        count: usize,
        control: Control,
    ) -> Result<Vec<u8>, Error> {
        match self
            .submit(
                handle.0.worker,
                Operation::Read {
                    handle,
                    offset,
                    count,
                },
                control,
            )
            .await?
        {
            Answer::Read(buffer) => Ok(buffer),
            _ => unreachable!(),
        }
    }
    pub(crate) async fn native_open(
        &self,
        registration: Arc<Registration>,
        lease: Arc<ReadLease>,
        control: Control,
    ) -> Result<NativeOpened, Error> {
        let worker = self.0.next.fetch_add(1, Ordering::Relaxed);
        match self
            .submit(
                worker,
                Operation::NativeOpen {
                    registration,
                    lease,
                },
                control,
            )
            .await?
        {
            Answer::NativeOpen(opened) => Ok(opened),
            _ => unreachable!(),
        }
    }
    pub(crate) async fn native_check(
        &self,
        registration: Arc<Registration>,
        handle: Handle,
        expected: Stamp,
        lease: Arc<ReadLease>,
        control: Control,
    ) -> Result<MediaObservation, Error> {
        match self
            .submit(
                handle.0.worker,
                Operation::NativeCheck {
                    registration,
                    handle,
                    expected,
                    lease,
                },
                control,
            )
            .await?
        {
            Answer::NativeCheck(observation) => Ok(observation),
            _ => unreachable!(),
        }
    }
    pub(crate) fn joined(&self) -> bool {
        self.0
            .workers
            .lock()
            .map(|workers| workers.is_empty())
            .unwrap_or(false)
    }
    #[cfg(test)]
    pub(crate) fn worker_exit_observation(&self) -> (usize, bool) {
        // Observation only: retain every real handle for production drain/join.
        // Logical body completion alone does not prove the thread has exited.
        let workers = self.0.workers.lock().unwrap();
        (
            *self.0.done.borrow(),
            workers.iter().all(std::thread::JoinHandle::is_finished),
        )
    }
    #[cfg(test)]
    pub(crate) fn pause_for(&self, path: PathBuf) -> (oneshot::Receiver<()>, mpsc::SyncSender<()>) {
        let (entered, waiting) = oneshot::channel();
        let (release, held) = mpsc::sync_channel(1);
        *self.0.pause.lock().unwrap() = Some(TestPause {
            path,
            entered,
            release: held,
        });
        (waiting, release)
    }
    #[cfg(test)]
    pub(crate) fn pause_exit(&self, panic: bool) -> (oneshot::Receiver<()>, mpsc::SyncSender<()>) {
        let (entered, waiting) = oneshot::channel();
        let (release, held) = mpsc::sync_channel(1);
        *self.0.exit_pause.lock().unwrap() = Some(TestExitPause {
            entered,
            release: held,
            panic,
        });
        (waiting, release)
    }
    pub(crate) fn close(&self) {
        self.0.close();
    }
    pub(crate) async fn drain(&self) -> Result<(), Error> {
        self.close();
        let mut done = self.0.done.clone();
        while *done.borrow_and_update() != 0 {
            done.changed().await.map_err(|_| {
                Error::new("assets_worker", "Native media completion channel closed")
            })?;
        }
        loop {
            if self
                .0
                .workers
                .lock()
                .map_err(|_| Error::new("assets_worker", "Native worker join lock poisoned"))?
                .iter()
                .all(std::thread::JoinHandle::is_finished)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        // No await after taking handles: canceling an observer cannot detach an
        // actual join. OS/TLS final thread teardown is not preemptible.
        for worker in std::mem::take(
            &mut *self
                .0
                .workers
                .lock()
                .map_err(|_| Error::new("assets_worker", "Native worker join lock poisoned"))?,
        ) {
            if worker.join().is_err() {
                *self
                    .0
                    .failure
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(Error::new(
                    "assets_worker",
                    "Native media worker panicked during teardown",
                ));
            }
        }
        self.0
            .failure
            .lock()
            .map_err(|_| Error::new("assets_worker", "Native worker failure lock poisoned"))?
            .clone()
            .map_or(Ok(()), Err)
    }
}
