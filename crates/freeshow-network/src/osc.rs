//! Source parity: Electron `utils/api.ts::emitOSC/convertToType`, osc-js 2.4.1
//! `Message/prepareAddress/typeTag`, and `utils/oscPatch.ts`.
//!
//! This command receives a space-separated string, not JSON OSC arguments.
//! Reachable tags are i/f/s/T/F; typed messages, blobs and bundles are not an
//! input format accepted by the Electron command.

use serde::Serialize;
use serde_json::Value;
use std::{
    fmt,
    future::Future,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs},
    sync::{mpsc as std_mpsc, Arc, OnceLock},
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot, watch, Mutex},
    task::JoinHandle,
    time::{timeout_at, Instant},
};

/// Maximum UDP payload for IPv4, the family used by Electron's udp4 plugin.
pub const MAX_PACKET_BYTES: usize = 65_507;
/// Bounds parsing cost even for numeric strings that encode to just four bytes.
pub const MAX_COMMAND_BYTES: usize = 1_048_576;
const MAX_HOST_BYTES: usize = 4096;
const MAX_QUEUE_CAPACITY: usize = 1024;
const MAX_SEND_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OscErrorKind {
    InvalidPayload,
    InvalidAddress,
    InvalidHost,
    InvalidPort,
    PacketTooLarge,
    InvalidConfig,
    QueueFull,
    Closed,
    Cancelled,
    Timeout,
    ResolveFailed,
    ResolverBusy,
    BindFailed,
    SendFailed,
    WorkerUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OscError {
    pub kind: OscErrorKind,
    pub message: String,
}

impl OscError {
    fn new(kind: OscErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn closed() -> Self {
        Self::new(OscErrorKind::Closed, "OSC sender is closed")
    }

    fn cancelled() -> Self {
        Self::new(OscErrorKind::Cancelled, "OSC send was cancelled")
    }
}

impl fmt::Display for OscError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OscError {}

#[derive(Clone, Debug, Serialize)]
pub struct EncodedMessage {
    address: String,
    #[serde(rename = "typeTags")]
    type_tags: String,
    packet: Vec<u8>,
}

impl EncodedMessage {
    pub fn address(&self) -> &str {
        &self.address
    }
    pub fn type_tags(&self) -> &str {
        &self.type_tags
    }
    pub fn packet(&self) -> &[u8] {
        &self.packet
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PreparedOsc {
    host: String,
    port: u16,
    #[serde(flatten)]
    message: EncodedMessage,
}

impl PreparedOsc {
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn message(&self) -> &EncodedMessage {
        &self.message
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EmitOutcome {
    Ignored,
    /// Accepted by the local UDP stack; UDP does not acknowledge reception.
    Sent {
        host: String,
        port: u16,
        address: String,
        bytes: usize,
    },
}

/// Prepare the actual EMIT_OSC JSON payload, without opening any socket.
/// Non-string `data` is the original handler's intentional ignored outcome.
pub fn prepare_emit(payload: &Value) -> Result<Option<PreparedOsc>, OscError> {
    let Some(data) = command_data(payload)? else {
        return Ok(None);
    };
    let message = encode_message(data)?;
    let signal = payload.get("signal");
    let port = match signal
        .and_then(|signal| signal.get("port"))
        .filter(|value| truthy(value))
    {
        None => 8080,
        Some(value) => {
            let number = match value {
                Value::Number(number) => number.as_f64(),
                Value::String(port) if !trim_js(port).is_empty() => js_number(port),
                _ => None,
            }
            .ok_or_else(|| {
                OscError::new(
                    OscErrorKind::InvalidPort,
                    "OSC port must be a number or numeric string",
                )
            })?;
            if !number.is_finite() || number < 0.0 || number > 65535.0 || number.fract() != 0.0 {
                return Err(OscError::new(
                    OscErrorKind::InvalidPort,
                    "OSC port must be an integer between 0 and 65535",
                ));
            }
            // Node accepts a truthy string "0" here, then the UDP send fails
            // with EINVAL. Numeric zero takes the earlier 8080 default.
            number as u16
        }
    };
    // Node dgram validates the port before checking the destination's type.
    let host = match signal
        .and_then(|signal| signal.get("host"))
        .filter(|value| truthy(value))
    {
        None => "0.0.0.0",
        Some(Value::String(host)) => host,
        Some(_) => {
            return Err(OscError::new(
                OscErrorKind::InvalidHost,
                "OSC host must be a string",
            ))
        }
    };
    if host.len() > MAX_HOST_BYTES || host.contains('\0') {
        return Err(OscError::new(
            OscErrorKind::InvalidHost,
            "OSC host is too long or contains a null byte",
        ));
    }
    Ok(Some(PreparedOsc {
        host: host.to_owned(),
        port,
        message,
    }))
}

fn command_data(payload: &Value) -> Result<Option<&str>, OscError> {
    if payload.is_null() {
        return Err(OscError::new(
            OscErrorKind::InvalidPayload,
            "OSC payload must not be null",
        ));
    }
    Ok(payload.get("data").and_then(Value::as_str))
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Encode only the message variants the retained frontend can send.
pub fn encode_message(data: &str) -> Result<EncodedMessage, OscError> {
    if data.len() > MAX_COMMAND_BYTES {
        return Err(OscError::new(
            OscErrorKind::PacketTooLarge,
            "OSC command exceeds the parsing limit",
        ));
    }
    let mut tokens = data.split(' ');
    let mut address = tokens.next().unwrap_or_default().to_owned();
    if address.encode_utf16().count() > 1 && address.ends_with('/') {
        address.pop();
    }
    if address.encode_utf16().count() > 1 && !address.starts_with('/') {
        address.insert(0, '/');
    }
    if !address.starts_with('/') {
        return Err(OscError::new(
            OscErrorKind::InvalidAddress,
            "OSC Message has an invalid address",
        ));
    }
    let mut args = Vec::new();
    let mut type_tags = String::new();
    for token in tokens {
        let tag = match token {
            "true" => 'T',
            "false" => 'F',
            "" => {
                pack_string(token, &mut args);
                's'
            }
            _ => match js_number(token) {
                Some(number) if number.is_finite() && number.fract() == 0.0 => {
                    // DataView.setInt32 applies ToInt32, including overflow.
                    let integer = number.rem_euclid(4_294_967_296.0) as u32;
                    args.extend_from_slice(&integer.to_be_bytes());
                    'i'
                }
                Some(number) => {
                    args.extend_from_slice(&(number as f32).to_be_bytes());
                    'f'
                }
                None => {
                    pack_string(token, &mut args);
                    's'
                }
            },
        };
        type_tags.push(tag);
        if padded(address.len() + 1) + padded(type_tags.len() + 2) + args.len() > MAX_PACKET_BYTES {
            return Err(OscError::new(
                OscErrorKind::PacketTooLarge,
                "OSC message exceeds the IPv4 UDP payload limit",
            ));
        }
    }
    let mut packet = Vec::new();
    pack_string(&address, &mut packet);
    pack_string(&format!(",{type_tags}"), &mut packet);
    packet.extend_from_slice(&args);
    if packet.len() > MAX_PACKET_BYTES {
        return Err(OscError::new(
            OscErrorKind::PacketTooLarge,
            "OSC message exceeds the IPv4 UDP payload limit",
        ));
    }
    Ok(EncodedMessage {
        address,
        type_tags,
        packet,
    })
}

fn padded(length: usize) -> usize {
    (length + 3) & !3
}

fn pack_string(value: &str, target: &mut Vec<u8>) {
    target.extend_from_slice(value.as_bytes());
    target.resize(target.len() + padded(value.len() + 1) - value.len(), 0);
}

fn trim_js(value: &str) -> &str {
    value.trim_matches(|character| {
        matches!(character,
            '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
            '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
            '\u{205f}' | '\u{3000}' | '\u{feff}'
        )
    })
}

/// ECMAScript Number(string), returning None for NaN (which stays a string).
fn js_number(value: &str) -> Option<f64> {
    let value = trim_js(value);
    if value.is_empty() {
        return Some(0.0);
    }
    match value {
        "Infinity" | "+Infinity" => return Some(f64::INFINITY),
        "-Infinity" => return Some(f64::NEG_INFINITY),
        _ => {}
    }
    if value.len() >= 2 && value.as_bytes()[0] == b'0' {
        let bits_per_digit = match value.as_bytes()[1] {
            b'x' | b'X' => Some(4),
            b'o' | b'O' => Some(3),
            b'b' | b'B' => Some(1),
            _ => None,
        };
        if let Some(bits_per_digit) = bits_per_digit {
            return radix_number(&value[2..], bits_per_digit);
        }
    }
    let bytes = value.as_bytes();
    let mut offset = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let start = offset;
    while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
        offset += 1;
    }
    let mut digits = offset - start;
    if bytes.get(offset) == Some(&b'.') {
        offset += 1;
        let start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        digits += offset - start;
    }
    if digits == 0 {
        return None;
    }
    if matches!(bytes.get(offset), Some(b'e' | b'E')) {
        offset += 1;
        if matches!(bytes.get(offset), Some(b'+' | b'-')) {
            offset += 1;
        }
        let start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        if offset == start {
            return None;
        }
    }
    if offset != bytes.len() {
        return None;
    }
    value.parse().ok()
}

/// Correctly rounded binary/octal/hex integers, including values beyond u64.
/// Keeping the leading 54 bits and a sticky bit avoids double rounding.
fn radix_number(digits: &str, bits_per_digit: usize) -> Option<f64> {
    if digits.is_empty() {
        return None;
    }
    let mut significant_bits = 0usize;
    let mut leading = 0u64;
    let mut sticky = false;
    for character in digits.chars() {
        let digit = character.to_digit(1 << bits_per_digit)?;
        for bit in (0..bits_per_digit).rev() {
            let one = digit & (1 << bit) != 0;
            if significant_bits == 0 && !one {
                continue;
            }
            significant_bits += 1;
            if significant_bits <= 54 {
                leading = (leading << 1) | u64::from(one);
            } else {
                sticky |= one;
            }
        }
    }
    if significant_bits <= 53 {
        return Some(leading as f64);
    }
    if significant_bits > 1024 {
        return Some(f64::INFINITY);
    }
    let guard = leading & 1 != 0;
    let mut significand = leading >> 1;
    if guard && (sticky || significand & 1 != 0) {
        significand += 1;
    }
    Some(significand as f64 * 2_f64.powi((significant_bits - 53) as i32))
}

#[derive(Clone, Debug)]
pub struct OscSenderConfig {
    /// Pending requests, in addition to the one active request.
    pub queue_capacity: usize,
    /// Includes time spent in the queue, name resolution and the OS send.
    pub send_timeout: Duration,
}

impl Default for OscSenderConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 64,
            send_timeout: Duration::from_secs(3),
        }
    }
}

struct Job {
    prepared: PreparedOsc,
    deadline: Instant,
    reply: oneshot::Sender<Result<EmitOutcome, OscError>>,
}

/// Dropping a ticket cancels an unsent job. Cancellation cannot retract a UDP
/// packet already accepted by the local kernel.
#[derive(Debug)]
pub struct OscTicket {
    reply: oneshot::Receiver<Result<EmitOutcome, OscError>>,
    stopped: watch::Receiver<bool>,
}

impl OscTicket {
    pub async fn wait(mut self) -> Result<EmitOutcome, OscError> {
        tokio::select! {
            biased;
            result = &mut self.reply => result.unwrap_or_else(|_| Err(OscError::closed())),
            _ = wait_stopped(&mut self.stopped) => Err(OscError::cancelled()),
        }
    }
}

struct SenderState {
    shutdown: watch::Sender<bool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for SenderState {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
    }
}

/// A cloneable handle to one FIFO worker. Clones share the queue and shutdown.
/// At most one socket is open per sender; each send binds udp4 0.0.0.0:0 and
/// drops that socket as soon as the async send finishes or is cancelled.
#[derive(Clone)]
pub struct OscSender {
    queue: mpsc::Sender<Job>,
    state: Arc<SenderState>,
    send_timeout: Duration,
}

impl OscSender {
    pub fn start(config: OscSenderConfig) -> Result<Self, OscError> {
        Self::start_with(config, send_packet)
    }

    fn start_with<F, S>(config: OscSenderConfig, send: S) -> Result<Self, OscError>
    where
        F: Future<Output = Result<EmitOutcome, OscError>> + Send + 'static,
        S: Fn(PreparedOsc) -> F + Send + 'static,
    {
        if config.queue_capacity == 0
            || config.queue_capacity > MAX_QUEUE_CAPACITY
            || config.send_timeout.is_zero()
            || config.send_timeout > MAX_SEND_TIMEOUT
        {
            return Err(OscError::new(
                OscErrorKind::InvalidConfig,
                "OSC requires a queue capacity of 1..=1024 and a timeout in (0, 60s]",
            ));
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            OscError::new(
                OscErrorKind::WorkerUnavailable,
                "OSC sender requires a Tokio runtime",
            )
        })?;
        let (queue, receiver) = mpsc::channel(config.queue_capacity);
        let (shutdown, stopped) = watch::channel(false);
        let worker = runtime.spawn(run_worker(receiver, stopped, send));
        Ok(Self {
            queue,
            state: Arc::new(SenderState {
                shutdown,
                worker: Mutex::new(Some(worker)),
            }),
            send_timeout: config.send_timeout,
        })
    }

    /// Nonblocking admission: a full queue returns QueueFull instead of
    /// spawning work, waiting for capacity or allocating an unbounded backlog.
    pub fn try_emit(&self, payload: &Value) -> Result<OscTicket, OscError> {
        if *self.state.shutdown.borrow() {
            return Err(OscError::closed());
        }
        let prepared = prepare_emit(payload)?;
        let (reply, receiver) = oneshot::channel();
        let Some(prepared) = prepared else {
            let _ = reply.send(Ok(EmitOutcome::Ignored));
            return Ok(OscTicket {
                reply: receiver,
                stopped: self.state.shutdown.subscribe(),
            });
        };
        let job = Job {
            prepared,
            deadline: Instant::now() + self.send_timeout,
            reply,
        };
        self.queue.try_send(job).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => {
                OscError::new(OscErrorKind::QueueFull, "OSC send queue is full")
            }
            mpsc::error::TrySendError::Closed(_) => OscError::closed(),
        })?;
        Ok(OscTicket {
            reply: receiver,
            stopped: self.state.shutdown.subscribe(),
        })
    }

    /// Async admission with bounded backpressure. The same deadline covers
    /// capacity, queueing and send; cancelling this future cancels its ticket.
    pub async fn emit(&self, payload: &Value) -> Result<EmitOutcome, OscError> {
        if *self.state.shutdown.borrow() {
            return Err(OscError::closed());
        }
        if command_data(payload)?.is_none() {
            return Ok(EmitOutcome::Ignored);
        }
        let deadline = Instant::now() + self.send_timeout;
        let mut stopped = self.state.shutdown.subscribe();
        let permit = tokio::select! {
            biased;
            _ = wait_stopped(&mut stopped) => return Err(OscError::closed()),
            result = timeout_at(deadline, self.queue.reserve()) => result
                .map_err(|_| OscError::new(OscErrorKind::Timeout, "OSC queue admission deadline expired"))?
                .map_err(|_| OscError::closed())?,
        };
        if *self.state.shutdown.borrow() {
            return Err(OscError::closed());
        }
        // Do not retain an encoded packet in every caller waiting for queue
        // capacity. Reservations bound encoding allocations as well as jobs.
        let Some(prepared) = prepare_emit(payload)? else {
            return Ok(EmitOutcome::Ignored);
        };
        if Instant::now() >= deadline {
            return Err(OscError::new(
                OscErrorKind::Timeout,
                "OSC queue admission deadline expired",
            ));
        }
        let (reply, receiver) = oneshot::channel();
        permit.send(Job {
            prepared,
            deadline,
            reply,
        });
        OscTicket {
            reply: receiver,
            stopped: self.state.shutdown.subscribe(),
        }
        .wait()
        .await
    }

    /// Cancels the active request and all queued requests, then waits for socket
    /// cleanup. Safe to call more than once or concurrently through clones.
    pub async fn shutdown(&self) {
        self.state.shutdown.send_replace(true);
        let mut worker = self.state.worker.lock().await;
        if let Some(handle) = worker.as_mut() {
            let _ = handle.await;
        }
        worker.take();
    }
}

