use super::*;
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex as StdMutex,
};
use tokio::sync::{Notify, Semaphore};

struct Probe {
    active: AtomicUsize,
    maximum: AtomicUsize,
    starts: StdMutex<Vec<String>>,
    started: Notify,
    dropped: Notify,
    permits: Semaphore,
}

impl Default for Probe {
    fn default() -> Self {
        Self {
            active: 0.into(),
            maximum: 0.into(),
            starts: StdMutex::new(Vec::new()),
            started: Notify::new(),
            dropped: Notify::new(),
            permits: Semaphore::new(0),
        }
    }
}

struct Active(Arc<Probe>);

impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.dropped.notify_one();
    }
}

fn controlled_sender(capacity: usize, timeout: Duration) -> (OscSender, Arc<Probe>) {
    let probe = Arc::new(Probe::default());
    let capture = probe.clone();
    let sender = OscSender::start_with(
        OscSenderConfig {
            queue_capacity: capacity,
            send_timeout: timeout,
        },
        move |prepared| {
            let probe = capture.clone();
            async move {
                let active = probe.active.fetch_add(1, Ordering::SeqCst) + 1;
                probe.maximum.fetch_max(active, Ordering::SeqCst);
                probe
                    .starts
                    .lock()
                    .unwrap()
                    .push(prepared.message.address.clone());
                let _active = Active(probe.clone());
                probe.started.notify_one();
                probe.permits.acquire().await.unwrap().forget();
                if prepared.message.address == "/fail" {
                    return Err(OscError::new(
                        OscErrorKind::SendFailed,
                        "injected OS boundary failure",
                    ));
                }
                Ok(EmitOutcome::Sent {
                    host: prepared.host,
                    port: prepared.port,
                    address: prepared.message.address,
                    bytes: prepared.message.packet.len(),
                })
            }
        },
    )
    .unwrap();
    (sender, probe)
}

fn payload(address: &str) -> Value {
    json!({"signal":{"host":"127.0.0.1","port":1},"data":address})
}

#[tokio::test]
async fn queue_is_bounded_cancellation_and_failure_release_capacity() {
    let (sender, probe) = controlled_sender(2, Duration::from_secs(2));
    let active = sender.try_emit(&payload("/active")).unwrap();
    probe.started.notified().await;
    let failed = sender.try_emit(&payload("/fail")).unwrap();
    let kept = sender.try_emit(&payload("/kept")).unwrap();
    assert_eq!(
        sender.try_emit(&payload("/full")).unwrap_err().kind,
        OscErrorKind::QueueFull
    );
    assert_eq!(*probe.starts.lock().unwrap(), ["/active"]);
    drop(active);
    probe.started.notified().await;
    probe.permits.add_permits(1);
    assert_eq!(
        failed.wait().await.unwrap_err().kind,
        OscErrorKind::SendFailed
    );
    probe.started.notified().await;
    let recovered = sender.try_emit(&payload("/recovered")).unwrap();
    probe.permits.add_permits(2);
    kept.wait().await.unwrap();
    recovered.wait().await.unwrap();
    sender.shutdown().await;
    assert_eq!(
        *probe.starts.lock().unwrap(),
        ["/active", "/fail", "/kept", "/recovered"]
    );
    assert_eq!(probe.maximum.load(Ordering::SeqCst), 1);
    assert_eq!(probe.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn async_admission_waits_for_capacity_and_preserves_fifo() {
    let (sender, probe) = controlled_sender(1, Duration::from_secs(2));
    let first = sender.try_emit(&payload("/first")).unwrap();
    probe.started.notified().await;
    let second = sender.try_emit(&payload("/second")).unwrap();
    let third_payload = payload("/third");
    let third = sender.emit(&third_payload);
    tokio::pin!(third);
    tokio::select! {
        biased;
        _ = &mut third => panic!("full queue admitted an extra request"),
        _ = tokio::task::yield_now() => {},
    }
    assert_eq!(*probe.starts.lock().unwrap(), ["/first"]);
    probe.permits.add_permits(3);
    let (first, second, third) = tokio::join!(first.wait(), second.wait(), third);
    first.unwrap();
    second.unwrap();
    third.unwrap();
    sender.shutdown().await;
    assert_eq!(
        *probe.starts.lock().unwrap(),
        ["/first", "/second", "/third"]
    );
    assert_eq!(probe.maximum.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn shutdown_cancels_active_queued_and_pending_admission() {
    let (sender, probe) = controlled_sender(1, Duration::from_secs(2));
    let active = sender.try_emit(&payload("/active")).unwrap();
    probe.started.notified().await;
    let queued = sender.try_emit(&payload("/queued")).unwrap();
    let blocked_payload = payload("/blocked");
    let blocked = sender.emit(&blocked_payload);
    tokio::pin!(blocked);
    tokio::select! {
        biased;
        _ = &mut blocked => panic!("full queue admitted an extra request"),
        _ = tokio::task::yield_now() => {},
    }
    sender.shutdown().await;
    assert_eq!(
        active.wait().await.unwrap_err().kind,
        OscErrorKind::Cancelled
    );
    assert_eq!(
        queued.wait().await.unwrap_err().kind,
        OscErrorKind::Cancelled
    );
    assert_eq!(blocked.await.unwrap_err().kind, OscErrorKind::Closed);
    assert_eq!(probe.active.load(Ordering::SeqCst), 0);
    assert_eq!(*probe.starts.lock().unwrap(), ["/active"]);
}

#[tokio::test]
async fn shutdown_never_waits_for_an_outstanding_queue_reservation() {
    let (sender, probe) = controlled_sender(1, Duration::from_secs(2));
    let active = sender.try_emit(&payload("/active")).unwrap();
    probe.started.notified().await;
    // Model the exact reserve-ready / concurrent shutdown race independently
    // of scheduler timing. The holder deliberately keeps its permit alive.
    let permit = sender.queue.reserve().await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), sender.shutdown())
        .await
        .unwrap();
    assert_eq!(
        active.wait().await.unwrap_err().kind,
        OscErrorKind::Cancelled
    );
    let (reply, receiver) = oneshot::channel();
    permit.send(Job {
        prepared: prepare_emit(&payload("/late")).unwrap().unwrap(),
        deadline: Instant::now() + Duration::from_secs(1),
        reply,
    });
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            OscTicket {
                reply: receiver,
                stopped: sender.state.shutdown.subscribe()
            }
            .wait()
        )
        .await
        .unwrap()
        .unwrap_err()
        .kind,
        OscErrorKind::Cancelled
    );
    assert_eq!(*probe.starts.lock().unwrap(), ["/active"]);
}

