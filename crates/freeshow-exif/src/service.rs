use crate::{missing, parser, Context, Control, Error, ExifOutcome, Limits, Stats};
use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::Read,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::Instant,
};
use tokio::sync::{oneshot, Notify, Semaphore};
use tokio_util::sync::CancellationToken;

pub(crate) fn validate_input(id: &str, context: &Context, limits: &Limits) -> Result<(), Error> {
    if id.is_empty()
        || id.len() > limits.path_bytes
        || id.contains('\0')
        || context.runtime_id.is_empty()
        || context.runtime_id.len() > 256
        || context.runtime_id.contains('\0')
        || context.data_path.is_empty()
        || context.data_path.len() > limits.path_bytes
        || context.data_path.contains('\0')
        || context.library_generation > 9_007_199_254_740_991
    {
        return Err(Error::new(
            "exif_invalid",
            "Invalid or oversized EXIF path/context",
        ));
    }
    if base_allocation(id, context) > limits.allocation_bytes {
        return Err(Error::new(
            "exif_limit",
            "EXIF path/context exceeds allocation budget",
        ));
    }
    Ok(())
}
pub(crate) fn base_allocation(id: &str, context: &Context) -> usize {
    (id.len() + context.data_path.len() + context.runtime_id.len())
        .saturating_mul(8)
        .saturating_add(4096)
}
struct Work {
    id: String,
    context: Context,
    control: Control,
    reply: oneshot::Sender<Result<ExifOutcome, Error>>,
}
struct Shared {
    limits: Limits,
    closing: AtomicBool,
    remaining: AtomicUsize,
    active: AtomicUsize,
    queued: AtomicUsize,
    drained: Notify,
    #[cfg(test)]
    gate: Mutex<Option<Arc<crate::tests::Gate>>>,
}
struct Inner {
    shared: Arc<Shared>,
    sender: Mutex<Option<SyncSender<Work>>>,
    slots: Arc<Semaphore>,
    next: AtomicU64,
    controls: Mutex<HashMap<u64, CancellationToken>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        if let Ok(controls) = self.controls.get_mut() {
            for token in controls.values() {
                token.cancel();
            }
        }
    }
}
/// Two dedicated OS/file/parser workers. Admission includes both active workers
/// and queued/completed-but-not-collected replies; no per-request task/thread spawn.
#[derive(Clone)]
pub struct ExifService {
    inner: Arc<Inner>,
}
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub workers: usize,
    pub active: usize,
    pub queued: usize,
    pub admitted: usize,
    pub closing: bool,
}
struct Pending {
    inner: Arc<Inner>,
    key: u64,
    cancel: CancellationToken,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.inner
            .controls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}
