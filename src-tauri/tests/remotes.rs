#[path = "../src/context_changes.rs"]
mod context_changes;
#[path = "../src/remotes.rs"]
mod remotes;

use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_remotes::{RemoteConfig, ServerKind};
use serde_json::{json, Value};
use std::{
    future::{poll_fn, Future},
    net::{Ipv4Addr, SocketAddr},
    task::Poll,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
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
    let mut config = RemoteConfig {
        bind_ip: Ipv4Addr::LOCALHOST.into(),
        max_clients: 4,
        data_events: 4,
        ..RemoteConfig::default()
    };
    for kind in ServerKind::ALL {
        let dir = temp.path().join(kind.key());
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("index.html"),
            format!("explicit {} test fixture", kind.key()),
        )
        .unwrap();
        config.static_roots.insert(kind, dir);
    }
    let native = remotes::NativeRemotes::new(library.clone(), config, move |event| {
        send.try_send(event).map_err(|e| e.to_string())
    })
    .unwrap();
    (temp, library, native, receive)
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
    for _ in 0..8 {
        let connected = http(address, "GET", &path, "").await;
        for packet in connected.split('\u{1e}') {
            if let Some(json) = packet.strip_prefix("40") {
                let socket: Value = serde_json::from_str(json).unwrap();
                return (path, socket["sid"].as_str().unwrap().to_string());
            }
            if packet == "2" {
                http(address, "POST", &path, "3").await;
            }
        }
    }
    panic!("No Socket.IO namespace confirmation")
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
    let received = http(address, "GET", &path, "").await;
    assert_eq!(
        serde_json::from_str::<Value>(received.strip_prefix("42").unwrap()).unwrap(),
        json!(["REMOTE", reply])
    );
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
