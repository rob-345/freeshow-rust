//! One shared bounded OS owner for live IP queries and frozen Bonjour startup.
use freeshow_core::WireError;
use freeshow_discovery::NetworkSnapshot;
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, watch, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
pub type Result<T> = std::result::Result<T, WireError>;
type Failure = (&'static str, String);
fn error(code: &'static str, message: impl Into<String>) -> WireError {
    let mut message = message.into();
    if message.len() > 2048 {
        let mut at = 2030;
        while !message.is_char_boundary(at) {
            at -= 1;
        }
        message.truncate(at);
        message.push_str(" [truncated]");
    }
    WireError::new(code, message)
}
#[derive(Clone, Debug)]
pub struct SnapshotLimits {
    /// Active + queued + completed/uncollected work, all sharing one OS thread.
    pub capacity: usize,
    pub timeout: Duration,
    pub shutdown_timeout: Duration,
    pub output_bytes: usize,
    pub operations: u64,
}
impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            capacity: 8,
            timeout: Duration::from_secs(2),
            shutdown_timeout: Duration::from_secs(3),
            output_bytes: 128 * 1024,
            operations: 2_000_000,
        }
    }
}
impl SnapshotLimits {
    fn validate(&self) -> Result<()> {
        if !(1..=32).contains(&self.capacity)
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(30)
            || self.shutdown_timeout.is_zero()
            || self.shutdown_timeout > Duration::from_secs(30)
            || !(2..=512 * 1024).contains(&self.output_bytes)
            || !(1..=4_000_000).contains(&self.operations)
        {
            return Err(error(
                "network_snapshot_configuration",
                "Invalid native network snapshot limits",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct SnapshotControl {
    pub deadline: Instant,
    pub cancel: CancellationToken,
}
impl Default for SnapshotControl {
    fn default() -> Self {
        Self {
            deadline: Instant::now() + Duration::from_secs(2),
            cancel: CancellationToken::new(),
        }
    }
}
impl SnapshotControl {
    fn check(&self, shared: &Shared) -> Result<()> {
        if shared.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        if self.cancel.is_cancelled() {
            return Err(error(
                "network_snapshot_cancelled",
                "Native network capture cancelled",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(error(
                "network_snapshot_deadline",
                "Native network capture deadline elapsed",
            ));
        }
        Ok(())
    }
}
struct Budget {
    control: SnapshotControl,
    shared: Arc<Shared>,
    remaining: u64,
}
impl Budget {
    fn check(&mut self) -> Result<()> {
        self.control.check(&self.shared)?;
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or_else(|| snapshot_limit("Native network operation budget exceeded"))?;
        Ok(())
    }
}
struct Shared {
    closing: AtomicBool,
    cancelled: CancellationToken,
    queued: AtomicUsize,
    active: AtomicUsize,
    running: AtomicBool,
    native_captures: AtomicU64,
    failure: Mutex<Option<Failure>>,
    #[cfg(test)]
    before_capture: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    #[cfg(test)]
    fail_interfaces: AtomicBool,
    #[cfg(test)]
    fail_hostname: AtomicBool,
}
enum Operation {
    Snapshot,
    Addresses,
    #[cfg(test)]
    Panic,
}
enum Answer {
    Snapshot(NetworkSnapshot),
    Addresses(Vec<String>),
}
struct Work {
    operation: Operation,
    budget: Budget,
    reply: oneshot::Sender<Completion>,
    queued: Queued,
    permit: OwnedSemaphorePermit,
    keepalive: Box<dyn Send>,
}
struct Queued(Arc<Shared>);
impl Drop for Queued {
    fn drop(&mut self) {
        self.0.queued.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Completion {
    result: Result<Answer>,
    permit: OwnedSemaphorePermit,
    keepalive: Box<dyn Send>,
}
/// Retains both real admission and an optional native consumer guard until the
/// completed result is collected/dropped, including abandoned IPC waiters.
pub struct Captured<T> {
    value: T,
    _permit: OwnedSemaphorePermit,
    _keepalive: Box<dyn Send>,
}
impl<T> Captured<T> {
    pub fn value(&self) -> &T {
        &self.value
    }
    pub fn into_value(self) -> T {
        self.value
    }
}
struct Inner {
    limits: SnapshotLimits,
    shared: Arc<Shared>,
    sender: Mutex<Option<mpsc::SyncSender<Work>>>,
    slots: Arc<Semaphore>,
    thread: Mutex<Option<JoinHandle<()>>>,
    done: watch::Receiver<Option<std::result::Result<(), Failure>>>,
    shutdown: AsyncMutex<()>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        self.shared.cancelled.cancel();
        self.sender
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}
#[derive(Clone)]
pub struct NetworkSnapshots {
    inner: Arc<Inner>,
}
fn closed() -> WireError {
    error(
        "network_snapshot_shutdown",
        "Native network capture admission is closed",
    )
}
fn failure(shared: &Shared) -> Option<WireError> {
    shared
        .failure
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|(code, message)| error(code, message.clone()))
}
fn retain(shared: &Shared, e: WireError) {
    shared.closing.store(true, Ordering::Release);
    shared.cancelled.cancel();
    let mut first = shared.failure.lock().unwrap_or_else(|e| e.into_inner());
    if first.is_none() {
        *first = Some((e.code, e.message));
    }
}
impl NetworkSnapshots {
    pub fn start(limits: SnapshotLimits) -> Result<Self> {
        limits.validate()?;
        let shared = Arc::new(Shared {
            closing: AtomicBool::new(false),
            cancelled: CancellationToken::new(),
            queued: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            running: AtomicBool::new(true),
            native_captures: AtomicU64::new(0),
            failure: Mutex::new(None),
            #[cfg(test)]
            before_capture: Mutex::new(None),
            #[cfg(test)]
            fail_interfaces: AtomicBool::new(false),
            #[cfg(test)]
            fail_hostname: AtomicBool::new(false),
        });
        let (sender, receiver) = mpsc::sync_channel::<Work>(limits.capacity);
        let (finished, done) = watch::channel(None);
        let state = shared.clone();
        let worker_limits = limits.clone();
        let thread = std::thread::Builder::new()
            .name("freeshow-network-snapshot".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    while let Ok(Work {
                        operation,
                        mut budget,
                        reply,
                        queued,
                        permit,
                        keepalive,
                    }) = receiver.recv()
                    {
                        drop(queued);
                        state.active.store(1, Ordering::Release);
                        let result = (|| {
                            budget.check()?;
                            #[cfg(test)]
                            {
                                let hook = state.before_capture.lock().unwrap().take();
                                if let Some((entered, release)) = hook {
                                    let _ = entered.send(());
                                    let _ = release.recv();
                                }
                            }
                            budget.check()?;
                            let answer = match operation {
                                Operation::Snapshot => {
                                    let interfaces = capture_interfaces(&mut budget)?;
                                    #[cfg(test)]
                                    if state.fail_hostname.load(Ordering::Acquire) {
                                        return Err(native_error("Controlled hostname failure"));
                                    }
                                    let hostname = native_hostname(&mut budget)?;
                                    Answer::Snapshot(NetworkSnapshot {
                                        hostname,
                                        interfaces,
                                    })
                                }
                                Operation::Addresses => {
                                    let interfaces = capture_interfaces(&mut budget)?;
                                    // Source MAIN.IP never reads/validates hostname.
                                    let snapshot = NetworkSnapshot {
                                        hostname: "localhost".into(),
                                        interfaces,
                                    };
                                    let ips = freeshow_discovery::get_local_ips(&snapshot)
                                        .map_err(|e| {
                                            error(
                                                if e.code == "discovery_source_shape" {
                                                    "network_source_shape"
                                                } else {
                                                    "network_snapshot_invalid"
                                                },
                                                e.message,
                                            )
                                        })?;
                                    Answer::Addresses(ips)
                                }
                                #[cfg(test)]
                                Operation::Panic => {
                                    panic!("controlled network snapshot worker panic")
                                }
                            };
                            check_output(&answer, &mut budget, worker_limits.output_bytes)?;
                            budget.check()?;
                            Ok(answer)
                        })();
                        state.active.store(0, Ordering::Release);
                        let _ = reply.send(Completion {
                            result,
                            permit,
                            keepalive,
                        });
                    }
                }));
                if result.is_err() {
                    retain(
                        &state,
                        error(
                            "network_snapshot_worker",
                            "Native network OS worker panicked",
                        ),
                    );
                }
                state.active.store(0, Ordering::Release);
                state.running.store(false, Ordering::Release);
                finished.send_replace(Some(
                    failure(&state).map_or(Ok(()), |e| Err((e.code, e.message))),
                ));
            })
            .map_err(|e| native_error(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                slots: Arc::new(Semaphore::new(limits.capacity)),
                limits,
                shared,
                sender: Mutex::new(Some(sender)),
                thread: Mutex::new(Some(thread)),
                done,
                shutdown: AsyncMutex::new(()),
            }),
        })
    }
    async fn request(
        &self,
        operation: Operation,
        mut control: SnapshotControl,
        keepalive: Box<dyn Send>,
    ) -> Result<Completion> {
        if let Some(e) = failure(&self.inner.shared) {
            return Err(e);
        }
        control.deadline = control
            .deadline
            .min(Instant::now() + self.inner.limits.timeout);
        control.check(&self.inner.shared)?;
        let permit = self.inner.slots.clone().try_acquire_owned().map_err(|_| {
            error(
                "network_snapshot_busy",
                "Native network work is at capacity",
            )
        })?;
        let (reply, response) = oneshot::channel();
        {
            let sender = self.inner.sender.lock().unwrap_or_else(|e| e.into_inner());
            control.check(&self.inner.shared)?;
            self.inner.shared.queued.fetch_add(1, Ordering::AcqRel);
            let work = Work {
                operation,
                budget: Budget {
                    control: control.clone(),
                    shared: self.inner.shared.clone(),
                    remaining: self.inner.limits.operations,
                },
                reply,
                queued: Queued(self.inner.shared.clone()),
                permit,
                keepalive,
            };
            let result = match sender.as_ref() {
                Some(sender) => sender.try_send(work).map_err(|_| closed()),
                None => {
                    drop(work);
                    Err(closed())
                }
            };
            result?;
        }
        let completion = tokio::select! {biased;
            _=self.inner.shared.cancelled.cancelled()=>return Err(failure(&self.inner.shared).unwrap_or_else(closed)),
            _=control.cancel.cancelled()=>return Err(error("network_snapshot_cancelled","Native network capture cancelled")),
            _=tokio::time::sleep_until(control.deadline.into())=>return Err(error("network_snapshot_deadline","Native network capture deadline elapsed")),
            completion=response=>completion.map_err(|_|failure(&self.inner.shared).unwrap_or_else(||error("network_snapshot_worker","Native network worker lost the reply")))?,
        };
        control.check(&self.inner.shared)?;
        Ok(completion)
    }
    pub async fn snapshot(&self, control: SnapshotControl) -> Result<Captured<NetworkSnapshot>> {
        let Completion {
            result,
            permit,
            keepalive,
        } = self
            .request(Operation::Snapshot, control, Box::new(()))
            .await?;
        match result? {
            Answer::Snapshot(value) => Ok(Captured {
                value,
                _permit: permit,
                _keepalive: keepalive,
            }),
            _ => unreachable!(),
        }
    }
    pub async fn addresses_owned<G: Send + 'static>(
        &self,
        control: SnapshotControl,
        guard: G,
    ) -> Result<Captured<Vec<String>>> {
        let Completion {
            result,
            permit,
            keepalive,
        } = self
            .request(Operation::Addresses, control, Box::new(guard))
            .await?;
        match result? {
            Answer::Addresses(value) => Ok(Captured {
                value,
                _permit: permit,
                _keepalive: keepalive,
            }),
            _ => unreachable!(),
        }
    }
    pub fn terminal_error(&self) -> Option<WireError> {
        failure(&self.inner.shared)
    }
    pub fn status(&self) -> serde_json::Value {
        json!({"closing":self.inner.shared.closing.load(Ordering::Acquire),"workerRunning":self.inner.shared.running.load(Ordering::Acquire),"workers":1,"active":self.inner.shared.active.load(Ordering::Acquire),"queued":self.inner.shared.queued.load(Ordering::Acquire),"admitted":self.inner.limits.capacity-self.inner.slots.available_permits(),"capacity":self.inner.limits.capacity,"nativeCaptures":self.inner.shared.native_captures.load(Ordering::Acquire),"failure":failure(&self.inner.shared).map(|e|json!({"code":e.code,"message":e.message}))})
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.shared.closing.store(true, Ordering::Release);
        self.inner.shared.cancelled.cancel();
        self.inner
            .sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let deadline = Instant::now() + self.inner.limits.shutdown_timeout;
        let timeout = || {
            error(
                "network_snapshot_shutdown_timeout",
                "Native OS work still owns the network snapshot worker; drain can be retried",
            )
        };
        let _guard = tokio::time::timeout_at(deadline.into(), self.inner.shutdown.lock())
            .await
            .map_err(|_| timeout())?;
        loop {
            let finished = self
                .inner
                .thread
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_none_or(|t| t.is_finished());
            if finished {
                break;
            }
            if Instant::now() >= deadline {
                return Err(timeout());
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        if let Some(thread) = self
            .inner
            .thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            if thread.join().is_err() {
                retain(
                    &self.inner.shared,
                    error(
                        "network_snapshot_worker",
                        "Native network OS worker join failed",
                    ),
                );
            }
        }
        if let Some(e) = failure(&self.inner.shared) {
            return Err(e);
        }
        match self.inner.done.borrow().clone() {
            Some(Ok(())) => Ok(()),
            Some(Err((code, message))) => Err(error(code, message)),
            None => {
                let e = error(
                    "network_snapshot_worker",
                    "Native network worker completion was lost",
                );
                retain(&self.inner.shared, error(e.code, e.message.clone()));
                Err(e)
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn pause_next(
        &self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        *self.inner.shared.before_capture.lock().unwrap() = Some((entered, release));
    }
    #[cfg(test)]
    pub(crate) fn fail_interfaces(&self, value: bool) {
        self.inner
            .shared
            .fail_interfaces
            .store(value, Ordering::Release);
    }
    #[cfg(test)]
    pub(crate) fn fail_hostname(&self, value: bool) {
        self.inner
            .shared
            .fail_hostname
            .store(value, Ordering::Release);
    }
    #[cfg(test)]
    pub(crate) async fn panic_worker(&self) -> Result<()> {
        self.request(Operation::Panic, SnapshotControl::default(), Box::new(()))
            .await
            .map(|_| ())
    }
}
fn capture_interfaces(budget: &mut Budget) -> Result<Vec<freeshow_discovery::NetworkInterface>> {
    budget.check()?;
    #[cfg(test)]
    if budget.shared.fail_interfaces.load(Ordering::Acquire) {
        return Err(native_error("Controlled interface capture failure"));
    }
    native_interfaces(budget)
}
fn check_output(answer: &Answer, budget: &mut Budget, maximum: usize) -> Result<()> {
    struct Counter<'a> {
        bytes: usize,
        maximum: usize,
        budget: &'a mut Budget,
        failure: Option<WireError>,
    }
    impl std::io::Write for Counter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Err(e) = self.budget.check() {
                self.failure = Some(e);
                return Err(std::io::Error::other("network budget"));
            }
            if bytes.len() > self.maximum.saturating_sub(self.bytes) {
                return Err(std::io::Error::other("network output"));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter {
        bytes: 0,
        maximum,
        budget,
        failure: None,
    };
    let result = match answer {
        Answer::Snapshot(snapshot) => serde_json::to_writer(&mut count, snapshot),
        Answer::Addresses(ips) => serde_json::to_writer(&mut count, ips),
    };
    if let Some(e) = count.failure {
        return Err(e);
    }
    result.map_err(|_| snapshot_limit("Native network encoded output bound"))
}

fn snapshot_limit(message: &str) -> WireError {
    error("network_snapshot_limit", message)
}
fn native_error(message: impl Into<String>) -> WireError {
    error("network_snapshot_os", message)
}
fn mac_text(bytes: [u8; 6]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}
fn add_address(
    interfaces: &mut Vec<freeshow_discovery::NetworkInterface>,
    name: String,
    address: freeshow_discovery::InterfaceAddress,
) -> Result<()> {
    if name.len() > 128 {
        return Err(snapshot_limit("Native interface name exceeds 128 bytes"));
    }
    let index = if let Some(index) = interfaces.iter().position(|i| i.name == name) {
        index
    } else {
        if interfaces.len() == 32 {
            return Err(snapshot_limit("More than 32 native interfaces"));
        }
        interfaces.push(freeshow_discovery::NetworkInterface {
            name,
            addresses: vec![],
        });
        interfaces.len() - 1
    };
    if interfaces[index].addresses.len() == 16 {
        return Err(snapshot_limit(
            "More than 16 addresses per native interface",
        ));
    }
    interfaces[index].addresses.push(address);
    Ok(())
}
fn key_order(interfaces: &mut [freeshow_discovery::NetworkInterface]) {
    interfaces.sort_by_key(|i| {
        let n = i
            .name
            .parse::<u32>()
            .ok()
            .filter(|n| *n != u32::MAX && n.to_string() == i.name);
        (n.is_none(), n.unwrap_or(0))
    });
}
/// Native ordered candidates used by source os.networkInterfaces(), restricted to
/// bounded IPv4/IPv6 data required by Bonjour. No subprocess or LAN operations.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn native_interfaces(budget: &mut Budget) -> Result<Vec<freeshow_discovery::NetworkInterface>> {
    use freeshow_discovery::InterfaceAddress;
    use std::{
        ffi::CStr,
        net::{Ipv4Addr, Ipv6Addr},
    };
    let mut head = std::ptr::null_mut();
    // SAFETY: OS writes an owned list, freed exactly once by List below.
    budget.check()?;
    budget.shared.native_captures.fetch_add(1, Ordering::AcqRel);
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Err(native_error(std::io::Error::last_os_error().to_string()));
    }
    struct List(*mut libc::ifaddrs);
    impl Drop for List {
        fn drop(&mut self) {
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
    let _list = List(head);
    let mut entries = Vec::new();
    let mut cursor = head;
    while !cursor.is_null() {
        budget.check()?;
        if entries.len() == 1024 {
            return Err(snapshot_limit("Native getifaddrs exceeded 1024 entries"));
        }
        entries.push(cursor);
        // SAFETY: owned getifaddrs list remains alive throughout traversal.
        cursor = unsafe { (*cursor).ifa_next };
    }
    let mut interfaces = Vec::new();
    for ptr in &entries {
        budget.check()?;
        let entry = unsafe { &**ptr };
        let enabled = libc::IFF_UP as u32 | libc::IFF_RUNNING as u32;
        if entry.ifa_addr.is_null() || entry.ifa_flags & enabled != enabled {
            continue;
        }
        let family = unsafe { (*entry.ifa_addr).sa_family as i32 };
        let (address, family) = match family {
            libc::AF_INET => {
                let addr = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in>() };
                (
                    Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes()).to_string(),
                    "IPv4",
                )
            }
            libc::AF_INET6 => {
                let addr = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in6>() };
                (Ipv6Addr::from(addr.sin6_addr.s6_addr).to_string(), "IPv6")
            }
            #[cfg(target_os = "linux")]
            libc::AF_PACKET => continue,
            #[cfg(target_os = "macos")]
            libc::AF_LINK => continue,
            _ => {
                return Err(error(
                    "network_snapshot_unsupported",
                    "Native interface family is not IPv4/IPv6/link metadata",
                ))
            }
        };
        if entry.ifa_name.is_null() {
            return Err(native_error("Native interface name is null"));
        }
        let name_bytes = unsafe { CStr::from_ptr(entry.ifa_name) }.to_bytes();
        let name = String::from_utf8_lossy(name_bytes).into_owned();
        if name.len() > 128 {
            return Err(snapshot_limit("Native interface name exceeds bound"));
        }
        let mut mac = [0u8; 6];
        for physical in &entries {
            budget.check()?;
            let physical = unsafe { &**physical };
            if physical.ifa_addr.is_null()
                || physical.ifa_name.is_null()
                || physical.ifa_flags & enabled != enabled
            {
                continue;
            }
            let physical_family = unsafe { (*physical.ifa_addr).sa_family as i32 };
            let physical_name = unsafe { CStr::from_ptr(physical.ifa_name) }.to_bytes();
            #[cfg(target_os = "linux")]
            if physical_family == libc::AF_PACKET
                && (name_bytes == physical_name
                    || name_bytes
                        .strip_prefix(physical_name)
                        .is_some_and(|tail| tail.starts_with(b":")))
            {
                let link = unsafe { &*physical.ifa_addr.cast::<libc::sockaddr_ll>() };
                mac.copy_from_slice(&link.sll_addr[..6]);
            }
            #[cfg(target_os = "macos")]
            if physical_family == libc::AF_LINK && name_bytes == physical_name {
                let link = unsafe { &*physical.ifa_addr.cast::<libc::sockaddr_dl>() };
                let start =
                    std::mem::offset_of!(libc::sockaddr_dl, sdl_data) + usize::from(link.sdl_nlen);
                if start + 6 <= usize::from(link.sdl_len) {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            physical.ifa_addr.cast::<u8>().add(start),
                            mac.as_mut_ptr(),
                            6,
                        )
                    };
                }
            }
        }
        add_address(
            &mut interfaces,
            name,
            InterfaceAddress {
                address,
                family: json!(family),
                internal: entry.ifa_flags & libc::IFF_LOOPBACK as u32 != 0,
                mac: mac_text(mac),
            },
        )?;
    }
    key_order(&mut interfaces);
    budget.check()?;
    Ok(interfaces)
}

