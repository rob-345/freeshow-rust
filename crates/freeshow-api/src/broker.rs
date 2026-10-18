use crate::{semantics::truthy, ApiConfig, ApiError, ApiEvent, ApiEventKind, ApiGeneration};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use uuid::Uuid;

type Reply = Result<Value, ApiError>;
struct Pending {
    dynamic: bool,
    reply: oneshot::Sender<Reply>,
}
#[derive(Default)]
struct Table {
    pending: HashMap<String, Pending>,
    finished: VecDeque<String>,
}

pub(crate) struct Run {
    pub generation: ApiGeneration,
    pub config: ApiConfig,
    pub password: String,
    pub events: mpsc::Sender<ApiEvent>,
    pub cancel: CancellationToken,
    pub tracker: TaskTracker,
    pub inflight: Arc<Semaphore>,
    pub engine_sessions: Arc<Semaphore>,
    pub tcp_connections: Arc<Semaphore>,
    pending: Mutex<Table>,
    pub dropped_diagnostics: AtomicU64,
    pub polling_sids: Arc<Mutex<HashSet<String>>>,
    pub runtime: std::sync::OnceLock<tokio::runtime::Handle>,
    admission: Mutex<bool>,
    #[cfg(test)]
    pub handshake_pause: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
}
impl Run {
    pub fn new(
        generation: ApiGeneration,
        config: ApiConfig,
        password: String,
        events: mpsc::Sender<ApiEvent>,
    ) -> Arc<Self> {
        Arc::new(Self {
            generation,
            password,
            events,
            cancel: CancellationToken::new(),
            tracker: TaskTracker::new(),
            inflight: Arc::new(Semaphore::new(config.max_inflight)),
            engine_sessions: Arc::new(Semaphore::new(config.max_engine_sessions)),
            tcp_connections: Arc::new(Semaphore::new(config.max_tcp_connections)),
            config,
            pending: Mutex::new(Table::default()),
            polling_sids: Arc::new(Mutex::new(HashSet::new())),
            runtime: std::sync::OnceLock::new(),
            admission: Mutex::new(true),
            #[cfg(test)]
            handshake_pause: Mutex::new(None),
            dropped_diagnostics: AtomicU64::new(0),
        })
    }
    pub fn spawn<F>(&self, future: F) -> Result<(), ApiError>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let admission = self.admission.lock().unwrap();
        if !*admission || self.cancel.is_cancelled() {
            return Err(ApiError::new("cancelled", "API task admission is closed"));
        }
        self.tracker.spawn(future);
        Ok(())
    }
    pub fn close_admission(&self) {
        *self.admission.lock().unwrap() = false;
        self.cancel.cancel();
    }
    pub fn event(&self, kind: ApiEventKind) -> Result<(), ApiError> {
        if self.cancel.is_cancelled() {
            return Err(ApiError::new("cancelled", "API run was stopped"));
        }
        self.events
            .try_send(ApiEvent {
                generation: self.generation.clone(),
                kind,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => {
                    ApiError::new("event_queue_full", "API host event queue is full")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    ApiError::new("host_unavailable", "API host event receiver is closed")
                }
            })
    }
    pub fn diagnostic(&self, code: &'static str, message: impl Into<String>) {
        if self
            .event(ApiEventKind::Diagnostic {
                code,
                message: message.into(),
            })
            .is_err()
        {
            self.dropped_diagnostics.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().pending.len()
    }
    fn remember(&self, table: &mut Table, id: String) {
        table.finished.push_back(id);
        while table.finished.len() > self.config.max_pending_replies * 2 {
            table.finished.pop_front();
        }
    }
    fn register(
        self: &Arc<Self>,
        dynamic: bool,
    ) -> Result<(PendingGuard, oneshot::Receiver<Reply>), ApiError> {
        if self.cancel.is_cancelled() {
            return Err(ApiError::new("cancelled", "API run was stopped"));
        }
        let mut table = self.pending.lock().unwrap();
        if table.pending.len() >= self.config.max_pending_replies {
            return Err(ApiError::new(
                "pending_limit",
                "API pending reply limit reached",
            ));
        }
        let prefix = if dynamic { "GET_DYNAMIC_VALUES" } else { "api" };
        let id = format!(
            "{prefix}:{}:{}",
            self.generation.run,
            Uuid::new_v4().simple()
        );
        let (reply, receiver) = oneshot::channel();
        table.pending.insert(id.clone(), Pending { dynamic, reply });
        Ok((
            PendingGuard {
                run: self.clone(),
                id,
            },
            receiver,
        ))
    }
    fn complete(
        &self,
        generation: &ApiGeneration,
        id: &str,
        dynamic: bool,
        value: Reply,
    ) -> Result<(), ApiError> {
        if generation != &self.generation || self.cancel.is_cancelled() {
            return Err(ApiError::new(
                "stale_generation",
                "API reply belongs to a stopped or previous run",
            ));
        }
        let prefix = format!(
            "{}:{}:",
            if dynamic { "GET_DYNAMIC_VALUES" } else { "api" },
            self.generation.run
        );
        if !id.starts_with(&prefix) {
            return Err(ApiError::new(
                "stale_generation",
                "API correlation ID does not belong to this run",
            ));
        }
        let mut table = self.pending.lock().unwrap();
        let Some(pending) = table.pending.get(id) else {
            return Err(ApiError::new(
                if table.finished.iter().any(|done| done == id) {
                    "duplicate_reply"
                } else {
                    "unknown_reply"
                },
                "API reply is no longer pending",
            ));
        };
        if pending.dynamic != dynamic {
            return Err(ApiError::new(
                "invalid_reply",
                "API reply kind does not match its request",
            ));
        }
        let pending = table.pending.remove(id).unwrap();
        self.remember(&mut table, id.to_owned());
        drop(table);

        let oversized = value.is_err();
        pending.reply.send(value).map_err(|_| {
            ApiError::new("cancelled", "API request was cancelled before its reply")
        })?;
        if oversized {
            return Err(ApiError::new(
                "response_too_large",
                "API host reply exceeds payload limit",
            ));
        }
        Ok(())
    }
    pub fn complete_action(
        &self,
        generation: &ApiGeneration,
        payload: &Value,
    ) -> Result<(), ApiError> {
        let value = payload
            .as_object()
            .ok_or_else(|| ApiError::new("invalid_reply", "API_TRIGGER requires an object"))?;
        let id = value
            .get("returnId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ApiError::new("invalid_reply", "API_TRIGGER requires a private returnId")
            })?;
        // Count serialization without allocating an oversized encoded buffer;
        // only clone a response after it is known to fit the configured bound.
        let response = check_size(payload, self.config.max_socket_payload).map(|_| {
            let mut response = value.clone();
            response.remove("returnId");
            crate::semantics::normalize(Value::Object(response))
        });
        self.complete(generation, id, false, response)
    }
    pub fn complete_dynamic(
        &self,
        generation: &ApiGeneration,
        id: &str,
        value: Value,
    ) -> Result<(), ApiError> {
        let response = check_size(&value, self.config.max_socket_payload)
            .map(|_| crate::semantics::normalize(value));
        self.complete(generation, id, true, response)
    }
    pub fn cancel_pending(&self) {
        let mut table = self.pending.lock().unwrap();
        for (id, pending) in std::mem::take(&mut table.pending) {
            self.remember(&mut table, id);
            let _ = pending.reply.send(Err(ApiError::new(
                "cancelled",
                "API pending host request was cancelled",
            )));
        }
    }
    async fn wait(
        &self,
        receiver: oneshot::Receiver<Reply>,
        dynamic: bool,
        client: &CancellationToken,
    ) -> Result<Option<Value>, ApiError> {
        let duration = if dynamic {
            self.config.dynamic_timeout
        } else {
            self.config.request_timeout
        };
        tokio::select! { biased;
            _=self.cancel.cancelled()=>Err(ApiError::new("cancelled","API run was stopped")),
            _=client.cancelled()=>Err(ApiError::new("cancelled","API client disconnected")),
            result=tokio::time::timeout(duration,receiver)=>match result{
                Ok(Ok(result))=>result.map(Some),Ok(Err(_))=>Err(ApiError::new("host_unavailable","API host reply channel closed")),Err(_)=>Ok(None),
            }
        }
    }
    pub async fn received(
        self: &Arc<Self>,
        mut data: Value,
        client: &CancellationToken,
    ) -> Result<Option<Value>, ApiError> {
        let Some(action) = data.get("action").filter(|v| truthy(v)) else {
            return Ok(None);
        };
        let action = action
            .as_str()
            .ok_or_else(|| ApiError::new("invalid_action", "API action must be a string"))?;
        let correlated = action.starts_with("get_") || data.get("returnId").is_some_and(truthy);
        if !correlated {
            self.event(ApiEventKind::Trigger { data })?;
            return Ok(None);
        }
        let (guard, receiver) = self.register(false)?;
        data.as_object_mut()
            .ok_or_else(|| ApiError::new("invalid_data", "API action payload must be an object"))?
            .insert("returnId".into(), Value::String(guard.id.clone()));
        self.event(ApiEventKind::Trigger { data })?;
        self.wait(receiver, false, client).await
    }
    pub async fn dynamic(
        self: &Arc<Self>,
        keys: Value,
        client: &CancellationToken,
    ) -> Result<Value, ApiError> {
        let (guard, receiver) = self.register(true)?;
        self.event(ApiEventKind::DynamicValues {
            listener_id: guard.id.clone(),
            keys,
        })?;
        let result = self
            .wait(receiver, true, client)
            .await?
            .filter(truthy)
            .unwrap_or(json!({}));
        Ok(json!({"isVariable":true,"values":result}))
    }
}
struct PendingGuard {
    run: Arc<Run>,
    id: String,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        let mut table = self.run.pending.lock().unwrap();
        if table.pending.remove(&self.id).is_some() {
            self.run.remember(&mut table, self.id.clone());
        }
    }
}