async fn run_worker<F, S>(
    mut receiver: mpsc::Receiver<Job>,
    mut stopped: watch::Receiver<bool>,
    send: S,
) where
    F: Future<Output = Result<EmitOutcome, OscError>>,
    S: Fn(PreparedOsc) -> F,
{
    loop {
        let job = tokio::select! {
            biased;
            _ = wait_stopped(&mut stopped) => break,
            job = receiver.recv() => match job { Some(job) => job, None => break },
        };
        let Job {
            prepared,
            deadline,
            mut reply,
        } = job;
        if reply.is_closed() {
            continue;
        }
        if Instant::now() >= deadline {
            let _ = reply.send(Err(OscError::new(
                OscErrorKind::Timeout,
                "OSC send deadline expired",
            )));
            continue;
        }
        let result = tokio::select! {
            biased;
            _ = wait_stopped(&mut stopped) => Err(OscError::cancelled()),
            _ = reply.closed() => continue,
            result = timeout_at(deadline, send(prepared)) => result.unwrap_or_else(|_| {
                Err(OscError::new(OscErrorKind::Timeout, "OSC send deadline expired"))
            }),
        };
        let _ = reply.send(result);
    }
    receiver.close();
    // Closing a Tokio receiver does not revoke outstanding reserve permits.
    // Never await recv while draining shutdown: another task may still hold a
    // permit and prevent completion. Tickets also observe shutdown directly:
    // a late permit send can otherwise retain its reply in a closed channel.
    while let Ok(job) = receiver.try_recv() {
        let _ = job.reply.send(Err(OscError::cancelled()));
    }
}