impl ExifService {
    pub fn start(limits: Limits) -> Result<Self, Error> {
        limits.validate()?;
        let (sender, receiver) = mpsc::sync_channel::<Work>(limits.queue_capacity);
        let receiver = Arc::new(Mutex::new(receiver));
        let shared = Arc::new(Shared {
            limits: limits.clone(),
            closing: AtomicBool::new(false),
            remaining: AtomicUsize::new(2),
            active: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            drained: Notify::new(),
            #[cfg(test)]
            gate: Mutex::new(None),
        });
        for index in 0..2 {
            let shared_worker = shared.clone();
            let receiver = receiver.clone();
            if let Err(error) = thread::Builder::new()
                .name(format!("freeshow-exif-{index}"))
                .spawn(move || {
                    loop {
                        let work = receiver.lock().unwrap_or_else(|e| e.into_inner()).recv();
                        let Ok(work) = work else {
                            break;
                        };
                        shared_worker.queued.fetch_sub(1, Ordering::AcqRel);
                        shared_worker.active.fetch_add(1, Ordering::AcqRel);
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            run(&shared_worker, &work).and_then(|outcome| {
                                parser::finish(
                                    outcome,
                                    &shared_worker.limits,
                                    &work.control,
                                    &shared_worker.closing,
                                )
                            })
                        }))
                        .unwrap_or_else(|_| {
                            Err(Error::new("exif_internal", "EXIF worker panicked"))
                        });
                        shared_worker.active.fetch_sub(1, Ordering::AcqRel);
                        let _ = work.reply.send(result);
                    }
                    shared_worker.remaining.fetch_sub(1, Ordering::AcqRel);
                    shared_worker.drained.notify_waiters();
                })
            {
                shared.closing.store(true, Ordering::Release);
                drop(sender);
                return Err(Error::new("exif_start", error.to_string()));
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                shared,
                sender: Mutex::new(Some(sender)),
                slots: Arc::new(Semaphore::new(limits.queue_capacity + 2)),
                next: AtomicU64::new(0),
                controls: Mutex::new(HashMap::new()),
            }),
        })
    }
    pub async fn read(
        &self,
        id: String,
        context: Context,
        mut control: Control,
    ) -> Result<ExifOutcome, Error> {
        validate_input(&id, &context, &self.inner.shared.limits)?;
        control.deadline = control
            .deadline
            .min(Instant::now() + self.inner.shared.limits.max_job_duration);
        control.check(&self.inner.shared.closing)?;
        let _permit = self
            .inner
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::new("exif_busy", "EXIF worker admission is full"))?;
        // A child token lets dropping this one request cancel its worker without
        // cancelling a caller's shared library-context token.
        control.cancel = control.cancel.child_token();
        let key = self.inner.next.fetch_add(1, Ordering::Relaxed);
        self.inner
            .controls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, control.cancel.clone());
        let _pending = Pending {
            inner: self.inner.clone(),
            key,
            cancel: control.cancel.clone(),
        };
        let (reply, receive) = oneshot::channel();
        {
            let sender = self.inner.sender.lock().unwrap_or_else(|e| e.into_inner());
            control.check(&self.inner.shared.closing)?;
            self.inner.shared.queued.fetch_add(1, Ordering::AcqRel);
            let result = sender
                .as_ref()
                .ok_or_else(|| Error::new("exif_shutdown", "EXIF service is shut down"))?
                .try_send(Work {
                    id,
                    context,
                    control: control.clone(),
                    reply,
                });
            if let Err(error) = result {
                self.inner.shared.queued.fetch_sub(1, Ordering::AcqRel);
                return Err(match error {
                    mpsc::TrySendError::Full(_) => Error::new("exif_busy", "EXIF queue is full"),
                    mpsc::TrySendError::Disconnected(_) => {
                        Error::new("exif_shutdown", "EXIF workers stopped")
                    }
                });
            }
        }
        let result = tokio::select! {
            result=receive => result.map_err(|_|Error::new("exif_internal","EXIF worker reply disconnected"))?,
            _=control.cancel.cancelled() => return control.check(&self.inner.shared.closing).and(Err(Error::new("exif_cancelled","EXIF request cancelled"))),
            _=tokio::time::sleep_until(control.deadline.into()) => return control.check(&self.inner.shared.closing).and(Err(Error::new("exif_deadline","EXIF request deadline elapsed"))),
        };
        control.check(&self.inner.shared.closing)?;
        result
    }
    pub fn status(&self) -> Status {
        let shared = &self.inner.shared;
        Status {
            workers: shared.remaining.load(Ordering::Acquire),
            active: shared.active.load(Ordering::Acquire),
            queued: shared.queued.load(Ordering::Acquire),
            admitted: self
                .inner
                .controls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            closing: shared.closing.load(Ordering::Acquire),
        }
    }
    /// Closes admission and cancels queued/in-progress requests. A blocked kernel
    /// read cannot be force-killed: report timeout, retain the fixed worker count.
    pub async fn shutdown(&self) -> Result<(), Error> {
        self.inner.shared.closing.store(true, Ordering::Release);
        for token in self
            .inner
            .controls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            token.cancel();
        }
        self.inner
            .sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let wait = async {
            loop {
                let notified = self.inner.shared.drained.notified();
                if self.inner.shared.remaining.load(Ordering::Acquire) == 0 {
                    break;
                }
                notified.await;
            }
        };
        tokio::time::timeout(self.inner.shared.limits.shutdown_timeout, wait)
            .await
            .map_err(|_| {
                Error::new(
                    "exif_shutdown_timeout",
                    "EXIF OS workers did not drain before shutdown deadline",
                )
            })
    }
    #[cfg(test)]
    pub(crate) fn gate(&self, gate: Arc<crate::tests::Gate>) {
        *self.inner.shared.gate.lock().unwrap() = Some(gate);
    }
}
fn run(shared: &Shared, work: &Work) -> Result<ExifOutcome, Error> {
    work.control.check(&shared.closing)?;
    #[cfg(test)]
    {
        let gate = shared.gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.wait(&work.control, &shared.closing)?;
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    // O_NONBLOCK prevents FIFOs/devices from holding a worker before metadata
    // rejects non-regular files. Regular local-file reads retain normal semantics.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = match options.open(&work.id) {
        Ok(file) => file,
        Err(_) => {
            return Ok(missing(
                work.context.clone(),
                work.id.clone(),
                "exif_read",
                "Original file read failed",
                true,
                Stats {
                    allocated_bytes: base_allocation(&work.id, &work.context),
                    ..Stats::default()
                },
            ))
        }
    };
    work.control.check(&shared.closing)?;
    let metadata = match file.metadata() {
        Ok(m) => m,
        Err(_) => {
            return Ok(missing(
                work.context.clone(),
                work.id.clone(),
                "exif_read",
                "Original file metadata read failed",
                true,
                Stats {
                    allocated_bytes: base_allocation(&work.id, &work.context),
                    ..Stats::default()
                },
            ))
        }
    };
    if !metadata.is_file() {
        return Ok(if metadata.is_dir() {
            missing(
                work.context.clone(),
                work.id.clone(),
                "exif_read",
                "Original directory read failed",
                true,
                Stats {
                    allocated_bytes: base_allocation(&work.id, &work.context),
                    ..Stats::default()
                },
            )
        } else {
            missing(
                work.context.clone(),
                work.id.clone(),
                "exif_unsupported",
                "Non-regular EXIF inputs are unsupported",
                false,
                Stats {
                    allocated_bytes: base_allocation(&work.id, &work.context),
                    ..Stats::default()
                },
            )
        });
    }
    if metadata.len() > shared.limits.input_bytes as u64
        || metadata
            .len()
            .saturating_mul(2)
            .saturating_add(base_allocation(&work.id, &work.context) as u64)
            > shared.limits.allocation_bytes as u64
    {
        return Err(Error::new("exif_limit", "EXIF file exceeds input limit"));
    }
    let mut data = Vec::with_capacity((metadata.len() as usize).min(shared.limits.input_bytes));
    let mut chunk = [0u8; 16 * 1024];
    loop {
        work.control.check(&shared.closing)?;
        let count = match file.read(&mut chunk) {
            Ok(n) => n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                return Ok(missing(
                    work.context.clone(),
                    work.id.clone(),
                    "exif_read",
                    "Original file read failed",
                    true,
                    Stats {
                        allocated_bytes: base_allocation(&work.id, &work.context),
                        ..Stats::default()
                    },
                ))
            }
        };
        if count == 0 {
            break;
        }
        if count > shared.limits.input_bytes.saturating_sub(data.len())
            || (data.len() + count)
                .saturating_mul(2)
                .saturating_add(base_allocation(&work.id, &work.context))
                > shared.limits.allocation_bytes
        {
            return Err(Error::new(
                "exif_limit",
                "EXIF growing file exceeds input limit",
            ));
        }
        data.extend_from_slice(&chunk[..count]);
    }
    parser::parse(
        work.id.clone(),
        work.context.clone(),
        &data,
        &shared.limits,
        &work.control,
        &shared.closing,
    )
}
