#[path = "../src/api.rs"]
mod api;

use freeshow_api::ApiConfig;
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use serde_json::{json, Value};
use std::{
    future::{poll_fn, Future},
    net::{Ipv4Addr, TcpListener, UdpSocket},
    sync::atomic::{AtomicU32, Ordering},
    task::Poll,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
    time::timeout,
};

fn request(command: &str, data: Value, context: &RuntimeContext) -> Request {
    serde_json::from_value(
        json!({"channel":"MAIN","data":{"channel":command,"data":data},"context":context}),
    )
    .unwrap()
}
fn fixture() -> (
    tempfile::TempDir,
    LibraryService,
    api::NativeApi,
    mpsc::Receiver<Event>,
) {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("data")).unwrap(),
    )
    .unwrap();
    let (events, receive) = mpsc::channel(32);
    let native = api::NativeApi::new(
        library.clone(),
        ApiConfig {
            bind_ip: Ipv4Addr::LOCALHOST.into(),
            ..ApiConfig::default()
        },
        move |event| events.try_send(event).map_err(|error| error.to_string()),
    )
    .unwrap();
    (temp, library, native, receive)
}
fn free_port() -> u16 {
    // Parallel tests must not hand out the same pair after dropping their
    // probes but before their asynchronous native start owns the listeners.
    static NEXT_PAIR: AtomicU32 = AtomicU32::new(0);
    for _ in 0..128 {
        let index = NEXT_PAIR.fetch_add(1, Ordering::Relaxed);
        let port = 32768 + (((std::process::id() + index) % 8191) * 2) as u16;
        let Ok(_first) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) else {
            continue;
        };
        if let (Ok(_next), Ok(_udp)) = (
            TcpListener::bind((Ipv4Addr::LOCALHOST, port + 1)),
            UdpSocket::bind((Ipv4Addr::LOCALHOST, port)),
        ) {
            return port;
        }
    }
    panic!("No free loopback API ports")
}
async fn http(port: u16, body: Value) -> String {
    raw_http(port, body).await.unwrap()
}
async fn raw_http(port: u16, body: Value) -> std::io::Result<String> {
    timeout(Duration::from_secs(5),async move {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST,port)).await?;
        let body = body.to_string();
        let request = format!("POST / HTTP/1.1\r\nHost:127.0.0.1\r\nConnection:close\r\nContent-Type:application/json\r\nContent-Length:{}\r\n\r\n{}",body.len(),body);
        stream.write_all(request.as_bytes()).await?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await?;
        Ok(String::from_utf8(bytes).unwrap())
    }).await.unwrap()
}
async fn event(receive: &mut mpsc::Receiver<Event>) -> Event {
    timeout(Duration::from_secs(3), receive.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn start(native: &api::NativeApi, context: &RuntimeContext, port: u16) {
    assert!(native
        .dispatch(
            request(
                "WEBSOCKET_START",
                json!({"port":port,"password":"native-test"}),
                context
            ),
            "main"
        )
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn actual_rest_actions_route_frozen_context_and_private_ids_outside_blocked_library_io() {
    let (_temp, library, native, mut events) = fixture();
    let context = library.context().unwrap();
    let port = free_port();
    start(&native, &context, port).await;
    let (entered, ready) = oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let blocked = library.clone();
    let captured = context.clone();
    let owner = tokio::spawn(async move {
        blocked
            .commit(
                request("host-test", Value::Null, &captured),
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
    let client = tokio::spawn(http(
        port,
        json!({"action":"get_test","returnId":"external-id","token":"native-test"}),
    ));
    let wire = event(&mut events).await;
    assert_eq!(wire.context.as_ref(), Some(&context));
    assert_eq!(wire.channel, "MAIN");
    assert_eq!(wire.message.channel, "API_TRIGGER2");
    assert!(wire.listener_id.is_none());
    let mut data = wire.message.data.unwrap();
    let id = data["returnId"].as_str().unwrap().to_owned();
    assert!(id.starts_with("api:") && id != "external-id");
    data["data"] = json!({"text":"敬拜 café"});
    native
        .dispatch(request("API_TRIGGER", data, &context), "main")
        .await
        .unwrap();
    let response = client.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let result: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(result["data"], json!({"text":"敬拜 café"}));
    assert!(result.get("returnId").is_none());
    drop(release);
    owner.await.unwrap().unwrap();
    native.shutdown().await.unwrap();
    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .await
        .is_err());
    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port + 1))
        .await
        .is_err());
    assert!(UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).is_ok());
}

#[tokio::test]
async fn library_switch_restarts_live_listeners_and_old_ids_never_resolve_reused_external_ids() {
    let (temp, library, native, mut events) = fixture();
    let first = library.context().unwrap();
    let port = free_port();
    start(&native, &first, port).await;
    let before = native.status();
    native.context_changed().await.unwrap();
    assert_eq!(
        native.status()["binding"],
        before["binding"],
        "Unchanged selection must preserve connections"
    );
    let old_client = tokio::spawn(raw_http(
        port,
        json!({"action":"get_test","returnId":"reused","token":"native-test"}),
    ));
    let old = event(&mut events).await.message.data.unwrap();
    let destination = temp.path().join("next");
    std::fs::create_dir(&destination).unwrap();
    library
        .dispatch(
            request(
                "UPDATE_DATA_PATH",
                json!({"oldPath":first.data_path,"newPath":destination}),
                &first,
            ),
            "main",
        )
        .await
        .unwrap();
    let next = library.context().unwrap();
    native.context_changed().await.unwrap();
    let after = native.status();
    assert_eq!(after["binding"]["context"], json!(next));
    assert_ne!(
        after["binding"]["endpoints"]["generation"]["run"],
        before["binding"]["endpoints"]["generation"]["run"]
    );
    // Stop may close an in-flight HTTP connection. It cannot supply new-library data.
    match old_client.await.unwrap() {
        Ok(response) => assert!(!response.contains("new-library-value")),
        Err(error) => assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::UnexpectedEof
            ),
            "{error}"
        ),
    }
    let new_client = tokio::spawn(http(
        port,
        json!({"action":"get_test","returnId":"reused","token":"native-test"}),
    ));
    let current = event(&mut events).await;
    assert_eq!(current.context.as_ref(), Some(&next));
    assert_eq!(
        native
            .dispatch(request("API_TRIGGER", old, &next), "main")
            .await
            .unwrap_err()
            .code,
        "stale_generation"
    );
    let mut data = current.message.data.unwrap();
    data["data"] = json!("new-library-value");
    native
        .dispatch(request("API_TRIGGER", data, &next), "main")
        .await
        .unwrap();
    assert!(new_client.await.unwrap().contains("new-library-value"));
    assert_eq!(
        native
            .dispatch(request("WEBSOCKET_STOP", Value::Null, &first), "main")
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    native.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropped_start_reply_does_not_abandon_an_accepted_listener_and_shutdown_is_terminal() {
    let (_temp, library, native, _events) = fixture();
    let context = library.context().unwrap();
    let port = free_port();
    let mut starting = Box::pin(native.dispatch(
        request("WEBSOCKET_START", json!({"port":port}), &context),
        "main",
    ));
    assert!(poll_fn(|cx| Poll::Ready(starting.as_mut().poll(cx)))
        .await
        .is_pending());
    drop(starting);
    timeout(Duration::from_secs(3), async {
        while native.status()["binding"].is_null() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(http(port, json!({"action":"noop"}))
        .await
        .starts_with("HTTP/1.1 204"));
    native.shutdown().await.unwrap();
    native.shutdown().await.unwrap();
    assert_eq!(
        native
            .dispatch(
                request("WEBSOCKET_START", json!({"port":port}), &context),
                "main"
            )
            .await
            .unwrap_err()
            .code,
        "api_shutdown"
    );
}

#[tokio::test]
async fn native_api_roles_context_and_payloads_fail_explicitly_without_starting_endpoints() {
    let (_temp, library, native, _events) = fixture();
    let mut context = library.context().unwrap();
    let port = free_port();
    assert_eq!(
        native
            .dispatch(
                request("WEBSOCKET_START", json!({"port":port}), &context),
                "output-test"
            )
            .await
            .unwrap_err()
            .code,
        "window_error"
    );
    assert_eq!(
        native
            .dispatch(
                request(
                    "WEBSOCKET_START",
                    json!({"password":"a".repeat(16*1024)}),
                    &context
                ),
                "main"
            )
            .await
            .unwrap_err()
            .code,
        "api_input_limit"
    );
    context.library_generation += 1;
    assert_eq!(
        native
            .dispatch(
                request("WEBSOCKET_START", json!({"port":port}), &context),
                "main"
            )
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    assert!(native.status()["binding"].is_null());
    native.shutdown().await.unwrap();
}
