//! Lazy, process-global AI_GET_BIN inventory. Native setup alone selects root.
use freeshow_ai_local::{CancelToken, Control, InventoryService, Limits, Ticket, TrustedUserData};
use freeshow_core::{
    protocol::{Event, NativeLifetime, Request, RuntimeContext},
    service::LibraryService,
    WireError,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct State {
    closed: bool,
    next: u64,
    active: HashMap<u64, (RuntimeContext, CancelToken)>,
    root: Option<TrustedUserData>,
    service: Option<InventoryService>,
    failure: Option<freeshow_ai_local::Error>,
}
#[derive(Clone)]
pub struct NativeInventory {
    state: Arc<Mutex<State>>,
    #[cfg(test)]
    after_begin: Arc<Mutex<Option<AfterBegin>>>,
}
#[cfg(test)]
struct AfterBegin {
    entered: tokio::sync::oneshot::Sender<CancelToken>,
    release: tokio::sync::oneshot::Receiver<()>,
}
struct Caller {
    id: u64,
    cancel: CancelToken,
    state: Arc<Mutex<State>>,
}
impl Drop for Caller {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .remove(&self.id);
    }
}
fn error(e: freeshow_ai_local::Error) -> WireError {
    WireError::new(e.code, e.message)
}
impl NativeInventory {
    pub fn new(root: PathBuf) -> Self {
        let (root, failure) = match TrustedUserData::from_host(root) {
            Ok(root) => (Some(root), None),
            Err(e) => (None, Some(e)),
        };
        Self {
            state: Arc::new(Mutex::new(State {
                closed: false,
                next: 0,
                active: HashMap::new(),
                root,
                service: None,
                failure,
            })),
            #[cfg(test)]
            after_begin: Arc::new(Mutex::new(None)),
        }
    }
    fn begin(&self, context: RuntimeContext) -> Result<(Ticket, Caller), WireError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Err(WireError::new(
                "ai_local_shutdown",
                "Local inventory admission is closed",
            ));
        }
        if let Some(e) = &state.failure {
            return Err(error(e.clone()));
        }
        if state.service.is_none() {
            let root = state.root.take().ok_or_else(|| {
                WireError::new("ai_local_worker", "Local inventory root is unavailable")
            })?;
            match InventoryService::start(root, Limits::default()) {
                Ok(service) => state.service = Some(service),
                Err(e) => {
                    state.failure = Some(e.clone());
                    return Err(error(e));
                }
            }
        }
        let id = state.next;
        state.next = id.checked_add(1).ok_or_else(|| {
            WireError::new("ai_local_worker", "Local inventory sequence exhausted")
        })?;
        let control = Control::default();
        let ticket = state
            .service
            .as_ref()
            .unwrap()
            .try_submit(control.clone())
            .map_err(error)?;
        state.active.insert(id, (context, control.cancel.clone()));
        Ok((
            ticket,
            Caller {
                id,
                cancel: control.cancel,
                state: self.state.clone(),
            },
        ))
    }
    pub fn seal(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        for (_, cancel) in state.active.values() {
            cancel.cancel();
        }
    }
    /// Cancel stale observers; the process-global filesystem owner/root survives.
    pub fn context_changed(&self, current: &RuntimeContext) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        for (context, cancel) in state.active.values() {
            if context != current {
                cancel.cancel();
            }
        }
    }
    pub async fn shutdown(&self) -> Result<(), WireError> {
        self.seal();
        let service = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .service
            .clone();
        match service {
            Some(service) => service.shutdown().await.map_err(error),
            None => Ok(()),
        }
    }
    pub async fn dispatch(
        &self,
        library: &LibraryService,
        mut request: Request,
        window: &str,
    ) -> Result<Vec<Event>, WireError> {
        if window != "main" {
            return Err(WireError::new(
                "ai_local_role",
                "Only the main window may request local inventory",
            ));
        }
        if request.channel != "MAIN" || request.data["channel"] != "AI_GET_BIN" {
            return Err(WireError::new(
                "unsupported_command",
                "Unknown local inventory command",
            ));
        }
        library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        if request
            .listener_id
            .as_ref()
            .is_some_and(|id| id.len() > 8192)
        {
            return Err(WireError::new(
                "ai_local_limit",
                "Local inventory listener exceeds bounds",
            ));
        }
        request.data = serde_json::json!({"channel":"AI_GET_BIN"});
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(WireError::new(
                "ai_local_runtime",
                "Local inventory reply requires the native async runtime",
            ));
        }
        let (ticket, _caller) = self.begin(request.context.clone().unwrap())?;
        #[cfg(test)]
        {
            // Hold the actual admitted Ticket/Caller, without replacing the
            // filesystem query or requiring a per-request production task.
            let gate = self.after_begin.lock().unwrap().take();
            if let Some(gate) = gate {
                let _ = gate.entered.send(_caller.cancel.clone());
                let _ = gate.release.await;
            }
        }
        let read = ticket.finish().await.map_err(error)?;
        let rows = read.result().map_err(|e| error(e.clone()))?;
        let data = serde_json::to_value(rows)
            .map_err(|_| WireError::new("ai_local_worker", "Unable to encode local inventory"))?;
        let mut event = Event::reply("MAIN", "AI_GET_BIN", data, request.listener_id.clone());
        // Rust-only lifetime, skipped by serde. Clones retain the same native
        // Read through actual event encoding/disposal, without a source ACK.
        event.native_lifetime = Some(NativeLifetime::new(read));
        let mut events = vec![event];
        library
            .contextualize(&request, &mut events)
            .map_err(WireError::from)?;
        Ok(events)
    }
    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the isolated native integration tests")]
    pub fn test_hold_after_begin(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<CancelToken>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, observed) = tokio::sync::oneshot::channel();
        let (release, held) = tokio::sync::oneshot::channel();
        let mut slot = self.after_begin.lock().unwrap();
        assert!(
            slot.is_none(),
            "Only one post-admission test gate may be set"
        );
        *slot = Some(AfterBegin {
            entered,
            release: held,
        });
        (observed, release)
    }
    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the isolated native integration tests")]
    pub fn test_registered(&self) -> usize {
        self.state.lock().unwrap().active.len()
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn status(&self) -> Option<freeshow_ai_local::Status> {
        self.state
            .lock()
            .unwrap()
            .service
            .as_ref()
            .map(InventoryService::status)
    }
}
