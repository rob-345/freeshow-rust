use crate::*;
use socket2::{SockRef, TcpKeepalive};
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot, Notify, OwnedSemaphorePermit, Semaphore},
    time::{Instant, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;
static OWNER: AtomicBool = AtomicBool::new(false);
type Connect = Pin<Box<dyn Future<Output = Result<TcpStream>> + Send>>;
enum Operation {
    Audio {
        packet: Vec<u8>,
        config: Option<Config>,
    },
    Metadata(String),
    Disconnect,
    #[cfg(test)]
    Hold {
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    },
}
struct Command {
    op: Operation,
    reply: oneshot::Sender<Result<MetadataReceipt>>,
    _bytes: OwnedSemaphorePermit,
}
struct Shared {
    closing: AtomicBool,
    wake: CancellationToken,
    done: AtomicBool,
    notify: Notify,
    event_notify: Notify,
    terminal: Mutex<Option<Error>>,
    events: Mutex<VecDeque<StatusEvent>>,
    event_limit: usize,
}
impl Shared {
    fn fail(&self, e: Error) {
        let mut error = self.terminal.lock().unwrap_or_else(|e| e.into_inner());
        if error.is_none() {
            *error = Some(e);
        }
        self.closing.store(true, Ordering::Release);
        self.wake.cancel();
    }
    fn error(&self) -> Option<Error> {
        self.terminal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}
struct Exit(Arc<Shared>);
impl Drop for Exit {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0
                .fail(Error::new("owner_panicked", "Icecast owner panicked"));
        }
        self.0.closing.store(true, Ordering::Release);
        self.0.done.store(true, Ordering::Release);
        OWNER.store(false, Ordering::Release);
        self.0.notify.notify_waiters();
        self.0.event_notify.notify_waiters();
    }
}
/// Exactly one live owner per process, matching the original static sender.
/// Accepted commands own their buffers/effects even if their awaiting caller is dropped.
pub struct IcecastService {
    context: ContextId,
    limits: Limits,
    sender: mpsc::Sender<Command>,
    bytes: Arc<Semaphore>,
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
impl IcecastService {
    pub fn start(context: ContextId, limits: Limits) -> Result<Self> {
        limits.validate()?;
        if OWNER
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Error::new("owner_exists", "Icecast has one global owner"));
        }
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                OWNER.store(false, Ordering::Release);
                return Err(Error::new("runtime_unavailable", e));
            }
        };
        let (sender, receiver) = mpsc::channel(limits.queue);
        let shared = Arc::new(Shared {
            closing: AtomicBool::new(false),
            wake: CancellationToken::new(),
            done: AtomicBool::new(false),
            notify: Notify::new(),
            event_notify: Notify::new(),
            terminal: Mutex::new(None),
            events: Mutex::new(VecDeque::new()),
            event_limit: limits.events,
        });
        let worker_shared = shared.clone();
        let worker_limits = limits.clone();
        let thread = std::thread::Builder::new()
            .name("freeshow-icecast".into())
            .spawn(move || {
                let _exit = Exit(worker_shared.clone());
                runtime.block_on(async move {
                    let mut owner = Owner::new(context, worker_limits, worker_shared.clone());
                    if let Err(e) = owner.run(receiver).await {
                        worker_shared.fail(e);
                    }
                    if let Err(e) = owner.disconnect(None).await {
                        worker_shared.fail(e);
                    }
                });
            })
            .map_err(|e| {
                OWNER.store(false, Ordering::Release);
                Error::new("worker_unavailable", e)
            })?;
        Ok(Self {
            context,
            bytes: Arc::new(Semaphore::new(limits.queued_bytes)),
            limits,
            sender,
            shared,
            thread: Mutex::new(Some(thread)),
        })
    }
    fn check(&self, context: ContextId, cancel: &CancellationToken) -> Result<()> {
        if context != self.context {
            return Err(Error::new(
                "stale_context",
                "Icecast context retired or foreign",
            ));
        }
        if self.shared.closing.load(Ordering::Acquire) {
            return Err(self
                .terminal_error()
                .unwrap_or_else(|| Error::new("shutdown", "Icecast owner closed")));
        }
        if cancel.is_cancelled() {
            return Err(Error::new("cancelled", "Cancelled before admission"));
        }
        Ok(())
    }
    async fn submit(
        &self,
        context: ContextId,
        op: impl FnOnce() -> Operation,
        bytes: usize,
        cancel: &CancellationToken,
    ) -> Result<Receipt> {
        let observed = self.submit_observed(context, op, bytes, cancel).await?;
        match observed.diagnostic {
            Some(error) => Err(error),
            None => Ok(observed.receipt),
        }
    }
    async fn submit_observed(
        &self,
        context: ContextId,
        op: impl FnOnce() -> Operation,
        bytes: usize,
        cancel: &CancellationToken,
    ) -> Result<MetadataReceipt> {
        self.check(context, cancel)?;
        let permit = self
            .bytes
            .clone()
            .try_acquire_many_owned(bytes.max(1) as u32)
            .map_err(|_| Error::new("busy", "Icecast queued byte limit"))?;
        // Serialize shutdown/admission through channel closure; a queued command is accepted
        // at the reserved channel send and deliberately ignores subsequent request
        // cancellation/caller Drop. Receiver::close also waits for reserved slots.
        self.check(context, cancel)?;
        let slot = self.sender.try_reserve().map_err(|e| {
            if self.shared.closing.load(Ordering::Acquire) {
                return Error::new("shutdown", "Icecast owner closing");
            }
            match e {
                mpsc::error::TrySendError::Full(_) => {
                    Error::new("busy", "Icecast command queue full")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    Error::new("shutdown", "Icecast owner closed")
                }
            }
        })?;
        self.check(context, cancel)?;
        let (reply, receiver) = oneshot::channel();
        slot.send(Command {
            op: op(),
            reply,
            _bytes: permit,
        });
        receiver.await.map_err(|_| {
            self.terminal_error().unwrap_or_else(|| {
                Error::new("owner_unavailable", "Icecast owner stopped without receipt")
            })
        })?
    }
    pub async fn send_audio(
        &self,
        context: ContextId,
        packet: &[u8],
        config: Option<&serde_json::Value>,
        cancel: CancellationToken,
    ) -> Result<Receipt> {
        self.check(context, &cancel)?;
        if packet.len() > self.limits.packet_bytes {
            return Err(Error::new("resource_limit", "Encoded packet byte limit"));
        }
        let config = Config::parse(config, &self.limits)?;
        let bytes = packet.len() + config.as_ref().map_or(0, Config::storage);
        self.submit(
            context,
            || Operation::Audio {
                packet: packet.to_vec(),
                config,
            },
            bytes,
            &cancel,
        )
        .await
    }
    pub async fn update_metadata(
        &self,
        context: ContextId,
        title: &str,
        cancel: CancellationToken,
    ) -> Result<Receipt> {
        let observed = self
            .update_metadata_observed(context, title, cancel)
            .await?;
        match observed.diagnostic {
            Some(error) => Err(error),
            None => Ok(observed.receipt),
        }
    }
    /// Unlike the legacy error projection, retains actual title application when
    /// connected header regeneration fails. Admission/context errors remain Err.
    pub async fn update_metadata_observed(
        &self,
        context: ContextId,
        title: &str,
        cancel: CancellationToken,
    ) -> Result<MetadataReceipt> {
        self.check(context, &cancel)?;
        if title.len() > self.limits.metadata_bytes
            || title.bytes().any(|b| b == 0 || b == b'\r' || b == b'\n')
        {
            return Err(Error::new(
                "unsupported_metadata",
                "Song title size/control characters",
            ));
        }
        self.submit_observed(
            context,
            || Operation::Metadata(title.into()),
            title.len(),
            &cancel,
        )
        .await
    }
    pub async fn disconnect(
        &self,
        context: ContextId,
        cancel: CancellationToken,
    ) -> Result<Receipt> {
        self.submit(context, || Operation::Disconnect, 1, &cancel)
            .await
    }
    pub fn take_events(&self) -> Vec<StatusEvent> {
        self.shared
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }
    /// Waits without an idle poll/timer. One trusted host consumer should drain statuses.
    pub async fn next_events(&self) -> Result<Vec<StatusEvent>> {
        loop {
            let notified = self.shared.event_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let events = self.take_events();
            if !events.is_empty() {
                return Ok(events);
            }
            if self.shared.done.load(Ordering::Acquire) {
                return Err(self
                    .terminal_error()
                    .unwrap_or_else(|| Error::new("shutdown", "Icecast events closed")));
            }
            notified.await;
        }
    }
    pub fn terminal_error(&self) -> Option<Error> {
        self.shared.error()
    }
    /// Retires the frozen context, closes admission, drains accepted effects, closes the socket.
    /// No caller runtime is required; this future can be awaited on any executor.
    pub async fn shutdown(&self) -> Result<()> {
        self.shared.closing.store(true, Ordering::Release);
        self.shared.wake.cancel();
        loop {
            let notified = self.shared.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.shared.done.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
        let handle = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        // Exit signals just before actual OS thread exit; final TLS/destructors can still
        // make this join briefly blocking. DNS is process-wide and is not joined here.
        if let Some(handle) = handle {
            if handle.join().is_err() {
                self.shared
                    .fail(Error::new("owner_panicked", "Icecast owner join failed"));
            }
        }
        self.terminal_error().map_or(Ok(()), Err)
    }
    pub async fn retire(&self, context: ContextId) -> Result<()> {
        if context != self.context {
            return Err(Error::new("stale_context", "Foreign retirement"));
        }
        self.shutdown().await
    }
}
impl Drop for IcecastService {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        self.shared.wake.cancel();
    }
}

