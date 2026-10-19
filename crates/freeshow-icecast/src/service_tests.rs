use super::*;
use serde_json::json;
use std::sync::Mutex;
use tokio::net::TcpListener;
static TEST_OWNER: Mutex<()> = Mutex::new(());
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn cancel() -> CancellationToken {
    CancellationToken::new()
}
async fn wait_status(s: &IcecastService, connected: bool) -> Vec<StatusEvent> {
    let until = Instant::now() + Duration::from_secs(3);
    let mut all = Vec::new();
    loop {
        let events = s.take_events();
        let found = events
            .iter()
            .any(|e| e.data.connected == connected && !e.data.connecting);
        all.extend(events);
        if found {
            return all;
        }
        assert!(Instant::now() < until, "status timeout");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}
#[test]
fn loopback_real_packets_metadata_eos_and_retirement() {
    let _guard = TEST_OWNER.lock().unwrap();
    runtime().block_on(async{
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();
        let server=tokio::spawn(async move{let(mut socket,_)=listener.accept().await.unwrap();let mut bytes=Vec::new();socket.read_to_end(&mut bytes).await.unwrap();bytes});
        let context=ContextId(2,3);let s=IcecastService::start(context,Limits::default()).unwrap();
        assert_eq!(IcecastService::start(context,Limits::default()).err().unwrap().code,"owner_exists");
        let config=json!({"enabled":true,"host":"127.0.0.1","port":port,"mount":"music","password":"秘密"});
        s.update_metadata(context,"Artist - First",cancel()).await.unwrap();
        assert_eq!(s.send_audio(context,&[9],Some(&config),cancel()).await.unwrap().disposition(),Disposition::ConnectingDropped);
        wait_status(&s,true).await;
        assert_eq!(s.send_audio(context,&[1,2,3],Some(&config),cancel()).await.unwrap().disposition(),Disposition::Written);
        s.update_metadata(context,"New",cancel()).await.unwrap();s.send_audio(context,&[4],Some(&config),cancel()).await.unwrap();
        s.retire(context).await.unwrap();s.shutdown().await.unwrap();
        assert_eq!(s.send_audio(context,&[],None,cancel()).await.unwrap_err().code,"shutdown");
        let bytes=server.await.unwrap();let head_end=bytes.windows(4).position(|w|w==b"\r\n\r\n").unwrap()+4;
        assert_eq!(&bytes[..head_end],Config::parse(Some(&config),&Limits::default()).unwrap().unwrap().request_header("Artist - First",&Limits::default()).unwrap());
        let mut cursor=head_end;let mut payloads=Vec::new();let mut serials=Vec::new();
        while cursor<bytes.len(){assert_eq!(&bytes[cursor..cursor+4],b"OggS");let laces=bytes[cursor+26]as usize;let len=bytes[cursor+27..cursor+27+laces].iter().map(|b|*b as usize).sum::<usize>();let end=cursor+27+laces+len;
            payloads.push(bytes[cursor+27+laces..end].to_vec());serials.push(u32::from_le_bytes(bytes[cursor+14..cursor+18].try_into().unwrap()));cursor=end;}
        assert_eq!(payloads,[OPUS_HEAD.to_vec(),opus_tags("Artist - First").unwrap(),vec![1,2,3],OPUS_HEAD.to_vec(),opus_tags("New").unwrap(),vec![4],vec![]]);
        assert_eq!(serials[0],serials[2]);assert_eq!(serials[3],serials[6]);
    });
}

#[test]
fn observed_metadata_keeps_admission_failures_separate_from_applied_title() {
    let _guard = TEST_OWNER.lock().unwrap();
    runtime().block_on(async {
        let context = ContextId(19, 3);
        let service = IcecastService::start(context, Limits::default()).unwrap();
        assert_eq!(
            service
                .update_metadata_observed(ContextId(19, 2), "foreign", cancel())
                .await
                .unwrap_err()
                .code,
            "stale_context"
        );
        let stopped = cancel();
        stopped.cancel();
        assert_eq!(
            service
                .update_metadata_observed(context, "cancelled", stopped)
                .await
                .unwrap_err()
                .code,
            "cancelled"
        );
        assert_eq!(
            service
                .update_metadata_observed(context, "invalid\n", cancel())
                .await
                .unwrap_err()
                .code,
            "unsupported_metadata"
        );
        let observed = service
            .update_metadata_observed(context, "Artist - Applied without a socket", cancel())
            .await
            .unwrap();
        assert_eq!(observed.receipt().context(), context);
        assert_eq!(observed.receipt().disposition(), Disposition::Updated);
        assert!(observed.diagnostic().is_none());
        service.shutdown().await.unwrap();
        assert!(service.thread.lock().unwrap().is_none());
    });
}

#[tokio::test]
async fn real_kernel_header_failure_retains_title_receipt_and_actual_cleanup_failure() {
    // The real socket is shut down for writing only after initial headers reach
    // the kernel. No fabricated write result or disconnected-title cache is used.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (mut peer, _) = listener.accept().await.unwrap();
    let state = shared(64);
    let context = ContextId(19, 4);
    let mut owner = Owner::new(context, Limits::default(), state.clone());
    owner.config = Config::parse(
        Some(&json!({"enabled":true,"host":"127.0.0.1"})),
        &owner.limits,
    )
    .unwrap();
    owner.song = "Initial title".into();
    owner.connected(client).await.unwrap();
    // Avoid an unrelated idle-silence write before the admitted metadata job.
    owner.timer = None;
    SockRef::from(owner.socket.as_ref().unwrap())
        .shutdown(std::net::Shutdown::Write)
        .unwrap();
    let (sender, receiver) = mpsc::channel(1);
    let (reply, observed) = oneshot::channel();
    sender
        .send(Command {
            op: Operation::Metadata("Artist - Applied despite failed headers".into()),
            reply,
            _bytes: Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap(),
        })
        .await
        .unwrap();
    drop(sender);
    owner.run(receiver).await.unwrap();
    let receipt = observed.await.unwrap().unwrap();
    assert_eq!(receipt.receipt().context(), context);
    assert_eq!(receipt.receipt().disposition(), Disposition::Updated);
    assert_eq!(receipt.diagnostic().unwrap().code, "socket_write");
    assert_eq!(owner.song, "Artist - Applied despite failed headers");
    assert!(!owner.status.connected);
    assert!(owner.socket.is_none());
    assert!(owner.timer.is_none());
    assert_eq!(state.error().unwrap().code, "teardown_failed");
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), peer.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert!(bytes
        .windows(b"TITLE=Initial title".len())
        .any(|w| w == b"TITLE=Initial title"));
    assert!(!bytes
        .windows(b"Applied despite failed headers".len())
        .any(|w| w == b"Applied despite failed headers"));
}
#[test]
fn admitted_drop_saturation_cancel_and_shutdown_drain() {
    let _guard = TEST_OWNER.lock().unwrap();
    runtime().block_on(async {
        let context = ContextId(1, 9);
        let s = Arc::new(
            IcecastService::start(
                context,
                Limits {
                    queue: 1,
                    ..Limits::default()
                },
            )
            .unwrap(),
        );
        let (entered, entry) = oneshot::channel();
        let (release, gate) = oneshot::channel();
        let hold = s.clone();
        let running = tokio::spawn(async move {
            hold.submit(
                context,
                || Operation::Hold {
                    entered,
                    release: gate,
                },
                1,
                &cancel(),
            )
            .await
        });
        entry.await.unwrap();
        let queued = s.clone();
        let dropped = tokio::spawn(async move {
            queued
                .update_metadata(context, "accepted after drop", cancel())
                .await
        });
        while s.sender.capacity() != 0 {
            tokio::task::yield_now().await;
        }
        dropped.abort();
        let _ = dropped.await;
        assert_eq!(
            s.disconnect(context, cancel()).await.unwrap_err().code,
            "busy"
        );
        let cancelled = cancel();
        cancelled.cancel();
        assert_eq!(
            s.disconnect(context, cancelled).await.unwrap_err().code,
            "cancelled"
        );
        assert_eq!(
            s.disconnect(ContextId(1, 8), cancel())
                .await
                .unwrap_err()
                .code,
            "stale_context"
        );
        let closing = s.clone();
        let shutdown = tokio::spawn(async move { closing.shutdown().await });
        while !s.shared.closing.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            s.disconnect(context, cancel()).await.unwrap_err().code,
            "shutdown"
        );
        assert!(!shutdown.is_finished());
        release.send(()).unwrap();
        running.await.unwrap().unwrap();
        shutdown.await.unwrap().unwrap();
        assert_eq!(s.bytes.available_permits(), s.limits.queued_bytes);
        assert!(s.shared.done.load(Ordering::Acquire));
    });
}
#[test]
fn source_rejection_transient_error_clear_and_cooldown() {
    let _guard = TEST_OWNER.lock().unwrap();
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            assert!(socket.read(&mut bytes).await.unwrap() > 0);
            socket
                .write_all(b"HTTP/1.1 409 Conflict\r\n\r\n")
                .await
                .unwrap();
            let mut all = Vec::new();
            socket.read_to_end(&mut all).await.unwrap();
        });
        let c = ContextId(1, 1);
        let s = IcecastService::start(c, Limits::default()).unwrap();
        let config = json!({"enabled":true,"host":"127.0.0.1","port":port});
        s.send_audio(c, &[1], Some(&config), cancel())
            .await
            .unwrap();
        let mut all = wait_status(&s, false).await;
        all.extend(s.take_events());
        assert!(all
            .iter()
            .any(|e| e.data.error.as_deref() == Some("Mountpoint in use")));
        assert!(all.last().unwrap().data.error.is_none());
        assert_eq!(
            s.send_audio(c, &[2], Some(&config), cancel())
                .await
                .unwrap()
                .disposition(),
            Disposition::CooldownDropped
        );
        s.shutdown().await.unwrap();
        server.await.unwrap();
    });
}
#[test]
fn silence_starts_at_sixty_ms_and_stops_after_disconnect() {
    let _guard = TEST_OWNER.lock().unwrap();
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut out = Vec::new();
            socket.read_to_end(&mut out).await.unwrap();
            out
        });
        let c = ContextId(1, 1);
        let s = IcecastService::start(c, Limits::default()).unwrap();
        s.send_audio(
            c,
            &[],
            Some(&json!({"enabled":true,"host":"127.0.0.1","port":port})),
            cancel(),
        )
        .await
        .unwrap();
        wait_status(&s, true).await;
        tokio::time::sleep(Duration::from_millis(125)).await;
        s.disconnect(c, cancel()).await.unwrap();
        let bytes = server.await.unwrap();
        assert!(bytes.windows(3).filter(|w| *w == SILENCE).count() >= 3);
        assert_eq!(
            s.take_events().iter().filter(|e| !e.data.connected).count(),
            1
        );
        s.shutdown().await.unwrap();
    });
}
#[test]
fn response_overflow_is_sticky_terminal_failure() {
    let _guard = TEST_OWNER.lock().unwrap();
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut b = [0; 4096];
            let _ = socket.read(&mut b).await;
            let _ = socket.write_all(&vec![b'x'; 16384]).await;
            let _ = socket.read_to_end(&mut Vec::new()).await;
        });
        let c = ContextId(1, 1);
        let s = IcecastService::start(
            c,
            Limits {
                response_bytes: 1024,
                ..Limits::default()
            },
        )
        .unwrap();
        s.send_audio(
            c,
            &[],
            Some(&json!({"enabled":true,"host":"127.0.0.1","port":port})),
            cancel(),
        )
        .await
        .unwrap();
        while !s.shared.done.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let a = s.shutdown().await.unwrap_err();
        assert_eq!(a.code, "resource_limit");
        assert_eq!(s.shutdown().await.unwrap_err(), a);
        server.await.unwrap();
    });
}

