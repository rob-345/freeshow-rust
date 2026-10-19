//! One lazy process-lifetime Spotify owner; real request context stays separate.
use freeshow_core::{
    protocol::{Event, NativeLifetime, Request, RuntimeContext},
    service::LibraryService,
    WireError,
};
use freeshow_spotify::{Backend, Limits, Owner, SpotifyService, Ticket};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
struct State {
    closed: bool,
    next: u64,
    active: HashMap<u64, (RuntimeContext, CancellationToken)>,
    owner: Option<Owner>,
    backend: Option<Backend>,
    service: Option<SpotifyService>,
    failure: Option<freeshow_spotify::Error>,
}
#[derive(Clone)]
pub struct NativeSpotify {
    state: Arc<Mutex<State>>,
}
struct Caller {
    id: u64,
    cancel: CancellationToken,
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
fn error(e: freeshow_spotify::Error) -> WireError {
    WireError::new(e.code, e.message)
}
fn native_backend() -> freeshow_spotify::Result<Backend> {
    #[cfg(target_os = "linux")]
    {
        Backend::capture(std::path::Path::new("/usr/bin/dbus-send"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(freeshow_spotify::Error {
            code: "spotify_unsupported",
            message: "Native desktop Spotify state is certified only on Linux",
        })
    }
}
impl NativeSpotify {
    pub fn new(initial: &RuntimeContext) -> Self {
        let setup = (|| {
            let owner = Owner::new(
                &initial.runtime_id,
                initial.library_generation,
                &initial.data_path,
            )?;
            let backend = native_backend()?;
            Ok((owner, backend))
        })();
        Self::setup(setup)
    }
    fn setup(setup: freeshow_spotify::Result<(Owner, Backend)>) -> Self {
        let (owner, backend, failure) = match setup {
            Ok((owner, backend)) => (Some(owner), Some(backend), None),
            Err(e) => (None, None, Some(e)),
        };
        Self {
            state: Arc::new(Mutex::new(State {
                closed: false,
                next: 0,
                active: HashMap::new(),
                owner,
                backend,
                service: None,
                failure,
            })),
        }
    }
    fn begin(
        &self,
        context: RuntimeContext,
        deadline: Instant,
    ) -> Result<(Ticket, Caller, Owner), WireError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Err(WireError::new(
                "spotify_shutdown",
                "Desktop Spotify admission is closed",
            ));
        }
        if let Some(e) = &state.failure {
            return Err(error(e.clone()));
        }
        let owner = state.owner.clone().ok_or_else(|| {
            WireError::new("spotify_owner", "Native Spotify identity is unavailable")
        })?;
        if state.service.is_none() {
            let backend = state.backend.clone().ok_or_else(|| {
                WireError::new("spotify_owner", "Native Spotify backend is unavailable")
            })?;
            match SpotifyService::start(owner.clone(), backend, Limits::default()) {
                Ok(service) => state.service = Some(service),
                // A still-owned prior service is retryable after its real join.
                Err(e) => return Err(error(e)),
            }
        }
        let id = state.next;
        state.next = id
            .checked_add(1)
            .ok_or_else(|| WireError::new("spotify_owner", "Native Spotify sequence exhausted"))?;
        let ticket = state
            .service
            .as_ref()
            .unwrap()
            .begin_state(&owner, deadline)
            .map_err(error)?;
        let cancel = CancellationToken::new();
        state.active.insert(id, (context, cancel.clone()));
        Ok((
            ticket,
            Caller {
                id,
                cancel,
                state: self.state.clone(),
            },
            owner,
        ))
    }
    pub fn seal(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        for (_, cancel) in state.active.values() {
            cancel.cancel();
        }
        if let Some(service) = &state.service {
            service.close();
        }
    }
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
        if let Some(service) = service {
            if tokio::runtime::Handle::try_current().is_err() {
                return service.shutdown().await.map_err(error);
            }
            tokio::time::timeout(Duration::from_secs(2), service.shutdown())
                .await
                .map_err(|_| {
                    WireError::new(
                        "spotify_shutdown_timeout",
                        "Native Spotify owner remains retained; shutdown can be retried",
                    )
                })?
                .map_err(error)?;
        }
        Ok(())
    }
    pub async fn dispatch(
        &self,
        library: &LibraryService,
        mut request: Request,
        window: &str,
    ) -> Result<Vec<Event>, WireError> {
        if window != "main" {
            return Err(WireError::new(
                "spotify_role",
                "Only the main window may request desktop Spotify state",
            ));
        }
        if request.channel != "MAIN" || request.data["channel"] != "SPOTIFY_GET_STATE" {
            return Err(WireError::new(
                "unsupported_command",
                "Unknown desktop Spotify command",
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
                "spotify_resource_limit",
                "Desktop Spotify listener exceeds bounds",
            ));
        }
        request.data = serde_json::json!({"channel":"SPOTIFY_GET_STATE"});
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(WireError::new(
                "spotify_runtime",
                "Desktop Spotify reply requires the native async runtime",
            ));
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let (ticket, caller, owner) = self.begin(request.context.clone().unwrap(), deadline)?;
        let receipt = tokio::select! {
            _=caller.cancel.cancelled()=>return Err(WireError::new("spotify_cancelled","Desktop Spotify observer cancelled; accepted work remains owned")),
            reply=tokio::time::timeout_at(deadline.into(),ticket.finish())=>reply.map_err(|_|WireError::new("spotify_deadline","Desktop Spotify observer deadline elapsed; accepted work remains owned"))?.map_err(error)?,
        };
        if caller.cancel.is_cancelled() {
            return Err(WireError::new(
                "spotify_cancelled",
                "Desktop Spotify observer cancelled",
            ));
        }
        if Instant::now() >= deadline {
            return Err(WireError::new(
                "spotify_deadline",
                "Desktop Spotify observer deadline elapsed",
            ));
        }
        let wire = receipt.reply(&owner).map_err(error)?.wire();
        let mut event = Event::reply(
            "MAIN",
            "SPOTIFY_GET_STATE",
            wire.data,
            request.listener_id.clone(),
        );
        event.native_spotify = Some(
            serde_json::json!({"undefinedFields":wire.undefined_fields,"specialNumbers":wire.special_numbers}),
        );
        event.native_lifetime = Some(NativeLifetime::new(receipt));
        let mut events = vec![event];
        library
            .contextualize(&request, &mut events)
            .map_err(WireError::from)?;
        Ok(events)
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn status(&self) -> Option<freeshow_spotify::Status> {
        self.state
            .lock()
            .unwrap()
            .service
            .as_ref()
            .map(SpotifyService::status)
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn with_backend(initial: &RuntimeContext, backend: Backend) -> Self {
        Self::setup(
            Owner::new(
                &initial.runtime_id,
                initial.library_generation,
                &initial.data_path,
            )
            .map(|owner| (owner, backend)),
        )
    }
}
