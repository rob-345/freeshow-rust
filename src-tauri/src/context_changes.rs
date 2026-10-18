//! Accepted root changes retain their post-commit cleanup after IPC cancellation.
use freeshow_core::{protocol::Event, WireError};
use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::sync::{oneshot, Semaphore};

const CAPACITY: usize = 4;
#[derive(Clone)]
pub struct ContextChanges {
    slots: Arc<Semaphore>,
    closing: Arc<AtomicBool>,
}
impl ContextChanges {
    pub fn new() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(CAPACITY)),
            closing: Arc::new(AtomicBool::new(false)),
        }
    }
    pub async fn run(
        &self,
        continuation: impl Future<Output = Result<Vec<Event>, WireError>> + Send + 'static,
    ) -> Result<Vec<Event>, WireError> {
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new("context_change_busy", "Library changes are at capacity")
        })?;
        if self.closing.load(Ordering::Acquire) {
            return Err(WireError::new(
                "context_change_shutdown",
                "Library changes are closed",
            ));
        }
        let (reply, response) = oneshot::channel();
        // No await separates admission and ownership transfer. Dropping the
        // receiving IPC future cannot drop this accepted continuation.
        tokio::spawn(async move {
            let result = continuation.await;
            if let Err(Err(error)) = reply.send(result) {
                eprintln!("Detached library change failed: {}", error.message);
            }
            drop(permit);
        });
        response.await.map_err(|_| {
            WireError::new(
                "context_change_worker",
                "Library-change continuation stopped",
            )
        })?
    }
    pub async fn close_and_drain(&self) -> Result<(), WireError> {
        self.closing.store(true, Ordering::Release);
        let _drained = self
            .slots
            .clone()
            .acquire_many_owned(CAPACITY as u32)
            .await
            .map_err(|_| WireError::new("context_change_worker", "Library-change drain failed"))?;
        Ok(())
    }
}