fn shared(events: usize) -> Arc<Shared> {
    Arc::new(Shared {
        closing: AtomicBool::new(false),
        wake: CancellationToken::new(),
        done: AtomicBool::new(false),
        notify: Notify::new(),
        event_notify: Notify::new(),
        terminal: Mutex::new(None),
        events: Mutex::new(VecDeque::new()),
        event_limit: events,
    })
}

#[tokio::test(start_paused = true)]
async fn exact_native_threshold_and_cooldown_boundaries() {
    let mut owner = Owner::new(ContextId(1, 1), Limits::default(), shared(64));
    assert!(owner.timer.is_none());
    tokio::time::advance(Duration::from_millis(59)).await;
    assert!(!owner.silence_due());
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(owner.silence_due());
    owner.last_real = Instant::now();
    assert!(!owner.silence_due());
    owner.config = Config::parse(
        Some(&json!({"enabled":true,"host":"127.0.0.1"})),
        &owner.limits,
    )
    .unwrap();
    assert!(owner.begin_connect().unwrap());
    owner.disconnect(None).await.unwrap();
    tokio::time::advance(Duration::from_millis(2999)).await;
    assert!(!owner.begin_connect().unwrap());
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(owner.begin_connect().unwrap());
    owner.disconnect(None).await.unwrap();
    assert!(owner.timer.is_none());
}
#[tokio::test]
async fn canceled_connection_future_cannot_deliver_stale_run_effects() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (mut peer, _) = listener.accept().await.unwrap();
    let state = shared(64);
    let mut owner = Owner::new(ContextId(5, 6), Limits::default(), state.clone());
    owner.config = Config::parse(
        Some(&json!({"enabled":true,"host":"127.0.0.1"})),
        &owner.limits,
    )
    .unwrap();
    owner.begin_connect().unwrap();
    let (release, gate) = oneshot::channel::<()>();
    owner.connect = Some(Box::pin(async move {
        let _ = gate.await;
        Ok(client)
    }));
    let (sender, receiver) = mpsc::channel(1);
    let (reply, result) = oneshot::channel();
    sender
        .send(Command {
            op: Operation::Disconnect,
            reply,
            _bytes: Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap(),
        })
        .await
        .unwrap();
    drop(sender);
    owner.run(receiver).await.unwrap();
    result.await.unwrap().unwrap();
    assert!(
        release.send(()).is_err(),
        "retired connection future was dropped"
    );
    let mut byte = [0];
    assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
    assert!(!state
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.data.connected));
}
#[tokio::test]
async fn anchored_socket_teardown_error_is_sticky_and_cleanup_continues() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (peer, _) = listener.accept().await.unwrap();
    SockRef::from(&peer)
        .set_linger(Some(Duration::ZERO))
        .unwrap();
    drop(peer);
    let state = shared(64);
    let mut owner = Owner::new(ContextId(1, 1), Limits::default(), state.clone());
    owner.socket = Some(client);
    owner.status(true, false, None).unwrap();
    // Consume the reset so the next local write fails rather than racing its delivery.
    let mut b = [0];
    let _ = owner.socket.as_mut().unwrap().read(&mut b).await;
    let error = owner.disconnect(None).await.unwrap_err();
    assert_eq!(error.code, "teardown_failed");
    assert!(owner.socket.is_none());
    assert!(owner.timer.is_none());
    assert_eq!(state.error().unwrap(), error);
    owner.disconnect(None).await.unwrap();
    assert_eq!(state.error().unwrap(), error);
}
#[tokio::test]
async fn event_overflow_is_bounded_and_teardown_still_drops_socket() {
    let state = shared(1);
    let mut owner = Owner::new(ContextId(1, 1), Limits::default(), state.clone());
    owner.status(false, true, None).unwrap();
    assert_eq!(
        owner.status(true, false, None).unwrap_err().code,
        "event_overflow"
    );
    assert_eq!(state.events.lock().unwrap().len(), 1);
    assert_eq!(
        owner.disconnect(None).await.unwrap_err().code,
        "event_overflow"
    );
    assert!(owner.socket.is_none());
    assert!(owner.timer.is_none());
}
#[test]
fn receiver_close_waits_for_reserved_admission_and_service_drop_reaps_owner() {
    let _guard = TEST_OWNER.lock().unwrap();
    runtime().block_on(async{
        let c=ContextId(7,8);let s=IcecastService::start(c,Limits::default()).unwrap();let state=s.shared.clone();
        {let slot=s.sender.try_reserve().unwrap();let closing=s.shutdown();tokio::pin!(closing);
        tokio::select!{result=&mut closing=>panic!("shutdown crossed reserved admission: {result:?}"),_=tokio::time::sleep(Duration::from_millis(10))=>{}}
        assert!(state.closing.load(Ordering::Acquire));drop(slot);closing.await.unwrap();}drop(s);
        let s=IcecastService::start(c,Limits::default()).unwrap();let state=s.shared.clone();drop(s);
        let until=Instant::now()+Duration::from_secs(2);while !state.done.load(Ordering::Acquire){assert!(Instant::now()<until);tokio::time::sleep(Duration::from_millis(1)).await;}
    });
}
#[test]
fn shutdown_can_be_awaited_without_tokio_runtime() {
    let _guard = TEST_OWNER.lock().unwrap();
    let s = IcecastService::start(ContextId(9, 9), Limits::default()).unwrap();
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
    let mut future = Box::pin(s.shutdown());
    loop {
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(result) => {
                result.unwrap();
                break;
            }
            std::task::Poll::Pending => std::thread::park(),
        }
    }
}

