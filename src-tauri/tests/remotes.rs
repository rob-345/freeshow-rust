#[path = "../src/context_changes.rs"]
mod context_changes;
#[allow(dead_code)]
#[path = "../src/discovery.rs"]
mod discovery;
#[allow(dead_code)]
#[path = "../src/network_snapshot.rs"]
mod network_snapshot;
#[allow(dead_code)]
#[path = "../src/now_playing_cover_file.rs"]
mod now_playing_cover_file;
#[allow(dead_code)]
#[path = "../src/remote_cover.rs"]
mod remote_cover;
#[allow(dead_code)]
#[path = "../src/remote_cover_publications.rs"]
mod remote_cover_publications;
#[path = "../src/remotes.rs"]
mod remotes;

use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_discovery::{
    decode, DiscoveryConfig, DiscoveryPlatform, DiscoveryTransport, NetworkSnapshot, RecordData,
};
use freeshow_remotes::{RemoteConfig, ServerKind};
use serde_json::{json, Value};
use std::{
    future::{poll_fn, Future},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    task::Poll,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    sync::{mpsc, oneshot},
    time::timeout,
};

fn request(channel: &str, command: &str, data: Value, context: &RuntimeContext) -> Request {
    serde_json::from_value(json!({"channel":channel,"data":if channel=="MAIN"{json!({"channel":command,"data":data})}else{data},"context":context})).unwrap()
}
fn fixture() -> (
    tempfile::TempDir,
    LibraryService,
    remotes::NativeRemotes,
    mpsc::Receiver<Event>,
) {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("data")).unwrap(),
    )
    .unwrap();
    let (send, receive) = mpsc::channel(16);
    let config = remote_config(temp.path());
    let native = remotes::NativeRemotes::new(library.clone(), config, move |event| {
        send.try_send(event).map_err(|e| e.to_string())
    })
    .unwrap();
    (temp, library, native, receive)
}
fn remote_config(root: &std::path::Path) -> RemoteConfig {
    let mut config = RemoteConfig {
        bind_ip: Ipv4Addr::LOCALHOST.into(),
        max_clients: 4,
        data_events: 4,
        ..RemoteConfig::default()
    };
    for kind in ServerKind::ALL {
        let dir = root.join(kind.key());
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("index.html"),
            format!("explicit {} test fixture", kind.key()),
        )
        .unwrap();
        config.static_roots.insert(kind, dir);
    }
    config
}
async fn start(native: &remotes::NativeRemotes, context: &RuntimeContext) -> SocketAddr {
    native.dispatch(request("MAIN","START",json!({"ports":{"remote":"0","stage":"0","controller":"0"},"disabled":{"remote":false,"stage":false,"controller":false},"max":10,"data":{}}),context),"main").await.unwrap();
    native.status()["binding"]["bound"]["endpoints"]["REMOTE"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}
async fn http(address: SocketAddr, method: &str, path: &str, body: &str) -> String {
    timeout(Duration::from_secs(3),async{
        let mut socket=TcpStream::connect(address).await.unwrap();
        let wire=format!("{method} {path} HTTP/1.1\r\nHost:127.0.0.1\r\nConnection:close\r\nContent-Type:text/plain;charset=UTF-8\r\nContent-Length:{}\r\n\r\n{body}",body.len());
        socket.write_all(wire.as_bytes()).await.unwrap();
        let mut raw=Vec::new();socket.read_to_end(&mut raw).await.unwrap();
        let raw=String::from_utf8(raw).unwrap();
        let (header,body)=raw.split_once("\r\n\r\n").unwrap();
        assert!(header.starts_with("HTTP/1.1 200"),"{raw}");
        assert!(!header.to_lowercase().contains("transfer-encoding: chunked"),"Unexpected chunked fixture response");
        body.to_string()
    }).await.unwrap()
}
async fn connect(address: SocketAddr) -> (String, String) {
    let hello = http(address, "GET", "/socket.io/?EIO=4&transport=polling", "").await;
    let engine: Value = serde_json::from_str(hello.strip_prefix('0').unwrap()).unwrap();
    let path = format!(
        "/socket.io/?EIO=4&transport=polling&sid={}",
        engine["sid"].as_str().unwrap()
    );
    assert_eq!(http(address, "POST", &path, "40").await, "ok");
    let socket = poll_packet(address, &path, "40").await;
    (path, socket["sid"].as_str().unwrap().to_string())
}
async fn poll_packet(address: SocketAddr, path: &str, expected: &str) -> Value {
    // Engine.IO may enqueue a no-op before or alongside a Socket.IO payload.
    // Await the actual packet instead of using the HTTP response as a barrier.
    for _ in 0..8 {
        let payload = http(address, "GET", path, "").await;
        for packet in payload.split('\u{1e}') {
            if let Some(json) = packet.strip_prefix(expected) {
                return serde_json::from_str(json).unwrap();
            }
            if packet == "2" {
                http(address, "POST", path, "3").await;
            } else {
                assert_eq!(
                    packet, "6",
                    "Unexpected packet while waiting for {expected}: {packet}"
                );
            }
        }
    }
    panic!("No Socket.IO {expected} packet")
}

async fn event(events: &mut mpsc::Receiver<Event>) -> Event {
    timeout(Duration::from_secs(8), events.recv())
        .await
        .unwrap()
        .unwrap()
}
fn complete(native: &remotes::NativeRemotes, event: &Event, accepted: bool) {
    native
        .complete(
            "main",
            event.native_remote.as_ref().unwrap()["deliveryId"]
                .as_str()
                .unwrap(),
            accepted,
        )
        .unwrap();
}
async fn settle(native: &remotes::NativeRemotes, connections: usize) {
    timeout(Duration::from_secs(2), async {
        loop {
            let status = native.status();
            if status["pendingDeliveries"] == 0 && status["binding"]["connections"] == connections {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn real_clients_and_targeted_replies_continue_outside_blocked_library_io() {
    let (_temp, library, native, mut events) = fixture();
    let context = library.context().unwrap();
    let address = start(&native, &context).await;
    assert_eq!(
        http(address, "GET", "/", "").await,
        "explicit remote test fixture"
    );
    let (entered, ready) = oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let held = library.clone();
    let captured = context.clone();
    let owner = tokio::spawn(async move {
        held.commit(
            request("MAIN", "host-test", Value::Null, &captured),
            "main",
            move |_| {
                let _ = entered.send(());
                let _ = gate.recv();
                Ok(vec![])
            },
        )
        .await
    });
    ready.await.unwrap();
    let (path, id) = connect(address).await;
    let connection = event(&mut events).await;
    assert_eq!(connection.context.as_ref(), Some(&context));
    assert_eq!(
        connection.native_remote.as_ref().unwrap()["origin"],
        "connection"
    );
    assert_eq!(
        native.complete("output", "bad", true).unwrap_err().code,
        "window_error"
    );
    assert_eq!(
        native
            .complete("main", &format!("native-remote-{}", "f".repeat(32)), true)
            .unwrap_err()
            .code,
        "invalid_remote_completion"
    );
    complete(&native, &connection, true);
    settle(&native, 1).await;
    assert_eq!(
        native
            .complete(
                "main",
                connection.native_remote.as_ref().unwrap()["deliveryId"]
                    .as_str()
                    .unwrap(),
                true
            )
            .unwrap_err()
            .code,
        "invalid_remote_completion"
    );
    let raw =
        json!({"channel":"CONNECTION","id":"spoofed","origin":"connection","future":[true,"敬拜"]});
    http(
        address,
        "POST",
        &path,
        &format!("42{}", json!(["REMOTE", raw])),
    )
    .await;
    let wire = event(&mut events).await;
    assert_eq!(wire.native_remote.as_ref().unwrap()["message"], raw);
    assert_eq!(wire.native_remote.as_ref().unwrap()["sourceSocket"], id);
    assert_eq!(wire.native_remote.as_ref().unwrap()["origin"], "client");
    complete(&native, &wire, true);
    settle(&native, 1).await;
    let reply = json!({"channel":"REPLY","id":id,"data":{"value":"café 日本語"}});
    native
        .dispatch(request("REMOTE", "", reply.clone(), &context), "main")
        .await
        .unwrap();
    let received = poll_packet(address, &path, "42").await;
    assert_eq!(received, json!(["REMOTE", reply]));
    let stop = tokio::spawn({
        let native = native.clone();
        let context = context.clone();
        async move {
            native
                .dispatch(request("MAIN", "STOP", Value::Null, &context), "main")
                .await
        }
    });
    let disconnected = event(&mut events).await;
    assert_eq!(
        disconnected.native_remote.as_ref().unwrap()["message"],
        json!({"channel":"DISCONNECT","id":id})
    );
    complete(&native, &disconnected, true);
    stop.await.unwrap().unwrap();
    assert!(native.status()["binding"].is_null());
    drop(release);
    owner.await.unwrap().unwrap();
    native.shutdown().await.unwrap();
    assert!(TcpStream::connect(address).await.is_err());
}

#[tokio::test]
async fn library_change_drains_old_owned_connections_then_waits_for_retained_settings_start() {
    let (temp, library, native, mut events) = fixture();
    let first = library.context().unwrap();
    let address = start(&native, &first).await;
    let (_, id) = connect(address).await;
    let connection = event(&mut events).await;
    complete(&native, &connection, true);
    settle(&native, 1).await;
    std::fs::create_dir(temp.path().join("new")).unwrap();
    library
        .dispatch(
            request(
                "MAIN",
                "UPDATE_DATA_PATH",
                json!({"oldPath":first.data_path,"newPath":temp.path().join("new")}),
                &first,
            ),
            "main",
        )
        .await
        .unwrap();
    let next = library.context().unwrap();
    assert_ne!(next, first);
    let refresh = tokio::spawn({
        let native = native.clone();
        async move { native.context_changed().await }
    });
    let cleanup = event(&mut events).await;
    assert_eq!(cleanup.context.as_ref(), Some(&first));
    assert_eq!(
        cleanup.native_remote.as_ref().unwrap()["message"],
        json!({"channel":"DISCONNECT","id":id})
    );
    complete(&native, &cleanup, true);
    refresh.await.unwrap().unwrap();
    assert!(
        native.status()["binding"].is_null(),
        "new listeners must not race DATA_PATH delivery"
    );
    assert!(TcpStream::connect(address).await.is_err());
    assert_eq!(
        native
            .dispatch(
                request("REMOTE", "", json!({"channel":"STALE"}), &first),
                "main"
            )
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    let fresh = start(&native, &next).await;
    assert_ne!(
        native.status()["binding"]["bound"]["generation"]["run"],
        connection.native_remote.as_ref().unwrap()["run"]
    );
    assert_eq!(
        native
            .dispatch(request("MAIN", "STOP", Value::Null, &next), "output")
            .await
            .unwrap_err()
            .code,
        "window_error"
    );
    native.shutdown().await.unwrap();
    assert!(TcpStream::connect(fresh).await.is_err());
}

#[tokio::test]
async fn one_delivery_in_flight_bounds_flood_and_terminal_control_bypasses_full_public_queue() {
    let (_temp, library, native, mut events) = fixture();
    let context = library.context().unwrap();
    let address = start(&native, &context).await;
    let (path, _) = connect(address).await;
    let connection = event(&mut events).await;
    for index in 0..4 {
        http(
            address,
            "POST",
            &path,
            &format!("42{}", json!(["REMOTE",{"channel":"FLOOD","data":index}])),
        )
        .await;
    }
    assert_eq!(native.status()["pendingDeliveries"], 1);
    assert!(
        events.try_recv().is_err(),
        "WebView queue does not consume the bounded service queue"
    );
    let mut queued = Vec::new();
    for _ in 0..32 {
        let mut work =
            Box::pin(native.dispatch(request("MAIN", "STOP", Value::Null, &context), "main"));
        assert!(poll_fn(|cx| Poll::Ready(work.as_mut().poll(cx)))
            .await
            .is_pending());
        queued.push(work);
    }
    assert_eq!(
        native
            .dispatch(request("MAIN", "STOP", Value::Null, &context), "main")
            .await
            .unwrap_err()
            .code,
        "remotes_busy"
    );
    drop(queued.remove(0));
    assert_eq!(
        native.status()["admittedRequests"],
        32,
        "accepted work retains its lease after caller cancellation"
    );
    let mut changed = Box::pin(native.context_changed());
    assert!(poll_fn(|cx| Poll::Ready(changed.as_mut().poll(cx)))
        .await
        .is_pending());
    drop(changed);
    let closing = tokio::spawn({
        let native = native.clone();
        async move { native.shutdown().await }
    });
    complete(&native, &connection, true);
    let cleanup = event(&mut events).await;
    assert_eq!(
        cleanup.native_remote.as_ref().unwrap()["origin"],
        "disconnect"
    );
    complete(&native, &cleanup, true);
    timeout(Duration::from_secs(3), closing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    for work in queued {
        assert!(work.await.is_err());
    }
    assert_eq!(native.status()["admittedRequests"], 0);
    assert_eq!(native.status()["pendingDeliveries"], 0);
    assert!(native.status()["binding"].is_null());
    assert!(TcpStream::connect(address).await.is_err());
}

#[tokio::test]
async fn lost_connection_completion_still_cleans_a_potentially_published_id_and_stops() {
    let (_temp, library, native, mut events) = fixture();
    let context = library.context().unwrap();
    let address = start(&native, &context).await;
    let (_, id) = connect(address).await;
    let _lost_completion = event(&mut events).await;
    let cleanup = event(&mut events).await;
    assert_eq!(
        cleanup.native_remote.as_ref().unwrap()["message"],
        json!({"channel":"DISCONNECT","id":id})
    );
    complete(&native, &cleanup, true);
    assert_eq!(
        native.shutdown().await.unwrap_err().code,
        "remote_delivery_timeout"
    );
    assert!(native.status()["binding"].is_null());
    assert_eq!(native.status()["pendingDeliveries"], 0);
    assert!(TcpStream::connect(address).await.is_err());
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn accepted_library_change_cleanup_outlives_its_ipc_caller_and_is_drained_before_close() {
    let (temp, library, native, mut events) = fixture();
    let context = library.context().unwrap();
    let address = start(&native, &context).await;
    let (_, id) = connect(address).await;
    let connection = event(&mut events).await;
    complete(&native, &connection, true);
    settle(&native, 1).await;
    let changes = context_changes::ContextChanges::new();
    let retained_native = native.clone();
    let held_library = library.clone();
    let (committed, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    let new_path = temp.path().join("owned-new");
    std::fs::create_dir(&new_path).unwrap();
    let mut call = Box::pin(changes.run(async move {
        let events = held_library
            .dispatch(
                request(
                    "MAIN",
                    "UPDATE_DATA_PATH",
                    json!({"oldPath":context.data_path,"newPath":new_path}),
                    &context,
                ),
                "main",
            )
            .await
            .map_err(freeshow_core::WireError::from)?;
        let _ = committed.send(());
        let _ = gate.await;
        retained_native.context_changed().await?;
        Ok(events)
    }));
    assert!(poll_fn(|cx| Poll::Ready(call.as_mut().poll(cx)))
        .await
        .is_pending());
    ready.await.unwrap();
    drop(call);
    let mut drain = Box::pin(changes.close_and_drain());
    assert!(poll_fn(|cx| Poll::Ready(drain.as_mut().poll(cx)))
        .await
        .is_pending());
    release.send(()).unwrap();
    let cleanup = event(&mut events).await;
    assert_eq!(
        cleanup.native_remote.as_ref().unwrap()["message"],
        json!({"channel":"DISCONNECT","id":id})
    );
    complete(&native, &cleanup, true);
    timeout(Duration::from_secs(3), drain)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        changes.run(async { Ok(vec![]) }).await.unwrap_err().code,
        "context_change_shutdown"
    );
    assert!(native.status()["binding"].is_null());
    native.shutdown().await.unwrap();
}

#[tokio::test]
async fn root_change_capacity_covers_detached_continuations_until_cleanup_finishes() {
    let changes = context_changes::ContextChanges::new();
    let completed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut callers = Vec::new();
    let mut releases = Vec::new();
    for _ in 0..4 {
        let (release, gate) = oneshot::channel();
        releases.push(release);
        let completed = completed.clone();
        let mut caller = Box::pin(changes.run(async move {
            let _ = gate.await;
            completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![])
        }));
        assert!(poll_fn(|cx| Poll::Ready(caller.as_mut().poll(cx)))
            .await
            .is_pending());
        callers.push(caller);
    }
    assert_eq!(
        changes.run(async { Ok(vec![]) }).await.unwrap_err().code,
        "context_change_busy"
    );
    drop(callers);
    let mut drain = Box::pin(changes.close_and_drain());
    assert!(poll_fn(|cx| Poll::Ready(drain.as_mut().poll(cx)))
        .await
        .is_pending());
    for release in releases {
        release.send(()).unwrap();
    }
    timeout(Duration::from_secs(2), drain)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.load(std::sync::atomic::Ordering::SeqCst), 4);
    assert_eq!(
        changes.run(async { Ok(vec![]) }).await.unwrap_err().code,
        "context_change_shutdown"
    );
}

fn frozen_snapshot() -> NetworkSnapshot {
    serde_json::from_value(json!({"hostname":"private-remote","interfaces":[{"name":"lo","addresses":[{"address":"127.0.0.1","family":"IPv4","internal":true,"mac":"00:00:00:00:00:00"}]}]})).unwrap()
}
struct DiscoveryFixture {
    temp: tempfile::TempDir,
    library: LibraryService,
    native: remotes::NativeRemotes,
    renderer: mpsc::Receiver<Event>,
    discovery: discovery::NativeDiscovery,
    peer: UdpSocket,
}
impl DiscoveryFixture {
    async fn new(adjust: impl FnOnce(&mut DiscoveryConfig)) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
        )
        .unwrap();
        let peer = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let mut config = DiscoveryConfig::new([0xab, 0xcd, 0xef]);
        config.platform = DiscoveryPlatform::Other;
        config.probe_jitter = Duration::ZERO;
        config.transport = DiscoveryTransport::Loopback {
            bind: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
            target: match peer.local_addr().unwrap() {
                SocketAddr::V4(v) => v,
                _ => unreachable!(),
            },
        };
        adjust(&mut config);
        let (discovery, events) = discovery::NativeDiscovery::new(
            library.clone(),
            config,
            discovery::NativeDiscovery::test_inputs(frozen_snapshot(), [0xab, 0xcd, 0xef]),
        )
        .unwrap();
        let (send, renderer) = mpsc::channel(16);
        let native = remotes::NativeRemotes::new_with_discovery(
            library.clone(),
            remote_config(temp.path()),
            discovery.clone(),
            events,
            move |event| send.try_send(event).map_err(|e| e.to_string()),
        )
        .unwrap();
        Self {
            temp,
            library,
            native,
            renderer,
            discovery,
            peer,
        }
    }
    fn context(&self) -> RuntimeContext {
        self.library.context().unwrap()
    }
    fn endpoints(&self) -> std::collections::BTreeMap<String, SocketAddr> {
        serde_json::from_value(self.native.status()["binding"]["bound"]["endpoints"].clone())
            .unwrap()
    }
    async fn stop(&self) {
        assert!(self
            .native
            .dispatch(
                request("MAIN", "STOP", Value::Null, &self.context()),
                "main"
            )
            .await
            .unwrap()
            .is_empty());
    }
}
async fn condition(mut test: impl FnMut() -> bool) {
    timeout(Duration::from_secs(4), async {
        while !test() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
fn normalize(record: &freeshow_discovery::Record) -> Value {
    let (kind, data) = match &record.data {
        RecordData::Name(name) => ("PTR", json!(name)),
        RecordData::Srv {
            port,
            target,
            priority,
            weight,
        } => (
            "SRV",
            json!({"port":port,"target":target,"priority":priority,"weight":weight}),
        ),
        RecordData::Txt(bytes) => ("TXT", json!(bytes)),
        RecordData::A(ip) => ("A", json!(ip)),
        RecordData::Aaaa(ip) => ("AAAA", json!(ip)),
        _ => panic!("unexpected source record"),
    };
    json!({"name":record.name,"type":kind,"ttl":record.ttl,"data":data})
}
async fn positive_records(peer: &UdpSocket, services: usize) -> Vec<Value> {
    timeout(Duration::from_secs(5), async {
        let mut records = std::collections::BTreeMap::new();
        let mut ports = std::collections::BTreeSet::new();
        let mut bytes = [0; 9001];
        loop {
            let (n, _) = peer.recv_from(&mut bytes).await.unwrap();
            for record in decode(&bytes[..n])
                .unwrap()
                .answers
                .into_iter()
                .filter(|r| r.ttl > 0)
            {
                if let RecordData::Srv { port, .. } = &record.data {
                    ports.insert(*port);
                }
                let value = normalize(&record);
                records.insert(value.to_string(), value);
            }
            if ports.len() == services {
                return records.into_values().collect();
            }
        }
    })
    .await
    .unwrap()
}
async fn no_late_positive(peer: &UdpSocket) {
    let until = tokio::time::Instant::now() + Duration::from_millis(2100);
    let mut bytes = [0; 9001];
    while let Ok(Ok((n, _))) = timeout(
        until.saturating_duration_since(tokio::time::Instant::now()),
        peer.recv_from(&mut bytes),
    )
    .await
    {
        assert!(
            decode(&bytes[..n])
                .unwrap()
                .answers
                .iter()
                .all(|r| r.ttl == 0),
            "positive publication survived stop"
        );
    }
}
fn source_records(endpoints: &std::collections::BTreeMap<String, SocketAddr>) -> Vec<Value> {
    use std::{
        io::Read,
        process::{Command, Stdio},
        time::Instant,
    };
    let source = std::path::PathBuf::from(file!())
        .parent()
        .unwrap()
        .join("../../tests/parity/discovery-oracle.mjs")
        .canonicalize()
        .unwrap();
    let script = r#"import {pathToFileURL} from 'node:url'; const {originalDiscovery,dns}=await import(pathToFileURL(process.argv[1]));const snapshot=JSON.parse(process.argv[2]),ports=JSON.parse(process.argv[3]);const original=originalDiscovery(snapshot,{noIO:true});try{const map=new Map();for(const [name,port]of Object.entries(ports)){for(const r of dns.decode(dns.encode({type:'response',answers:original.records(name,port)})).answers){const v={name:r.name,type:r.type,ttl:r.ttl,data:r.type==='TXT'?r.data.map(v=>Array.from(v)):r.data};map.set(JSON.stringify(v),v);}} console.log(JSON.stringify([...map.values()]));}finally{await original.close();}"#;
    let stdout = tempfile::NamedTempFile::new().unwrap();
    let stderr = tempfile::NamedTempFile::new().unwrap();
    let mut child =
        Command::new(std::env::var_os("FREESHOW_TEST_NODE").unwrap_or_else(|| "node".into()))
            .args(["--input-type=module", "-e", script])
            .arg(source)
            .arg(serde_json::to_string(&frozen_snapshot()).unwrap())
            .arg(
                serde_json::to_string(
                    &endpoints
                        .iter()
                        .map(|(name, address)| (name, address.port()))
                        .collect::<std::collections::BTreeMap<_, _>>(),
                )
                .unwrap(),
            )
            .stdin(Stdio::null())
            .stdout(stdout.reopen().unwrap())
            .stderr(stderr.reopen().unwrap())
            .spawn()
            .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline
            || stdout.as_file().metadata().unwrap().len() > 1024 * 1024
            || stderr.as_file().metadata().unwrap().len() > 1024 * 1024
        {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bounded discovery source oracle failed");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(stdout.as_file().metadata().unwrap().len() <= 1024 * 1024);
    assert!(stderr.as_file().metadata().unwrap().len() <= 1024 * 1024);
    let mut out = String::new();
    let mut err = String::new();
    stdout.as_file().read_to_string(&mut out).unwrap();
    stderr.as_file().read_to_string(&mut err).unwrap();
    assert!(status.success(), "{err}");
    let records: Vec<Value> = serde_json::from_str(&out).unwrap();
    records
        .into_iter()
        .map(|v| (v.to_string(), v))
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_values()
        .collect()
}
#[tokio::test]
async fn bound_three_ports_publish_actual_source_records_and_restart_with_frozen_txt() {
    let mut fixture = DiscoveryFixture::new(|_| {}).await;
    let first_context = fixture.context();
    start(&fixture.native, &first_context).await;
    let endpoints = fixture.endpoints();
    assert_eq!(endpoints.len(), 3);
    for endpoint in endpoints.values() {
        assert!(TcpStream::connect(endpoint).await.is_ok());
        assert_ne!(endpoint.port(), 0);
    }
    let records = positive_records(&fixture.peer, 3).await;
    assert_eq!(records, source_records(&endpoints));
    condition(|| {
        fixture.native.status()["discovery"]["acceptedEvents"]
            .as_u64()
            .unwrap()
            > 0
    })
    .await;
    let generation = fixture.native.status()["binding"]["bound"]["generation"].clone();
    assert_eq!(
        fixture.native.status()["discovery"]["lastEvent"]["listenerRun"],
        generation["run"]
    );
    // SERVER_DATA remains data only; embedded ports cannot redirect native listeners
    // or replace source's first port retained until actual STOP/START.
    fixture
        .native
        .dispatch(
            request(
                "MAIN",
                "SERVER_DATA",
                json!({"ports":{"remote":1},"remote":{"port":2}}),
                &first_context,
            ),
            "main",
        )
        .await
        .unwrap();
    assert_eq!(fixture.endpoints(), endpoints);
    for (kind, address) in &endpoints {
        assert_eq!(
            fixture.discovery.status()["binding"]["actualPorts"][kind],
            address.to_string()
        );
    }
    fixture.stop().await;
    assert!(fixture.discovery.status()["binding"].is_null());
    for endpoint in endpoints.values() {
        assert!(TcpStream::connect(endpoint).await.is_err());
    }
    assert!(
        fixture.renderer.try_recv().is_err(),
        "discovery never invents a renderer ACK"
    );
    // Consume all old zero-TTL records before the fresh run.
    while timeout(
        Duration::from_millis(15),
        fixture.peer.recv_from(&mut [0; 9001]),
    )
    .await
    .is_ok()
    {}
    start(&fixture.native, &first_context).await;
    let next = fixture.endpoints();
    assert_ne!(
        fixture.native.status()["binding"]["bound"]["generation"],
        generation
    );
    assert_eq!(
        positive_records(&fixture.peer, 3).await,
        source_records(&next)
    );
    fixture.native.shutdown().await.unwrap();
    assert_eq!(fixture.native.status()["discovery"]["joined"], true);
    assert_eq!(fixture.discovery.status()["service"]["socket_open"], false);
}
#[tokio::test]
async fn stop_and_context_retirement_before_browse_deadline_cancel_all_delayed_positives() {
    let mut fixture = DiscoveryFixture::new(|_| {}).await;
    let old = fixture.context();
    start(&fixture.native, &old).await;
    fixture.stop().await;
    no_late_positive(&fixture.peer).await;
    start(&fixture.native, &old).await;
    let old_endpoints = fixture.endpoints();
    let (_, id) = connect(old_endpoints["REMOTE"]).await;
    let connected = event(&mut fixture.renderer).await;
    complete(&fixture.native, &connected, true);
    settle(&fixture.native, 1).await;
    let new_path = fixture.temp.path().join("next");
    std::fs::create_dir(&new_path).unwrap();
    fixture
        .library
        .dispatch(
            request(
                "MAIN",
                "UPDATE_DATA_PATH",
                json!({"oldPath":old.data_path,"newPath":new_path}),
                &old,
            ),
            "main",
        )
        .await
        .unwrap();
    let retiring = fixture.native.clone();
    let refresh = tokio::spawn(async move { retiring.context_changed().await });
    let cleanup = event(&mut fixture.renderer).await;
    assert_eq!(cleanup.context.as_ref(), Some(&old));
    assert_eq!(
        cleanup.native_remote.as_ref().unwrap()["origin"],
        "disconnect"
    );
    assert_eq!(
        cleanup.native_remote.as_ref().unwrap()["message"],
        json!({"channel":"DISCONNECT","id":id})
    );
    complete(&fixture.native, &cleanup, true);
    refresh.await.unwrap().unwrap();
    assert!(fixture.native.status()["binding"].is_null());
    no_late_positive(&fixture.peer).await;
    for endpoint in old_endpoints.values() {
        assert!(TcpStream::connect(endpoint).await.is_err());
    }
    assert_eq!(
        fixture
            .native
            .dispatch(request("MAIN", "START", Value::Null, &old), "main")
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    fixture.native.shutdown().await.unwrap();
}
#[tokio::test]
async fn disabled_servers_never_advertise_and_transport_bind_failure_does_not_disable_tcp() {
    let occupied = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::{
            setsockopt, SOL_SOCKET, SO_EXCLUSIVEADDRUSE,
        };
        let enabled = 1i32;
        // SAFETY: this live owned socket receives a correctly sized BOOL value.
        assert_eq!(
            unsafe {
                setsockopt(
                    occupied.as_raw_socket() as _,
                    SOL_SOCKET,
                    SO_EXCLUSIVEADDRUSE,
                    (&enabled as *const i32).cast(),
                    4,
                )
            },
            0
        );
    }
    occupied
        .bind(&SocketAddr::from((Ipv4Addr::LOCALHOST, 0)).into())
        .unwrap();
    let address = occupied.local_addr().unwrap().as_socket_ipv4().unwrap();
    let fixture = DiscoveryFixture::new(|config| {
        if let DiscoveryTransport::Loopback { bind, .. } = &mut config.transport {
            *bind = address;
        }
    })
    .await;
    let context = fixture.context();
    let options = json!({"ports":{"remote":"0","stage":"0","controller":"0"},"disabled":{"remote":false,"stage":true,"controller":true},"data":{}});
    assert!(fixture
        .native
        .dispatch(request("MAIN", "START", options, &context), "main")
        .await
        .unwrap()
        .is_empty());
    assert_eq!(fixture.endpoints().len(), 1);
    let endpoint = fixture.endpoints()["REMOTE"];
    assert_eq!(
        http(endpoint, "GET", "/", "").await,
        "explicit remote test fixture"
    );
    assert!(
        fixture.native.status()["discovery"]["warnings"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(fixture.discovery.status()["binding"].is_null());
    fixture.stop().await;
    fixture.native.shutdown().await.unwrap();
    assert!(TcpStream::connect(endpoint).await.is_err());
    drop(occupied);
    let rebound = UdpSocket::bind(address).await.unwrap();
    drop(rebound);
}
#[tokio::test]
async fn tokenless_begin_timeout_and_caller_drop_are_owned_until_stop_and_terminal() {
    let fixture = DiscoveryFixture::new(|_| {}).await;
    fixture
        .native
        .test_discovery_timeout(Duration::from_millis(50));
    let (entered, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    let paused = fixture.discovery.clone();
    let hold = tokio::spawn(async move { paused.pause(entered, gate).await });
    ready.await.unwrap();
    let caller = fixture.native.clone();
    let context = fixture.context();
    let task = tokio::spawn(async move { start(&caller, &context).await });
    condition(|| fixture.discovery.status()["admittedRequests"] == 2).await;
    task.abort();
    let _ = task.await;
    // After the operation deadline the native begin reply is still retained.
    condition(|| {
        fixture.native.status()["discovery"]["warnings"]
            .as_u64()
            .unwrap()
            > 0
    })
    .await;
    assert!(fixture.native.status()["binding"].is_object());
    assert!(fixture.native.status()["binding"]["discoveryRun"].is_null());
    release.send(()).unwrap();
    hold.await.unwrap().unwrap();
    fixture.stop().await;
    assert!(fixture.discovery.status()["binding"].is_null());
    no_late_positive(&fixture.peer).await;
    // Cancel a second accepted begin before it has returned any token.
    let (entered, ready) = oneshot::channel();
    let (_release, gate) = oneshot::channel();
    let paused = fixture.discovery.clone();
    let hold = tokio::spawn(async move { paused.pause(entered, gate).await });
    ready.await.unwrap();
    let caller = fixture.native.clone();
    let context = fixture.context();
    let task = tokio::spawn(async move { start(&caller, &context).await });
    condition(|| fixture.discovery.status()["admittedRequests"] == 2).await;
    fixture
        .native
        .test_discovery_timeout(Duration::from_secs(3));
    fixture.native.shutdown().await.unwrap();
    assert!(task.await.is_err());
    assert!(hold.await.unwrap().is_err());
    assert_eq!(fixture.native.status()["discovery"]["joined"], true);
    assert_eq!(fixture.discovery.status()["service"]["socket_open"], false);
}
#[cfg(unix)]
#[tokio::test]
async fn shutdown_timeout_retains_actual_child_and_later_retry_joins_after_remote_cleanup() {
    use std::os::unix::fs::PermissionsExt;
    let executable = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(executable.path(), "#!/bin/sh\nexec /bin/sleep 60\n").unwrap();
    std::fs::set_permissions(executable.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let executable = executable.into_temp_path();
    let fixture = DiscoveryFixture::new(|config| {
        config.platform = DiscoveryPlatform::Darwin {
            dns_sd: Some(executable.to_path_buf()),
        }
    })
    .await;
    start(&fixture.native, &fixture.context()).await;
    let endpoints = fixture.endpoints();
    condition(|| fixture.discovery.status()["service"]["children"] == 3).await;
    let (pause_entered, paused) = oneshot::channel();
    let (_pause_release, pause_gate) = oneshot::channel();
    let held = fixture.discovery.clone();
    let hold = tokio::spawn(async move { held.pause(pause_entered, pause_gate).await });
    paused.await.unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    fixture
        .native
        .test_discovery_drain(Duration::from_millis(30), entered, gate);
    let caller = fixture.native.clone();
    let task = tokio::spawn(async move { caller.shutdown().await });
    ready.await.unwrap();
    // Abort one shutdown waiter; the actual child/drain remain owned.
    task.abort();
    let _ = task.await;
    condition(|| fixture.native.status()["binding"].is_null()).await;
    assert_eq!(fixture.native.status()["discovery"]["joined"], false);
    assert_eq!(fixture.discovery.status()["service"]["children"], 3);
    // STOP is held behind the actual native owner, so these real direct children
    // remain owned after remote TCP/renderer cleanup and a timed-out drain.
    assert_eq!(
        fixture.native.shutdown().await.unwrap_err().code,
        "remote_discovery_shutdown_timeout"
    );
    for endpoint in endpoints.values() {
        assert!(TcpStream::connect(endpoint).await.is_err());
    }
    fixture
        .native
        .test_discovery_timeout(Duration::from_secs(3));
    release.send(()).unwrap();
    fixture.native.shutdown().await.unwrap();
    assert!(hold.await.unwrap().is_err());
    fixture.native.shutdown().await.unwrap();
    assert_eq!(fixture.native.status()["discovery"]["joined"], true);
    assert_eq!(fixture.discovery.status()["service"]["children"], 0);
    assert_eq!(fixture.discovery.status()["service"]["socket_open"], false);
}

#[tokio::test]
async fn enabled_subset_and_all_disabled_use_only_actual_listener_evidence_and_old_tokens_fail() {
    let fixture = DiscoveryFixture::new(|_| {}).await;
    let context = fixture.context();
    // Retain a genuine old native token, never a deserialized renderer token.
    let _old_listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let old = fixture
        .discovery
        .begin(&context, "0123456789abcdef0123456789abcdef")
        .await
        .unwrap();
    let options = json!({"ports":{"remote":"0","stage":"0","controller":"0"},"disabled":{"remote":true,"stage":false,"controller":true},"data":{}});
    fixture
        .native
        .dispatch(request("MAIN", "START", options, &context), "main")
        .await
        .unwrap();
    assert_eq!(
        fixture.endpoints().keys().collect::<Vec<_>>(),
        vec!["STAGE"]
    );
    assert_eq!(
        positive_records(&fixture.peer, 1).await,
        source_records(&fixture.endpoints())
    );
    assert_eq!(
        fixture.discovery.stop(&old).await.unwrap_err().code,
        "stale_generation"
    );
    let actual = fixture.endpoints()["STAGE"];
    assert_eq!(
        fixture
            .discovery
            .started_port(&old, freeshow_discovery::DiscoveryName::Stage, actual)
            .await
            .unwrap_err()
            .code,
        "stale_generation"
    );
    assert!(TcpStream::connect(actual).await.is_ok());
    fixture.stop().await;
    let options = json!({"ports":{"remote":"0","stage":"0","controller":"0"},"disabled":{"remote":true,"stage":true,"controller":true},"data":{}});
    fixture
        .native
        .dispatch(request("MAIN", "START", options, &context), "main")
        .await
        .unwrap();
    assert!(fixture.endpoints().is_empty());
    assert!(fixture.discovery.status()["binding"].is_null());
    assert_eq!(fixture.discovery.status()["service"]["socket_open"], false);
    // Drop only this consumer's discovery events; no renderer callback exists.
    fixture.native.shutdown().await.unwrap();
}
#[tokio::test]
async fn actual_discovery_owner_panic_is_sticky_but_does_not_skip_remote_teardown() {
    let fixture = DiscoveryFixture::new(|_| {}).await;
    let context = fixture.context();
    start(&fixture.native, &context).await;
    let endpoints = fixture.endpoints();
    assert!(fixture.discovery.panic_owner().await.is_err());
    assert_eq!(
        http(endpoints["REMOTE"], "GET", "/", "").await,
        "explicit remote test fixture"
    );
    let first = fixture.native.shutdown().await.unwrap_err();
    assert_eq!(first.code, "discovery_owner");
    assert_eq!(
        fixture.native.shutdown().await.unwrap_err().message,
        first.message
    );
    for endpoint in endpoints.values() {
        assert!(TcpStream::connect(endpoint).await.is_err());
    }
    assert_eq!(fixture.discovery.status()["service"]["socket_open"], false);
}

#[tokio::test]
async fn context_control_retires_discovery_work_after_timed_out_stop_removed_listener_binding() {
    let fixture = DiscoveryFixture::new(|_| {}).await;
    fixture
        .native
        .test_discovery_timeout(Duration::from_millis(50));
    let (entered, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    let paused = fixture.discovery.clone();
    let hold = tokio::spawn(async move { paused.pause(entered, gate).await });
    ready.await.unwrap();
    start(&fixture.native, &fixture.context()).await;
    fixture.stop().await;
    assert!(fixture.native.status()["binding"].is_null());
    assert_eq!(fixture.discovery.status()["admittedRequests"], 2);
    let old = fixture.context();
    let new_path = fixture.temp.path().join("after-timed-stop");
    std::fs::create_dir(&new_path).unwrap();
    fixture
        .library
        .dispatch(
            request(
                "MAIN",
                "UPDATE_DATA_PATH",
                json!({"oldPath":old.data_path,"newPath":new_path}),
                &old,
            ),
            "main",
        )
        .await
        .unwrap();
    fixture.native.context_changed().await.unwrap();
    assert_eq!(
        fixture.discovery.status()["admittedControls"],
        1,
        "no remote binding must not skip owned discovery context cleanup"
    );
    assert_eq!(
        fixture.native.status()["discovery"]["lastWarning"]["stage"],
        "context"
    );
    release.send(()).unwrap();
    hold.await.unwrap().unwrap();
    condition(|| {
        fixture.discovery.status()["admittedControls"] == 0
            && fixture.discovery.status()["admittedRequests"] == 0
    })
    .await;
    assert!(fixture.discovery.status()["binding"].is_null());
    assert_eq!(fixture.discovery.status()["service"]["socket_open"], false);
    fixture
        .native
        .test_discovery_timeout(Duration::from_secs(3));
    // Consume the held old begin result and prove the current native owner recovers.
    start(&fixture.native, &fixture.context()).await;
    fixture.native.shutdown().await.unwrap();
    no_late_positive(&fixture.peer).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn configured_native_start_serves_real_selected_covers_and_stop_drains_independent_alert_delivery(
) {
    use freeshow_remotes::{CoverErrorRendering, CoverSource};
    let (_temp, library, native, _ordinary_events) = fixture();
    let context = library.context().unwrap();
    let audio = context.data_path.join("Audio");
    std::fs::create_dir_all(&audio).unwrap();
    std::fs::write(audio.join("NowPlayingCover.png"), b"WIRE-COVER").unwrap();
    let files = now_playing_cover_file::CoverFiles::default();
    let publications = remote_cover_publications::Publications::new();
    let (send, mut alerts) = mpsc::channel(4);
    let source = std::sync::Arc::new(remote_cover::Source::new(
        context.clone(),
        library.clone(),
        files.clone(),
        publications.clone(),
        std::sync::Arc::new(move |event| send.try_send(event).map_err(|e| e.to_string())),
        CoverErrorRendering::Production,
    ));
    assert!(matches!(
        source.error_rendering(),
        CoverErrorRendering::Production
    ));
    let weak_source = std::sync::Arc::downgrade(&source);
    let ready_owner = publications.clone();
    let ready_context = context.clone();
    let ready_files = files.clone();
    let cover = remotes::CoverStart::new(source, move || {
        assert_eq!(
            ready_files.retained(),
            0,
            "ready follows actual old IO disposal"
        );
        ready_owner
            .ready(ready_context)
            .map_err(|e| freeshow_core::WireError::new(e.code, e.message))
    });
    let options = json!({"ports":{"remote":"0","stage":"0","controller":"0"},"disabled":{"remote":false,"stage":false,"controller":false},"max":10,"data":{}});
    native
        .dispatch_with_cover(
            request("MAIN", "START", options, &context),
            "main",
            Some(cover),
        )
        .await
        .unwrap();
    assert!(weak_source.upgrade().is_some());
    for kind in ServerKind::ALL {
        let address: SocketAddr = native.status()["binding"]["bound"]["endpoints"][kind.channel()]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            http(address, "GET", "/NoWpLaYiNgCoVeR/?file=/other.png", "").await,
            "WIRE-COVER"
        );
    }
    assert!(alerts.try_recv().is_err());
    timeout(Duration::from_secs(3), async {
        while files.retained() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    std::fs::remove_file(audio.join("NowPlayingCover.png")).unwrap();
    std::fs::remove_dir(&audio).unwrap();
    let blocker = b"unchanged real native Audio blocker";
    std::fs::write(&audio, blocker).unwrap();
    let address: SocketAddr = native.status()["binding"]["bound"]["endpoints"]["REMOTE"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let wire = timeout(Duration::from_secs(3), async {
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket
            .write_all(
                b"GET /nowplayingcover HTTP/1.1\r\nHost:127.0.0.1\r\nConnection:close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut raw = Vec::new();
        socket.read_to_end(&mut raw).await.unwrap();
        raw
    })
    .await
    .unwrap();
    let response = String::from_utf8(wire).unwrap();
    assert!(response.starts_with("HTTP/1.1 404"), "{response}");
    assert!(response.ends_with("\r\n\r\n"));
    let alert = alerts.try_recv().unwrap();
    assert_eq!(
        alert.message.data,
        Some(json!(format!(
            "Error: Could not create folder at: {}!",
            audio.display()
        )))
    );
    assert_eq!(
        (files.retained(), publications.retained()),
        (1, 1),
        "response completion is not private notification disposal"
    );
    let stop_request = request("MAIN", "STOP", Value::Null, &context);
    let mut stop = Box::pin(native.dispatch(stop_request, "main"));
    poll_fn(|cx| {
        assert!(stop.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(
        weak_source.upgrade().is_some(),
        "actual run still owns its emitter"
    );
    publications
        .complete(
            &context,
            alert.native_cover.as_ref().unwrap()["id"].as_str().unwrap(),
            true,
        )
        .unwrap();
    stop.await.unwrap();
    assert_eq!((files.retained(), publications.retained()), (0, 0));
    assert!(
        weak_source.upgrade().is_none(),
        "joined stop releases actual per-run source/emitter"
    );
    assert_eq!(std::fs::read(&audio).unwrap(), blocker);
    assert!(alerts.try_recv().is_err());
    native.shutdown().await.unwrap();
}