fn check_size(value: &Value, maximum: usize) -> Result<(), ApiError> {
    struct Counter {
        remaining: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.remaining {
                return Err(std::io::ErrorKind::FileTooLarge.into());
            }
            self.remaining -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter { remaining: maximum }, value)
        .map_err(|_| ApiError::new("response_too_large", "API host reply exceeds payload limit"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(capacity: usize) -> (Arc<Run>, mpsc::Receiver<ApiEvent>) {
        let config = ApiConfig {
            event_capacity: capacity,
            max_pending_replies: 1,
            max_socket_payload: 512,
            ..ApiConfig::default()
        };
        let (tx, rx) = mpsc::channel(capacity);
        (
            Run::new(
                ApiGeneration {
                    context: 1,
                    run: "unit".into(),
                },
                config,
                String::new(),
                tx,
            ),
            rx,
        )
    }
    #[tokio::test]
    async fn full_or_closed_host_queue_releases_pending_registration() {
        let (run, rx) = run(1);
        let client = CancellationToken::new();
        run.received(json!({"action":"next"}), &client)
            .await
            .unwrap();
        assert_eq!(
            run.received(json!({"action":"get_value"}), &client)
                .await
                .unwrap_err()
                .code,
            "event_queue_full"
        );
        assert_eq!(run.pending_count(), 0);
        drop(rx);
        assert_eq!(
            run.received(json!({"action":"get_value"}), &client)
                .await
                .unwrap_err()
                .code,
            "host_unavailable"
        );
        assert_eq!(run.pending_count(), 0);
    }
    #[tokio::test]
    async fn oversized_completion_fails_both_host_and_waiter_then_recovers() {
        let (run, mut rx) = run(4);
        let task_run = run.clone();
        let waiter = tokio::spawn(async move {
            task_run
                .received(json!({"action":"get_value"}), &CancellationToken::new())
                .await
        });
        let ApiEventKind::Trigger { data } = rx.recv().await.unwrap().kind else {
            panic!("trigger")
        };
        let id = data["returnId"].clone();
        let payload = json!({"returnId":id,"data":"x".repeat(1024)});
        assert_eq!(
            run.complete_action(&run.generation, &payload)
                .unwrap_err()
                .code,
            "response_too_large"
        );
        assert_eq!(
            waiter.await.unwrap().unwrap_err().code,
            "response_too_large"
        );
        assert_eq!(run.pending_count(), 0);
        assert_eq!(
            run.complete_action(&run.generation, &payload)
                .unwrap_err()
                .code,
            "duplicate_reply"
        );
    }
    #[tokio::test]
    async fn dynamic_cancel_rejects_late_completion_and_reclaims_limit() {
        let (run, mut rx) = run(4);
        let task_run = run.clone();
        let waiter = tokio::spawn(async move {
            task_run
                .dynamic(json!(["{show}"]), &CancellationToken::new())
                .await
        });
        let ApiEventKind::DynamicValues { listener_id, .. } = rx.recv().await.unwrap().kind else {
            panic!("dynamic")
        };
        run.cancel_pending();
        assert_eq!(waiter.await.unwrap().unwrap_err().code, "cancelled");
        assert_eq!(
            run.complete_dynamic(&run.generation, &listener_id, json!({}))
                .unwrap_err()
                .code,
            "duplicate_reply"
        );
        assert_eq!(run.pending_count(), 0);
    }
    #[tokio::test]
    async fn closed_task_admission_cannot_spawn_after_tracker_wait() {
        let (run, _) = run(1);
        run.close_admission();
        run.tracker.close();
        run.tracker.wait().await;
        let task = async { panic!("closed admission spawned a task") };
        assert_eq!(run.spawn(task).unwrap_err().code, "cancelled");
        assert!(run.tracker.is_empty());
    }
}
