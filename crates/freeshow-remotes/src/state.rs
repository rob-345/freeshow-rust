use crate::{
    RemoteConfig, RemoteError, RemoteEvent, RemoteEventKind, RemoteGeneration, RemoteOrigin,
    RemoteStats, Result, ServerKind, StartSpec,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub struct RemoteEvents {
    pub(crate) controls: mpsc::Receiver<RemoteEvent>,
    pub(crate) data: mpsc::Receiver<RemoteEvent>,
    control_head: Option<RemoteEvent>,
    data_head: Option<RemoteEvent>,
}
impl RemoteEvents {
    pub(crate) fn new(
        controls: mpsc::Receiver<RemoteEvent>,
        data: mpsc::Receiver<RemoteEvent>,
    ) -> Self {
        Self {
            controls,
            data,
            control_head: None,
            data_head: None,
        }
    }
    pub async fn recv(&mut self) -> Option<RemoteEvent> {
        loop {
            if self.control_head.is_none() {
                self.control_head = self.controls.try_recv().ok();
            }
            if self.data_head.is_none() {
                self.data_head = self.data.try_recv().ok();
            }
            match (&self.control_head, &self.data_head) {
                (Some(a), Some(b)) => {
                    return if a.generation == b.generation && b.sequence < a.sequence {
                        self.data_head.take()
                    } else {
                        self.control_head.take()
                    }
                }
                (Some(_), None) => return self.control_head.take(),
                (None, Some(_)) => return self.data_head.take(),
                (None, None) => {}
            }
            if self.controls.is_closed() && self.data.is_closed() {
                return None;
            }
            tokio::select! {value=self.controls.recv(),if !self.controls.is_closed()=>{self.control_head=value;},value=self.data.recv(),if !self.data.is_closed()=>{self.data_head=value;},else=>return None}
        }
    }
}
#[derive(Clone, Copy)]
pub(crate) enum SendMode {
    Renderer,
    Backend,
    Subscribers,
}
pub(crate) struct Command {
    pub server: ServerKind,
    pub message: Value,
    pub mode: SendMode,
    pub reply: oneshot::Sender<Result<()>>,
}
pub(crate) struct Client {
    pub disconnect: mpsc::OwnedPermit<RemoteEvent>,
    pub _permit: OwnedSemaphorePermit,
}
pub(crate) struct Run {
    pub config: RemoteConfig,
    pub generation: RemoteGeneration,
    pub spec: StartSpec,
    pub cancel: CancellationToken,
    pub tracker: TaskTracker,
    pub engine_sessions: Arc<Semaphore>,
    pub tcp_connections: Arc<Semaphore>,
    pub client_limit: Arc<Semaphore>,
    pub clients: Mutex<HashMap<(ServerKind, String), Client>>,
    pub subscribers: Mutex<Vec<(String, u64)>>,
    pub controls: mpsc::Sender<RemoteEvent>,
    pub events: mpsc::Sender<RemoteEvent>,
    pub commands: mpsc::Sender<Command>,
    pub polling_sids: Arc<Mutex<HashSet<String>>>,
    pub runtime: OnceLock<tokio::runtime::Handle>,
    pub gate: Arc<Mutex<HashMap<String, bool>>>,
    pub dropped_diagnostics: AtomicU64,
    pub static_requests: Arc<Semaphore>,
    pub cover: Option<Arc<dyn crate::CoverSource>>,
    pub file_failure: Mutex<Option<RemoteError>>,
    sequence: AtomicU64,
    admission: Mutex<bool>,
    #[cfg(test)]
    pub handshake_pause: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
    #[cfg(test)]
    pub transport_fault: (Mutex<Option<bool>>, tokio::sync::Notify),
    #[cfg(test)]
    pub socket_send_buffer: std::sync::atomic::AtomicUsize,
}
impl Run {
    pub fn new(
        config: RemoteConfig,
        generation: RemoteGeneration,
        spec: StartSpec,
        controls: mpsc::Sender<RemoteEvent>,
        events: mpsc::Sender<RemoteEvent>,
        gate: Arc<Mutex<HashMap<String, bool>>>,
        cover: Option<Arc<dyn crate::CoverSource>>,
    ) -> (Arc<Self>, mpsc::Receiver<Command>) {
        let (commands, receiver) = mpsc::channel(config.command_capacity);
        (
            Arc::new(Self {
                engine_sessions: Arc::new(Semaphore::new(config.max_engine_sessions)),
                tcp_connections: Arc::new(Semaphore::new(config.max_tcp_connections)),
                client_limit: Arc::new(Semaphore::new(config.max_clients)),
                static_requests: Arc::new(Semaphore::new(
                    config.static_workers + config.static_queue,
                )),
                config,
                cover,
                file_failure: Mutex::new(None),
                generation,
                spec,
                cancel: CancellationToken::new(),
                tracker: TaskTracker::new(),
                clients: Mutex::new(HashMap::new()),
                subscribers: Mutex::new(vec![]),
                controls,
                events,
                commands,
                polling_sids: Arc::new(Mutex::new(HashSet::new())),
                runtime: OnceLock::new(),
                gate,
                dropped_diagnostics: AtomicU64::new(0),
                sequence: AtomicU64::new(0),
                admission: Mutex::new(true),
                #[cfg(test)]
                handshake_pause: Mutex::new(None),
                #[cfg(test)]
                transport_fault: (Mutex::new(None), tokio::sync::Notify::new()),
                #[cfg(test)]
                socket_send_buffer: std::sync::atomic::AtomicUsize::new(0),
            }),
            receiver,
        )
    }
    pub fn event(&self, server: ServerKind, kind: RemoteEventKind) -> RemoteEvent {
        RemoteEvent {
            generation: self.generation.clone(),
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            server,
            kind,
        }
    }
    pub fn diagnostic(&self, server: ServerKind, error: RemoteError) {
        if self
            .controls
            .try_send(self.event(
                server,
                RemoteEventKind::Diagnostic {
                    code: error.code,
                    message: error.message,
                },
            ))
            .is_err()
        {
            self.dropped_diagnostics.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(
        &self,
        future: F,
    ) -> Result<()> {
        let admission = self.admission.lock().unwrap();
        if !*admission || self.cancel.is_cancelled() {
            return Err(RemoteError::new(
                "cancelled",
                "Remote task admission closed",
            ));
        }
        self.tracker.spawn(future);
        Ok(())
    }
    pub fn close_admission(&self) {
        *self.admission.lock().unwrap() = false;
        self.cancel.cancel();
    }
    pub fn disconnect(&self, server: ServerKind, id: &str) {
        if let Some(client) = self.clients.lock().unwrap().remove(&(server, id.into())) {
            if server == ServerKind::STAGE {
                self.subscribers
                    .lock()
                    .unwrap()
                    .retain(|(key, _)| key != id);
            }
            client.disconnect.send(self.event(
                server,
                RemoteEventKind::Renderer {
                    origin: RemoteOrigin::Disconnect,
                    source_socket: Some(id.into()),
                    message: json!({"channel":"DISCONNECT","id":id}),
                },
            ));
        }
    }
    pub fn close_clients(&self) {
        let clients = std::mem::take(&mut *self.clients.lock().unwrap());
        for ((server, id), client) in clients {
            client.disconnect.send(self.event(
                server,
                RemoteEventKind::Renderer {
                    origin: RemoteOrigin::Disconnect,
                    source_socket: Some(id.clone()),
                    message: json!({"channel":"DISCONNECT","id":id}),
                },
            ));
        }
        self.subscribers.lock().unwrap().clear();
    }
    pub fn subscriber_ids(&self) -> Vec<String> {
        let now = (self.config.clock)();
        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.retain(|(_, expiry)| *expiry >= now);
        subscribers.iter().map(|(id, _)| id.clone()).collect()
    }
    pub fn gate(&self, message: &Value, done: bool) -> Result<bool> {
        let key = crate::semantics::stream_key(message)?;
        if key == "__proto__" {
            return Ok(true);
        }
        let mut gate = self.gate.lock().unwrap();
        if !done && gate.get(&key) == Some(&false) {
            return Ok(false);
        }
        if !gate.contains_key(&key) && gate.len() >= self.config.max_response_ids {
            return Err(RemoteError::new(
                "response_id_limit",
                "Stream response state reached capacity",
            ));
        }
        gate.insert(key, done);
        Ok(true)
    }
    pub fn stats(&self) -> RemoteStats {
        let clients = self.clients.lock().unwrap();
        RemoteStats {
            generation: self.generation.clone(),
            tcp_connections: self.config.max_tcp_connections
                - self.tcp_connections.available_permits(),
            engine_sessions: self.config.max_engine_sessions
                - self.engine_sessions.available_permits(),
            clients: ServerKind::ALL
                .into_iter()
                .map(|kind| {
                    (
                        kind,
                        clients.keys().filter(|(server, _)| *server == kind).count(),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
            subscribers: self.subscriber_ids().len(),
            queued_commands: self.commands.max_capacity() - self.commands.capacity(),
            runtime_tasks: self
                .runtime
                .get()
                .map_or(0, |handle| handle.metrics().num_alive_tasks()),
            dropped_diagnostics: self.dropped_diagnostics.load(Ordering::Relaxed),
            static_requests: self.config.static_workers + self.config.static_queue
                - self.static_requests.available_permits(),
        }
    }
}
#[derive(Clone)]
pub(crate) struct SessionLease(pub Arc<Lease>);
pub(crate) struct Lease {
    pub _permit: OwnedSemaphorePermit,
    pub sid: Mutex<Option<String>>,
    pub sids: Weak<Mutex<HashSet<String>>>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let (Some(sids), Some(sid)) = (self.sids.upgrade(), self.sid.get_mut().unwrap().take()) {
            sids.lock().unwrap().remove(&sid);
        }
    }
}