struct Owner {
    context: ContextId,
    limits: Limits,
    shared: Arc<Shared>,
    config: Option<Config>,
    song: String,
    socket: Option<TcpStream>,
    connect: Option<Connect>,
    timer: Option<tokio::time::Interval>,
    ogg: OggState,
    run: u64,
    last_attempt: Option<Instant>,
    last_real: Instant,
    status: Status,
    response_bytes: usize,
}
impl Owner {
    fn new(context: ContextId, limits: Limits, shared: Arc<Shared>) -> Self {
        Self {
            context,
            limits,
            shared,
            config: None,
            song: String::new(),
            socket: None,
            connect: None,
            timer: None,
            ogg: OggState::new(0),
            run: 0,
            last_attempt: None,
            last_real: Instant::now(),
            status: Status {
                connected: false,
                connecting: false,
                error: None,
            },
            response_bytes: 0,
        }
    }
    fn status(&mut self, connected: bool, connecting: bool, error: Option<String>) -> Result<()> {
        self.status_cause(connected, connecting, error, None)
    }
    fn status_cause(
        &mut self,
        connected: bool,
        connecting: bool,
        error: Option<String>,
        diagnostic: Option<Error>,
    ) -> Result<()> {
        let next = Status {
            connected,
            connecting,
            error,
        };
        if self.status == next {
            return Ok(());
        }
        self.status = next.clone();
        let mut events = self.shared.events.lock().unwrap_or_else(|e| e.into_inner());
        if events.len() >= self.shared.event_limit {
            return Err(Error::new(
                "event_overflow",
                "Icecast status queue full; host must drain events",
            ));
        }
        events.push_back(StatusEvent {
            context: self.context,
            run: self.run,
            data: next,
            diagnostic,
        });
        self.shared.event_notify.notify_waiters();
        Ok(())
    }
    fn serial() -> Result<u32> {
        let mut bytes = [0; 4];
        getrandom::fill(&mut bytes).map_err(|e| Error::new("random_unavailable", e))?;
        Ok(u32::from_le_bytes(bytes))
    }
    fn begin_connect(&mut self) -> Result<bool> {
        // Date.now starts at the Unix epoch. In real supported runs it is already>3s;
        // elapsed cooldown uses a monotonic clock, avoiding system-clock jumps.
        if self
            .last_attempt
            .is_some_and(|last| last.elapsed() < Duration::from_millis(3000))
        {
            return Ok(false);
        }
        self.last_attempt = Some(Instant::now());
        self.run = self
            .run
            .checked_add(1)
            .ok_or_else(|| Error::new("resource_limit", "Icecast run counter overflow"))?;
        self.ogg = OggState::new(Self::serial()?);
        self.response_bytes = 0;
        self.status(false, true, None)?;
        let config = self.config.as_ref().expect("enabled config").clone();
        let duration = self.limits.connect_timeout;
        self.connect = Some(Box::pin(async move {
            tokio::time::timeout(duration, async move {
                let addresses = resolver::resolve(&config.host, config.port).await?;
                let mut last = Error::new("connect_failed", "No resolved socket addresses");
                for address in addresses {
                    match TcpStream::connect(address).await {
                        Ok(stream) => {
                            SockRef::from(&stream)
                                .set_tcp_keepalive(
                                    &TcpKeepalive::new().with_time(Duration::from_secs(10)),
                                )
                                .map_err(|e| Error::new("keepalive_failed", e))?;
                            return Ok(stream);
                        }
                        Err(e) => last = Error::new("connect_failed", e),
                    }
                }
                Err(last)
            })
            .await
            .map_err(|_| Error::new("connect_timeout", "Icecast DNS/connect deadline"))?
        }));
        Ok(true)
    }
    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let Some(socket) = self.socket.as_mut() else {
            return Err(Error::new("not_connected", "Icecast socket absent"));
        };
        tokio::time::timeout(self.limits.write_timeout, socket.write_all(bytes))
            .await
            .map_err(|_| Error::new("write_timeout", "Icecast socket write deadline"))?
            .map_err(|e| Error::new("socket_write", e))
    }
    async fn headers(&mut self) -> Result<()> {
        let bytes = self.ogg.headers(&self.song)?;
        self.write(&bytes).await
    }
    async fn connected(&mut self, socket: TcpStream) -> Result<()> {
        self.connect = None;
        self.socket = Some(socket);
        let header = self
            .config
            .as_ref()
            .expect("connection config")
            .request_header(&self.song, &self.limits)?;
        self.write(&header).await?;
        self.status(true, false, None)?;
        self.headers().await?;
        self.last_real = Instant::now();
        let mut timer = tokio::time::interval_at(
            Instant::now() + Duration::from_millis(20),
            Duration::from_millis(20),
        );
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        self.timer = Some(timer);
        Ok(())
    }
    async fn disconnect(&mut self, error: Option<Error>) -> Result<()> {
        let had_socket = self.socket.is_some() || self.connect.is_some() || self.status.connecting;
        let close_clears_error = had_socket && error.is_some();
        self.timer = None;
        self.connect = None;
        let mut failure = None;
        if self.status.connected && self.socket.is_some() {
            match self.ogg.eos() {
                Ok(bytes) => {
                    if let Err(e) = self.write(&bytes).await {
                        failure = Some(Error::new("teardown_failed", e));
                    }
                }
                Err(e) => failure = Some(e),
            }
        }
        if let Some(mut socket) = self.socket.take() {
            match tokio::time::timeout(self.limits.write_timeout, socket.shutdown()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    failure.get_or_insert_with(|| Error::new("teardown_failed", e));
                }
                Err(_) => {
                    failure.get_or_insert_with(|| {
                        Error::new("teardown_timeout", "Socket shutdown deadline")
                    });
                }
            }
        }
        let status = self.status_cause(
            false,
            false,
            error.as_ref().map(|e| e.message.clone()),
            error,
        );
        // net.Socket.destroy() later emits close, whose original callback calls
        // disconnect() again and clears the previously reported error.
        let close_status = if close_clears_error {
            self.status(false, false, None)
        } else {
            Ok(())
        };
        if let Some(e) = failure {
            self.shared.fail(e.clone());
            return Err(e);
        }
        status.and(close_status)
    }
    async fn operation(&mut self, op: Operation) -> Result<MetadataReceipt> {
        let disposition = match op {
            Operation::Audio { packet, config } => {
                let Some(config) = config else {
                    return Ok(self.observation(Disposition::Ignored, None));
                };
                if self.config.as_ref().is_some_and(|old| old != &config) {
                    self.last_attempt = None;
                    self.disconnect(None).await?;
                }
                self.config = Some(config);
                if !self.config.as_ref().unwrap().enabled {
                    if self.status.connected || self.status.connecting {
                        self.disconnect(None).await?;
                    }
                    Disposition::Disabled
                } else if !self.status.connected {
                    if self.connect.is_some() || self.begin_connect()? {
                        Disposition::ConnectingDropped
                    } else {
                        Disposition::CooldownDropped
                    }
                } else if packet.is_empty() {
                    Disposition::Ignored
                } else {
                    self.last_real = Instant::now();
                    let bytes = self.ogg.audio(&packet)?;
                    if let Err(e) = self.write(&bytes).await {
                        let _ = self.disconnect(None).await;
                        return Err(e);
                    }
                    Disposition::Written
                }
            }
            Operation::Metadata(title) => {
                if self.song != title {
                    self.song = title;
                    if self.status.connected {
                        let diagnostic = async {
                            self.ogg = OggState::new(Self::serial()?);
                            self.last_real = Instant::now();
                            self.headers().await
                        }
                        .await
                        .err();
                        return Ok(self.observation(Disposition::Updated, diagnostic));
                    }
                }
                Disposition::Updated
            }
            Operation::Disconnect => {
                self.disconnect(None).await?;
                Disposition::Disconnected
            }
            #[cfg(test)]
            Operation::Hold { entered, release } => {
                let _ = entered.send(());
                let _ = release.await;
                Disposition::Ignored
            }
        };
        Ok(self.observation(disposition, None))
    }
    fn observation(&self, disposition: Disposition, diagnostic: Option<Error>) -> MetadataReceipt {
        MetadataReceipt {
            receipt: self.receipt(disposition),
            diagnostic,
        }
    }
    fn receipt(&self, disposition: Disposition) -> Receipt {
        Receipt {
            context: self.context,
            run: self.run,
            disposition,
        }
    }
    fn silence_due(&self) -> bool {
        self.last_real.elapsed() >= Duration::from_millis(60)
    }
    async fn run(&mut self, mut receiver: mpsc::Receiver<Command>) -> Result<()> {
        let mut draining = false;
        let mut response = [0u8; 8192];
        loop {
            tokio::select! {
                _=self.shared.wake.cancelled(),if !draining=>{draining=true;receiver.close();self.timer=None;},
                command=receiver.recv()=>match command{
                    Some(command)=>{
                        let result=if let Some(error)=self.shared.error(){Err(error)}else{self.operation(command.op).await};
                        if let Some(error)=match &result {Err(error)=>Some(error),Ok(observed)=>observed.diagnostic.as_ref()}{
                            if matches!(error.code,"socket_write"|"write_timeout"){let _=self.disconnect(None).await;}
                            else if matches!(error.code,"event_overflow"|"resource_limit"|"random_unavailable"){self.shared.fail(error.clone());}
                        }
                        let _=command.reply.send(result);
                        if self.shared.error().is_some(){draining=true;receiver.close();self.timer=None;}
                    },
                    None=>break,
                },
                result=async{self.connect.as_mut().unwrap().await},if self.connect.is_some()&&!draining=>{
                    self.connect=None;
                    match result{Ok(socket)=>{if let Err(e)=self.connected(socket).await{self.disconnect(Some(e)).await?;}},Err(e)=>self.disconnect(Some(e)).await?}
                },
                read=async{self.socket.as_mut().unwrap().read(&mut response).await},if self.socket.is_some()&&!draining=>{
                    match read{
                        Ok(0)=>{self.disconnect(None).await?;},
                        Ok(n)=>{self.response_bytes=self.response_bytes.saturating_add(n);if self.response_bytes>self.limits.response_bytes{return Err(Error::new("resource_limit","Icecast response byte limit"));}
                            let text=String::from_utf8_lossy(&response[..n]);
                            let rejection=if text.contains("409 Conflict")||text.contains("Mountpoint in use"){Some("Mountpoint in use".into())}
                            else if text.contains("401 Unauthorized")||text.contains("403 Forbidden")||text.contains("404 Not Found"){Some(ogg::js_trim(&text).split("\r\n").next().unwrap_or_default().to_owned())}else{None};
                            if rejection.as_ref().is_some_and(|s|s.len()>2048){return Err(Error::new("resource_limit","Icecast status error byte limit"));}
                            if let Some(error)=rejection{self.disconnect(Some(Error::new("server_rejected",error))).await?;}
                        },Err(e)=>self.disconnect(Some(Error::new("socket_read",e))).await?,
                    }
                },
                _=async{self.timer.as_mut().unwrap().tick().await},if self.timer.is_some()&&!draining=>{
                    if self.silence_due(){let bytes=self.ogg.audio(SILENCE)?;if self.write(&bytes).await.is_err(){self.disconnect(None).await?;}}
                },
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
