use crate::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
static OWNER: AtomicBool = AtomicBool::new(false);
struct Shared {
    closed: AtomicBool,
    done: AtomicBool,
    joined: AtomicBool,
    notify: Notify,
    join_notify: Notify,
    ready: Notify,
    capability: Mutex<Option<Capability>>,
    terminal: Mutex<Option<Error>>,
}
impl Shared {
    fn fail(&self, e: Error) {
        let mut first = self.terminal.lock().unwrap_or_else(|e| e.into_inner());
        if first.is_none() {
            *first = Some(e);
        }
        self.closed.store(true, Ordering::Release);
    }
    fn error(&self) -> Option<Error> {
        self.terminal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}
struct Exit(Arc<Shared>);
impl Drop for Exit {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0
                .fail(Error::new("owner_panicked", "Opus CPU owner panicked"));
        }
        self.0.closed.store(true, Ordering::Release);
        self.0.done.store(true, Ordering::Release);
        OWNER.store(false, Ordering::Release);
        self.0.notify.notify_waiters();
        self.0.ready.notify_waiters();
    }
}
struct Work {
    pcm: Box<[u8; PCM_BYTES]>,
    reply: oneshot::Sender<Result<EncodedPacket>>,
    lease: OwnedSemaphorePermit,
}
/// A native encoded packet, with its retained-work permit. Not Clone/Deserialize.
pub struct EncodedPacket {
    bytes: Vec<u8>,
    context: ContextId,
    sequence: u64,
    shared: Arc<Shared>,
    _lease: OwnedSemaphorePermit,
}
impl std::fmt::Debug for EncodedPacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncodedPacket")
            .field("context", &self.context)
            .field("sequence", &self.sequence)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}
