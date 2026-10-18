use crate::*;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::JoinHandle,
};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
#[derive(Clone)]
pub struct ScriptureService {
    inner: Arc<Inner>,
}
struct Shared {
    cancel: CancellationToken,
    active: AtomicUsize,
    #[cfg(all(test, target_os = "linux"))]
    hooks: Arc<Hooks>,
}
struct Inner {
    limits: Limits,
    shared: Arc<Shared>,
    slots: Arc<Semaphore>,
    sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    shutdown: tokio::sync::Mutex<()>,
}
struct Work {
    request: Request,
    budget: Budget,
    permit: OwnedSemaphorePermit,
    response: oneshot::Sender<Completion>,
}
struct Completion {
    result: Result<PreparedScriptureReply>,
    _permit: OwnedSemaphorePermit,
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
#[cfg(all(test, target_os = "linux"))]
#[derive(Default)]
pub(crate) struct Hooks {
    pub process: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    pub after_first: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub workers: usize,
    pub active: usize,
    pub admitted: usize,
    pub closing: bool,
}
impl ScriptureService {
    pub fn new(limits: Limits) -> Result<Self> {
        limits.validate()?;
        let (sender, receiver) = mpsc::sync_channel::<Work>(WORKERS + limits.queued_jobs);
        let receiver = Arc::new(Mutex::new(receiver));
        let shared = Arc::new(Shared {
            cancel: CancellationToken::new(),
            active: AtomicUsize::new(0),
            #[cfg(all(test, target_os = "linux"))]
            hooks: Arc::new(Hooks::default()),
        });
        let mut workers = Vec::new();
        for index in 0..WORKERS {
            let receive = receiver.clone();
            let shared = shared.clone();
            match std::thread::Builder::new()
                .name(format!("freeshow-scriptures-{index}"))
                .spawn(move || loop {
                    let work = receive.lock().unwrap_or_else(|e| e.into_inner()).recv();
                    let Ok(Work {
                        request,
                        mut budget,
                        permit,
                        response,
                    }) = work
                    else {
                        break;
                    };
                    shared.active.fetch_add(1, Ordering::AcqRel);
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        #[cfg(all(test, target_os = "linux"))]
                        {
                            let hook = budget.hooks.process.lock().unwrap().clone();
                            if let Some(hook) = hook {
                                hook();
                            }
                        }
                        engine::process(request, &mut budget)
                    }))
                    .unwrap_or_else(|_| {
                        Err(Error::new("scripture_worker", "Scripture worker panicked"))
                    });
                    shared.active.fetch_sub(1, Ordering::AcqRel);
                    let _ = response.send(Completion {
                        result,
                        _permit: permit,
                    });
                }) {
                Ok(worker) => workers.push(worker),
                Err(_) => {
                    drop(sender);
                    drop(receiver);
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return Err(Error::new(
                        "scripture_worker",
                        "Could not start scripture workers",
                    ));
                }
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                slots: Arc::new(Semaphore::new(WORKERS + limits.queued_jobs)),
                limits,
                shared,
                sender: Mutex::new(Some(sender)),
                workers: Mutex::new(workers),
                shutdown: tokio::sync::Mutex::new(()),
            }),
        })
    }
    pub async fn load(
        &self,
        context: &ScriptureContext,
        payload: &Value,
        cancel: CancellationToken,
    ) -> Result<PreparedScriptureReply> {
        self.load_with_deadline(
            context,
            payload,
            cancel,
            Instant::now() + self.inner.limits.timeout,
        )
        .await
    }
    pub async fn load_with_deadline(
        &self,
        context: &ScriptureContext,
        payload: &Value,
        caller: CancellationToken,
        deadline: Instant,
    ) -> Result<PreparedScriptureReply> {
        if self.inner.shared.cancel.is_cancelled() {
            return Err(Error::shutdown());
        }
        if caller.is_cancelled() {
            return Err(Error::cancelled());
        }
        let deadline = deadline.min(Instant::now() + self.inner.limits.timeout);
        if Instant::now() >= deadline {
            return Err(Error::new(
                "scripture_deadline",
                "Scripture deadline elapsed",
            ));
        }
        let name = payload
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::unsupported("BIBLE requires a string name"))?;
        if name.len() > self.inner.limits.path_bytes
            || context.bibles.as_os_str().len() > self.inner.limits.path_bytes
            || context.legacy.as_os_str().len() > self.inner.limits.path_bytes
        {
            return Err(Error::limit("Scripture path bound"));
        }
        let default_id = Value::String(String::new());
        let id = payload.get("id").unwrap_or(&default_id);
        let id_bytes = bounded_id(id)?;
        let permit =
            self.inner.slots.clone().try_acquire_owned().map_err(|_| {
                Error::new("scripture_busy", "Scripture workers and queue are full")
            })?;
        let cancel = CancellationToken::new();
        let _drop = CancelOnDrop(cancel.clone());
        let mut budget = Budget {
            limits: self.inner.limits.clone(),
            cancel,
            caller: caller.clone(),
            service: self.inner.shared.cancel.clone(),
            deadline,
            stats: Stats::default(),
            allocated: 0,
            #[cfg(all(test, target_os = "linux"))]
            hooks: self.inner.shared.hooks.clone(),
        };
        budget.charge(
            context.bibles.as_os_str().len()
                + context.legacy.as_os_str().len()
                + name.len()
                + id_bytes * 4
                + 4096,
        )?;
        let (response, receive) = oneshot::channel();
        {
            let sender = self.inner.sender.lock().unwrap_or_else(|e| e.into_inner());
            budget.check()?;
            sender
                .as_ref()
                .ok_or_else(Error::shutdown)?
                .try_send(Work {
                    request: Request {
                        context: context.clone(),
                        id: id.clone(),
                        name: name.into(),
                    },
                    budget,
                    permit,
                    response,
                })
                .map_err(|error| match error {
                    mpsc::TrySendError::Full(_) => {
                        Error::new("scripture_busy", "Scripture queue is full")
                    }
                    mpsc::TrySendError::Disconnected(_) => {
                        Error::new("scripture_worker", "Scripture workers unavailable")
                    }
                })?;
        }
        let done = tokio::select! {biased;
            _=self.inner.shared.cancel.cancelled()=>return Err(Error::shutdown()),
            _=caller.cancelled()=>return Err(Error::cancelled()),
            _=tokio::time::sleep_until(deadline.into())=>return Err(Error::new("scripture_deadline","Scripture deadline elapsed")),
            result=receive=>result.map_err(|_|Error::new("scripture_worker","Scripture worker response closed"))?,
        };
        if self.inner.shared.cancel.is_cancelled() {
            return Err(Error::shutdown());
        }
        if caller.is_cancelled() {
            return Err(Error::cancelled());
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                "scripture_deadline",
                "Scripture deadline elapsed",
            ));
        }
        done.result
    }
    pub fn status(&self) -> Status {
        Status {
            workers: self
                .inner
                .workers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|w| !w.is_finished())
                .count(),
            active: self.inner.shared.active.load(Ordering::Acquire),
            admitted: WORKERS + self.inner.limits.queued_jobs
                - self.inner.slots.available_permits(),
            closing: self.inner.shared.cancel.is_cancelled(),
        }
    }
    /// Terminal, bounded wait for both fixed workers. A blocked kernel call is
    /// not force-killed; timeout is explicit and a later shutdown can finish.
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.shared.cancel.cancel();
        self.inner
            .sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let deadline = Instant::now() + self.inner.limits.shutdown_timeout;
        let _serial = tokio::time::timeout_at(deadline.into(), self.inner.shutdown.lock())
            .await
            .map_err(|_| {
                Error::new(
                    "scripture_shutdown_timeout",
                    "Scripture shutdown owner did not finish before deadline",
                )
            })?;
        loop {
            if self
                .inner
                .workers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .all(JoinHandle::is_finished)
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err(Error::new(
                    "scripture_shutdown_timeout",
                    "Scripture workers did not finish before shutdown deadline",
                ));
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        for worker in self
            .inner
            .workers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            if worker.join().is_err() {
                return Err(Error::new(
                    "scripture_worker",
                    "Scripture worker exited unexpectedly",
                ));
            }
        }
        Ok(())
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn hooks(&self) -> Arc<Hooks> {
        self.inner.shared.hooks.clone()
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.cancel.cancel();
        self.sender
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}
fn bounded_id(id: &Value) -> Result<usize> {
    let mut stack = vec![(id, 0)];
    let mut nodes = 0;
    let mut bytes = 0usize;
    while let Some((value, depth)) = stack.pop() {
        nodes += 1;
        if depth > 8 || nodes > 128 {
            return Err(Error::limit("Scripture ID shape bound"));
        }
        match value {
            Value::String(text) => bytes = bytes.saturating_add(text.len()),
            Value::Array(values) => {
                if values.len() > 128 - nodes - stack.len() {
                    return Err(Error::limit("Scripture ID node bound"));
                }
                for value in values {
                    stack.push((value, depth + 1));
                }
            }
            Value::Object(values) => {
                if values.len() > 128 - nodes - stack.len() {
                    return Err(Error::limit("Scripture ID node bound"));
                }
                for (key, value) in values {
                    bytes = bytes.saturating_add(key.len());
                    stack.push((value, depth + 1));
                }
            }
            _ => bytes = bytes.saturating_add(8),
        }
        if bytes > 8192 {
            return Err(Error::limit("Scripture ID byte bound"));
        }
    }
    Ok(bytes + nodes * 256)
}
