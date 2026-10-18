//! Bounded native API lifecycle and generation-tagged renderer routing.
use freeshow_api::{ApiConfig, ApiEvent, ApiEventKind, ApiGeneration, ApiService, BoundEndpoints};
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::LibraryService,
    WireError,
};
use serde_json::{json, Value};
use std::{
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::{
    mpsc, oneshot, watch, Mutex as AsyncMutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore,
};

pub const COMMANDS: &[&str] = &[
    "WEBSOCKET_START",
    "WEBSOCKET_STOP",
    "API_TRIGGER",
    "GET_DYNAMIC_VALUES",
];
const MAX_REQUESTS: usize = 32;

#[derive(Clone)]
struct Binding {
    context: RuntimeContext,
    endpoints: BoundEndpoints,
    options: Value,
}

enum Job {
    #[cfg(test)]
    Pause {
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    },
    Dispatch {
        request: Request,
        reply: oneshot::Sender<Result<Vec<Event>, WireError>>,
        permit: OwnedSemaphorePermit,
    },
    Context {
        reply: oneshot::Sender<Result<(), WireError>>,
        _guard: OwnedMutexGuard<()>,
    },
    Shutdown {
        _guard: OwnedMutexGuard<()>,
    },
}

struct Gate {
    sender: mpsc::Sender<Job>,
    shutdown_requested: bool,
}

#[derive(Clone)]
enum Completion {
    Pending,
    Finished(Option<(&'static str, String)>),
}

#[derive(Clone)]
pub struct NativeApi {
    gate: Arc<Mutex<Gate>>,
    slots: Arc<Semaphore>,
    control: Arc<AsyncMutex<()>>,
    closing: Arc<AtomicBool>,
    binding: Arc<Mutex<Option<Binding>>>,
    done: watch::Receiver<Completion>,
    pump_done: watch::Receiver<bool>,
}

fn service_error(error: freeshow_api::ApiError) -> WireError {
    WireError::new(error.code, error.message)
}

impl NativeApi {
    /// Call inside the native Tokio runtime. Exactly one lifecycle task and one
    /// bounded event pump are created; network tasks belong to ApiService.
    pub fn new(
        library: LibraryService,
        config: ApiConfig,
        sink: impl Fn(Event) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<Self, WireError> {
        let (service, events) = ApiService::new(config).map_err(service_error)?;
        // Public leases include active work. One extra slot is reserved for the
        // single host control job, whose mutex guard travels with the job.
        let (sender, receive) = mpsc::channel(MAX_REQUESTS + 1);
        let (complete, done) = watch::channel(Completion::Pending);
        let (pump_complete, pump_done) = watch::channel(false);
        let binding = Arc::new(Mutex::new(None));
        let closing = Arc::new(AtomicBool::new(false));
        let publication = Arc::new(AsyncMutex::new(()));
        tokio::spawn(owner(
            library.clone(),
            service,
            binding.clone(),
            publication.clone(),
            receive,
            complete,
        ));
        tokio::spawn(pump(
            library,
            binding.clone(),
            publication,
            closing.clone(),
            events,
            sink,
            pump_complete,
        ));
        Ok(Self {
            gate: Arc::new(Mutex::new(Gate {
                sender,
                shutdown_requested: false,
            })),
            slots: Arc::new(Semaphore::new(MAX_REQUESTS)),
            control: Arc::new(AsyncMutex::new(())),
            closing,
            binding,
            done,
            pump_done,
        })
    }

    pub fn status(&self) -> Value {
        let binding = self
            .binding
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        json!({
            "closing":self.closing.load(Ordering::Acquire),
            "admittedRequests":MAX_REQUESTS-self.slots.available_permits(),
            "binding":binding.as_ref().map(|binding|json!({"context":binding.context,"endpoints":binding.endpoints}))
        })
    }

    pub async fn dispatch(&self, request: Request, window: &str) -> Result<Vec<Event>, WireError> {
        if window != "main" {
            return Err(WireError::new(
                "window_error",
                "Only the main window handles API lifecycle and replies",
            ));
        }
        let command = request.data["channel"].as_str().unwrap_or("");
        if !COMMANDS.contains(&command) {
            return Err(WireError::new("unsupported_command", "Unknown API command"));
        }
        if request
            .listener_id
            .as_ref()
            .is_some_and(|id| id.len() > 256)
        {
            return Err(WireError::new(
                "api_input_limit",
                "API correlation ID exceeds its native byte limit",
            ));
        }
        let limit = if command == "WEBSOCKET_START" {
            16 * 1024
        } else {
            1024 * 1024
        };
        if !within_limit(&request.data, limit) {
            return Err(WireError::new(
                "api_input_limit",
                "API command payload exceeds its native byte limit",
            ));
        }
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new(
                if self.closing.load(Ordering::Acquire) {
                    "api_shutdown"
                } else {
                    "api_busy"
                },
                "Native API requests are closed or at capacity",
            )
        })?;
        let (reply, response) = oneshot::channel();
        {
            let gate = self
                .gate
                .lock()
                .map_err(|_| WireError::new("api_worker", "API admission lock poisoned"))?;
            if self.closing.load(Ordering::Acquire) {
                return Err(WireError::new(
                    "api_shutdown",
                    "Native API admission is closed",
                ));
            }
            gate.sender
                .try_send(Job::Dispatch {
                    request,
                    reply,
                    permit,
                })
                .map_err(|_| WireError::new("api_worker", "API lifecycle owner is unavailable"))?;
        }
        response
            .await
            .map_err(|_| WireError::new("api_worker", "API owner stopped before replying"))?
    }

    /// A live listener automatically restarts for the newly committed context.
    /// The private slot cannot be starved by 32 public lifecycle requests.
    pub async fn context_changed(&self) -> Result<(), WireError> {
        let guard = self.control.clone().lock_owned().await;
        let (reply, response) = oneshot::channel();
        {
            let gate = self
                .gate
                .lock()
                .map_err(|_| WireError::new("api_worker", "API admission lock poisoned"))?;
            if self.closing.load(Ordering::Acquire) {
                return Err(WireError::new(
                    "api_shutdown",
                    "Native API admission is closed",
                ));
            }
            gate.sender
                .try_send(Job::Context {
                    reply,
                    _guard: guard,
                })
                .map_err(|_| WireError::new("api_worker", "API control slot is unavailable"))?;
        }
        response.await.map_err(|_| {
            WireError::new("api_worker", "API owner stopped before refreshing context")
        })?
    }

    pub async fn shutdown(&self) -> Result<(), WireError> {
        {
            let _gate = self
                .gate
                .lock()
                .map_err(|_| WireError::new("api_worker", "API admission lock poisoned"))?;
            self.closing.store(true, Ordering::Release);
            self.slots.close();
        }
        let guard = self.control.clone().lock_owned().await;
        {
            let mut gate = self
                .gate
                .lock()
                .map_err(|_| WireError::new("api_worker", "API admission lock poisoned"))?;
            if !gate.shutdown_requested {
                gate.sender
                    .try_send(Job::Shutdown { _guard: guard })
                    .map_err(|_| {
                        WireError::new("api_worker", "API shutdown control slot is unavailable")
                    })?;
                gate.shutdown_requested = true;
            }
        }
        let mut done = self.done.clone();
        let failure = loop {
            match done.borrow_and_update().clone() {
                Completion::Finished(failure) => break failure,
                Completion::Pending => {}
            }
            done.changed()
                .await
                .map_err(|_| WireError::new("api_worker", "API completion channel closed"))?;
        };
        let mut pump_done = self.pump_done.clone();
        while !*pump_done.borrow_and_update() {
            pump_done.changed().await.map_err(|_| {
                WireError::new("api_worker", "API event pump stopped without completion")
            })?;
        }
        match failure {
            Some((code, message)) => Err(WireError::new(code, message)),
            None => Ok(()),
        }
    }
}

async fn owner(
    library: LibraryService,
    service: ApiService,
    binding: Arc<Mutex<Option<Binding>>>,
    publication: Arc<AsyncMutex<()>>,
    mut receive: mpsc::Receiver<Job>,
    complete: watch::Sender<Completion>,
) {
    while let Some(job) = receive.recv().await {
        // Events accepted immediately after bind wait until the frozen host
        // binding is published; an early connection must not disappear.
        let published = publication.lock().await;
        match job {
            #[cfg(test)]
            Job::Pause { entered, release } => {
                let _ = entered.send(());
                let _ = release.await;
            }
            Job::Dispatch {
                request,
                reply,
                permit,
            } => {
                let result = dispatch(&library, &service, &binding, request).await;
                drop(published);
                drop(permit);
                if let Err(Err(error)) = reply.send(result) {
                    eprintln!("Detached API command failed: {}", error.message);
                }
            }
            Job::Context { reply, _guard } => {
                let result = refresh_context(&library, &service, &binding).await;
                drop(published);
                drop(_guard);
                if let Err(Err(error)) = reply.send(result) {
                    eprintln!("Detached API context refresh failed: {}", error.message);
                }
            }
            Job::Shutdown { .. } => break,
        }
    }
    binding
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    let result = service.shutdown().await;
    // Closing the last service sender lets the event pump finish as well.
    drop(service);
    complete.send_replace(Completion::Finished(
        result.err().map(|error| (error.code, error.message)),
    ));
}

async fn dispatch(
    library: &LibraryService,
    service: &ApiService,
    binding: &Mutex<Option<Binding>>,
    request: Request,
) -> Result<Vec<Event>, WireError> {
    library
        .contextualize(&request, &mut [])
        .map_err(WireError::from)?;
    match request.data["channel"].as_str().unwrap_or("") {
        "WEBSOCKET_START" => {
            binding
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            let options = &request.data["data"];
            let endpoints = service
                .start(
                    options,
                    request.context.as_ref().unwrap().library_generation,
                )
                .await
                .map_err(service_error)?;
            if let Err(error) = library.contextualize(&request, &mut []) {
                service.stop().await.map_err(service_error)?;
                return Err(error.into());
            }
            *binding.lock().unwrap_or_else(|error| error.into_inner()) = Some(Binding {
                context: request.context.unwrap(),
                endpoints,
                options: options.clone(),
            });
        }
        "WEBSOCKET_STOP" => {
            binding
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            service.stop().await.map_err(service_error)?;
        }
        command => {
            let current = binding
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
                .ok_or_else(|| WireError::new("stale_generation", "No current API listener"))?;
            if request.context.as_ref() != Some(&current.context) {
                return Err(WireError::new(
                    "stale_generation",
                    "API listener belongs to an obsolete library",
                ));
            }
            let generation = &current.endpoints.generation;
            if command == "API_TRIGGER" {
                let id = request.data["data"]["returnId"]
                    .as_str()
                    .unwrap_or_default();
                validate_id(id, "api", generation)?;
                service
                    .api_trigger(generation, &request.data["data"])
                    .map_err(service_error)?;
            } else {
                let id = request.listener_id.as_deref().unwrap_or_default();
                validate_id(id, "GET_DYNAMIC_VALUES", generation)?;
                service
                    .dynamic_values_reply(generation, id, request.data["data"].clone())
                    .map_err(service_error)?;
            }
        }
    }
    Ok(vec![])
}

fn validate_id(id: &str, kind: &str, generation: &ApiGeneration) -> Result<(), WireError> {
    if !id.starts_with(&format!("{kind}:{}:", generation.run)) {
        return Err(WireError::new(
            "stale_generation",
            "API reply does not belong to the listener's private run",
        ));
    }
    Ok(())
}

async fn refresh_context(
    library: &LibraryService,
    service: &ApiService,
    binding: &Mutex<Option<Binding>>,
) -> Result<(), WireError> {
    let context = library.context().map_err(WireError::from)?;
    let previous = binding
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let Some(previous) = previous else {
        return Ok(());
    };
    if previous.context == context {
        return Ok(());
    }
    binding
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    service.cancel_pending();
    service.stop().await.map_err(service_error)?;
    let endpoints = service
        .start(&previous.options, context.library_generation)
        .await
        .map_err(service_error)?;
    if library.context().map_err(WireError::from)? != context {
        service.stop().await.map_err(service_error)?;
        return Err(WireError::new(
            "stale_library",
            "Library changed during API restart",
        ));
    }
    *binding.lock().unwrap_or_else(|error| error.into_inner()) = Some(Binding {
        context,
        endpoints,
        options: previous.options,
    });
    Ok(())
}

async fn pump(
    library: LibraryService,
    binding: Arc<Mutex<Option<Binding>>>,
    publication: Arc<AsyncMutex<()>>,
    closing: Arc<AtomicBool>,
    mut events: mpsc::Receiver<ApiEvent>,
    sink: impl Fn(Event) -> Result<(), String> + Send + Sync,
    complete: watch::Sender<bool>,
) {
    while let Some(event) = events.recv().await {
        let _published = publication.lock().await;
        if closing.load(Ordering::Acquire) {
            continue;
        }
        let binding = binding.lock().unwrap_or_else(|error| error.into_inner());
        let Some(current) = binding.as_ref() else {
            continue;
        };
        if current.endpoints.generation != event.generation
            || library.context().ok().as_ref() != Some(&current.context)
        {
            continue;
        }
        let mut wire = match event.kind {
            ApiEventKind::Connected => Event::reply("MAIN", "WEBSOCKET", json!("connected"), None),
            ApiEventKind::Trigger { data } => Event::reply("MAIN", "API_TRIGGER2", data, None),
            ApiEventKind::DynamicValues { listener_id, keys } => {
                Event::reply("MAIN", "GET_DYNAMIC_VALUES", keys, Some(listener_id))
            }
            ApiEventKind::Diagnostic { code, message } => {
                eprintln!("API {code}: {message}");
                continue;
            }
        };
        wire.context = Some(current.context.clone());
        if let Err(error) = sink(wire) {
            eprintln!("API renderer event failed: {error}");
        }
    }
    complete.send_replace(true);
}

fn within_limit(value: &Value, limit: usize) -> bool {
    struct Counter {
        left: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.left = self
                .left
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("API byte limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter { left: limit }, value).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use freeshow_core::runtime::Runtime;
    use std::{
        future::{poll_fn, Future},
        task::Poll,
        time::Duration,
    };

    fn request(context: &RuntimeContext) -> Request {
        serde_json::from_value(
            json!({"channel":"MAIN","context":context,"data":{"channel":"WEBSOCKET_STOP"}}),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn full_public_queue_retains_cancelled_jobs_and_reserves_context_and_shutdown_control() {
        let temp = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(temp.path().join("app"), temp.path().join("data")).unwrap(),
        )
        .unwrap();
        let context = library.context().unwrap();
        let native = NativeApi::new(library, ApiConfig::default(), |_| Ok(())).unwrap();
        let (entered, ready) = oneshot::channel();
        let (release, gate) = oneshot::channel();
        native
            .gate
            .lock()
            .unwrap()
            .sender
            .try_send(Job::Pause {
                entered,
                release: gate,
            })
            .unwrap_or_else(|_| panic!("test gate"));
        ready.await.unwrap();
        let mut waiting = Vec::new();
        for _ in 0..MAX_REQUESTS {
            let mut job = Box::pin(native.dispatch(request(&context), "main"));
            assert!(poll_fn(|cx| Poll::Ready(job.as_mut().poll(cx)))
                .await
                .is_pending());
            waiting.push(job);
        }
        assert_eq!(
            native
                .dispatch(request(&context), "main")
                .await
                .unwrap_err()
                .code,
            "api_busy"
        );
        drop(waiting.remove(0));
        assert_eq!(
            native.slots.available_permits(),
            0,
            "An accepted job outlives its lost reply receiver"
        );
        let mut changed = Box::pin(native.context_changed());
        assert!(poll_fn(|cx| Poll::Ready(changed.as_mut().poll(cx)))
            .await
            .is_pending());
        assert_eq!(native.gate.lock().unwrap().sender.capacity(), 0);
        drop(changed);
        let mut later_change = Box::pin(native.context_changed());
        assert!(poll_fn(|cx| Poll::Ready(later_change.as_mut().poll(cx)))
            .await
            .is_pending());
        drop(later_change);
        let mut closing = Box::pin(native.shutdown());
        assert!(poll_fn(|cx| Poll::Ready(closing.as_mut().poll(cx)))
            .await
            .is_pending());
        assert_eq!(
            native
                .dispatch(request(&context), "main")
                .await
                .unwrap_err()
                .code,
            "api_shutdown"
        );
        release.send(()).unwrap();
        for job in waiting {
            assert!(job.await.unwrap().is_empty());
        }
        tokio::time::timeout(Duration::from_secs(3), closing)
            .await
            .unwrap()
            .unwrap();
        native.shutdown().await.unwrap();
        assert_eq!(native.status()["admittedRequests"], 0);
    }
}