#[test]
fn localhost_dns_and_header_limit_have_real_owned_socket_diagnostics() {
    let _guard = TEST_OWNER.lock().unwrap();
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut out = Vec::new();
            socket.read_to_end(&mut out).await.unwrap();
            out
        });
        let c = ContextId(1, 2);
        let s = IcecastService::start(
            c,
            Limits {
                header_bytes: 1024,
                ..Limits::default()
            },
        )
        .unwrap();
        s.update_metadata(c, &"A".repeat(2000), cancel())
            .await
            .unwrap();
        assert_eq!(
            s.send_audio(
                c,
                &[1],
                Some(&json!({"enabled":true,"host":"localhost","port":port})),
                cancel()
            )
            .await
            .unwrap()
            .disposition(),
            Disposition::ConnectingDropped
        );
        let events = wait_status(&s, false).await;
        assert!(!events.iter().any(|e| e.data.connected));
        assert!(events.iter().any(|e| e
            .diagnostic
            .as_ref()
            .is_some_and(|e| e.code == "resource_limit")));
        assert!(server.await.unwrap().is_empty());
        s.shutdown().await.unwrap();
    });
}

#[tokio::test]
async fn real_tcp_backpressure_has_finite_write_deadline_and_releases_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (peer, _) = listener.accept().await.unwrap();
    SockRef::from(&client).set_send_buffer_size(1024).unwrap();
    SockRef::from(&peer).set_recv_buffer_size(1024).unwrap();
    let mut owner = Owner::new(
        ContextId(1, 1),
        Limits {
            write_timeout: Duration::from_millis(20),
            ..Limits::default()
        },
        shared(64),
    );
    owner.socket = Some(client);
    owner.status(true, false, None).unwrap();
    let packet = vec![0u8; 16384];
    let until = Instant::now() + Duration::from_secs(3);
    let mut failure = None;
    for _ in 0..128 {
        match owner.write(&packet).await {
            Ok(()) => {}
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
        assert!(Instant::now() < until);
    }
    assert_eq!(failure.unwrap().code, "write_timeout");
    let _ = owner.disconnect(None).await;
    assert!(owner.socket.is_none());
    assert!(owner.timer.is_none());
    drop(peer);
}

