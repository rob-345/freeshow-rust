use crate::{
    json as source_json,
    process::{self, Metrics},
    profiles, truthy, unavailable, CacheWriteIntent, DetectionSource, Diagnostic, EncoderConfig,
    EncoderError, EncoderStats, FrozenDetectionInput, PreparedDetection, PreparedResolution,
    Result, SettingIntent,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
use tokio::{
    sync::{mpsc, oneshot, Notify, OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct EncoderService {
    inner: Arc<Inner>,
}
struct Inner {
    config: EncoderConfig,
    sender: mpsc::Sender<Request>,
    cancel: CancellationToken,
    admission: Arc<Semaphore>,
    metrics: Arc<Metrics>,
    finished: Arc<Completion>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    settings: Mutex<BTreeMap<String, String>>,
}
#[derive(Default)]
struct Completion {
    value: Mutex<Option<Result<()>>>,
    notify: Notify,
}
struct Reply {
    sender: oneshot::Sender<Result<PreparedDetection>>,
    _permit: OwnedSemaphorePermit,
}
struct Request {
    input: FrozenDetectionInput,
    force: bool,
    reply: Reply,
}
struct Job {
    fingerprint: Fingerprint,
    replies: Vec<Reply>,
}
impl EncoderService {
    pub fn new(config: EncoderConfig) -> Result<Self> {
        config.validate()?;
        let (sender, receiver) = mpsc::channel(config.max_callers);
        let metrics = Arc::new(Metrics::default());
        let cancel = CancellationToken::new();
        let finished = Arc::new(Completion::default());
        let (task_metrics, task_cancel, task_done, task_config) = (
            metrics.clone(),
            cancel.clone(),
            finished.clone(),
            config.clone(),
        );
        let thread = std::thread::Builder::new()
            .name("freeshow-encoders".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| EncoderError::new("runtime_failed", e.to_string()))?;
                    runtime.block_on(work(
                        receiver,
                        task_config,
                        task_cancel,
                        task_metrics.clone(),
                    ));
                    drop(runtime);
                    if task_metrics.children.load(Ordering::Relaxed) != 0 {
                        return Err(EncoderError::new(
                            "shutdown_incomplete",
                            "Encoder children were not all reaped",
                        ));
                    }
                    Ok(())
                }))
                .unwrap_or_else(|_| {
                    Err(EncoderError::new(
                        "worker_panicked",
                        "Encoder worker panicked",
                    ))
                });
                *task_done.value.lock().unwrap() = Some(result);
                task_done.notify.notify_waiters();
            })
            .map_err(|e| EncoderError::new("worker_unavailable", e.to_string()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                admission: Arc::new(Semaphore::new(config.max_callers)),
                config,
                sender,
                cancel,
                metrics,
                finished,
                thread: Mutex::new(Some(thread)),
                settings: Mutex::new(BTreeMap::new()),
            }),
        })
    }
    pub async fn detect(
        &self,
        input: FrozenDetectionInput,
        force: bool,
    ) -> Result<PreparedDetection> {
        if self.inner.cancel.is_cancelled() {
            return Err(EncoderError::new(
                "shutdown",
                "Encoder service has shut down",
            ));
        }
        input.validate(&self.inner.config)?;
        let permit = self
            .inner
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                EncoderError::new("caller_limit", "Encoder detection caller limit reached")
            })?;
        let (sender, receive) = oneshot::channel();
        self.inner
            .sender
            .try_send(Request {
                input,
                force,
                reply: Reply {
                    sender,
                    _permit: permit,
                },
            })
            .map_err(|_| {
                EncoderError::new("queue_unavailable", "Encoder request queue unavailable")
            })?;
        receive.await.unwrap_or_else(|_| {
            Err(EncoderError::new(
                "worker_unavailable",
                "Encoder detection owner stopped",
            ))
        })
    }
    pub async fn resolve(
        &self,
        input: FrozenDetectionInput,
        setting: Option<&str>,
        disable_hardware: bool,
    ) -> Result<PreparedResolution> {
        if setting.is_none_or(|value| value.is_empty() || value == "auto") && disable_hardware {
            return Ok(PreparedResolution {
                encoder: "x264".into(),
                detection: None,
            });
        }
        let detection = self.detect(input, false).await?;
        let encoder = crate::resolve_detection(&detection.detection, setting)?;
        Ok(PreparedResolution {
            encoder,
            detection: Some(detection),
        })
    }
    /// Original SET_RTMP_ENCODER has no reply and always asks the capture owner
    /// to reconcile RTMP state. The returned intent is for native host wiring.
    pub fn set_rtmp_encoder(&self, output_id: &str, encoder: &str) -> Result<SettingIntent> {
        if self.inner.cancel.is_cancelled() {
            return Err(EncoderError::new(
                "shutdown",
                "Encoder service has shut down",
            ));
        }
        if output_id.len() > 4096 || encoder.len() > 4096 {
            return Err(EncoderError::new(
                "setting_limit",
                "Encoder setting exceeds byte limit",
            ));
        }
        let mut settings = self.inner.settings.lock().unwrap();
        // Plain JS object __proto__ assignment ignores primitive strings.
        let encoder = if encoder.is_empty() { "auto" } else { encoder };
        if output_id != "__proto__" {
            if !settings.contains_key(output_id) && settings.len() >= self.inner.config.max_settings
            {
                return Err(EncoderError::new(
                    "setting_limit",
                    "Encoder output setting limit reached",
                ));
            }
            settings.insert(output_id.into(), encoder.into());
        }
        Ok(SettingIntent {
            output_id: output_id.into(),
            encoder: encoder.into(),
            reconfigure_rtmp: true,
        })
    }
    pub fn rtmp_encoder(&self, output_id: &str) -> Result<String> {
        if output_id == "__proto__"
            || [
                "constructor",
                "toString",
                "valueOf",
                "hasOwnProperty",
                "isPrototypeOf",
                "propertyIsEnumerable",
                "toLocaleString",
                "__defineGetter__",
                "__defineSetter__",
                "__lookupGetter__",
                "__lookupSetter__",
            ]
            .contains(&output_id)
                && !self.inner.settings.lock().unwrap().contains_key(output_id)
        {
            return Err(EncoderError::new(
                "unsupported_prototype_setting",
                "Source plain-object prototype value is not an encoder string",
            ));
        }
        Ok(self
            .inner
            .settings
            .lock()
            .unwrap()
            .get(output_id)
            .cloned()
            .unwrap_or_else(|| "auto".into()))
    }
    pub fn stats(&self) -> EncoderStats {
        EncoderStats {
            admitted_callers: self.inner.config.max_callers
                - self.inner.admission.available_permits(),
            active_probes: self.inner.metrics.probes.load(Ordering::Relaxed),
            active_children: self.inner.metrics.children.load(Ordering::Relaxed),
            children_started: self.inner.metrics.started.load(Ordering::Relaxed),
            children_reaped: self.inner.metrics.reaped.load(Ordering::Relaxed),
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.cancel.cancel();
        loop {
            let notified = self.inner.finished.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let value = self.inner.finished.value.lock().unwrap().clone();
            if let Some(result) = value {
                if let Some(thread) = self.inner.thread.lock().unwrap().take() {
                    thread.join().map_err(|_| {
                        EncoderError::new("worker_panicked", "Encoder worker join failed")
                    })?;
                }
                return result;
            }
            notified.await;
        }
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
#[derive(PartialEq, Eq)]
struct Fingerprint {
    generation: u64,
    path: Option<std::path::PathBuf>,
    key: Option<String>,
    device: String,
}
fn fingerprint(input: &FrozenDetectionInput) -> Result<Fingerprint> {
    Ok(Fingerprint {
        generation: input.generation,
        path: input.executable.as_ref().map(|v| v.path.clone()),
        key: input.cache_key()?,
        device: input.vaapi_device.clone(),
    })
}
fn cached(input: &FrozenDetectionInput) -> Result<Option<Value>> {
    let Some(bytes) = &input.cache else {
        return Ok(None);
    };
    let value = match source_json::parse(&String::from_utf8_lossy(bytes)) {
        Ok(value) => value,
        Err(error) if error.code == "invalid_json" => return Ok(None),
        Err(error) => return Err(error),
    };
    let key = input.cache_key()?;
    if value.get("version").and_then(Value::as_f64) == Some(1.0)
        && value.get("key").and_then(Value::as_str) == key.as_deref()
    {
        return Ok(value.get("detection").filter(|v| truthy(v)).cloned());
    }
    Ok(None)
}
async fn work(
    mut receiver: mpsc::Receiver<Request>,
    config: EncoderConfig,
    cancel: CancellationToken,
    metrics: Arc<Metrics>,
) {
    let mut tasks: JoinSet<(u64, Result<PreparedDetection>)> = JoinSet::new();
    let mut jobs = BTreeMap::<u64, Job>::new();
    let sequence = AtomicU64::new(1);
    loop {
        tokio::select! {biased;
            _=cancel.cancelled()=>break,
            result=tasks.join_next(),if !tasks.is_empty()=>{
                match result {
                    Some(Ok((id,result)))=>{metrics.probes.fetch_sub(1,Ordering::Relaxed);if let Some(job)=jobs.remove(&id) {for reply in job.replies {let _=reply.sender.send(result.clone());}}},
                    Some(Err(_))=>{cancel.cancel();break;},
                    None=>{},
                }
            },
            request=receiver.recv()=>{
                let Some(request)=request else {break};
                let input=request.input;let id=sequence.fetch_add(1,Ordering::Relaxed);
                let fingerprint=match fingerprint(&input) {Ok(value)=>value,Err(error)=>{let _=request.reply.sender.send(Err(error));continue;}};
                if !request.force {
                    if let Some((_,job))=jobs.iter_mut().rev().find(|(_,job)|job.fingerprint==fingerprint) {job.replies.push(request.reply);continue;}
                }
                let direct=if input.executable.is_none() {Some((DetectionSource::Missing,unavailable()))} else if !request.force {
                    match cached(&input) {Ok(value)=>value.map(|value|(DetectionSource::Cache,value)),Err(error)=>{let _=request.reply.sender.send(Err(error));continue;}}
                } else {None};
                if let Some((source,detection))=direct {let _=request.reply.sender.send(Ok(PreparedDetection {generation:input.generation,probe_id:id,source,detection,cache_write:None,diagnostics:vec![]}));continue;}
                if jobs.len()>=config.max_probes {let _=request.reply.sender.send(Err(EncoderError::new("probe_limit","Encoder probe concurrency limit reached")));continue;}
                jobs.insert(id,Job {fingerprint,replies:vec![request.reply]});metrics.probes.fetch_add(1,Ordering::Relaxed);
                let (config,cancel,metrics)=(config.clone(),cancel.clone(),metrics.clone());
                tasks.spawn(async move {(id,probe(input,id,&config,&cancel,&metrics).await)});
            }
        }
    }
    receiver.close();
    while let Ok(request) = receiver.try_recv() {
        let _ = request.reply.sender.send(Err(EncoderError::new(
            "cancelled",
            "Encoder service shut down",
        )));
    }
    while let Some(result) = tasks.join_next().await {
        if let Ok((id, result)) = result {
            metrics.probes.fetch_sub(1, Ordering::Relaxed);
            if let Some(job) = jobs.remove(&id) {
                for reply in job.replies {
                    let _ = reply.sender.send(result.clone());
                }
            }
        }
    }
    for (_, job) in jobs {
        metrics.probes.fetch_sub(1, Ordering::Relaxed);
        for reply in job.replies {
            let _ = reply.sender.send(Err(EncoderError::new(
                "worker_panicked",
                "Encoder probe task failed",
            )));
        }
    }
}
async fn probe(
    input: FrozenDetectionInput,
    id: u64,
    config: &EncoderConfig,
    cancel: &CancellationToken,
    metrics: &Arc<Metrics>,
) -> Result<PreparedDetection> {
    let deadline = Instant::now() + config.operation_timeout;
    let executable = &input.executable.as_ref().unwrap().path;
    let mut diagnostics = vec![];
    let listed = match process::run(
        executable,
        &["-hide_banner".into(), "-encoders".into()],
        config,
        config.listing_limit,
        cancel,
        deadline,
        metrics,
    )
    .await
    {
        Ok(output) => profiles::parse_available_encoders(&output.stdout),
        Err(failure)
            if failure.error.code == "cancelled" || failure.error.code == "reap_failed" =>
        {
            return Err(failure.error)
        }
        Err(failure) => {
            diagnostics.push(Diagnostic {
                code: failure.error.code,
                message: failure.reason,
            });
            vec![]
        }
    };
    let mut encoders = vec![];
    for profile in profiles::profiles(&input.vaapi_device) {
        if cancel.is_cancelled() {
            return Err(EncoderError::new(
                "cancelled",
                "Encoder detection cancelled",
            ));
        }
        if !profiles::supported(profile.id, &input.platform) {
            continue;
        }
        let mut entry = json!({"id":profile.id,"label":profile.label,"hardware":profile.hardware,"available":false});
        if !listed.contains(&profile.id) {
            entry["reason"] = json!("Not included in this FFmpeg build");
        } else {
            if Instant::now() >= deadline {
                return Err(EncoderError::new(
                    "operation_timeout",
                    "Encoder operation deadline reached",
                ));
            }
            match process::run(
                executable,
                &profiles::test_encode_command(profile.id, &input.vaapi_device)?,
                config,
                config.encode_limit,
                cancel,
                deadline,
                metrics,
            )
            .await
            {
                Ok(_) => entry["available"] = json!(true),
                Err(failure)
                    if failure.error.code == "cancelled" || failure.error.code == "reap_failed" =>
                {
                    return Err(failure.error)
                }
                Err(failure) => {
                    entry["reason"] = json!(failure.reason);
                    diagnostics.push(Diagnostic {
                        code: failure.error.code,
                        message: format!("{}: {}", profile.id, failure.error.message),
                    });
                }
            }
        }
        encoders.push(entry);
    }
    let recommended = encoders
        .iter()
        .find(|value| value["hardware"] == true && value["available"] == true)
        .map(|v| v["id"].as_str().unwrap())
        .unwrap_or("x264");
    let detection = json!({"encoders":encoders,"recommended":recommended});
    let key = input.cache_key()?.unwrap();
    let bytes = serde_json::to_vec(&json!({"version":1,"key":key,"detection":detection}))
        .map_err(|e| EncoderError::new("cache_serialization", e.to_string()))?;
    Ok(PreparedDetection {
        generation: input.generation,
        probe_id: id,
        source: DetectionSource::Probe,
        detection,
        diagnostics,
        cache_write: Some(CacheWriteIntent {
            generation: input.generation,
            key,
            bytes,
        }),
    })
}