#[tokio::test]
async fn dropping_last_sender_cancels_active_and_queued_jobs() {
    let (sender, probe) = controlled_sender(2, Duration::from_secs(2));
    let active = sender.try_emit(&payload("/active")).unwrap();
    probe.started.notified().await;
    let queued = sender.try_emit(&payload("/queued")).unwrap();
    drop(sender);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), active.wait())
            .await
            .unwrap()
            .unwrap_err()
            .kind,
        OscErrorKind::Cancelled
    );
    assert_eq!(
        queued.wait().await.unwrap_err().kind,
        OscErrorKind::Cancelled
    );
    // Drop signals the worker; explicit shutdown additionally joins it. Ticket
    // cancellation is observable before that detached worker is rescheduled.
    tokio::time::timeout(Duration::from_secs(1), probe.dropped.notified())
        .await
        .unwrap();
    assert_eq!(probe.active.load(Ordering::SeqCst), 0);
    assert_eq!(*probe.starts.lock().unwrap(), ["/active"]);
}

#[tokio::test(start_paused = true)]
async fn deadlines_cancel_active_and_expired_queued_work_without_sending() {
    let (sender, probe) = controlled_sender(2, Duration::from_millis(100));
    let active = sender.try_emit(&payload("/active")).unwrap();
    probe.started.notified().await;
    let queued = sender.try_emit(&payload("/expired")).unwrap();
    tokio::time::advance(Duration::from_millis(101)).await;
    assert_eq!(active.wait().await.unwrap_err().kind, OscErrorKind::Timeout);
    assert_eq!(queued.wait().await.unwrap_err().kind, OscErrorKind::Timeout);
    assert_eq!(*probe.starts.lock().unwrap(), ["/active"]);
    assert_eq!(probe.active.load(Ordering::SeqCst), 0);
    // A deadline failure must not poison the worker or its remaining capacity.
    probe.permits.add_permits(1);
    sender.emit(&payload("/recovered")).await.unwrap();
    sender.shutdown().await;
}

