use crate::{
    records::{instance_name, query_response, SERVICE_TYPE},
    *,
};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::BTreeMap,
    future::pending,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
};
use tokio::{
    net::UdpSocket,
    process::{Child, Command},
    sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore},
    time::{timeout, Instant},
};
use tokio_util::sync::CancellationToken;

pub type DiscoveryEvents = mpsc::Receiver<DiscoveryEvent>;
struct Job {
    command: CommandKind,
    reply: oneshot::Sender<Result<Reply>>,
    _permit: OwnedSemaphorePermit,
}
enum CommandKind {
    #[cfg(test)]
    Pause(oneshot::Sender<()>, oneshot::Receiver<()>),
    Begin(DiscoveryGeneration, NetworkSnapshot),
    Publish(DiscoveryGeneration, DiscoveryName, u16),
    Stop(DiscoveryGeneration),
}
enum Reply {
    Begin(BeginReceipt),
    Publish(PublishReceipt),
    Unit,
}
struct Inner {
    jobs: mpsc::Sender<Job>,
    slots: Arc<Semaphore>,
    terminal: CancellationToken,
    closing: AtomicBool,
    done: watch::Receiver<Option<Result<()>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    stats: Arc<Mutex<DiscoveryStats>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.terminal.cancel();
    }
}
#[derive(Clone)]
pub struct DiscoveryService {
    inner: Arc<Inner>,
}
fn closed() -> DiscoveryError {
    DiscoveryError::new("discovery_closed", "Discovery owner is closed")
}
fn io(error: impl std::fmt::Display) -> DiscoveryError {
    DiscoveryError::new("discovery_io", error.to_string())
}
fn stale() -> DiscoveryError {
    DiscoveryError::new(
        "stale_generation",
        "Discovery request belongs to a stopped/previous run",
    )
}
impl DiscoveryConfig {
    fn validate(&self) -> Result<()> {
        if !(1..=64).contains(&self.command_capacity)
            || !(1..=1024).contains(&self.event_capacity)
            || !(1..=1024).contains(&self.responses_per_second)
            || self.probe_jitter > Duration::from_millis(250)
        {
            return Err(DiscoveryError::new(
                "invalid_discovery_configuration",
                "Invalid discovery queue/rate/jitter limit",
            ));
        }
        match &self.transport {
            DiscoveryTransport::Multicast => {}
            DiscoveryTransport::Loopback { bind, target } => {
                if !bind.ip().is_loopback() || !target.ip().is_loopback() || target.port() == 0 {
                    return Err(DiscoveryError::new(
                        "invalid_discovery_configuration",
                        "Controlled transport requires loopback endpoints and nonzero peer port",
                    ));
                }
            }
        }
        if let DiscoveryPlatform::Darwin { dns_sd } = &self.platform {
            if !cfg!(unix) {
                return Err(DiscoveryError::new(
                    "unsupported_discovery_platform",
                    "Native dns-sd requires Unix process supervision",
                ));
            }
            let path = dns_sd.as_ref().ok_or_else(|| {
                DiscoveryError::new(
                    "dns_sd_unavailable",
                    "Trusted native dns-sd executable was not supplied",
                )
            })?;
            if path.as_os_str().len() > 4096
                || !path.is_absolute()
                || !std::fs::metadata(path)
                    .map_err(|e| DiscoveryError::new("dns_sd_unavailable", e.to_string()))?
                    .is_file()
            {
                return Err(DiscoveryError::new(
                    "dns_sd_unavailable",
                    "dns-sd authority must name an absolute regular executable",
                ));
            }
        }
        Ok(())
    }
}
impl DiscoveryService {
    /// No publication occurs until begin/publish. Caller must supply the same
    /// random three-byte instance ID for this process across library runs.
    pub fn new(config: DiscoveryConfig) -> Result<(Self, DiscoveryEvents)> {
        config.validate()?;
        let (jobs, receive) = mpsc::channel(config.command_capacity);
        let (events, event_rx) = mpsc::channel(config.event_capacity);
        let terminal = CancellationToken::new();
        let cancellation = terminal.clone();
        let (finished, done) = watch::channel(None);
        let stats = Arc::new(Mutex::new(DiscoveryStats::default()));
        let thread_stats = stats.clone();
        let capacity = config.command_capacity;
        let thread = std::thread::Builder::new()
            .name("freeshow-discovery".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(io)?;
                    let result = runtime.block_on(owner(
                        config,
                        receive,
                        events,
                        cancellation,
                        thread_stats,
                    ));
                    drop(runtime);
                    result
                }))
                .unwrap_or_else(|_| {
                    Err(DiscoveryError::new(
                        "discovery_panicked",
                        "Discovery owner panicked",
                    ))
                });
                finished.send_replace(Some(result));
            })
            .map_err(io)?;
        Ok((
            Self {
                inner: Arc::new(Inner {
                    jobs,
                    slots: Arc::new(Semaphore::new(capacity)),
                    terminal,
                    closing: AtomicBool::new(false),
                    done,
                    thread: Mutex::new(Some(thread)),
                    stats,
                }),
            },
            event_rx,
        ))
    }
    async fn request(&self, command: CommandKind) -> Result<Reply> {
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        let permit = self.inner.slots.clone().try_acquire_owned().map_err(|_| {
            DiscoveryError::new("discovery_busy", "Discovery job admission is full")
        })?;
        let (reply, response) = oneshot::channel();
        self.inner
            .jobs
            .try_send(Job {
                command,
                reply,
                _permit: permit,
            })
            .map_err(|_| closed())?;
        response.await.map_err(|_| closed())?
    }
    pub async fn begin(
        &self,
        generation: DiscoveryGeneration,
        snapshot: NetworkSnapshot,
    ) -> Result<BeginReceipt> {
        generation.validate()?;
        snapshot.validate()?;
        match self
            .request(CommandKind::Begin(generation, snapshot))
            .await?
        {
            Reply::Begin(value) => Ok(value),
            _ => unreachable!(),
        }
    }
    pub async fn publish(
        &self,
        generation: &DiscoveryGeneration,
        name: DiscoveryName,
        port: u16,
    ) -> Result<PublishReceipt> {
        generation.validate()?;
        match self
            .request(CommandKind::Publish(generation.clone(), name, port))
            .await?
        {
            Reply::Publish(value) => Ok(value),
            _ => unreachable!(),
        }
    }
    pub async fn stop(&self, generation: &DiscoveryGeneration) -> Result<()> {
        generation.validate()?;
        self.request(CommandKind::Stop(generation.clone()))
            .await
            .map(|_| ())
    }
    pub fn stats(&self) -> DiscoveryStats {
        let mut stats = self
            .inner
            .stats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        stats.admitted_jobs = self.inner.jobs.max_capacity() - self.inner.slots.available_permits();
        stats
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.closing.store(true, Ordering::Release);
        self.inner.terminal.cancel();
        let mut done = self.inner.done.clone();
        let result = loop {
            if let Some(result) = done.borrow_and_update().clone() {
                break result;
            }
            done.changed().await.map_err(|_| closed())?;
        };
        if let Some(thread) = self
            .inner
            .thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            thread.join().map_err(|_| {
                DiscoveryError::new("discovery_panicked", "Discovery thread join failed")
            })?;
        }
        result
    }
}
struct Run {
    generation: DiscoveryGeneration,
    snapshot: NetworkSnapshot,
    txt_ips: Vec<String>,
    socket: Option<Arc<UdpSocket>>,
    target: SocketAddrV4,
    entries: BTreeMap<DiscoveryName, Entry>,
    config: DiscoveryConfig,
    events: mpsc::Sender<DiscoveryEvent>,
    stats: Arc<Mutex<DiscoveryStats>>,
    rate_start: Instant,
    child_poll_at: Instant,
    responses: usize,
}
struct Entry {
    records: Vec<Record>,
    phase: Phase,
}
enum Phase {
    Browsing(Instant),
    Probing {
        at: Instant,
        sent: u8,
    },
    Published {
        at: Option<Instant>,
        delay: Duration,
    },
    Failed,
    Child(Child),
}
impl Run {
    fn event(&self, name: Option<DiscoveryName>, kind: DiscoveryEventKind) {
        if self
            .events
            .try_send(DiscoveryEvent {
                generation: self.generation.clone(),
                name,
                kind,
            })
            .is_err()
        {
            self.stats
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .dropped_events += 1;
        }
    }
    fn diagnostic(&self, name: Option<DiscoveryName>, error: DiscoveryError) {
        self.event(
            name,
            DiscoveryEventKind::Diagnostic {
                code: error.code,
                message: error.message,
            },
        );
    }
    fn sync_stats(&self) {
        let mut stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        stats.generation = Some(self.generation.clone());
        stats.services = self.entries.len();
        stats.children = self
            .entries
            .values()
            .filter(|v| matches!(v.phase, Phase::Child(_)))
            .count();
        stats.socket_open = self.socket.is_some();
    }
    async fn send(&self, packet: Packet) -> Result<()> {
        let bytes = encode(&packet)?;
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| io("mDNS transport absent"))?;
        match timeout(
            Duration::from_millis(250),
            socket.send_to(&bytes, self.target),
        )
        .await
        {
            Ok(Ok(n)) if n == bytes.len() => Ok(()),
            Ok(Ok(_)) => Err(io("Incomplete UDP send")),
            Ok(Err(error)) => Err(io(error)),
            Err(_) => Err(DiscoveryError::new(
                "discovery_send_timeout",
                "UDP send exceeded its deadline",
            )),
        }
    }
    async fn query(&self, name: String, kind: RecordType) -> Result<()> {
        self.send(Packet {
            questions: vec![Question { name, kind }],
            ..Packet::default()
        })
        .await
    }
    async fn publish(&mut self, name: DiscoveryName, port: u16) -> Result<PublishReceipt> {
        if port == 0 {
            return Err(DiscoveryError::new(
                "invalid_discovery_port",
                "Publication requires the actual nonzero bound server port",
            ));
        }
        if self.entries.contains_key(&name) {
            return Ok(PublishReceipt::AlreadyPresent);
        }
        let records = crate::records::service_records_with_ips(
            &self.snapshot,
            &self.txt_ips,
            name,
            port,
            self.config.instance_id,
        )?;
        let mut registry = Vec::new();
        for record in self
            .entries
            .values()
            .flat_map(|entry| &entry.records)
            .chain(&records)
        {
            if !registry.contains(record) {
                registry.push(record.clone());
            }
        }
        encode(&Packet {
            response: true,
            answers: registry,
            ..Packet::default()
        })?;
        let phase = match &self.config.platform {
            DiscoveryPlatform::Other => {
                self.query(SERVICE_TYPE.into(), RecordType::PTR).await?;
                Phase::Browsing(Instant::now() + Duration::from_secs(1))
            }
            DiscoveryPlatform::Darwin { dns_sd } => {
                let args = crate::records::dns_sd_args_with_ips(
                    &self.txt_ips,
                    name,
                    port,
                    self.config.instance_id,
                );
                if args.iter().map(|v| v.len()).sum::<usize>() > 8192 {
                    return Err(DiscoveryError::new(
                        "discovery_argument_limit",
                        "dns-sd arguments exceed 8192 bytes",
                    ));
                }
                let mut command = Command::new(dns_sd.as_ref().unwrap());
                command
                    .args(args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true);
                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt;
                    command.as_std_mut().process_group(0);
                }
                let child = command
                    .spawn()
                    .map_err(|e| DiscoveryError::new("dns_sd_spawn", e.to_string()))?;
                self.event(Some(name), DiscoveryEventKind::Spawned);
                Phase::Child(child)
            }
        };
        self.entries.insert(name, Entry { records, phase });
        self.sync_stats();
        Ok(PublishReceipt::Scheduled)
    }
    fn next_deadline(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter_map(|entry| match entry.phase {
                Phase::Browsing(at)
                | Phase::Probing { at, .. }
                | Phase::Published { at: Some(at), .. } => Some(at),
                Phase::Child(_) => Some(self.child_poll_at),
                _ => None,
            })
            .min()
    }
    async fn tick(&mut self) {
        let names: Vec<_> = self.entries.keys().copied().collect();
        for name in names {
            let mut entry = self.entries.remove(&name).unwrap();
            let now = Instant::now();
            let mut keep = true;
            let result = match &mut entry.phase {
                Phase::Browsing(at) if now >= *at => {
                    entry.phase = Phase::Probing {
                        at: now + self.config.probe_jitter,
                        sent: 0,
                    };
                    Ok(())
                }
                Phase::Probing { at, sent } if now >= *at => {
                    if *sent < 3 {
                        let result = self
                            .query(
                                format!(
                                    "{}.{}",
                                    instance_name(name, self.config.instance_id),
                                    SERVICE_TYPE
                                ),
                                RecordType::ANY,
                            )
                            .await;
                        *sent += 1;
                        *at = Instant::now() + Duration::from_millis(250);
                        result
                    } else {
                        let result = self
                            .send(Packet {
                                response: true,
                                answers: entry.records.clone(),
                                ..Packet::default()
                            })
                            .await;
                        if result.is_ok() {
                            self.event(Some(name), DiscoveryEventKind::Published);
                            entry.phase = Phase::Published {
                                at: Some(Instant::now() + Duration::from_secs(3)),
                                delay: Duration::from_secs(3),
                            };
                        } else {
                            entry.phase = Phase::Failed;
                        }
                        result
                    }
                }
                Phase::Published {
                    at: Some(at),
                    delay,
                } if now >= *at => {
                    let result = self
                        .send(Packet {
                            response: true,
                            answers: entry.records.clone(),
                            ..Packet::default()
                        })
                        .await;
                    *delay *= 3;
                    if *delay < Duration::from_secs(3600) {
                        *at = Instant::now() + *delay;
                    } else {
                        entry.phase = Phase::Published {
                            at: None,
                            delay: *delay,
                        };
                    }
                    result
                }
                Phase::Child(child) => match child.try_wait() {
                    Ok(Some(status)) => {
                        keep = false;
                        Err(DiscoveryError::new(
                            "dns_sd_exited",
                            format!("dns-sd exited unexpectedly: {status}"),
                        ))
                    }
                    Ok(None) => Ok(()),
                    Err(error) => Err(io(error)),
                },
                _ => Ok(()),
            };
            if let Err(error) = result {
                self.diagnostic(Some(name), error);
            }
            if keep {
                self.entries.insert(name, entry);
            }
        }
        self.child_poll_at = Instant::now() + Duration::from_millis(100);
        self.sync_stats();
    }
    async fn received(&mut self, packet: Packet) {
        if packet.response {
            let records: Vec<_> = packet.answers.iter().chain(&packet.additionals).collect();
            let names: Vec<_> = self.entries.keys().copied().collect();
            for name in names {
                let entry = self.entries.get(&name).unwrap();
                let fullname = format!(
                    "{}.{}",
                    instance_name(name, self.config.instance_id),
                    SERVICE_TYPE
                );
                let browsing = matches!(entry.phase, Phase::Browsing(_));
                let conflict = if browsing {
                    records
                        .iter()
                        .filter(|r| {
                            r.ttl > 0
                                && r.kind == RecordType::PTR
                                && r.name.eq_ignore_ascii_case(SERVICE_TYPE)
                        })
                        .any(|ptr| {
                            if let RecordData::Name(target) = &ptr.data {
                                records.iter().any(|srv| {
                                    srv.ttl > 0
                                        && srv.kind == RecordType::SRV
                                        && srv.name.eq_ignore_ascii_case(target)
                                        && srv.name.split('.').next().is_some_and(|n| {
                                            n.starts_with(&format!("freeshow-{}-", name.as_str()))
                                        })
                                })
                            } else {
                                false
                            }
                        })
                } else if matches!(entry.phase,Phase::Probing{sent,..}if sent>0) {
                    records
                        .iter()
                        .any(|r| r.name.eq_ignore_ascii_case(&fullname))
                } else {
                    false
                };
                if conflict {
                    self.event(
                        Some(name),
                        DiscoveryEventKind::Suppressed {
                            reason: if browsing {
                                "existing_service_prefix"
                            } else {
                                "instance_name_conflict"
                            },
                        },
                    );
                    if browsing {
                        self.entries.remove(&name);
                    } else {
                        let entry = self.entries.get_mut(&name).unwrap();
                        entry.phase = Phase::Failed;
                        let mut records = entry.records.clone();
                        for r in &mut records {
                            r.ttl = 0;
                        }
                        if let Err(error) = self
                            .send(Packet {
                                response: true,
                                answers: records,
                                ..Packet::default()
                            })
                            .await
                        {
                            self.diagnostic(Some(name), error);
                        }
                    }
                }
            }
        } else {
            let mut registry = Vec::new();
            for entry in self
                .entries
                .values()
                .filter(|e| matches!(e.phase, Phase::Published { .. }))
            {
                for record in &entry.records {
                    if !registry.contains(record) {
                        registry.push(record.clone());
                    }
                }
            }
            for question in &packet.questions {
                let response = query_response(&registry, question);
                if response.answers.is_empty() {
                    continue;
                }
                if self.rate_start.elapsed() >= Duration::from_secs(1) {
                    self.rate_start = Instant::now();
                    self.responses = 0;
                }
                if self.responses >= self.config.responses_per_second {
                    self.stats
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .dropped_responses += 1;
                    continue;
                }
                self.responses += 1;
                if let Err(error) = self.send(response).await {
                    self.diagnostic(None, error);
                }
            }
        }
        self.sync_stats();
    }
    async fn stop(&mut self) -> Result<()> {
        let mut failure = None;
        for (_, mut entry) in std::mem::take(&mut self.entries) {
            match &mut entry.phase {
                Phase::Child(child) => {
                    if let Err(error) = stop_child(child).await {
                        failure.get_or_insert(error);
                    }
                }
                Phase::Probing { .. } | Phase::Published { .. } => {
                    for record in &mut entry.records {
                        record.ttl = 0;
                    }
                    if let Err(error) = self
                        .send(Packet {
                            response: true,
                            answers: entry.records,
                            ..Packet::default()
                        })
                        .await
                    {
                        failure.get_or_insert(error);
                    }
                }
                _ => {}
            }
        }
        self.socket.take();
        self.sync_stats();
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
async fn stop_child(child: &mut Child) -> Result<()> {
    if child.try_wait().map_err(io)?.is_some() {
        return Ok(());
    }
    #[cfg(unix)]
    if let Some(id) = child.id() {
        unsafe {
            libc::kill(-(id as i32), libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    child.start_kill().map_err(io)?;
    if let Ok(result) = timeout(Duration::from_millis(250), child.wait()).await {
        return result.map(|_| ()).map_err(io);
    }
    #[cfg(unix)]
    if let Some(id) = child.id() {
        unsafe {
            libc::kill(-(id as i32), libc::SIGKILL);
        }
    }
    child.start_kill().map_err(io)?;
    child.wait().await.map_err(io)?;
    Ok(())
}
fn open_socket(
    config: &DiscoveryConfig,
    snapshot: &NetworkSnapshot,
) -> Result<(Option<Arc<UdpSocket>>, SocketAddrV4)> {
    if matches!(config.platform, DiscoveryPlatform::Darwin { .. }) {
        return Ok((None, SocketAddrV4::new(Ipv4Addr::LOCALHOST, 5353)));
    }
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(io)?;
    socket.set_reuse_address(true).map_err(io)?;
    let (bind, target) = match config.transport {
        DiscoveryTransport::Multicast => (
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 5353),
            SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353),
        ),
        DiscoveryTransport::Loopback { bind, target } => (bind, target),
    };
    socket
        .bind(&bind.into())
        .map_err(|e| DiscoveryError::new("discovery_bind", e.to_string()))?;
    if matches!(config.transport, DiscoveryTransport::Multicast) {
        let mut joined = false;
        let mut last = None;
        for interface in snapshot.memberships() {
            match socket.join_multicast_v4(target.ip(), &interface) {
                Ok(()) => joined = true,
                Err(error) => last = Some(error),
            }
        }
        if !joined {
            return Err(io(last
                .map(|e| e.to_string())
                .unwrap_or_else(|| "No IPv4 multicast interface".into())));
        }
        socket.set_multicast_ttl_v4(255).map_err(io)?;
        socket.set_multicast_loop_v4(true).map_err(io)?;
        let interface = if snapshot
            .interfaces
            .iter()
            .flat_map(|i| &i.addresses)
            .any(|a| !a.internal && a.is_v4())
        {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        };
        socket.set_multicast_if_v4(&interface).map_err(io)?;
    }
    socket.set_nonblocking(true).map_err(io)?;
    let socket: std::net::UdpSocket = socket.into();
    Ok((
        Some(Arc::new(UdpSocket::from_std(socket).map_err(io)?)),
        target,
    ))
}
fn clear_active(stats: &Mutex<DiscoveryStats>) {
    let mut stats = stats.lock().unwrap_or_else(|e| e.into_inner());
    stats.generation = None;
    stats.services = 0;
    stats.children = 0;
    stats.socket_open = false;
}
#[allow(clippy::too_many_arguments)]
async fn command(
    command: CommandKind,
    run: &mut Option<Run>,
    txt_ips: &mut Option<Vec<String>>,
    config: &DiscoveryConfig,
    events: &mpsc::Sender<DiscoveryEvent>,
    stats: &Arc<Mutex<DiscoveryStats>>,
    _terminal: &CancellationToken,
) -> Result<Reply> {
    match command {
        #[cfg(test)]
        CommandKind::Pause(entered, release) => {
            let _ = entered.send(());
            tokio::select! { _=release=>{}, _=_terminal.cancelled()=>{} };
            Ok(Reply::Unit)
        }
        CommandKind::Begin(generation, snapshot) => {
            generation.validate()?;
            snapshot.validate()?;
            let ips = txt_ips.get_or_insert(get_local_ips(&snapshot)?).clone();
            if let Some(previous) = run.as_mut() {
                let result = previous.stop().await;
                run.take();
                clear_active(stats);
                result?;
            }
            let (socket, target) = open_socket(config, &snapshot)?;
            let endpoint = socket
                .as_ref()
                .map(|s| s.local_addr().map_err(io))
                .transpose()?
                .map(|a| match a {
                    std::net::SocketAddr::V4(a) => a,
                    _ => unreachable!(),
                });
            let next = Run {
                generation: generation.clone(),
                snapshot,
                txt_ips: ips,
                socket,
                target,
                entries: BTreeMap::new(),
                config: config.clone(),
                events: events.clone(),
                stats: stats.clone(),
                rate_start: Instant::now(),
                child_poll_at: Instant::now(),
                responses: 0,
            };
            next.sync_stats();
            *run = Some(next);
            Ok(Reply::Begin(BeginReceipt {
                generation,
                endpoint,
            }))
        }
        CommandKind::Publish(generation, name, port) => {
            match run.as_mut().filter(|r| r.generation == generation) {
                Some(run) => run.publish(name, port).await.map(Reply::Publish),
                None => Err(stale()),
            }
        }
        CommandKind::Stop(generation) => {
            match run.as_mut().filter(|r| r.generation == generation) {
                Some(current) => {
                    let result = current.stop().await.map(|_| Reply::Unit);
                    run.take();
                    clear_active(stats);
                    result
                }
                None => Err(stale()),
            }
        }
    }
}
async fn owner(
    config: DiscoveryConfig,
    mut jobs: mpsc::Receiver<Job>,
    events: mpsc::Sender<DiscoveryEvent>,
    terminal: CancellationToken,
    stats: Arc<Mutex<DiscoveryStats>>,
) -> Result<()> {
    let mut run: Option<Run> = None;
    let mut txt_ips: Option<Vec<String>> = None;
    let mut buffer = vec![0; 9001];
    loop {
        let socket = run.as_ref().and_then(|r| r.socket.clone());
        let deadline = run.as_ref().and_then(Run::next_deadline);
        tokio::select! {biased;
            _ = terminal.cancelled() => break,
            _ = async { match deadline { Some(at) => tokio::time::sleep_until(at).await, None => pending().await } } => if let Some(run) = run.as_mut() { run.tick().await; },
            Some(job) = jobs.recv() => {
                let result = command(job.command, &mut run, &mut txt_ips, &config, &events, &stats, &terminal).await;
                let _ = job.reply.send(result);
            },
            received = async { match socket { Some(socket) => socket.recv_from(&mut buffer).await, None => pending().await } } => {
                if let Some(run) = run.as_mut() {
                    match received {
                        Ok((n, _)) => match decode(&buffer[..n]) { Ok(packet) => run.received(packet).await, Err(error) => run.diagnostic(None, error) },
                        Err(error) => run.diagnostic(None, io(error)),
                    }
                }
            },
            else => break,
        }
    }
    jobs.close();
    while let Ok(job) = jobs.try_recv() {
        let _ = job.reply.send(Err(closed()));
    }
    let result = match run.as_mut() {
        Some(run) => run.stop().await,
        None => Ok(()),
    };
    let mut final_stats = stats.lock().unwrap_or_else(|e| e.into_inner());
    final_stats.generation = None;
    final_stats.services = 0;
    final_stats.children = 0;
    final_stats.socket_open = false;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> NetworkSnapshot {
        NetworkSnapshot {
            hostname: "fixture-host".into(),
            interfaces: Vec::new(),
        }
    }
    fn generation(n: u64) -> DiscoveryGeneration {
        DiscoveryGeneration {
            context: n,
            run: format!("{n:032x}"),
        }
    }
    fn config(capacity: usize) -> DiscoveryConfig {
        let mut config = DiscoveryConfig::new([1, 2, 3]);
        config.platform = DiscoveryPlatform::Other;
        config.command_capacity = capacity;
        config.transport = DiscoveryTransport::Loopback {
            bind: "127.0.0.1:0".parse().unwrap(),
            target: "127.0.0.1:53535".parse().unwrap(),
        };
        config
    }
    async fn paused(
        service: &DiscoveryService,
    ) -> (tokio::task::JoinHandle<Result<Reply>>, oneshot::Sender<()>) {
        let (entered, wait) = oneshot::channel();
        let (release, receiver) = oneshot::channel();
        let cloned = service.clone();
        let task =
            tokio::spawn(
                async move { cloned.request(CommandKind::Pause(entered, receiver)).await },
            );
        wait.await.unwrap();
        (task, release)
    }
    #[tokio::test(flavor = "current_thread")]
    async fn accepted_begin_survives_caller_abort() {
        let (service, _events) = DiscoveryService::new(config(2)).unwrap();
        let (pause, release) = paused(&service).await;
        let cloned = service.clone();
        let task = tokio::spawn(async move { cloned.begin(generation(1), snapshot()).await });
        timeout(Duration::from_secs(1), async {
            while service.stats().admitted_jobs != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        task.abort();
        release.send(()).unwrap();
        pause.await.unwrap().unwrap();
        timeout(Duration::from_secs(1), async {
            while service.stats().generation.is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(service.stats().socket_open);
        service.shutdown().await.unwrap();
        assert_eq!(service.stats().admitted_jobs, 0);
        assert!(!service.stats().socket_open);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn terminal_cancellation_bypasses_saturated_and_dropped_requests() {
        let (service, _events) = DiscoveryService::new(config(2)).unwrap();
        let (pause, _release) = paused(&service).await;
        let cloned = service.clone();
        let task = tokio::spawn(async move { cloned.begin(generation(1), snapshot()).await });
        timeout(Duration::from_secs(1), async {
            while service.stats().admitted_jobs != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            service.stop(&generation(1)).await.unwrap_err().code,
            "discovery_busy"
        );
        task.abort();
        pause.abort();
        timeout(Duration::from_secs(1), service.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(service.stats().admitted_jobs, 0);
        assert_eq!(
            service
                .publish(&generation(1), DiscoveryName::Remote, 5510)
                .await
                .unwrap_err()
                .code,
            "discovery_closed"
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn stale_requests_and_missing_native_binary_are_explicit() {
        let (service, _events) = DiscoveryService::new(config(2)).unwrap();
        service.begin(generation(1), snapshot()).await.unwrap();
        service.begin(generation(2), snapshot()).await.unwrap();
        assert_eq!(
            service
                .publish(&generation(1), DiscoveryName::Remote, 5510)
                .await
                .unwrap_err()
                .code,
            "stale_generation"
        );
        assert_eq!(
            service.stop(&generation(1)).await.unwrap_err().code,
            "stale_generation"
        );
        service.stop(&generation(2)).await.unwrap();
        service.shutdown().await.unwrap();
        let mut c = config(2);
        c.platform = DiscoveryPlatform::Darwin { dns_sd: None };
        assert_eq!(
            DiscoveryService::new(c).err().unwrap().code,
            if cfg!(unix) {
                "dns_sd_unavailable"
            } else {
                "unsupported_discovery_platform"
            }
        );
    }
    #[test]
    fn pure_preparation_and_dns_allocation_bounds() {
        let mut s = snapshot();
        s.hostname = "x".repeat(64);
        assert!(get_local_ips(&s).is_err());
        let p = Packet {
            response: true,
            answers: vec![Record {
                name: "x".into(),
                kind: RecordType::TXT,
                ttl: 1,
                data: RecordData::Txt(vec![Vec::new(); 20_000]),
            }],
            ..Packet::default()
        };
        assert!(encode(&p).is_err());
        let c = config(0);
        assert!(DiscoveryService::new(c).is_err());
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    #[test]
    fn full_event_queue_caps_output_and_retains_drop_count() {
        let (events, mut receive) = mpsc::channel(1);
        let stats = Arc::new(Mutex::new(DiscoveryStats::default()));
        let run = Run {
            generation: DiscoveryGeneration {
                context: 1,
                run: "0".repeat(32),
            },
            snapshot: NetworkSnapshot {
                hostname: "fixture".into(),
                interfaces: Vec::new(),
            },
            txt_ips: vec!["localhost".into()],
            socket: None,
            target: "127.0.0.1:53535".parse().unwrap(),
            entries: BTreeMap::new(),
            config: DiscoveryConfig::new([1, 2, 3]),
            events,
            stats: stats.clone(),
            rate_start: Instant::now(),
            child_poll_at: Instant::now(),
            responses: 0,
        };
        for _ in 0..3 {
            run.event(
                None,
                DiscoveryEventKind::Diagnostic {
                    code: "fixture",
                    message: "bounded".into(),
                },
            );
        }
        assert_eq!(stats.lock().unwrap().dropped_events, 2);
        assert!(receive.try_recv().is_ok());
        assert!(receive.try_recv().is_err());
    }
}
