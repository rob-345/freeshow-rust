use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
};
fn config() -> RemoteConfig {
    RemoteConfig {
        bind_ip: Ipv4Addr::LOCALHOST.into(),
        handshake_timeout: Duration::from_millis(200),
        ..RemoteConfig::default()
    }
}
fn payload(port: Value) -> Value {
    json!({"ports":{"remote":port},"max":10,"disabled":{"remote":false},"data":{}})
}
async fn request(address: std::net::SocketAddr, method: &str, url: &str, body: &str) -> String {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let text=format!("{method} {url} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: text/plain;charset=UTF-8\r\nContent-Length: {}\r\n\r\n{body}",body.len());
    stream.write_all(text.as_bytes()).await.unwrap();
    let mut bytes = vec![];
    stream.read_to_end(&mut bytes).await.unwrap();
    String::from_utf8(bytes).unwrap()
}
async fn joined(service: &RemoteService, address: std::net::SocketAddr) -> String {
    let text = request(address, "GET", "/socket.io/?EIO=4&transport=polling", "").await;
    let body = text.split("\r\n\r\n").nth(1).unwrap();
    let value: Value = serde_json::from_str(&body[1..]).unwrap();
    let sid = value["sid"].as_str().unwrap().to_owned();
    let text = request(
        address,
        "POST",
        &format!("/socket.io/?EIO=4&transport=polling&sid={sid}"),
        "40",
    )
    .await;
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    tokio::time::timeout(Duration::from_secs(1), async {
        while service.stats().unwrap().clients[&ServerKind::REMOTE] != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    sid
}

#[tokio::test]
async fn stop_cancels_handshake_after_session_creation_before_sid_registration() {
    let (service, _events) = RemoteService::new(config()).unwrap();
    let bound = service.start(&payload(json!("0")), 1).await.unwrap();
    let run = service
        .inner
        .current
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    *run.handshake_pause.lock().unwrap() = Some((entered.clone(), release));
    let address = bound.endpoints[&ServerKind::REMOTE];
    let client = tokio::spawn(request(
        address,
        "GET",
        "/socket.io/?EIO=4&transport=polling",
        "",
    ));
    entered.notified().await;
    assert_eq!(
        run.config.max_engine_sessions - run.engine_sessions.available_permits(),
        1
    );
    tokio::time::timeout(Duration::from_secs(2), service.stop())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        run.engine_sessions.available_permits(),
        run.config.max_engine_sessions
    );
    assert_eq!(
        run.tcp_connections.available_permits(),
        run.config.max_tcp_connections
    );
    let _ = client.await;
    service
        .start(&payload(json!(address.port())), 2)
        .await
        .unwrap();
    service.stop().await.unwrap();
}

#[tokio::test]
async fn dropped_start_future_keeps_run_owned_until_next_serial_restart() {
    let (service, _events) = RemoteService::new(config()).unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    *service.inner.ready_pause.lock().unwrap() = Some((entered.clone(), release));
    let child = service.clone();
    let task = tokio::spawn(async move { child.start(&payload(json!("0")), 1).await });
    entered.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let address = service
        .inner
        .active
        .lock()
        .await
        .as_ref()
        .unwrap()
        .bound
        .endpoints[&ServerKind::REMOTE];
    let bound = tokio::time::timeout(
        Duration::from_secs(2),
        service.start(&payload(json!(address.port())), 2),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(bound.endpoints[&ServerKind::REMOTE].port(), address.port());
    service.stop().await.unwrap();
}

#[tokio::test]
async fn closed_event_receiver_and_reserved_disconnect_permit_cannot_stall_shutdown() {
    let (service, events) = RemoteService::new(RemoteConfig {
        max_clients: 1,
        data_events: 1,
        ..config()
    })
    .unwrap();
    let bound = service.start(&payload(json!("0")), 1).await.unwrap();
    joined(&service, bound.endpoints[&ServerKind::REMOTE]).await;
    drop(events);
    tokio::time::timeout(Duration::from_secs(2), service.stop())
        .await
        .unwrap()
        .unwrap();
    assert!(service.stats().is_none());
}

#[tokio::test]
async fn renderer_data_stays_between_its_reserved_connection_and_disconnect_events() {
    let (service, mut events) = RemoteService::new(config()).unwrap();
    let bound = service.start(&payload(json!("0")), 1).await.unwrap();
    let address = bound.endpoints[&ServerKind::REMOTE];
    let sid = joined(&service, address).await;
    request(
        address,
        "POST",
        &format!("/socket.io/?EIO=4&transport=polling&sid={sid}"),
        "42[\"REMOTE\",{\"channel\":\"MIDDLE\",\"id\":\"client-controlled\"}]",
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), service.stop())
        .await
        .unwrap()
        .unwrap();
    let mut channels = vec![];
    for _ in 0..3 {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        if let RemoteEventKind::Renderer { message, .. } = event.kind {
            channels.push(message["channel"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(channels, ["CONNECTION", "MIDDLE", "DISCONNECT"]);
}

#[test]
fn output_size_depth_and_configuration_limits_reject_before_clones() {
    assert_eq!(
        semantics::check_size(&json!({"text":"x".repeat(100)}), 20)
            .unwrap_err()
            .code,
        "payload_limit"
    );
    let mut value = Value::Null;
    for _ in 0..130 {
        value = json!([value]);
    }
    assert_eq!(
        semantics::check_size(&value, 10000).unwrap_err().code,
        "json_depth_limit"
    );
    assert!(RemoteService::new(RemoteConfig {
        max_clients: 0,
        ..config()
    })
    .is_err());
}

#[tokio::test]
async fn terminal_shutdown_has_capacity_independent_of_lifecycle_admission() {
    let (service, _events) = RemoteService::new(config()).unwrap();
    let bound = service.start(&payload(json!("0")), 1).await.unwrap();
    let permits = (0..8)
        .map(|_| service.inner.lifecycle.try_acquire().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        service.stop().await.unwrap_err().code,
        "lifecycle_queue_full"
    );
    tokio::time::timeout(Duration::from_secs(2), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    drop(permits);
    assert_eq!(
        service
            .start(&payload(json!("0")), 2)
            .await
            .unwrap_err()
            .code,
        "shutdown"
    );
    assert!(service.stats().is_none());
    let listener = bind_tcp(
        Ipv4Addr::LOCALHOST.into(),
        bound.endpoints[&ServerKind::REMOTE].port(),
    )
    .unwrap();
    drop(listener);
}