#[cfg(windows)]
fn native_interfaces(budget: &mut Budget) -> Result<Vec<freeshow_discovery::NetworkInterface>> {
    use freeshow_discovery::InterfaceAddress;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use windows_sys::Win32::{
        Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA},
        NetworkManagement::{IpHelper::*, Ndis::IfOperStatusUp},
        Networking::WinSock::*,
    };
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 0u32;
    let mut buffer = Vec::<u64>::new(); // alignment for all native adapter structs
    let mut ready = false;
    for _ in 0..3 {
        budget.check()?;
        budget.shared.native_captures.fetch_add(1, Ordering::AcqRel);
        let pointer = if buffer.is_empty() {
            std::ptr::null_mut()
        } else {
            buffer.as_mut_ptr().cast()
        };
        let code = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC as u32,
                flags,
                std::ptr::null(),
                pointer,
                &mut size,
            )
        };
        if code == ERROR_NO_DATA {
            return Ok(vec![]);
        }
        if code == 0 {
            ready = true;
            break;
        }
        if code != ERROR_BUFFER_OVERFLOW {
            return Err(native_error(format!("GetAdaptersAddresses: {code}")));
        }
        if size as usize > 256 * 1024 {
            return Err(snapshot_limit("Native adapter buffer exceeds 256 KiB"));
        }
        buffer.resize((size as usize).div_ceil(8), 0);
    }
    if !ready || buffer.is_empty() {
        return Err(native_error(
            "Native adapter list changed across three bounded calls",
        ));
    }
    let mut interfaces = Vec::new();
    let mut adapter = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    let mut count = 0;
    while !adapter.is_null() {
        budget.check()?;
        count += 1;
        if count > 1024 {
            return Err(snapshot_limit("More than 1024 native adapters"));
        }
        // SAFETY: the successful OS call owns linked records within this aligned
        // buffer for the whole traversal. No pointer escapes the function.
        let data = unsafe { &*adapter };
        if data.OperStatus == IfOperStatusUp && !data.FirstUnicastAddress.is_null() {
            let mut units = Vec::new();
            if data.FriendlyName.is_null() {
                return Err(native_error("Null native adapter name"));
            }
            for index in 0..=128 {
                let unit = unsafe { *data.FriendlyName.add(index) };
                if unit == 0 {
                    break;
                }
                if index == 128 {
                    return Err(snapshot_limit(
                        "Native adapter name exceeds 128 UTF-16 units",
                    ));
                }
                units.push(unit);
            }
            let name = String::from_utf16(&units).map_err(|_| {
                error(
                    "network_snapshot_unsupported",
                    "Adapter name contains lone UTF-16 surrogate",
                )
            })?;
            let mut mac = [0; 6];
            if data.PhysicalAddressLength == 6 {
                mac.copy_from_slice(&data.PhysicalAddress[..6]);
            }
            let mut address = data.FirstUnicastAddress;
            let mut address_count = 0;
            while !address.is_null() {
                budget.check()?;
                address_count += 1;
                if address_count > 16 {
                    return Err(snapshot_limit("More than 16 native adapter addresses"));
                }
                let unicast = unsafe { &*address };
                let socket = unicast.Address.lpSockaddr;
                if socket.is_null() {
                    return Err(native_error("Null native adapter address"));
                }
                let (ip, family) = match unsafe { (*socket).sa_family } {
                    AF_INET => {
                        let ip = unsafe { &*socket.cast::<SOCKADDR_IN>() };
                        (
                            Ipv4Addr::from(unsafe { ip.sin_addr.S_un.S_addr }.to_ne_bytes())
                                .to_string(),
                            "IPv4",
                        )
                    }
                    AF_INET6 => {
                        let ip = unsafe { &*socket.cast::<SOCKADDR_IN6>() };
                        (
                            Ipv6Addr::from(unsafe { ip.sin6_addr.u.Byte }).to_string(),
                            "IPv6",
                        )
                    }
                    _ => {
                        return Err(error(
                            "network_snapshot_unsupported",
                            "Native adapter address family unsupported",
                        ))
                    }
                };
                add_address(
                    &mut interfaces,
                    name.clone(),
                    InterfaceAddress {
                        address: ip,
                        family: json!(family),
                        internal: data.IfType == 24, /* IF_TYPE_SOFTWARE_LOOPBACK */
                        mac: mac_text(mac),
                    },
                )?;
                address = unicast.Next;
            }
        }
        adapter = data.Next;
    }
    key_order(&mut interfaces);
    budget.check()?;
    Ok(interfaces)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn native_interfaces(_budget: &mut Budget) -> Result<Vec<freeshow_discovery::NetworkInterface>> {
    Err(error(
        "network_snapshot_unsupported",
        "Native interface capture supports Linux, macOS and Windows",
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn native_hostname(budget: &mut Budget) -> Result<String> {
    budget.check()?;
    #[cfg(target_os = "linux")]
    let mut host = [0u8; 65];
    #[cfg(target_os = "macos")]
    let mut host = [0u8; 257];
    if unsafe { libc::gethostname(host.as_mut_ptr().cast(), host.len()) } != 0 {
        return Err(native_error(std::io::Error::last_os_error().to_string()));
    }
    *host.last_mut().unwrap() = 0;
    let end = host.iter().position(|b| *b == 0).unwrap();
    Ok(String::from_utf8_lossy(&host[..end]).into_owned())
}

#[cfg(windows)]
fn native_hostname(budget: &mut Budget) -> Result<String> {
    use windows_sys::Win32::Networking::WinSock::*;
    budget.check()?;
    let mut wsa = std::mem::MaybeUninit::<WSADATA>::uninit();
    let status = unsafe { WSAStartup(0x0202, wsa.as_mut_ptr()) };
    if status != 0 {
        return Err(native_error(format!("WSAStartup: {status}")));
    }
    struct Winsock;
    impl Drop for Winsock {
        fn drop(&mut self) {
            unsafe {
                WSACleanup();
            }
        }
    }
    let _winsock = Winsock;
    let mut host = [0u16; 256];
    if unsafe { GetHostNameW(host.as_mut_ptr(), host.len() as i32) } != 0 {
        return Err(native_error(format!("GetHostNameW: {}", unsafe {
            WSAGetLastError()
        })));
    }
    let end = host
        .iter()
        .position(|u| *u == 0)
        .ok_or_else(|| snapshot_limit("Hostname not terminated"))?;
    let hostname = String::from_utf16(&host[..end]).map_err(|_| {
        error(
            "network_snapshot_unsupported",
            "Hostname contains lone UTF-16 surrogate",
        )
    })?;
    budget.check()?;
    Ok(hostname)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn native_hostname(_budget: &mut Budget) -> Result<String> {
    Err(error(
        "network_snapshot_unsupported",
        "Native hostname capture supports Linux, macOS and Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, pin::Pin, task::Poll};
    async fn wait_for(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn completed_uncollected_work_and_values_keep_real_admission() {
        let service = NetworkSnapshots::start(SnapshotLimits {
            capacity: 2,
            ..SnapshotLimits::default()
        })
        .unwrap();
        let (entered, ready) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        service.pause_next(entered, gate);
        type Pending = Pin<Box<dyn Future<Output = Result<Captured<Vec<String>>>>>>;
        let mut pending: Vec<Pending> = (0..2)
            .map(|_| {
                let service = service.clone();
                Box::pin(async move {
                    service
                        .addresses_owned(SnapshotControl::default(), ())
                        .await
                }) as Pending
            })
            .collect();
        for future in &mut pending {
            std::future::poll_fn(|cx| {
                assert!(matches!(future.as_mut().poll(cx), Poll::Pending));
                Poll::Ready(())
            })
            .await;
        }
        wait_for(|| ready.try_recv().is_ok()).await;
        assert_eq!(service.status()["active"], 1);
        assert_eq!(service.status()["admitted"], 2);
        release.send(()).unwrap();
        wait_for(|| service.status()["active"] == 0 && service.status()["queued"] == 0).await;
        assert_eq!(
            service.status()["admitted"],
            2,
            "ready oneshots still own permits"
        );
        assert_eq!(
            service
                .addresses_owned(SnapshotControl::default(), ())
                .await
                .err()
                .unwrap()
                .code,
            "network_snapshot_busy"
        );
        drop(pending.pop());
        assert_eq!(service.status()["admitted"], 1);
        let captured = pending.pop().unwrap().await.unwrap();
        assert!(!captured.value().is_empty());
        assert_eq!(service.status()["admitted"], 1);
        let other = service.snapshot(SnapshotControl::default()).await.unwrap();
        assert!(!other.into_value().hostname.is_empty());
        drop(captured);
        assert_eq!(service.status()["admitted"], 0);
        service.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn deadlines_do_not_release_a_held_native_owner_and_shutdown_can_retry() {
        struct Guard(Arc<AtomicUsize>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::AcqRel);
            }
        }
        let service = NetworkSnapshots::start(SnapshotLimits {
            shutdown_timeout: Duration::from_millis(20),
            ..SnapshotLimits::default()
        })
        .unwrap();
        let (entered, ready) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        service.pause_next(entered, gate);
        let drops = Arc::new(AtomicUsize::new(0));
        let guard = Guard(drops.clone());
        let job = service.clone();
        let caller = tokio::spawn(async move {
            job.addresses_owned(
                SnapshotControl {
                    deadline: Instant::now() + Duration::from_millis(25),
                    cancel: CancellationToken::new(),
                },
                guard,
            )
            .await
        });
        wait_for(|| ready.try_recv().is_ok()).await;
        assert_eq!(
            caller.await.unwrap().err().unwrap().code,
            "network_snapshot_deadline"
        );
        assert_eq!(service.status()["admitted"], 1);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert_eq!(
            service.shutdown().await.err().unwrap().code,
            "network_snapshot_shutdown_timeout"
        );
        assert_eq!(service.status()["active"], 1);
        release.send(()).unwrap();
        service.shutdown().await.unwrap();
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert_eq!(service.status()["admitted"], 0);
    }
    #[tokio::test]
    async fn live_addresses_ignore_hostname_failure_and_os_failures_never_fabricate_addresses() {
        let service = NetworkSnapshots::start(SnapshotLimits::default()).unwrap();
        service.fail_hostname(true);
        let ips = service
            .addresses_owned(SnapshotControl::default(), ())
            .await
            .unwrap();
        assert!(!ips.value().is_empty());
        drop(ips);
        assert_eq!(
            service
                .snapshot(SnapshotControl::default())
                .await
                .err()
                .unwrap()
                .code,
            "network_snapshot_os"
        );
        service.fail_interfaces(true);
        assert_eq!(
            service
                .addresses_owned(SnapshotControl::default(), ())
                .await
                .err()
                .unwrap()
                .code,
            "network_snapshot_os"
        );
        service.fail_interfaces(false);
        service.fail_hostname(false);
        let snapshot = service.snapshot(SnapshotControl::default()).await.unwrap();
        assert!(!snapshot.value().hostname.is_empty());
        drop(snapshot);
        service.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn operation_output_cancel_and_deadline_bounds_are_independent() {
        let service = NetworkSnapshots::start(SnapshotLimits {
            operations: 1,
            ..SnapshotLimits::default()
        })
        .unwrap();
        assert_eq!(
            service
                .snapshot(SnapshotControl::default())
                .await
                .err()
                .unwrap()
                .code,
            "network_snapshot_limit"
        );
        assert_eq!(service.status()["nativeCaptures"], 0);
        service.shutdown().await.unwrap();
        let service = NetworkSnapshots::start(SnapshotLimits {
            output_bytes: 2,
            ..SnapshotLimits::default()
        })
        .unwrap();
        assert_eq!(
            service
                .addresses_owned(SnapshotControl::default(), ())
                .await
                .err()
                .unwrap()
                .code,
            "network_snapshot_limit"
        );
        assert!(service.status()["nativeCaptures"].as_u64().unwrap() > 0);
        let control = SnapshotControl::default();
        control.cancel.cancel();
        assert_eq!(
            service.snapshot(control).await.err().unwrap().code,
            "network_snapshot_cancelled"
        );
        assert_eq!(
            service
                .snapshot(SnapshotControl {
                    deadline: Instant::now(),
                    cancel: CancellationToken::new()
                })
                .await
                .err()
                .unwrap()
                .code,
            "network_snapshot_deadline"
        );
        service.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn actual_worker_panic_is_sticky_across_shutdown_retries_and_new_requests() {
        let service = NetworkSnapshots::start(SnapshotLimits::default()).unwrap();
        let (entered, ready) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        service.pause_next(entered, gate);
        let owner = service.clone();
        let panic = tokio::spawn(async move { owner.panic_worker().await });
        wait_for(|| ready.try_recv().is_ok()).await;
        let mut queued = Box::pin(service.addresses_owned(SnapshotControl::default(), ()));
        std::future::poll_fn(|cx| {
            assert!(matches!(queued.as_mut().poll(cx), Poll::Pending));
            Poll::Ready(())
        })
        .await;
        assert_eq!(service.status()["queued"], 1);
        release.send(()).unwrap();
        assert!(panic.await.unwrap().is_err());
        assert!(queued.await.is_err());
        let first = service.shutdown().await.err().unwrap();
        assert_eq!(first.code, "network_snapshot_worker");
        let next = service.shutdown().await.err().unwrap();
        assert_eq!(next.message, first.message);
        assert_eq!(
            service
                .snapshot(SnapshotControl::default())
                .await
                .err()
                .unwrap()
                .message,
            first.message
        );
        assert_eq!(service.terminal_error().unwrap().code, first.code);
        assert_eq!(service.status()["admitted"], 0);
        assert_eq!(service.status()["active"], 0);
        assert_eq!(service.status()["queued"], 0);
    }
}