#[test]
fn event_notifications_wake_on_status_and_terminal_close_without_polling() {
    let _guard = TEST_OWNER.lock().unwrap();
    runtime().block_on(async{
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();
        let server=tokio::spawn(async move{let(mut socket,_)=listener.accept().await.unwrap();let _=socket.read_to_end(&mut Vec::new()).await;});
        let c=ContextId(1,1);let s=IcecastService::start(c,Limits::default()).unwrap();
        {let pending=s.next_events();tokio::pin!(pending);tokio::select!{e=&mut pending=>panic!("unexpected idle status: {e:?}"),_=tokio::time::sleep(Duration::from_millis(5))=>{}}}
        s.send_audio(c,&[],Some(&json!({"enabled":true,"host":"127.0.0.1","port":port})),cancel()).await.unwrap();
        let mut all=s.next_events().await.unwrap();while !all.iter().any(|e|e.data.connected){all.extend(s.next_events().await.unwrap());}
        assert!(all[0].data.connecting);assert!(all.iter().all(|e|e.context==c&&e.run==1));
        s.shutdown().await.unwrap();let final_events=s.next_events().await.unwrap();assert!(!final_events.last().unwrap().data.connected);
        assert_eq!(s.next_events().await.unwrap_err().code,"shutdown");server.await.unwrap();
    });
}