impl EncodedPacket {
    pub fn context(&self) -> ContextId {
        self.context
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn ensure_current(&self, context: ContextId) -> Result<()> {
        if context != self.context {
            return Err(Error::new(
                "stale_context",
                "Foreign encoded packet context",
            ));
        }
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(self
                .shared
                .error()
                .unwrap_or_else(|| Error::new("shutdown", "Encoded packet context retired")));
        }
        Ok(())
    }
    pub fn bytes(&self, context: ContextId) -> Result<&[u8]> {
        self.ensure_current(context)?;
        Ok(&self.bytes)
    }
}
pub struct OpusService {
    context: ContextId,
    sender: Mutex<Option<mpsc::Sender<Work>>>,
    retained: Arc<Semaphore>,
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
type Factory = Box<dyn FnOnce() -> Result<Box<dyn Codec>> + Send>;
impl OpusService {
    pub fn start(context: ContextId, source: LibrarySource, limits: Limits) -> Result<Self> {
        Self::start_with(
            context,
            limits,
            Box::new(move || native::Encoder::open(source).map(|e| Box::new(e) as Box<dyn Codec>)),
        )
    }
    fn start_with(context: ContextId, limits: Limits, factory: Factory) -> Result<Self> {
        limits.validate()?;
        if OWNER
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Error::new("owner_exists", "Opus has one global CPU owner"));
        }
        let (sender, receiver) = mpsc::channel(limits.queue);
        let shared = Arc::new(Shared {
            closed: AtomicBool::new(false),
            done: AtomicBool::new(false),
            joined: AtomicBool::new(false),
            notify: Notify::new(),
            join_notify: Notify::new(),
            ready: Notify::new(),
            capability: Mutex::new(None),
            terminal: Mutex::new(None),
        });
        let worker = shared.clone();
        let budget = limits.call_budget;
        let thread = std::thread::Builder::new()
            .name("freeshow-opus".into())
            .spawn(move || {
                let _exit = Exit(worker.clone());
                let codec = std::panic::catch_unwind(std::panic::AssertUnwindSafe(factory))
                    .unwrap_or_else(|_| {
                        let error = Error::new("owner_panicked", "Opus initialization panicked");
                        worker.fail(error.clone());
                        Err(error)
                    });
                let capability = match &codec {
                    Ok(c) => Capability::Available(c.info()),
                    Err(e) => Capability::Unavailable(e.clone()),
                };
                *worker.capability.lock().unwrap_or_else(|e| e.into_inner()) = Some(capability);
                worker.ready.notify_waiters();
                run(context, budget, worker, receiver, codec);
            })
            .map_err(|e| {
                OWNER.store(false, Ordering::Release);
                Error::new("worker_unavailable", e)
            })?;
        Ok(Self {
            context,
            sender: Mutex::new(Some(sender)),
            retained: Arc::new(Semaphore::new(limits.retained)),
            shared,
            thread: Mutex::new(Some(thread)),
        })
    }
    pub async fn capability(&self) -> Capability {
        loop {
            let ready = self.shared.ready.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            if self.shared.closed.load(Ordering::Acquire) {
                return Capability::Unavailable(
                    self.terminal_error()
                        .unwrap_or_else(|| Error::new("shutdown", "Opus owner closing")),
                );
            }
            if let Some(capability) = self
                .shared
                .capability
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                return capability;
            }
            if self.shared.done.load(Ordering::Acquire) {
                return Capability::Unavailable(self.terminal_error().unwrap_or_else(|| {
                    Error::new("owner_unavailable", "Opus owner exited before capability")
                }));
            }
            ready.await;
        }
    }
    fn check(&self, context: ContextId, cancel: &CancellationToken) -> Result<()> {
        if context != self.context {
            return Err(Error::new("stale_context", "Foreign Opus context"));
        }
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(self
                .terminal_error()
                .unwrap_or_else(|| Error::new("shutdown", "Opus owner closing")));
        }
        if cancel.is_cancelled() {
            return Err(Error::new("cancelled", "Opus cancelled before admission"));
        }
        Ok(())
    }
    pub async fn encode(
        &self,
        context: ContextId,
        pcm: &[u8],
        cancel: CancellationToken,
    ) -> Result<EncodedPacket> {
        self.check(context, &cancel)?;
        if pcm.len() != PCM_BYTES {
            return Err(Error::new(
                "unsupported_frame",
                "Require 3840-byte interleaved PCM16 LE stereo frame",
            ));
        }
        let receiver = {
            // Shutdown closes the only sender under this same short admission lock.
            // No native call or blocking I/O occurs while it is held.
            let sender = self.sender.lock().unwrap_or_else(|e| e.into_inner());
            self.check(context, &cancel)?;
            let sender = sender
                .as_ref()
                .ok_or_else(|| Error::new("shutdown", "Opus queue closed"))?;
            let lease = self
                .retained
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::new("busy", "Opus retained-work quota full"))?;
            let slot = sender.try_reserve().map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => Error::new("busy", "Opus command queue full"),
                mpsc::error::TrySendError::Closed(_) => self
                    .terminal_error()
                    .unwrap_or_else(|| Error::new("shutdown", "Opus queue closed")),
            })?;
            self.check(context, &cancel)?;
            let mut bytes = Box::new([0u8; PCM_BYTES]);
            bytes.copy_from_slice(pcm);
            let (reply, receiver) = oneshot::channel();
            slot.send(Work {
                pcm: bytes,
                reply,
                lease,
            });
            receiver
        };
        receiver.await.map_err(|_| {
            self.terminal_error().unwrap_or_else(|| {
                Error::new("owner_unavailable", "Opus owner stopped without result")
            })
        })?
    }
    pub fn terminal_error(&self) -> Option<Error> {
        self.shared.error()
    }
    fn close(&self) {
        let mut sender = self.sender.lock().unwrap_or_else(|e| e.into_inner());
        self.shared.closed.store(true, Ordering::Release);
        sender.take();
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.close();
        loop {
            let notified = self.shared.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.shared.done.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
        let thread = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        // Exit signals immediately before thread scope exit. Joining can still wait
        // for final TLS/destructors; no claim of preemptible native execution.
        if let Some(thread) = thread {
            if thread.join().is_err() {
                self.shared
                    .fail(Error::new("owner_panicked", "Opus owner join failed"));
            }
            self.shared.joined.store(true, Ordering::Release);
            self.shared.join_notify.notify_waiters();
        } else {
            loop {
                let joined = self.shared.join_notify.notified();
                tokio::pin!(joined);
                joined.as_mut().enable();
                if self.shared.joined.load(Ordering::Acquire) {
                    break;
                }
                joined.await;
            }
        }
        self.terminal_error().map_or(Ok(()), Err)
    }
    pub async fn retire(&self, context: ContextId) -> Result<()> {
        if context != self.context {
            return Err(Error::new("stale_context", "Foreign Opus retirement"));
        }
        self.shutdown().await
    }
}
impl Drop for OpusService {
    fn drop(&mut self) {
        self.close();
    }
}
fn run(
    context: ContextId,
    budget: Duration,
    shared: Arc<Shared>,
    mut receiver: mpsc::Receiver<Work>,
    mut codec: Result<Box<dyn Codec>>,
) {
    let mut sequence = 0u64;
    while let Some(work) = receiver.blocking_recv() {
        if let Some(error) = shared.error() {
            let _ = work.reply.send(Err(error));
            continue;
        }
        let result = match codec.as_mut() {
            Err(e) => Err(e.clone()),
            Ok(codec) => {
                let start = Instant::now();
                let output = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    codec.encode(&work.pcm)
                }))
                .unwrap_or_else(|_| {
                    let error = Error::new("owner_panicked", "Opus encode panicked");
                    shared.fail(error.clone());
                    receiver.close();
                    Err(shared.error().unwrap_or(error))
                });
                // Fixed native C execution cannot be cancelled/preempted. A late result
                // fences the owner after return; never reset/reuse unknown partial state.
                if start.elapsed() > budget {
                    let error = Error::new(
                        "codec_deadline",
                        "Native encode exceeded cooperative call budget",
                    );
                    shared.fail(error.clone());
                    receiver.close();
                    Err(shared.error().unwrap_or(error))
                } else {
                    sequence = match sequence.checked_add(1) {
                        Some(n) => n,
                        None => {
                            let error = Error::new("sequence_overflow", "Opus sequence exhausted");
                            shared.fail(error.clone());
                            receiver.close();
                            let _ = work.reply.send(Err(error));
                            continue;
                        }
                    };
                    output.and_then(|bytes| {
                        if bytes.is_empty() || bytes.len() > MAX_PACKET {
                            let error = Error::new(
                                "codec_contract",
                                "Encoded packet outside source bounds",
                            );
                            shared.fail(error.clone());
                            receiver.close();
                            return Err(error);
                        }
                        Ok(EncodedPacket {
                            bytes,
                            context,
                            sequence,
                            shared: shared.clone(),
                            _lease: work.lease,
                        })
                    })
                }
            }
        };
        let _ = work.reply.send(result);
    }
    // Drop initialized codec/library on its CPU owner before signaling terminal exit.
    if let Ok(codec) = codec {
        let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| codec.dispose()))
            .unwrap_or_else(|_| Err(Error::new("owner_panicked", "Opus dispose panicked")));
        if let Err(error) = cleanup {
            shared.fail(error);
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
