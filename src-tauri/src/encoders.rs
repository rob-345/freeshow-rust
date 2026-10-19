//! Owned encoder detection through independent probes and ordered cache effects.
use crate::encoder_inputs::{EncoderInputs, PreparedEncoderInput};
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    service::LibraryService,
    WireError,
};
use freeshow_encoders::{EncoderConfig, EncoderService};
use freeshow_media::MediaService;
use serde_json::{json, Value};
use std::{
    io::{self, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::{oneshot, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore};

pub const COMMANDS: &[&str] = &["ENCODER_DETECT"];
const CAPACITY: usize = 4;
#[derive(Clone)]
struct Binding {
    context: RuntimeContext,
    run: String,
    service: EncoderService,
}
struct CacheAttempt {
    run: String,
    key: String,
    probe: u64,
    persisted: bool,
    complete: bool,
}
struct CacheEffect {
    prepared: PreparedEncoderInput,
    intent: freeshow_encoders::CacheWriteIntent,
    probe: u64,
    permit: Arc<RequestLease>,
}
struct RequestLease {
    _permit: OwnedSemaphorePermit,
    admitted: Arc<AtomicUsize>,
}
impl Drop for RequestLease {
    fn drop(&mut self) {
        self.admitted.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Inner {
    library: LibraryService,
    media: MediaService,
    inputs: EncoderInputs,
    root: PathBuf,
    binding: Mutex<Option<Binding>>,
    cache: Arc<Mutex<Option<CacheAttempt>>>,
    slots: Arc<Semaphore>,
    admitted: Arc<AtomicUsize>,
    closing: AtomicBool,
    lifecycle: AsyncMutex<()>,
    failure: Mutex<Option<(&'static str, String)>>,
}
#[derive(Clone)]
pub struct NativeEncoders {
    inner: Arc<Inner>,
}
fn service_error(error: freeshow_encoders::EncoderError) -> WireError {
    WireError::new(error.code, error.message)
}
fn closed() -> WireError {
    WireError::new("encoders_shutdown", "Encoder detection admission is closed")
}
fn binding(context: RuntimeContext) -> Result<Binding, WireError> {
    Ok(Binding {
        context,
        run: uuid::Uuid::new_v4().simple().to_string(),
        service: EncoderService::new(EncoderConfig::default()).map_err(service_error)?,
    })
}
impl NativeEncoders {
    pub fn new(
        library: LibraryService,
        media: MediaService,
        root: PathBuf,
    ) -> Result<Self, WireError> {
        let inputs = EncoderInputs::new(root.clone())?;
        let current = binding(library.context().map_err(WireError::from)?)?;
        Ok(Self {
            inner: Arc::new(Inner {
                library,
                media,
                inputs,
                root,
                binding: Mutex::new(Some(current)),
                cache: Arc::new(Mutex::new(None)),
                slots: Arc::new(Semaphore::new(CAPACITY)),
                admitted: Arc::new(AtomicUsize::new(0)),
                closing: AtomicBool::new(false),
                lifecycle: AsyncMutex::new(()),
                failure: Mutex::new(None),
            }),
        })
    }
    pub fn status(&self) -> Value {
        let binding = self.inner.binding.lock().unwrap_or_else(|e| e.into_inner());
        let cache = self.inner.cache.lock().unwrap_or_else(|e| e.into_inner());
        json!({"closing":self.inner.closing.load(Ordering::Acquire),"admittedRequests":self.inner.admitted.load(Ordering::Acquire),
            "binding":binding.as_ref().map(|b|json!({"context":b.context,"run":b.run,"service":b.service.stats()})),
            "cache":cache.as_ref().map(|c|json!({"run":c.run,"probe":c.probe,"persisted":c.persisted,"complete":c.complete})),
            "failure":self.failure().map(|error|json!({"code":error.code,"message":error.message}))})
    }
    fn failure(&self) -> Option<WireError> {
        self.inner
            .failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(code, message)| WireError::new(code, message.clone()))
    }
    fn retain_failure(&self, error: WireError) {
        self.inner.closing.store(true, Ordering::Release);
        let mut first = self.inner.failure.lock().unwrap_or_else(|e| e.into_inner());
        if first.is_none() {
            *first = Some((error.code, error.message));
        }
    }
    fn admit(&self) -> Result<Arc<RequestLease>, WireError> {
        let permit = self.inner.slots.clone().try_acquire_owned().map_err(|_| {
            WireError::new("encoders_busy", "Encoder host requests are at capacity")
        })?;
        self.inner.admitted.fetch_add(1, Ordering::AcqRel);
        Ok(Arc::new(RequestLease {
            _permit: permit,
            admitted: self.inner.admitted.clone(),
        }))
    }
    pub async fn dispatch(&self, request: Request, window: &str) -> Result<Vec<Event>, WireError> {
        if request.channel != "MAIN" || request.data["channel"] != "ENCODER_DETECT" {
            return Err(WireError::new(
                "unsupported_command",
                "Unknown encoder command",
            ));
        }
        if !within_limit(&request.data, 64 * 1024)
            || request
                .listener_id
                .as_ref()
                .is_some_and(|id| id.len() > 256)
        {
            return Err(WireError::new(
                "encoder_input_limit",
                "Encoder IPC input exceeds its byte limit",
            ));
        }
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        self.inner
            .library
            .contextualize(&request, &mut [])
            .map_err(WireError::from)?;
        let current = self
            .inner
            .binding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|b| Some(&b.context) == request.context.as_ref())
            .cloned()
            .ok_or_else(|| {
                WireError::new(
                    "stale_generation",
                    "Encoder owner belongs to an obsolete library",
                )
            })?;
        let permit = self.admit()?;
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        let (reply, response) = oneshot::channel();
        let native = self.clone();
        let window = window.to_string();
        // Accepted preparation, child supervision and cache effects remain owned
        // even when their IPC reply receiver disappears.
        tokio::spawn(async move {
            let result = native.run(request, window, current, permit).await;
            if let Err(Err(error)) = reply.send(result) {
                eprintln!("Detached encoder detection failed: {}", error.message);
            }
        });
        response
            .await
            .map_err(|_| WireError::new("encoders_worker", "Encoder request owner stopped"))?
    }
    fn validate(&self, request: &Request, current: &Binding) -> Result<(), WireError> {
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        self.inner
            .library
            .contextualize(request, &mut [])
            .map_err(WireError::from)?;
        if self
            .inner
            .binding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_none_or(|b| b.run != current.run)
        {
            return Err(WireError::new(
                "stale_generation",
                "Encoder request belongs to a retired owner",
            ));
        }
        Ok(())
    }
    async fn run(
        &self,
        request: Request,
        window: String,
        current: Binding,
        permit: Arc<RequestLease>,
    ) -> Result<Vec<Event>, WireError> {
        self.validate(&request, &current)?;
        let check = self
            .inner
            .media
            .dispatch("FFMPEG_CHECK", Value::Null)
            .await
            .map_err(|error| WireError::new(error.code(), error.to_string()))?;
        for diagnostic in &check.diagnostics {
            eprintln!(
                "Encoder executable {}: {}",
                diagnostic.code, diagnostic.message
            );
        }
        self.validate(&request, &current)?;
        let source = check.data["path"]
            .as_str()
            .filter(|_| check.data["installed"] == true)
            .map(str::to_owned);
        let force = freeshow_encoders::force_requested(request.data.get("data"));
        let prepared = self
            .inner
            .inputs
            .freeze(source, current.context.library_generation, force)
            .await?;
        self.validate(&request, &current)?;
        let mut detection = current
            .service
            .detect(prepared.input().clone(), force)
            .await
            .map_err(service_error)?;
        for diagnostic in &detection.diagnostics {
            eprintln!("Encoder {}: {}", diagnostic.code, diagnostic.message);
        }
        self.validate(&request, &current)?;
        if let Some(intent) = detection.cache_write.take() {
            self.commit(
                &request,
                &window,
                &current,
                CacheEffect {
                    prepared,
                    intent,
                    probe: detection.probe_id,
                    permit: permit.clone(),
                },
            )
            .await?;
        }
        self.validate(&request, &current)?;
        let mut events = vec![Event::reply(
            "MAIN",
            "ENCODER_DETECT",
            detection.detection,
            request.listener_id.clone(),
        )];
        self.inner
            .library
            .contextualize(&request, &mut events)
            .map_err(WireError::from)?;
        Ok(events)
    }
    async fn commit(
        &self,
        request: &Request,
        window: &str,
        current: &Binding,
        effect: CacheEffect,
    ) -> Result<(), WireError> {
        let CacheEffect {
            prepared,
            intent,
            probe,
            permit,
        } = effect;
        let owned = Request {
            channel: request.channel.clone(),
            data: request.data.clone(),
            context: request.context.clone(),
            listener_id: request.listener_id.clone(),
        };
        let cache = self.inner.cache.clone();
        let run = current.run.clone();
        let root = self.inner.root.clone();
        let generation = current.context.library_generation;
        let key = intent.key().to_owned();
        let owner = self.inner.clone();
        let result = self
            .inner
            .library
            .commit(owned, window, move |runtime| {
                let _lease = permit;
                // IO-owner ordering chooses the newest completed probe for this
                // private run; coalesced/late older callers do not rewrite its cache.
                if runtime.stores.app_data != root
                    || owner
                        .binding
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_ref()
                        .is_none_or(|binding| binding.run != run)
                {
                    return Err(freeshow_core::Error::Invalid(
                        "Encoder cache owner is obsolete".into(),
                    ));
                }
                {
                    let mut previous = cache.lock().unwrap_or_else(|e| e.into_inner());
                    if previous
                        .as_ref()
                        .is_some_and(|c| c.run == run && c.key == key && c.probe >= probe)
                    {
                        return Ok(vec![]);
                    }
                    *previous = Some(CacheAttempt {
                        run: run.clone(),
                        key: key.clone(),
                        probe,
                        persisted: false,
                        complete: false,
                    });
                }
                let persisted = match prepared.commit_cache(&intent, &root, generation) {
                    Ok(()) => true,
                    Err(error) => {
                        eprintln!(
                            "Encoder detection cache was not persisted: {}",
                            error.message
                        );
                        false
                    }
                };
                *cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(CacheAttempt {
                    run,
                    key,
                    probe,
                    persisted,
                    complete: true,
                });
                Ok(vec![])
            })
            .await;
        if let Err(error) = result {
            self.validate(request, current)?;
            // Source cache persistence errors warn, but valid detection remains
            // useful to the original settings UI.
            eprintln!("Encoder cache owner rejected persistence: {error}");
        }
        Ok(())
    }
    pub async fn context_changed(&self) -> Result<(), WireError> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        if let Some(error) = self.failure() {
            return Err(error);
        }
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        let context = self.inner.library.context().map_err(WireError::from)?;
        let old = {
            let mut current = self.inner.binding.lock().unwrap_or_else(|e| e.into_inner());
            if current.as_ref().is_some_and(|b| b.context == context) {
                return Ok(());
            }
            current.take()
        };
        if let Some(old) = old {
            if let Err(error) = old.service.shutdown().await {
                self.retain_failure(service_error(error));
                return Err(self.failure().unwrap());
            }
        }
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        *self.inner.binding.lock().unwrap_or_else(|e| e.into_inner()) = Some(binding(context)?);
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<(), WireError> {
        self.inner.closing.store(true, Ordering::Release);
        let _lifecycle = self.inner.lifecycle.lock().await;
        let old = self
            .inner
            .binding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let service = async {
            if let Some(old) = old {
                old.service.shutdown().await.map_err(service_error)
            } else {
                Ok(())
            }
        };
        let (service, inputs) = tokio::join!(service, self.inner.inputs.shutdown());
        let _drained = self
            .inner
            .slots
            .clone()
            .acquire_many_owned(CAPACITY as u32)
            .await
            .map_err(|_| WireError::new("encoders_worker", "Encoder host drain failed"))?;
        if let Err(error) = service.and(inputs) {
            self.retain_failure(error);
        }
        self.failure().map_or(Ok(()), Err)
    }
}
fn within_limit(value: &Value, limit: usize) -> bool {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("Encoder IPC byte limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(limit), value).is_ok()
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    use freeshow_core::runtime::Runtime;
    use freeshow_media::MediaConfig;

    #[tokio::test]
    async fn shutdown_drain_reservations_are_not_reported_as_admitted_requests() {
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        let dir = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(dir.path().join("app"), dir.path().join("data")).unwrap(),
        )
        .unwrap();
        let media = MediaService::new(MediaConfig {
            discover_system: false,
            ..MediaConfig::default()
        })
        .unwrap();
        let native =
            NativeEncoders::new(library.clone(), media.clone(), dir.path().join("app")).unwrap();
        let lease = native.admit().unwrap();
        let mut drain = Box::pin(
            native
                .inner
                .slots
                .clone()
                .acquire_many_owned(CAPACITY as u32),
        );
        let waker = Waker::noop();
        assert!(drain
            .as_mut()
            .poll(&mut Context::from_waker(waker))
            .is_pending());
        assert_eq!(
            native.inner.slots.available_permits(),
            0,
            "the real semaphore drain reserves the other three permits while waiting"
        );
        assert_eq!(native.status()["admittedRequests"], 1);
        drop(lease);
        let Poll::Ready(Ok(reserved)) = drain.as_mut().poll(&mut Context::from_waker(waker)) else {
            panic!("Released request did not unblock drain")
        };
        assert_eq!(native.inner.slots.available_permits(), 0);
        assert_eq!(
            native.status()["admittedRequests"],
            0,
            "owned drain permits are not live requests"
        );
        drop(reserved);
        native.shutdown().await.unwrap();
        media.shutdown().await;
        library.finish_writes().await.unwrap();
    }

    #[tokio::test]
    async fn real_input_worker_panic_remains_a_failed_teardown_on_retries_and_context_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(dir.path().join("app"), dir.path().join("data")).unwrap(),
        )
        .unwrap();
        let media = MediaService::new(MediaConfig {
            discover_system: false,
            ..MediaConfig::default()
        })
        .unwrap();
        let native =
            NativeEncoders::new(library.clone(), media.clone(), dir.path().join("app")).unwrap();
        assert!(native.inner.inputs.panic_worker_for_test().await.is_err());
        let first = native.shutdown().await.unwrap_err();
        assert!(first.message.contains("worker panicked"));
        assert_eq!(native.status()["admittedRequests"], 0);
        assert_eq!(native.status()["failure"]["code"], first.code);
        let second = native.shutdown().await.unwrap_err();
        assert_eq!(
            (second.code, second.message),
            (first.code, first.message.clone())
        );
        let retired = native.context_changed().await.unwrap_err();
        assert_eq!((retired.code, retired.message), (first.code, first.message));
        assert!(
            native.status()["binding"].is_null(),
            "failed teardown must not create a replacement owner"
        );
        media.shutdown().await;
        library.finish_writes().await.unwrap();
    }
}