async fn wait_stopped(stopped: &mut watch::Receiver<bool>) {
    loop {
        if *stopped.borrow_and_update() {
            return;
        }
        if stopped.changed().await.is_err() {
            return;
        }
    }
}

async fn send_packet(prepared: PreparedOsc) -> Result<EmitOutcome, OscError> {
    let address = resolve_ipv4(&prepared.host, prepared.port).await?;
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(|error| OscError::new(OscErrorKind::BindFailed, error.to_string()))?;
    // The pinned Node sender fails after coercing a truthy string "0".
    // Bare UDP send_to can accept port zero on Windows; preserve the original
    // send failure without changing prepare_emit's JavaScript coercion.
    if prepared.port == 0 {
        return Err(OscError::new(
            OscErrorKind::SendFailed,
            "The original UDP sender rejects destination port zero",
        ));
    }
    let bytes = socket
        .send_to(prepared.message.packet(), address)
        .await
        .map_err(|error| OscError::new(OscErrorKind::SendFailed, error.to_string()))?;
    if bytes != prepared.message.packet.len() {
        return Err(OscError::new(
            OscErrorKind::SendFailed,
            "OSC UDP send was incomplete",
        ));
    }
    Ok(EmitOutcome::Sent {
        host: prepared.host.clone(),
        port: prepared.port,
        address: prepared.message.address.clone(),
        bytes,
    })
}

