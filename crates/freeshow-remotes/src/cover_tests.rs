use super::*;
use crate::{
    CoverControl, CoverErrorRendering, CoverFileStat, CoverFuture, CoverSource, CoverState,
    PreparedCover,
};
use std::{fs::File, io::Write};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::watch,
};

#[derive(Clone, Debug, Default)]
struct Counts {
    entered: usize,
    resolved: usize,
    headers: usize,
    finish_entered: usize,
    finished: usize,
    dropped: usize,
    reads: usize,
    cancelled: usize,
}
struct Gate {
    entered: tokio::sync::Notify,
    release: Semaphore,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: tokio::sync::Notify::new(),
            release: Semaphore::new(0),
        })
    }
    async fn wait(&self) {
        self.entered.notify_one();
        self.release.acquire().await.unwrap().forget();
        // The same remotes runtime must still drive actual entered futures even
        // when transport has failed/panicked or a stop observer timed out.
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    fn open(&self) {
        self.release.add_permits(1);
    }
}
struct Source {
    file: File,
    state: CoverState,
    trace: watch::Sender<Counts>,
    prepare_gate: StdMutex<Option<Arc<Gate>>>,
    finish_gate: StdMutex<Option<Arc<Gate>>>,
    before_failed: AtomicBool,
    prepare_failed: AtomicBool,
    finish_failed: AtomicBool,
    production: bool,
}
impl Source {
    fn new(bytes: &[u8], state: Option<CoverState>, production: bool) -> Arc<Self> {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        Arc::new(Self {
            file,
            state: state.unwrap_or(CoverState::File(
                CoverFileStat::new(bytes.len() as u64, 1_700_000_000_123.0).unwrap(),
            )),
            trace: watch::channel(Counts::default()).0,
            prepare_gate: StdMutex::new(None),
            finish_gate: StdMutex::new(None),
            before_failed: AtomicBool::new(false),
            prepare_failed: AtomicBool::new(false),
            finish_failed: AtomicBool::new(false),
            production,
        })
    }
}
struct Prepared {
    source: Arc<Source>,
    control: CoverControl,
}
impl CoverSource for Arc<Source> {
    fn error_rendering(&self) -> CoverErrorRendering {
        if self.production {
            CoverErrorRendering::Production
        } else {
            CoverErrorRendering::Deferred
        }
    }
    fn prepare(&self, control: CoverControl) -> CoverFuture<'_, Box<dyn PreparedCover>> {
        Box::pin(async move {
            self.trace.send_modify(|s| s.entered += 1);
            let gate = self.prepare_gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.wait().await;
            }
            if control.cancel.is_cancelled() {
                self.trace.send_modify(|s| s.cancelled += 1);
            }
            self.trace.send_modify(|s| s.resolved += 1);
            if self.prepare_failed.load(Ordering::Acquire) {
                return Err(RemoteError::new(
                    "native_prepare_fault",
                    "Actual native preparation failed after settlement",
                ));
            }
            Ok(Box::new(Prepared {
                source: self.clone(),
                control,
            }) as Box<dyn PreparedCover>)
        })
    }
}
impl PreparedCover for Prepared {
    fn state(&self) -> CoverState {
        self.source.state
    }
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        assert!(buffer.len() <= 32768);
        self.source.trace.send_modify(|s| s.reads += 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.source.file.read_at(buffer, offset)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            self.source.file.seek_read(buffer, offset)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (offset, buffer);
            Err(io::ErrorKind::Unsupported.into())
        }
    }
    fn before_headers(&mut self) -> CoverFuture<'_, ()> {
        Box::pin(async move {
            self.source.trace.send_modify(|s| s.headers += 1);
            if self.control.cancel.is_cancelled()
                || self.source.before_failed.load(Ordering::Acquire)
            {
                Err(RemoteError::new(
                    "cover_changed",
                    "Native authority changed before headers",
                ))
            } else {
                Ok(())
            }
        })
    }
    fn finish(self: Box<Self>) -> CoverFuture<'static, ()> {
        Box::pin(async move {
            self.source.trace.send_modify(|s| s.finish_entered += 1);
            let gate = self.source.finish_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.wait().await;
            }
            self.source.trace.send_modify(|s| s.finished += 1);
            if self.source.finish_failed.load(Ordering::Acquire) {
                Err(RemoteError::new(
                    "cover_finish_fault",
                    "Actual native publication cleanup rejected",
                ))
            } else {
                Ok(())
            }
        })
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        self.source.trace.send_modify(|s| s.dropped += 1);
    }
}
fn cfg() -> RemoteConfig {
    RemoteConfig {
        bind_ip: Ipv4Addr::LOCALHOST.into(),
        ..RemoteConfig::default()
    }
}
fn all_ports() -> Value {
    json!({"ports":{"remote":"0","stage":"0","controller":"0"},"disabled":{"remote":false,"stage":false,"controller":false},"data":{}})
}
async fn seen(source: &Source, predicate: impl Fn(&Counts) -> bool) -> Counts {
    let mut trace = source.trace.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let value = trace.borrow_and_update().clone();
            if predicate(&value) {
                return value;
            }
            trace.changed().await.unwrap();
        }
    })
    .await
    .expect("actual native source boundary was not reached")
}
fn run(service: &RemoteService) -> Arc<Run> {
    service
        .inner
        .current
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone()
}
async fn leases_zero(run: &Run) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while run.static_requests.available_permits()
            != run.config.static_workers + run.config.static_queue
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
struct Http {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
async fn get(
    address: std::net::SocketAddr,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
) -> Http {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let mut raw = format!("{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    for (k, v) in headers {
        raw.push_str(&format!("{k}: {v}\r\n"));
    }
    raw.push_str("\r\n");
    stream.write_all(raw.as_bytes()).await.unwrap();
    let mut bytes = vec![];
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let split = bytes
        .windows(4)
        .position(|b| b == b"\r\n\r\n")
        .expect("response headers");
    let head = std::str::from_utf8(&bytes[..split]).unwrap();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    Http {
        status,
        headers: lines
            .map(|s| {
                let (k, v) = s.split_once(':').unwrap();
                (k.to_ascii_lowercase(), v.trim().into())
            })
            .collect(),
        body: bytes[split + 4..].to_vec(),
    }
}

#[tokio::test]
async fn configured_fixed_source_serves_all_ports_ranges_cache_head_and_options() {
    let bytes = b"opaque bytes are not decoded as PNG";
    let source = Source::new(bytes, None, true);
    let roots = tempfile::tempdir().unwrap();
    std::fs::write(roots.path().join("nowplayingcover"), b"public fallback").unwrap();
    let mut config = cfg();
    for kind in ServerKind::ALL {
        config.static_roots.insert(kind, roots.path().into());
    }
    let (service, _events) = RemoteService::new(config).unwrap();
    let bound = service
        .start_with_cover(&all_ports(), 7, Some(Arc::new(source.clone())))
        .await
        .unwrap();
    assert_eq!(
        bound.endpoints.len(),
        3,
        "all three real listeners must exist"
    );
    for address in bound.endpoints.values() {
        let whole = get(
            *address,
            "GET",
            "/NOWPLAYINGCOVER/?filePath=/other.png",
            &[],
        )
        .await;
        assert_eq!(whole.status, 200);
        assert_eq!(whole.body, bytes);
        assert_eq!(whole.headers["content-type"], "image/png");
        assert_eq!(whole.headers["cache-control"], "public, max-age=0");
        assert_eq!(whole.headers["x-powered-by"], "Express");
        let head = get(
            *address,
            "HEAD",
            "/nowplayingcover",
            &[("Range", "bytes=1-3")],
        )
        .await;
        assert_eq!(head.status, 206);
        assert!(head.body.is_empty());
        assert_eq!(head.headers["content-length"], "3");
        let partial = get(
            *address,
            "GET",
            "/nowplayingcover",
            &[("Range", "bytes=1-2,3-4")],
        )
        .await;
        assert_eq!(partial.status, 206);
        assert_eq!(partial.body, &bytes[1..5]);
        let cached = get(
            *address,
            "GET",
            "/nowplayingcover",
            &[("If-None-Match", &whole.headers["etag"])],
        )
        .await;
        assert_eq!(cached.status, 304);
        assert!(cached.body.is_empty());
        assert!(!cached.headers.contains_key("content-length"));
        assert!(!cached.headers.contains_key("content-type"));
        for (key, value, status, reason) in [
            ("If-Match", "bad", 412, "Precondition Failed"),
            ("Range", "bytes=9999-", 416, "Range Not Satisfiable"),
        ] {
            let failure = get(*address, "GET", "/nowplayingcover", &[(key, value)]).await;
            assert_eq!(failure.status, status);
            assert!(String::from_utf8(failure.body).unwrap().contains(reason));
        }
        let before = source.trace.borrow().entered;
        let options = get(
            *address,
            "OPTIONS",
            "/nowplayingcover?path=/other",
            &[("Range", "bytes=9999-")],
        )
        .await;
        assert_eq!(options.status, 200);
        assert_eq!(options.body, b"GET,HEAD");
        assert_eq!(options.headers["allow"], "GET,HEAD");
        assert_eq!(source.trace.borrow().entered, before);
        assert_eq!(
            get(*address, "GET", "/nowplaying%63over", &[]).await.body,
            b"public fallback"
        );
        assert_eq!(
            get(*address, "POST", "/nowplayingcover", &[]).await.status,
            404
        );
        assert_eq!(source.trace.borrow().entered, before);
    }
    let count = source.trace.borrow().entered;
    seen(&source, |s| s.finished == count).await;
    leases_zero(&run(&service)).await;
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_failed_empty_and_deferred_rendering_remain_distinct() {
    for (state, bytes, status, production) in [
        (Some(CoverState::Missing), b"".as_slice(), 404, true),
        (Some(CoverState::Failed), b"".as_slice(), 503, true),
        (None, b"".as_slice(), 200, true),
        (None, b"arbitrary non-PNG".as_slice(), 200, true),
        (None, b"file".as_slice(), 501, false),
    ] {
        let source = Source::new(bytes, state, production);
        let (service, _) = RemoteService::new(cfg()).unwrap();
        let bound = service
            .start_with_cover(&all_ports(), 8, Some(Arc::new(source.clone())))
            .await
            .unwrap();
        for address in bound.endpoints.values() {
            let headers = if production {
                vec![]
            } else {
                vec![("If-Match", "bad")]
            };
            let response = get(*address, "GET", "/nowplayingcover", &headers).await;
            assert_eq!(response.status, status);
            if status == 404 {
                assert!(response.body.is_empty());
                assert_eq!(response.headers["content-length"], "0");
            }
            if status == 200 {
                assert_eq!(response.body, bytes);
            }
            if status == 503 {
                assert_eq!(response.body, b"cover_source_failure");
            }
            if status == 501 {
                assert_eq!(response.body, b"cover_error_rendering_unsupported");
            }
        }
        seen(&source, |s| s.finished == 3).await;
        service.shutdown().await.unwrap();
        assert_eq!(source.trace.borrow().dropped, 3);
    }
    let (service, _) = RemoteService::new(cfg()).unwrap();
    let bound = service.start(&all_ports(), 9).await.unwrap();
    for address in bound.endpoints.values() {
        assert_eq!(
            get(*address, "GET", "/nowplayingcover", &[]).await.status,
            501
        );
    }
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_response_does_not_wait_for_independent_finish_but_stop_retains_it() {
    let source = Source::new(b"", Some(CoverState::Missing), true);
    let finish = Gate::new();
    *source.finish_gate.lock().unwrap() = Some(finish.clone());
    let (service, _) = RemoteService::new(cfg()).unwrap();
    let bound = service
        .start_with_cover(&all_ports(), 10, Some(Arc::new(source.clone())))
        .await
        .unwrap();
    let held = run(&service);
    assert_eq!(
        get(
            bound.endpoints[&ServerKind::REMOTE],
            "GET",
            "/nowplayingcover",
            &[]
        )
        .await
        .status,
        404
    );
    seen(&source, |s| s.finish_entered == 1).await;
    assert_eq!(source.trace.borrow().finished, 0);
    assert_eq!(service.stats().unwrap().static_requests, 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), service.stop())
            .await
            .is_err()
    );
    assert_eq!(source.trace.borrow().finished, 0);
    assert!(service.inner.active.lock().await.is_some());
    finish.open();
    service.stop().await.unwrap();
    assert_eq!(source.trace.borrow().finished, 1);
    assert_eq!(source.trace.borrow().dropped, 1);
    leases_zero(&held).await;
}

#[tokio::test]
async fn entered_preparation_survives_http_abort_and_stopped_or_faulted_transport_until_actual_finish(
) {
    for fault in [None, Some(false), Some(true)] {
        let source = Source::new(b"held native descriptor", None, true);
        let prepare = Gate::new();
        *source.prepare_gate.lock().unwrap() = Some(prepare.clone());
        let (service, _) = RemoteService::new(cfg()).unwrap();
        let bound = service
            .start_with_cover(&all_ports(), 11, Some(Arc::new(source.clone())))
            .await
            .unwrap();
        let held = run(&service);
        let mut stream = TcpStream::connect(bound.endpoints[&ServerKind::REMOTE])
            .await
            .unwrap();
        stream
            .write_all(
                b"GET /nowplayingcover HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        prepare.entered.notified().await;
        drop(stream);
        if let Some(panic) = fault {
            *held.transport_fault.0.lock().unwrap() = Some(panic);
            held.transport_fault.1.notify_one();
            tokio::time::timeout(Duration::from_secs(5), held.cancel.cancelled())
                .await
                .unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(20), service.stop())
                .await
                .is_err()
        );
        assert_eq!(source.trace.borrow().finished, 0);
        assert_eq!(source.trace.borrow().dropped, 0);
        assert!(service.inner.active.lock().await.is_some());
        prepare.open();
        let result = service.stop().await;
        if fault.is_some() {
            assert!(result.is_err());
        } else {
            result.unwrap();
        }
        assert_eq!(source.trace.borrow().finished, 1);
        assert_eq!(source.trace.borrow().dropped, 1);
        assert_eq!(source.trace.borrow().headers, 0);
        assert_eq!(source.trace.borrow().cancelled, 1);
        leases_zero(&held).await;
        service.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn source_revalidation_and_finish_failures_are_explicit_after_actual_cleanup() {
    let source = Source::new(b"not stale bytes", None, true);
    source.before_failed.store(true, Ordering::Release);
    let (service, mut events) = RemoteService::new(cfg()).unwrap();
    let bound = service
        .start_with_cover(&all_ports(), 12, Some(Arc::new(source.clone())))
        .await
        .unwrap();
    let response = get(
        bound.endpoints[&ServerKind::REMOTE],
        "GET",
        "/nowplayingcover",
        &[],
    )
    .await;
    assert_eq!(response.status, 503);
    assert_eq!(response.body, b"cover_changed");
    seen(&source, |s| s.finished == 1).await;
    source.before_failed.store(false, Ordering::Release);
    source.finish_failed.store(true, Ordering::Release);
    assert_eq!(
        get(
            bound.endpoints[&ServerKind::REMOTE],
            "GET",
            "/nowplayingcover",
            &[]
        )
        .await
        .status,
        200
    );
    seen(&source, |s| s.finished == 2).await;
    assert_eq!(service.stop().await.unwrap_err().code, "cover_finish_fault");
    assert_eq!(source.trace.borrow().dropped, 2);
    let mut codes = vec![];
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(20), events.recv()).await
    {
        if let crate::RemoteEventKind::Diagnostic { code, .. } = event.kind {
            codes.push(code);
        }
    }
    assert!(codes.contains(&"cover_changed"));
    assert!(codes.contains(&"cover_finish_fault"));
}

async fn active_requests(run: &Run, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while run.config.static_workers + run.config.static_queue
            - run.static_requests.available_permits()
            != count
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("whole request admission did not reach actual expected state");
}

#[tokio::test]
async fn fixed_workers_queue_and_whole_preparation_leases_saturate_and_drain_without_lost_work() {
    let source = Source::new(b"bounded accepted work", None, true);
    let gate = Gate::new();
    *source.prepare_gate.lock().unwrap() = Some(gate.clone());
    let (service, _events) = RemoteService::new(cfg()).unwrap();
    let bound = service
        .start_with_cover(&all_ports(), 13, Some(Arc::new(source.clone())))
        .await
        .unwrap();
    let held = run(&service);
    let address = bound.endpoints[&ServerKind::REMOTE];
    let mut clients = vec![];
    for index in 0..10 {
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(
                b"GET /nowplayingcover HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        clients.push(client);
        if index < 2 {
            seen(&source, |s| s.entered == index + 1).await;
        }
        active_requests(&held, index + 1).await;
    }
    assert_eq!(source.trace.borrow().entered, 2);
    let rejected = get(address, "GET", "/nowplayingcover", &[]).await;
    assert_eq!(rejected.status, 503);
    assert_eq!(rejected.body, b"cover_capacity");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), service.stop())
            .await
            .is_err()
    );
    assert_eq!(source.trace.borrow().finished, 0);
    assert!(service.inner.active.lock().await.is_some());
    gate.open();
    gate.open();
    service.stop().await.unwrap();
    assert_eq!(
        source.trace.borrow().entered,
        2,
        "queued cancelled work must not enter native preparation"
    );
    assert_eq!(source.trace.borrow().finished, 2);
    assert_eq!(source.trace.borrow().dropped, 2);
    leases_zero(&held).await;
    drop(clients);
    let mut payload = all_ports();
    for (kind, address) in &bound.endpoints {
        payload["ports"][kind.key()] = json!(address.port());
    }
    let rebound = service.start(&payload, 14).await.unwrap();
    assert_eq!(rebound.endpoints, bound.endpoints);
    assert_eq!(
        get(
            rebound.endpoints[&ServerKind::REMOTE],
            "GET",
            "/nowplayingcover",
            &[]
        )
        .await
        .status,
        501
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn slow_actual_socket_body_abort_deadline_and_run_cancel_drain_native_holder() {
    for variant in 0..3 {
        let source = Source::new(&vec![0xa5; 4 * 1024 * 1024], None, true);
        let finish = Gate::new();
        if variant == 0 {
            *source.finish_gate.lock().unwrap() = Some(finish.clone());
        }
        let mut config = cfg();
        if variant == 1 {
            config.body_timeout = Duration::from_secs(1);
        }
        let (service, _events) = RemoteService::new(config).unwrap();
        let bound = service
            .start_with_cover(&all_ports(), 15, Some(Arc::new(source.clone())))
            .await
            .unwrap();
        let held = run(&service);
        held.socket_send_buffer.store(1024, Ordering::Release);
        let mut stream = TcpStream::connect(bound.endpoints[&ServerKind::REMOTE])
            .await
            .unwrap();
        socket2::SockRef::from(&stream)
            .set_recv_buffer_size(1024)
            .unwrap();
        stream
            .write_all(
                b"GET /nowplayingcover HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut header = vec![];
        tokio::time::timeout(Duration::from_secs(5), async {
            while !header.ends_with(b"\r\n\r\n") {
                assert!(header.len() < 16384);
                header.push(stream.read_u8().await.unwrap());
            }
        })
        .await
        .unwrap();
        assert!(header.starts_with(b"HTTP/1.1 200"));
        seen(&source, |s| s.reads >= 4).await;
        assert_eq!(
            source.trace.borrow().finish_entered,
            0,
            "an unread body must retain its real HTTP disposition"
        );
        active_requests(&held, 1).await;
        match variant {
            0 => {
                drop(stream);
                seen(&source, |s| s.finish_entered == 1).await;
                assert_eq!(source.trace.borrow().finished, 0);
                active_requests(&held, 1).await;
                assert!(
                    tokio::time::timeout(Duration::from_millis(20), service.stop())
                        .await
                        .is_err()
                );
                finish.open();
                service.stop().await.unwrap();
            }
            1 => {
                seen(&source, |s| s.finished == 1).await;
                leases_zero(&held).await;
                drop(stream);
                service.stop().await.unwrap();
            }
            _ => {
                service.stop().await.unwrap();
                drop(stream);
            }
        }
        assert_eq!(source.trace.borrow().finished, 1);
        assert_eq!(source.trace.borrow().dropped, 1);
        leases_zero(&held).await;
        assert_eq!(
            held.tcp_connections.available_permits(),
            held.config.max_tcp_connections
        );
        service.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn bounded_headers_reject_before_lookup_and_actual_prepare_errors_never_become_missing() {
    let source = Source::new(b"file", None, true);
    let (service, _events) = RemoteService::new(cfg()).unwrap();
    let bound = service
        .start_with_cover(&all_ports(), 16, Some(Arc::new(source.clone())))
        .await
        .unwrap();
    let address = bound.endpoints[&ServerKind::REMOTE];
    let names: Vec<_> = (0..33).map(|i| format!("x-header-{i}")).collect();
    let many: Vec<_> = names.iter().map(|s| (s.as_str(), "value")).collect();
    let long = "x".repeat(8193);
    for (headers, code) in [
        (many, "cover_header_limit"),
        (vec![("x-long", long.as_str())], "cover_header_limit"),
        (
            vec![("Set-Cookie", "unsupported-array")],
            "cover_header_unsupported",
        ),
    ] {
        let response = get(address, "GET", "/nowplayingcover", &headers).await;
        assert_eq!(response.status, 400);
        assert_eq!(response.body, code.as_bytes());
    }
    assert_eq!(source.trace.borrow().entered, 0);
    source.prepare_failed.store(true, Ordering::Release);
    let gate = Gate::new();
    *source.prepare_gate.lock().unwrap() = Some(gate.clone());
    let request = tokio::spawn(async move { get(address, "GET", "/nowplayingcover", &[]).await });
    gate.entered.notified().await;
    assert!(!request.is_finished());
    assert_eq!(source.trace.borrow().resolved, 0);
    gate.open();
    let response = request.await.unwrap();
    assert_eq!(response.status, 503);
    assert_eq!(response.body, b"native_prepare_fault");
    assert_eq!(source.trace.borrow().resolved, 1);
    assert_eq!(source.trace.borrow().finish_entered, 0);
    service.shutdown().await.unwrap();
}
