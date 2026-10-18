use super::*;
use std::sync::mpsc as std_mpsc;
static TEST_OWNER: Mutex<()> = Mutex::new(());
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn cancel() -> CancellationToken {
    CancellationToken::new()
}
fn info() -> NativeInfo {
    NativeInfo {
        version: "controlled lifecycle fixture".into(),
        library: "test-only".into(),
        state_bytes: 16,
        lookahead: 312,
        default_bitrate: 120000,
        rate: RATE,
        channels: CHANNELS,
        frame_samples: SAMPLES,
        max_packet_bytes: MAX_PACKET,
    }
}
struct Fake {
    n: u8,
    entered: Option<std_mpsc::Sender<()>>,
    gate: Option<std_mpsc::Receiver<()>>,
    failure: Option<&'static str>,
    dropped: Arc<AtomicBool>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}
impl Codec for Fake {
    fn info(&self) -> NativeInfo {
        info()
    }
    fn encode(&mut self, _pcm: &[u8; PCM_BYTES]) -> Result<Vec<u8>> {
        self.n += 1;
        if let Some(entered) = self.entered.take() {
            entered.send(()).unwrap();
        }
        if let Some(gate) = self.gate.take() {
            gate.recv().unwrap();
        }
        match self.failure {
            Some("panic") => panic!("controlled owner failure"),
            Some("late") => std::thread::sleep(Duration::from_millis(20)),
            Some("encode") if self.n == 2 => return Err(Error::native("encode_failed", -1)),
            Some("empty") => return Ok(Vec::new()),
            _ => {}
        }
        Ok(vec![self.n])
    }
    fn dispose(self: Box<Self>) -> Result<()> {
        if self.failure == Some("dispose") {
            Err(Error::new(
                "library_close_failed",
                "controlled dispose failure",
            ))
        } else {
            Ok(())
        }
    }
}
fn fake(failure: Option<&'static str>) -> (Box<dyn Codec>, Arc<AtomicBool>) {
    let dropped = Arc::new(AtomicBool::new(false));
    (
        Box::new(Fake {
            n: 0,
            entered: None,
            gate: None,
            failure,
            dropped: dropped.clone(),
        }),
        dropped,
    )
}
#[test]
fn accepted_drop_drain_and_retained_packet_quota() {
    let _guard = TEST_OWNER.lock().unwrap();
    rt().block_on(async {
        let (entered, entry) = std_mpsc::channel();
        let (release, gate) = std_mpsc::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let codec = Fake {
            n: 0,
            entered: Some(entered),
            gate: Some(gate),
            failure: None,
            dropped: dropped.clone(),
        };
        let c = ContextId(1, 2);
        let service = Arc::new(
            OpusService::start_with(
                c,
                Limits {
                    queue: 1,
                    retained: 2,
                    ..Limits::default()
                },
                Box::new(move || Ok(Box::new(codec))),
            )
            .unwrap(),
        );
        assert!(service.capability().await.is_available());
        let first = service.clone();
        let waiting = tokio::spawn(async move { first.encode(c, &[0; PCM_BYTES], cancel()).await });
        loop {
            match entry.try_recv() {
                Ok(()) => break,
                Err(std_mpsc::TryRecvError::Empty) => tokio::task::yield_now().await,
                Err(e) => panic!("{e}"),
            }
        }
        let next = service.clone();
        let second = tokio::spawn(async move { next.encode(c, &[0; PCM_BYTES], cancel()).await });
        loop {
            let full = service.sender.lock().unwrap().as_ref().unwrap().capacity() == 0;
            if full {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            service
                .encode(c, &[0; PCM_BYTES], cancel())
                .await
                .unwrap_err()
                .code,
            "busy"
        );
        waiting.abort();
        let _ = waiting.await;
        release.send(()).unwrap();
        let packet = second.await.unwrap().unwrap();
        assert_eq!(packet.bytes(c).unwrap(), &[2]);
        assert_eq!(packet.sequence(), 2);
        let p = service.encode(c, &[0; PCM_BYTES], cancel()).await.unwrap();
        assert_eq!(p.bytes(c).unwrap(), &[3]);
        assert_eq!(
            service
                .encode(c, &[0; PCM_BYTES], cancel())
                .await
                .unwrap_err()
                .code,
            "busy"
        );
        service.retire(c).await.unwrap();
        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(packet.bytes(c).unwrap_err().code, "shutdown");
        drop(packet);
        drop(p);
        assert_eq!(service.retained.available_permits(), 2);
        service.shutdown().await.unwrap();
    });
}
#[test]
fn preadmission_cancel_stale_and_frame_rejection_do_not_advance() {
    let _guard = TEST_OWNER.lock().unwrap();
    rt().block_on(async {
        let (codec, _) = fake(None);
        let c = ContextId(1, 1);
        let s = OpusService::start_with(c, Limits::default(), Box::new(|| Ok(codec))).unwrap();
        let token = cancel();
        token.cancel();
        assert_eq!(
            s.encode(c, &[0; PCM_BYTES], token).await.unwrap_err().code,
            "cancelled"
        );
        assert_eq!(
            s.encode(ContextId(1, 2), &[0; PCM_BYTES], cancel())
                .await
                .unwrap_err()
                .code,
            "stale_context"
        );
        for size in [0, 1, PCM_BYTES - 1, PCM_BYTES + 1, PCM_BYTES * 2] {
            assert_eq!(
                s.encode(c, &vec![0; size], cancel())
                    .await
                    .unwrap_err()
                    .code,
                "unsupported_frame"
            );
        }
        let packet = s.encode(c, &[0; PCM_BYTES], cancel()).await.unwrap();
        assert_eq!(packet.sequence(), 1);
        assert_eq!(packet.bytes(c).unwrap(), &[1]);
        assert_eq!(
            packet.bytes(ContextId(4, 4)).unwrap_err().code,
            "stale_context"
        );
        s.shutdown().await.unwrap();
    });
}
#[test]
fn ordinary_encode_error_continues_without_state_reset() {
    let _guard = TEST_OWNER.lock().unwrap();
    rt().block_on(async {
        let (codec, _) = fake(Some("encode"));
        let c = ContextId(1, 1);
        let s = OpusService::start_with(c, Limits::default(), Box::new(|| Ok(codec))).unwrap();
        drop(s.encode(c, &[0; PCM_BYTES], cancel()).await.unwrap());
        assert_eq!(
            s.encode(c, &[0; PCM_BYTES], cancel())
                .await
                .unwrap_err()
                .native_code,
            Some(-1)
        );
        let packet = s.encode(c, &[0; PCM_BYTES], cancel()).await.unwrap();
        assert_eq!(packet.bytes(c).unwrap(), &[3]);
        assert_eq!(packet.sequence(), 3);
        s.shutdown().await.unwrap();
    });
}
#[test]
fn panic_deadline_and_contract_failures_are_sticky() {
    let _guard = TEST_OWNER.lock().unwrap();
    rt().block_on(async {
        for failure in ["panic", "late", "empty"] {
            let (codec, dropped) = fake(Some(failure));
            let c = ContextId(1, 1);
            let s = OpusService::start_with(
                c,
                Limits {
                    call_budget: Duration::from_millis(5),
                    ..Limits::default()
                },
                Box::new(|| Ok(codec)),
            )
            .unwrap();
            let e = s.encode(c, &[0; PCM_BYTES], cancel()).await.unwrap_err();
            assert_eq!(
                e.code,
                match failure {
                    "panic" => "owner_panicked",
                    "late" => "codec_deadline",
                    _ => "codec_contract",
                }
            );
            let terminal = s.shutdown().await.unwrap_err();
            assert_eq!(terminal, e);
            assert_eq!(s.shutdown().await.unwrap_err(), e);
            assert!(dropped.load(Ordering::Acquire));
            assert!(!s.capability().await.is_available());
        }
    });
}
#[test]
fn shutdown_drains_accepted_work_and_never_depends_on_retained_packets() {
    let _guard = TEST_OWNER.lock().unwrap();
    rt().block_on(async {
        let (entered, entry) = std_mpsc::channel();
        let (release, gate) = std_mpsc::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let codec = Fake {
            n: 0,
            entered: Some(entered),
            gate: Some(gate),
            failure: None,
            dropped: dropped.clone(),
        };
        let c = ContextId(1, 2);
        let s = Arc::new(
            OpusService::start_with(c, Limits::default(), Box::new(move || Ok(Box::new(codec))))
                .unwrap(),
        );
        let active = s.clone();
        let work = tokio::spawn(async move { active.encode(c, &[0; PCM_BYTES], cancel()).await });
        while entry.try_recv().is_err() {
            tokio::task::yield_now().await;
        }
        let closing = s.clone();
        let shutdown = tokio::spawn(async move { closing.shutdown().await });
        let closing = s.clone();
        let concurrent = tokio::spawn(async move { closing.shutdown().await });
        while !s.shared.closed.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        assert!(!shutdown.is_finished());
        assert!(!concurrent.is_finished());
        assert_eq!(
            s.encode(c, &[0; PCM_BYTES], cancel())
                .await
                .unwrap_err()
                .code,
            "shutdown"
        );
        release.send(()).unwrap();
        let packet = work.await.unwrap().unwrap();
        shutdown.await.unwrap().unwrap();
        concurrent.await.unwrap().unwrap();
        assert!(s.shared.joined.load(Ordering::Acquire));
        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(packet.bytes(c).unwrap_err().code, "shutdown");
    });
}
#[test]
fn absent_library_is_explicit_and_portable() {
    let _guard = TEST_OWNER.lock().unwrap();
    rt().block_on(async {
        let path = std::env::temp_dir().join("freeshow-opus-deliberately-absent-library-3d230c37");
        let s = OpusService::start(
            ContextId(1, 1),
            // Deliberately absent, host-owned test path; no library code executes.
            unsafe { LibrarySource::trusted_path(&path) }.unwrap(),
            Limits::default(),
        )
        .unwrap();
        let Capability::Unavailable(e) = s.capability().await else {
            panic!("Missing library reported available")
        };
        assert_eq!(e.code, "library_unavailable");
        assert_eq!(
            s.encode(ContextId(1, 1), &[0; PCM_BYTES], cancel())
                .await
                .unwrap_err()
                .code,
            "library_unavailable"
        );
        s.shutdown().await.unwrap();
    });
}
#[test]
fn real_system_codec_or_explicit_unavailable() {
    let _guard = TEST_OWNER.lock().unwrap();
    rt().block_on(async {
        let c = ContextId(1, 1);
        let s = OpusService::start(c, LibrarySource::system(), Limits::default()).unwrap();
        match s.capability().await {
            Capability::Available(info) => {
                assert!(matches!(
                    info.version.as_str(),
                    "libopus 1.3.1" | "libopus 1.4"
                ));
                let packet = s.encode(c, &[0; PCM_BYTES], cancel()).await.unwrap();
                let mut decoder = testing::Decoder::open(LibrarySource::system()).unwrap();
                assert_eq!(
                    decoder.decode(packet.bytes(c).unwrap()).unwrap().len(),
                    SAMPLES * 2
                );
            }
            Capability::Unavailable(e) => {
                eprintln!(
                    "REAL OPUS CAPABILITY UNAVAILABLE: {e}; controlled ownership tests still run"
                );
                assert!(
                    std::env::var_os("FREESHOW_REQUIRE_OPUS").is_none(),
                    "Real codec required: {e}"
                );
                assert_eq!(
                    s.encode(c, &[0; PCM_BYTES], cancel())
                        .await
                        .unwrap_err()
                        .code,
                    e.code
                );
            }
        }
        s.shutdown().await.unwrap();
    });
}
#[test]
fn singleton_factory_failure_drop_and_limits() {
    let _guard = TEST_OWNER.lock().unwrap();
    rt().block_on(async {
        let c = ContextId(1, 1);
        assert!(OpusService::start(
            c,
            LibrarySource::system(),
            Limits {
                queue: 17,
                ..Limits::default()
            }
        )
        .is_err());
        let (codec, _) = fake(None);
        let live = OpusService::start_with(c, Limits::default(), Box::new(|| Ok(codec))).unwrap();
        live.capability().await;
        assert_eq!(
            OpusService::start(c, LibrarySource::system(), Limits::default())
                .err()
                .unwrap()
                .code,
            "owner_exists"
        );
        live.shutdown().await.unwrap();
        let s = OpusService::start_with(
            c,
            Limits::default(),
            Box::new(|| panic!("initialization fixture")),
        )
        .unwrap();
        assert!(!s.capability().await.is_available());
        assert_eq!(s.shutdown().await.unwrap_err().code, "owner_panicked");
        drop(s);
        let (codec, dropped) = fake(None);
        let s = OpusService::start_with(c, Limits::default(), Box::new(|| Ok(codec))).unwrap();
        s.capability().await;
        let state = s.shared.clone();
        drop(s);
        while !state.done.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        assert!(dropped.load(Ordering::Acquire));
    });
}

#[test]
fn explicit_dispose_failure_is_sticky_and_retained_packets_do_not_hold_codec() {
    let _guard = TEST_OWNER.lock().unwrap();
    rt().block_on(async {
        let (codec, dropped) = fake(Some("dispose"));
        let c = ContextId(1, 1);
        let s = OpusService::start_with(c, Limits::default(), Box::new(|| Ok(codec))).unwrap();
        let packet = s.encode(c, &[0; PCM_BYTES], cancel()).await.unwrap();
        let error = s.shutdown().await.unwrap_err();
        assert_eq!(error.code, "library_close_failed");
        assert_eq!(s.shutdown().await.unwrap_err(), error);
        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(packet.bytes(c).unwrap_err(), error);
    });
}
#[test]
fn shutdown_is_runtime_independent_and_fences_unavailable_pending_admission() {
    let _guard = TEST_OWNER.lock().unwrap();
    let (codec, dropped) = fake(None);
    let s = OpusService::start_with(ContextId(1, 1), Limits::default(), Box::new(|| Ok(codec)))
        .unwrap();
    struct Wake(std::thread::Thread);
    impl std::task::Wake for Wake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Wake(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut shutdown = Box::pin(s.shutdown());
    loop {
        match std::future::Future::poll(shutdown.as_mut(), &mut context) {
            std::task::Poll::Ready(result) => {
                result.unwrap();
                break;
            }
            std::task::Poll::Pending => std::thread::park(),
        }
    }
    assert!(dropped.load(Ordering::Acquire));
    assert!(s.shared.closed.load(Ordering::Acquire));
    assert_eq!(s.retained.available_permits(), 8);
}