struct ResolveJob {
    host: String,
    port: u16,
    reply: oneshot::Sender<Result<SocketAddrV4, OscError>>,
}

/// OS getaddrinfo is not cancellable. A single process-wide thread and a
/// one-slot queue put a strict bound on it, even across sender recreation and
/// timed-out requests. No Tokio blocking-pool task or socket is held by DNS.
struct Resolver {
    queue: std_mpsc::SyncSender<ResolveJob>,
}

impl Resolver {
    fn start() -> Result<Self, OscError> {
        Self::start_with(|host, port| {
            (host, port)
                .to_socket_addrs()
                .map_err(|error| OscError::new(OscErrorKind::ResolveFailed, error.to_string()))
                .and_then(|mut addresses| {
                    addresses
                        .find_map(|address| match address {
                            SocketAddr::V4(address) => Some(address),
                            SocketAddr::V6(_) => None,
                        })
                        .ok_or_else(|| {
                            OscError::new(
                                OscErrorKind::ResolveFailed,
                                "OSC requires an IPv4 destination",
                            )
                        })
                })
        })
    }

    fn start_with<R>(mut resolve: R) -> Result<Self, OscError>
    where
        R: FnMut(&str, u16) -> Result<SocketAddrV4, OscError> + Send + 'static,
    {
        let (queue, receiver) = std_mpsc::sync_channel::<ResolveJob>(1);
        std::thread::Builder::new()
            .name("freeshow-osc-dns".into())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    if job.reply.is_closed() {
                        continue;
                    }
                    let result = resolve(&job.host, job.port);
                    let _ = job.reply.send(result);
                }
            })
            .map_err(|error| OscError::new(OscErrorKind::WorkerUnavailable, error.to_string()))?;
        Ok(Self { queue })
    }

    fn request(
        &self,
        host: &str,
        port: u16,
    ) -> Result<oneshot::Receiver<Result<SocketAddrV4, OscError>>, OscError> {
        let (reply, receiver) = oneshot::channel();
        self.queue
            .try_send(ResolveJob {
                host: host.to_owned(),
                port,
                reply,
            })
            .map_err(|error| match error {
                std_mpsc::TrySendError::Full(_) => {
                    OscError::new(OscErrorKind::ResolverBusy, "OSC DNS queue is full")
                }
                std_mpsc::TrySendError::Disconnected(_) => {
                    OscError::new(OscErrorKind::WorkerUnavailable, "OSC DNS worker is closed")
                }
            })?;
        Ok(receiver)
    }
}

static RESOLVER: OnceLock<Result<Resolver, OscError>> = OnceLock::new();

async fn resolve_ipv4(host: &str, port: u16) -> Result<SocketAddrV4, OscError> {
    if let Ok(address) = host.parse::<Ipv4Addr>() {
        return Ok(SocketAddrV4::new(address, port));
    }
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        return Err(OscError::new(
            OscErrorKind::ResolveFailed,
            "OSC requires an IPv4 destination",
        ));
    }
    let resolver = RESOLVER
        .get_or_init(Resolver::start)
        .as_ref()
        .map_err(Clone::clone)?;
    let receiver = resolver.request(host, port)?;
    receiver.await.map_err(|_| {
        OscError::new(
            OscErrorKind::WorkerUnavailable,
            "OSC DNS worker did not reply",
        )
    })?
}

#[cfg(test)]
#[path = "osc_tests.rs"]
mod tests;
