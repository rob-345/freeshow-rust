use crate::*;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread::JoinHandle;
use tokio::sync::Notify;

/// The cleanup thread holds State, never Owner or a sender. In particular, its
/// final Work destructor cannot become responsible for joining its own thread.
struct State {
    failed: AtomicBool,
    live: AtomicUsize,
    disposed: AtomicUsize,
    changed: Notify,
    #[cfg(test)]
    pub(crate) before_remove: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    pub(crate) before_exit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
struct Queue {
    sender: Option<mpsc::SyncSender<Removal>>,
    submitted: usize,
    // Unexpected receiver failure never drops a TempDir on the releasing
    // caller or returns its permit while its filesystem disposal is pending.
    stranded: Vec<Removal>,
}
struct Owner {
    queue: Mutex<Queue>,
    thread: Mutex<Option<JoinHandle<()>>>,
    state: Arc<State>,
}
#[derive(Clone)]
pub(crate) struct CleanupSender(Arc<Owner>);

pub(crate) struct StagedDirectory {
    directory: Option<TempDir>,
    // Field order matters: actual TempDir disposal precedes live-stage release.
    _live: LiveStage,
}
impl StagedDirectory {
    pub(crate) fn path(&self) -> &Path {
        self.directory.as_ref().unwrap().path()
    }
}
struct LiveStage(Arc<State>);
impl Drop for LiveStage {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}
struct Disposed(Arc<State>);
impl Drop for Disposed {
    fn drop(&mut self) {
        self.0.disposed.fetch_add(1, Ordering::Release);
        self.0.changed.notify_waiters();
    }
}
struct Removal {
    stage: StagedDirectory,
    _lease: Option<OwnedSemaphorePermit>,
    // Last, including unwind: the ordinary barrier observes actual value and
    // permit disposal, rather than just entry to a cleanup operation.
    _disposed: Disposed,
}
struct PanicGuard(Arc<State>);
impl Drop for PanicGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.failed.store(true, Ordering::Release);
            self.0.changed.notify_waiters();
        }
    }
}
impl CleanupSender {
    pub(crate) fn new(capacity: usize) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Removal>(capacity);
        let state = Arc::new(State {
            failed: AtomicBool::new(false),
            live: AtomicUsize::new(0),
            disposed: AtomicUsize::new(0),
            changed: Notify::new(),
            #[cfg(test)]
            before_remove: Mutex::new(None),
            #[cfg(test)]
            before_exit: Mutex::new(None),
        });
        let worker = state.clone();
        let thread = std::thread::Builder::new()
            .name("freeshow-import-cleanup".into())
            .spawn(move || {
                let _panic = PanicGuard(worker.clone());
                while let Ok(mut removal) = receiver.recv() {
                    // Publish panic failure before the Removal's own destructors
                    // release the real stage and whole-request permit.
                    let _panic = PanicGuard(worker.clone());
                    #[cfg(test)]
                    if let Some(hook) = worker.before_remove.lock().unwrap().take() {
                        hook();
                    }
                    if removal.stage.directory.take().unwrap().close().is_err() {
                        worker.failed.store(true, Ordering::Release);
                    }
                    // Keep the frame intact through the actual call: even a
                    // panic inside TempDir::close must dispose stage, permit,
                    // then completion in that order.
                    drop(removal);
                }
                #[cfg(test)]
                if let Some(hook) = worker.before_exit.lock().unwrap().take() {
                    hook();
                }
            })
            .map_err(ImportError::io)?;
        Ok(Self(Arc::new(Owner {
            queue: Mutex::new(Queue {
                sender: Some(sender),
                submitted: 0,
                stranded: vec![],
            }),
            thread: Mutex::new(Some(thread)),
            state,
        })))
    }
    pub(crate) fn stage(&self, directory: TempDir) -> StagedDirectory {
        self.0.state.live.fetch_add(1, Ordering::AcqRel);
        StagedDirectory {
            directory: Some(directory),
            _live: LiveStage(self.0.state.clone()),
        }
    }
    pub(crate) fn failed(&self) -> bool {
        self.0.state.failed.load(Ordering::Acquire)
    }
    fn failure() -> ImportError {
        ImportError::new("cleanup_failure", "Import private staging cleanup failed")
    }
    pub(crate) fn remove(&self, stage: StagedDirectory, lease: Option<OwnedSemaphorePermit>) {
        let mut queue = self.0.queue.lock().unwrap_or_else(|e| e.into_inner());
        let removal = Removal {
            stage,
            _lease: lease,
            _disposed: Disposed(self.0.state.clone()),
        };
        // At most max_prepared stages, each carrying its original permit. No
        // observer messages compete for these bounded native queue slots.
        queue.submitted += 1;
        let result = match &queue.sender {
            Some(sender) => sender.try_send(removal),
            None => Err(mpsc::TrySendError::Disconnected(removal)),
        };
        if let Err(mpsc::TrySendError::Full(removal) | mpsc::TrySendError::Disconnected(removal)) =
            result
        {
            self.0.state.failed.store(true, Ordering::Release);
            queue.stranded.push(removal);
            self.0.state.changed.notify_waiters();
        }
    }
    /// Snapshot only released stages; never waits for a live prepared plan.
    pub(crate) async fn fence(&self) -> Result<()> {
        let target = self
            .0
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .submitted;
        loop {
            let changed = self.0.state.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.0.state.disposed.load(Ordering::Acquire) >= target {
                return if self.failed() {
                    Err(Self::failure())
                } else {
                    Ok(())
                };
            }
            if self.failed()
                && !self
                    .0
                    .queue
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .stranded
                    .is_empty()
            {
                // Receiver failure has retained work which this fixed worker
                // cannot dispose. Report failure, never a successful barrier.
                return Err(Self::failure());
            }
            require_runtime()?;
            changed.await;
        }
    }
    pub(crate) async fn join(&self) -> Result<()> {
        loop {
            let changed = self.0.state.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.0.state.live.load(Ordering::Acquire) == 0 {
                break;
            }
            if self.failed() {
                if self.finished() {
                    self.join_finished();
                }
                return Err(Self::failure());
            }
            require_runtime()?;
            changed.await;
        }
        // Preparation workers have already joined, so no new Stage can arise.
        // A Remove cannot lose its sender: live becomes zero only after physical
        // disposal, and the thread join also covers the remaining Work frame.
        self.0
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sender
            .take();
        while !self.finished() {
            pause().await?;
        }
        self.join_finished();
        if self.failed() {
            Err(Self::failure())
        } else {
            Ok(())
        }
    }
    fn join_finished(&self) {
        let mut thread = self.0.thread.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(thread) = thread.take() {
            if thread.join().is_err() {
                self.0.state.failed.store(true, Ordering::Release);
            }
        }
    }
    fn finished(&self) -> bool {
        self.0
            .thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_none_or(JoinHandle::is_finished)
    }
    #[cfg(test)]
    pub(crate) fn before_remove(&self, hook: Box<dyn FnOnce() + Send>) {
        *self.0.state.before_remove.lock().unwrap() = Some(hook);
    }
    #[cfg(test)]
    pub(crate) fn before_exit(&self, hook: Box<dyn FnOnce() + Send>) {
        *self.0.state.before_exit.lock().unwrap() = Some(hook);
    }
    #[cfg(test)]
    pub(crate) fn joined(&self) -> bool {
        self.0.thread.lock().unwrap().is_none()
    }
    #[cfg(test)]
    pub(crate) fn thread_finished(&self) -> bool {
        self.finished()
    }
    #[cfg(test)]
    pub(crate) fn live(&self) -> usize {
        self.0.state.live.load(Ordering::Acquire)
    }
}
pub(crate) fn require_runtime() -> Result<()> {
    if tokio::runtime::Handle::try_current().is_err() {
        Err(ImportError::new(
            "runtime_required",
            "Pending import shutdown requires a Tokio runtime",
        ))
    } else {
        Ok(())
    }
}
pub(crate) async fn pause() -> Result<()> {
    require_runtime()?;
    tokio::time::sleep(Duration::from_millis(1)).await;
    Ok(())
}
impl Drop for Owner {
    fn drop(&mut self) {
        // Last-resort native ownership, never a successful async drain receipt.
        // The cleanup worker owns no Owner, so this cannot join itself.
        self.queue
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .sender
            .take();
        if let Some(thread) = self
            .thread
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = thread.join();
        }
        // Unexpected worker failure retains bounded stranded stages and permits
        // until actual final disposal here; failure is sticky during service life.
    }
}
