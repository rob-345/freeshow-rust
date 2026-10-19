//! One replaceable context-scoped host. Native callbacks live only inside the
//! actual supervisor; this registry never retains a Tauri application handle.
use crate::now_playing::{
    Callbacks, Command, Config, Dependencies, HostError, NativeNowPlaying, OperationBudget, Ticket,
};
use freeshow_core::protocol::RuntimeContext;
use serde_json::{json, Value};
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

struct State {
    ready: RuntimeContext,
    host: Option<(u64, RuntimeContext, NativeNowPlaying)>,
    next: u64,
    closed: bool,
    last_retired: Option<Value>,
    terminal_error: Option<HostError>,
}
pub struct Owner {
    dependencies: Dependencies,
    state: Mutex<State>,
    budget: OperationBudget,
    config: Config,
    pub changes: tokio::sync::Mutex<()>,
    pub observers: tokio::sync::Semaphore,
}
impl Owner {
    pub fn new(dependencies: Dependencies) -> Result<Self, HostError> {
        Self::with_config(dependencies, Config::default())
    }
    pub fn with_config(dependencies: Dependencies, config: Config) -> Result<Self, HostError> {
        let ready = dependencies
            .library
            .context()
            .map_err(|e| HostError::new("now_playing_context", e))?;
        Ok(Self {
            dependencies,
            state: Mutex::new(State {
                ready,
                host: None,
                next: 1,
                closed: false,
                last_retired: None,
                terminal_error: None,
            }),
            budget: OperationBudget::default(),
            config,
            changes: tokio::sync::Mutex::new(()),
            observers: tokio::sync::Semaphore::new(4),
        })
    }
    pub fn ready(&self, context: &RuntimeContext) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).ready == *context
    }
    pub fn submit(
        &self,
        context: &RuntimeContext,
        command: Command,
        cancel: CancellationToken,
        callbacks: Callbacks,
    ) -> Result<Ticket, HostError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Err(HostError::new(
                "now_playing_shutdown",
                "Native now-playing admission is closed",
            ));
        }
        if state.ready != *context
            || self
                .dependencies
                .library
                .context()
                .map_err(|e| HostError::new("now_playing_context", e))?
                != *context
        {
            return Err(HostError::new(
                "now_playing_context",
                "Shared dependencies have not completed this library transition",
            ));
        }
        if state.host.is_none() {
            let id = state.next;
            state.next = id
                .checked_add(1)
                .ok_or_else(|| HostError::new("now_playing_limit", "Host identity exhausted"))?;
            let host = NativeNowPlaying::start_with_budget(
                self.dependencies.clone(),
                self.config.clone(),
                callbacks,
                self.budget.clone(),
            )?;
            state.host = Some((id, context.clone(), host));
        }
        state
            .host
            .as_ref()
            .unwrap()
            .2
            .submit(context, command, cancel)
    }
    pub fn complete(
        &self,
        context: &RuntimeContext,
        id: &str,
        accepted: bool,
    ) -> Result<(), HostError> {
        let host = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .host
            .clone();
        let Some((_, captured, host)) = host else {
            return Err(HostError::new(
                "now_playing_completion",
                "No live now-playing delivery owner",
            ));
        };
        if captured != *context {
            return Err(HostError::new(
                "now_playing_completion",
                "Foreign now-playing delivery context",
            ));
        }
        host.complete(id, context, accepted)
    }
    /// An error after an actual joined drain permits dependency teardown; a
    /// timeout keeps the same host and every dependency for a later retry.
    pub async fn retire(
        &self,
        context: Option<&RuntimeContext>,
        terminal: bool,
    ) -> (bool, Result<(), HostError>) {
        let host = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if terminal {
                state.closed = true;
            }
            match &state.host {
                Some((_, captured, _))
                    if context.is_some_and(|context| context != captured) && !terminal =>
                {
                    return (
                        false,
                        Err(HostError::new(
                            "now_playing_context",
                            "Foreign now-playing retirement context",
                        )),
                    )
                }
                _ => state.host.clone(),
            }
        };
        let Some((id, _, host)) = host else {
            let error = terminal
                .then(|| {
                    self.state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .terminal_error
                        .clone()
                })
                .flatten();
            return (true, error.map_or(Ok(()), Err));
        };
        let result = if terminal {
            host.shutdown().await
        } else {
            host.context_changed().await
        };
        let status = host.status();
        let drained = status["drained"] == true;
        if drained {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if terminal && state.terminal_error.is_none() {
                state.terminal_error = result.as_ref().err().cloned();
            }
            if state
                .host
                .as_ref()
                .is_some_and(|(current, _, _)| *current == id)
            {
                state.host = None;
                state.last_retired = Some(status);
            }
        }
        (drained, result)
    }
    pub fn context_ready(&self, context: RuntimeContext) -> Result<(), HostError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.host.is_some()
            || self
                .dependencies
                .library
                .context()
                .map_err(|e| HostError::new("now_playing_context", e))?
                != context
        {
            return Err(HostError::new(
                "now_playing_context",
                "Library transition changed before dependency retirement completed",
            ));
        }
        state.ready = context;
        Ok(())
    }
    pub fn status(&self) -> Value {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        json!({"readyContext":state.ready,"closed":state.closed,"host":state.host.as_ref().map(|(_, _, host)|host.status()),"lastRetired":state.last_retired,"terminalError":state.terminal_error,"retained":self.budget.retained(),"contextObservers":4-self.observers.available_permits()})
    }
}