#[tokio::test]
async fn ignored_data_never_uses_the_queue_and_invalid_data_is_an_error() {
    let (sender, probe) = controlled_sender(1, Duration::from_secs(2));
    let active = sender.try_emit(&payload("/active")).unwrap();
    let ignored = sender
        .try_emit(&json!({"data":{"typed":[1,true]}}))
        .unwrap();
    assert_eq!(ignored.wait().await.unwrap(), EmitOutcome::Ignored);
    assert_eq!(
        sender.emit(&json!({"data":null})).await.unwrap(),
        EmitOutcome::Ignored
    );
    assert_eq!(
        sender.try_emit(&json!({"data":""})).unwrap_err().kind,
        OscErrorKind::InvalidAddress
    );
    sender.shutdown().await;
    assert_eq!(
        active.wait().await.unwrap_err().kind,
        OscErrorKind::Cancelled
    );
    assert!(probe.starts.lock().unwrap().is_empty());
}

#[test]
fn config_requires_bounded_resources_and_a_runtime() {
    assert!(OscSender::start(OscSenderConfig::default()).is_err());
    for config in [
        OscSenderConfig {
            queue_capacity: 0,
            ..Default::default()
        },
        OscSenderConfig {
            queue_capacity: 1025,
            ..Default::default()
        },
        OscSenderConfig {
            send_timeout: Duration::ZERO,
            ..Default::default()
        },
        OscSenderConfig {
            send_timeout: Duration::from_secs(61),
            ..Default::default()
        },
    ] {
        assert_eq!(
            OscSender::start(config).err().unwrap().kind,
            OscErrorKind::InvalidConfig
        );
    }
}

#[test]
fn message_and_parse_limits_are_checked_before_network_work() {
    assert_eq!(
        encode_message(&format!("/{}", "x".repeat(MAX_PACKET_BYTES)))
            .unwrap_err()
            .kind,
        OscErrorKind::PacketTooLarge
    );
    assert_eq!(
        encode_message(&format!("/x {}", "1".repeat(MAX_COMMAND_BYTES)))
            .unwrap_err()
            .kind,
        OscErrorKind::PacketTooLarge
    );
    assert_eq!(
        encode_message(&format!("/x {}", "true ".repeat(70_000)))
            .unwrap_err()
            .kind,
        OscErrorKind::PacketTooLarge
    );
    assert_eq!(
        prepare_emit(&json!({"data":"/x","signal":{"host":"x".repeat(MAX_HOST_BYTES + 1)}}))
            .unwrap_err()
            .kind,
        OscErrorKind::InvalidHost
    );
    assert_eq!(
        prepare_emit(&json!({"data":"/x","signal":{"host":"127.0.0.1\0"}}))
            .unwrap_err()
            .kind,
        OscErrorKind::InvalidHost
    );
}

#[tokio::test]
async fn dns_has_one_worker_one_pending_slot_skips_cancelled_and_recovers() {
    let (entered, entries) = std_mpsc::channel();
    let (unblock, gate) = std_mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let resolver = Resolver::start_with(move |host, port| {
        count.fetch_add(1, Ordering::SeqCst);
        entered.send(host.to_owned()).unwrap();
        if host == "blocked" {
            gate.recv().unwrap();
        }
        if host == "failure" {
            return Err(OscError::new(
                OscErrorKind::ResolveFailed,
                "injected DNS failure",
            ));
        }
        Ok(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
    })
    .unwrap();
    let active = resolver.request("blocked", 1).unwrap();
    assert_eq!(
        entries.recv_timeout(Duration::from_secs(1)).unwrap(),
        "blocked"
    );
    let queued = resolver.request("cancelled", 2).unwrap();
    assert_eq!(
        resolver.request("full", 3).unwrap_err().kind,
        OscErrorKind::ResolverBusy
    );
    // Timed-out/cancelled in-flight getaddrinfo cannot be interrupted. Dropping
    // receivers neither creates another worker nor retains a UDP socket.
    drop(active);
    drop(queued);
    unblock.send(()).unwrap();
    // Wait until the worker has consumed the cancelled queue entry before
    // submitting recovery work. The bounded busy result is safe to retry.
    let recovered = loop {
        match resolver.request("failure", 4) {
            Ok(reply) => break reply,
            Err(error) if error.kind == OscErrorKind::ResolverBusy => {
                tokio::task::yield_now().await
            }
            Err(error) => panic!("{error}"),
        }
    };
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), recovered)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .kind,
        OscErrorKind::ResolveFailed
    );
    let success = resolver
        .request("recovered", 5)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(success, SocketAddrV4::new(Ipv4Addr::LOCALHOST, 5));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "cancelled pending DNS should not run"
    );
    assert_eq!(
        entries.try_iter().collect::<Vec<_>>(),
        ["failure", "recovered"]
    );
}