#[test]
fn dropped_admitted_packet_and_cancelled_waiter_still_write_before_retirement_eos() {
    let _guard = TEST_OWNER.lock().unwrap();
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            socket.read_to_end(&mut bytes).await.unwrap();
            bytes
        });
        let context = ContextId(6, 7);
        let s = Arc::new(
            IcecastService::start(
                context,
                Limits {
                    queue: 1,
                    ..Limits::default()
                },
            )
            .unwrap(),
        );
        let config = json!({"enabled":true,"host":"127.0.0.1","port":port});
        s.send_audio(context, &[99], Some(&config), cancel())
            .await
            .unwrap();
        wait_status(&s, true).await;
        let (entered, entry) = oneshot::channel();
        let (release, gate) = oneshot::channel();
        let held = s.clone();
        let running = tokio::spawn(async move {
            held.submit(
                context,
                || Operation::Hold {
                    entered,
                    release: gate,
                },
                1,
                &cancel(),
            )
            .await
        });
        entry.await.unwrap();
        let request_cancel = cancel();
        let token = request_cancel.clone();
        let queued = s.clone();
        let dropped = tokio::spawn(async move {
            queued
                .send_audio(context, &[0xaa, 0xbb, 0xcc], Some(&config), token)
                .await
        });
        while s.sender.capacity() != 0 {
            tokio::task::yield_now().await;
        }
        request_cancel.cancel();
        dropped.abort();
        let _ = dropped.await;
        let retiring = s.clone();
        let shutdown = tokio::spawn(async move { retiring.retire(context).await });
        while !s.shared.closing.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        release.send(()).unwrap();
        running.await.unwrap().unwrap();
        shutdown.await.unwrap().unwrap();
        let bytes = server.await.unwrap();
        let mut cursor = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let mut payloads = Vec::new();
        let mut flags = Vec::new();
        while cursor < bytes.len() {
            let laces = bytes[cursor + 26] as usize;
            let start = cursor + 27 + laces;
            let end = start
                + bytes[cursor + 27..start]
                    .iter()
                    .map(|n| *n as usize)
                    .sum::<usize>();
            payloads.push(bytes[start..end].to_vec());
            flags.push(bytes[cursor + 5]);
            cursor = end;
        }
        assert!(payloads.iter().any(|p| p == &[0xaa, 0xbb, 0xcc]));
        assert_eq!(flags.last(), Some(&4));
        assert!(payloads.last().unwrap().is_empty());
    });
}
