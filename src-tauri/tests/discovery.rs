#[path = "../src/discovery.rs"]
mod discovery;
#[path = "../src/network_snapshot.rs"]
mod network_snapshot;
use discovery::{FrozenDiscoveryInputs, NativeDiscovery, NativeDiscoveryEvents};
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_discovery::{
    decode, DiscoveryConfig, DiscoveryEventKind, DiscoveryName, DiscoveryPlatform,
    DiscoveryTransport, NetworkSnapshot, RecordData,
};
use serde_json::{json, Value};
use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    process::Command,
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    sync::oneshot,
    time::{timeout, Instant},
};
fn library() -> (tempfile::TempDir, LibraryService) {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    (temp, library)
}
fn snapshot() -> NetworkSnapshot {
    serde_json::from_value(json!({"hostname":"private-host","interfaces":[{"name":"lo","addresses":[{"address":"127.0.0.1","family":"IPv4","internal":true,"mac":"00:00:00:00:00:00"}]}]})).unwrap()
}
async fn setup(library: LibraryService) -> (NativeDiscovery, NativeDiscoveryEvents, UdpSocket) {
    let peer = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut config = DiscoveryConfig::new([1, 2, 3]);
    config.platform = DiscoveryPlatform::Other;
    config.probe_jitter = Duration::ZERO;
    config.transport = DiscoveryTransport::Loopback {
        bind: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        target: match peer.local_addr().unwrap() {
            SocketAddr::V4(v) => v,
            _ => unreachable!(),
        },
    };
    let (native, events) = NativeDiscovery::new(
        library,
        config,
        NativeDiscovery::test_inputs(snapshot(), [0xab, 0xcd, 0xef]),
    )
    .unwrap();
    (native, events, peer)
}
async fn wait_for(mut condition: impl FnMut() -> bool) {
    timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
async fn announcement(peer: &UdpSocket) -> freeshow_discovery::Packet {
    timeout(Duration::from_secs(4), async {
        let mut bytes = [0; 9001];
        loop {
            let (n, _) = peer.recv_from(&mut bytes).await.unwrap();
            let packet = decode(&bytes[..n]).unwrap();
            if !packet.answers.is_empty() {
                break packet;
            }
        }
    })
    .await
    .unwrap()
}
async fn switch(library: &LibraryService, context: &RuntimeContext, path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap();
    let request:Request=serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"newPath":path,"oldPath":context.data_path}},"context":context})).unwrap();
    library.dispatch(request, "main").await.unwrap();
    assert_ne!(library.context().unwrap(), *context);
}
#[tokio::test]
async fn actual_bound_port_has_frozen_context_nonce_and_no_invented_renderer_ack() {
    let (_temp, library) = library();
    let context = library.context().unwrap();
    let (native, mut events, peer) = setup(library).await;
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let bound = listener.local_addr().unwrap();
    let run = native
        .begin(&context, "0123456789abcdef0123456789abcdef")
        .await
        .unwrap();
    assert_eq!(run.context(), &context);
    assert_eq!(run.listener_run(), "0123456789abcdef0123456789abcdef");
    assert_ne!(run.generation().run, run.listener_run());
    native
        .started_port(&run, DiscoveryName::Remote, bound)
        .await
        .unwrap();
    let packet = announcement(&peer).await;
    assert!(packet
        .answers
        .iter()
        .any(|r| matches!(&r.data,RecordData::Srv {port,..} if *port==bound.port())));
    assert!(packet
        .answers
        .iter()
        .any(|r| r.name.starts_with("freeshow-REMOTE-abcdef.")));
    let event = timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.context, context);
    assert_eq!(event.event.generation, *run.generation());
    assert!(matches!(event.event.kind, DiscoveryEventKind::Published));
    assert_eq!(event.listener_run, run.listener_run());
    assert_eq!(
        native.status()["binding"]["actualPorts"]["REMOTE"],
        bound.to_string()
    );
    // Source ignores later port changes until restart; retain the first actual
    // endpoint and do not manufacture a second publication result.
    let second = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    native
        .started_port(&run, DiscoveryName::Remote, second.local_addr().unwrap())
        .await
        .unwrap();
    assert_eq!(
        native.status()["binding"]["actualPorts"]["REMOTE"],
        bound.to_string()
    );
    native.stop(&run).await.unwrap();
    let goodbye = announcement(&peer).await;
    assert!(goodbye.answers.iter().all(|r| r.ttl == 0));
    native.shutdown().await.unwrap();
    native.shutdown().await.unwrap();
    assert_eq!(native.status()["service"]["socket_open"], false);
    assert!(events.recv().await.is_none());
}
#[tokio::test]
async fn context_refresh_cancels_pending_publication_and_reused_listener_ids_cannot_complete_old_run(
) {
    let (temp, library) = library();
    let old = library.context().unwrap();
    let (native, mut events, peer) = setup(library.clone()).await;
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let run = native
        .begin(&old, "11111111111111111111111111111111")
        .await
        .unwrap();
    native
        .started_port(&run, DiscoveryName::Stage, listener.local_addr().unwrap())
        .await
        .unwrap();
    switch(&library, &old, &temp.path().join("second")).await;
    native.context_changed().await.unwrap();
    assert!(native.status()["binding"].is_null());
    // Drain source browse query, then prove no late positive TTL announcement.
    let end = Instant::now() + Duration::from_millis(2100);
    let mut bytes = [0; 9001];
    while let Ok(Ok((n, _))) = timeout(
        end.saturating_duration_since(Instant::now()),
        peer.recv_from(&mut bytes),
    )
    .await
    {
        let packet = decode(&bytes[..n]).unwrap();
        assert!(packet.answers.iter().all(|r| r.ttl == 0));
    }
    assert!(events.try_recv().is_err());
    assert_eq!(
        native
            .started_port(&run, DiscoveryName::Remote, listener.local_addr().unwrap())
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    let new = library.context().unwrap();
    let next = native
        .begin(&new, "11111111111111111111111111111111")
        .await
        .unwrap();
    assert_ne!(next.generation(), run.generation());
    native.context_changed().await.unwrap();
    assert_eq!(
        native.status()["binding"]["generation"]["run"],
        next.generation().run
    );
    assert_eq!(
        native.stop(&run).await.unwrap_err().code,
        "stale_generation"
    );
    native.stop(&next).await.unwrap();
    native.shutdown().await.unwrap();
}
#[tokio::test]
async fn caller_drop_keeps_accepted_begin_and_bounded_admission_while_terminal_bypasses_the_queue()
{
    let (_temp, library) = library();
    let context = library.context().unwrap();
    let (native, _events, _peer) = setup(library).await;
    let (entered, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    let paused = native.clone();
    let hold = tokio::spawn(async move { paused.pause(entered, gate).await });
    ready.await.unwrap();
    let accepted = native.clone();
    let ctx = context.clone();
    let caller = tokio::spawn(async move {
        accepted
            .begin(&ctx, "22222222222222222222222222222222")
            .await
    });
    wait_for(|| native.status()["admittedRequests"] == 2).await;
    caller.abort();
    let _ = caller.await;
    release.send(()).unwrap();
    hold.await.unwrap().unwrap();
    wait_for(|| native.status()["binding"].is_object()).await;
    let (entered, ready) = oneshot::channel();
    let (_release, gate) = oneshot::channel();
    let paused = native.clone();
    let hold = tokio::spawn(async move { paused.pause(entered, gate).await });
    ready.await.unwrap();
    let mut requests = vec![];
    for _ in 0..15 {
        let host = native.clone();
        let ctx = context.clone();
        requests.push(tokio::spawn(async move {
            host.begin(&ctx, "33333333333333333333333333333333").await
        }));
    }
    wait_for(|| native.status()["admittedRequests"] == 16).await;
    assert_eq!(
        native
            .begin(&context, "44444444444444444444444444444444")
            .await
            .unwrap_err()
            .code,
        "discovery_busy"
    );
    let control = native.clone();
    let retirement = tokio::spawn(async move { control.context_changed().await });
    wait_for(|| native.status()["admittedControls"] == 1).await;
    retirement.abort();
    let _ = retirement.await;
    let mut controls = Vec::new();
    for _ in 0..4 {
        let control = native.clone();
        controls.push(tokio::spawn(async move { control.context_changed().await }));
    }
    wait_for(|| native.status()["waitingControls"] == 4).await;
    assert_eq!(
        native.context_changed().await.unwrap_err().code,
        "discovery_busy"
    );
    timeout(Duration::from_secs(2), native.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(native.status()["admittedControls"], 0);
    for control in controls {
        assert_eq!(control.await.unwrap().unwrap_err().code, "discovery_closed");
    }
    assert_eq!(hold.await.unwrap().unwrap_err().code, "discovery_closed");
    for request in requests {
        assert_eq!(request.await.unwrap().unwrap_err().code, "discovery_closed");
    }
    assert_eq!(native.status()["admittedRequests"], 0);
    assert_eq!(
        native
            .begin(&context, "44444444444444444444444444444444")
            .await
            .unwrap_err()
            .code,
        "discovery_closed"
    );
}
#[tokio::test]
async fn input_validation_no_bind_publication_and_real_bind_error_are_explicit() {
    let (_temp, library) = library();
    let context = library.context().unwrap();
    let (native, _events, _peer) = setup(library.clone()).await;
    let mut forged = context.clone();
    forged.runtime_id.push_str("-old");
    assert_eq!(
        native
            .begin(&forged, "55555555555555555555555555555555")
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    assert_eq!(
        native
            .begin(&context, "client supplied")
            .await
            .unwrap_err()
            .code,
        "invalid_listener_run"
    );
    assert!(native.status()["binding"].is_null());
    assert_eq!(native.status()["service"]["socket_open"], false);
    let run = native
        .begin(&context, "55555555555555555555555555555555")
        .await
        .unwrap();
    assert_eq!(
        native
            .started_port(&run, DiscoveryName::Remote, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap_err()
            .code,
        "invalid_discovery_port"
    );
    native.shutdown().await.unwrap();
    let blocked = socket2::Socket::new(
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
        assert_eq!(
            unsafe {
                setsockopt(
                    blocked.as_raw_socket() as _,
                    SOL_SOCKET,
                    SO_EXCLUSIVEADDRUSE,
                    (&enabled as *const i32).cast(),
                    4,
                )
            },
            0
        );
    }
    blocked
        .bind(&SocketAddr::from((Ipv4Addr::LOCALHOST, 0)).into())
        .unwrap();
    let mut config = DiscoveryConfig::new([1, 2, 3]);
    config.platform = DiscoveryPlatform::Other;
    config.transport = DiscoveryTransport::Loopback {
        bind: match blocked.local_addr().unwrap().as_socket().unwrap() {
            SocketAddr::V4(v) => v,
            _ => unreachable!(),
        },
        target: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 45678),
    };
    let (native, _events) = NativeDiscovery::new(
        library,
        config,
        NativeDiscovery::test_inputs(snapshot(), [1, 2, 3]),
    )
    .unwrap();
    assert_eq!(
        native
            .begin(&context, "66666666666666666666666666666666")
            .await
            .unwrap_err()
            .code,
        "discovery_bind"
    );
    assert!(native.status()["binding"].is_null());
    native.shutdown().await.unwrap();
}
#[tokio::test]
async fn coalesced_caller_drop_cannot_lose_a_context_change_at_retirement_completion() {
    let (temp, library) = library();
    let old = library.context().unwrap();
    let (native, _events, _peer) = setup(library.clone()).await;
    native
        .begin(&old, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .await
        .unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    native.pause_refresh_completion(entered, gate);
    let host = native.clone();
    let first = tokio::spawn(async move { host.context_changed().await });
    ready.await.unwrap();
    // First cleanup observed the original current context and did nothing.
    // The new accepted change arrives before its completion is published.
    switch(&library, &old, &temp.path().join("new-context")).await;
    let host = native.clone();
    let second = tokio::spawn(async move { host.context_changed().await });
    wait_for(|| native.status()["waitingControls"] == 2).await;
    second.abort();
    let _ = second.await;
    release.send(()).unwrap();
    first.await.unwrap().unwrap();
    assert!(native.status()["binding"].is_null());
    assert_eq!(native.status()["service"]["socket_open"], false);
    native.shutdown().await.unwrap();
}
#[tokio::test]
async fn queued_old_context_cannot_open_transport_after_library_selection() {
    let (temp, library) = library();
    let old = library.context().unwrap();
    let (native, _events, _peer) = setup(library.clone()).await;
    let (entered, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    let paused = native.clone();
    let hold = tokio::spawn(async move { paused.pause(entered, gate).await });
    ready.await.unwrap();
    let host = native.clone();
    let ctx = old.clone();
    let begin =
        tokio::spawn(async move { host.begin(&ctx, "99999999999999999999999999999999").await });
    wait_for(|| native.status()["admittedRequests"] == 2).await;
    switch(&library, &old, &temp.path().join("new-library")).await;
    release.send(()).unwrap();
    hold.await.unwrap().unwrap();
    assert_eq!(begin.await.unwrap().unwrap_err().code, "stale_library");
    assert_eq!(native.status()["service"]["socket_open"], false);
    assert!(native.status()["binding"].is_null());
    native.shutdown().await.unwrap();
}
#[tokio::test]
async fn real_owner_panic_is_terminal_sticky_and_supervision_reclaims_the_socket() {
    let (_temp, library) = library();
    let context = library.context().unwrap();
    let (native, mut events, _peer) = setup(library).await;
    native
        .begin(&context, "77777777777777777777777777777777")
        .await
        .unwrap();
    let endpoint: SocketAddr = native.status()["binding"]["discoveryEndpoint"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(native.panic_owner().await.is_err());
    let first = native.shutdown().await.unwrap_err();
    assert_eq!(first.code, "discovery_owner");
    let second = native.shutdown().await.unwrap_err();
    assert_eq!(second.code, first.code);
    assert_eq!(second.message, first.message);
    assert_eq!(native.context_changed().await.unwrap_err().code, first.code);
    assert_eq!(
        native
            .begin(&context, "77777777777777777777777777777777")
            .await
            .unwrap_err()
            .code,
        first.code
    );
    assert_eq!(native.status()["service"]["socket_open"], false);
    assert!(native.status()["binding"].is_null());
    let reclaimed = std::net::UdpSocket::bind(endpoint).unwrap();
    drop(reclaimed);
    assert!(events.recv().await.is_none());
}
#[tokio::test]
async fn lost_supervisor_completion_stays_failed_and_still_reclaims_independent_service() {
    let (_temp, library) = library();
    let context = library.context().unwrap();
    let (native, mut events, _peer) = setup(library).await;
    native
        .begin(&context, "cccccccccccccccccccccccccccccccc")
        .await
        .unwrap();
    let endpoint: SocketAddr = native.status()["binding"]["discoveryEndpoint"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    native.abort_supervisor();
    tokio::task::yield_now().await;
    let first = timeout(Duration::from_secs(2), native.shutdown())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(first.code, "discovery_owner");
    assert_eq!(native.shutdown().await.unwrap_err().message, first.message);
    assert_eq!(native.status()["service"]["socket_open"], false);
    let reclaimed = std::net::UdpSocket::bind(endpoint).unwrap();
    drop(reclaimed);
    assert!(timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .is_none());
}
#[cfg(unix)]
#[tokio::test]
async fn controlled_native_child_spawn_is_not_registration_and_terminal_kills_and_reaps() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, library) = library();
    let context = library.context().unwrap();
    let script = temp.path().join("controlled-dns-sd");
    let pid_file = temp.path().join("child.pid");
    let quoted = pid_file.to_str().unwrap().replace('\'', "'\\''");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntrap '' TERM\necho $$ > '{quoted}'\nexec /bin/sleep 60\n"),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = DiscoveryConfig::new([1, 2, 3]);
    config.platform = DiscoveryPlatform::Darwin {
        dns_sd: Some(script),
    };
    // Native child transport creates no UDP socket; loopback config retains the
    // test's explicit network authority even though that branch does not use it.
    config.transport = DiscoveryTransport::Loopback {
        bind: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        target: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 45678),
    };
    let (native, mut events) = NativeDiscovery::new(
        library,
        config,
        NativeDiscovery::test_inputs(snapshot(), [1, 2, 3]),
    )
    .unwrap();
    let run = native
        .begin(&context, "88888888888888888888888888888888")
        .await
        .unwrap();
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    native
        .started_port(
            &run,
            DiscoveryName::Controller,
            listener.local_addr().unwrap(),
        )
        .await
        .unwrap();
    let event = timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event.event.kind, DiscoveryEventKind::Spawned));
    assert_eq!(event.context, context);
    wait_for(|| {
        std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<libc::pid_t>().ok())
            .is_some()
    })
    .await;
    let pid: libc::pid_t = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    timeout(Duration::from_secs(3), native.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert_eq!(native.status()["service"]["children"], 0);
    assert!(events.recv().await.is_none());
}
#[cfg(unix)]
#[tokio::test]
async fn native_executable_disappearing_is_an_explicit_spawn_error_and_can_recover() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, library) = library();
    let context = library.context().unwrap();
    let script = temp.path().join("controlled-tool");
    std::fs::write(&script, "#!/bin/sh\nexec /bin/sleep 60\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = DiscoveryConfig::new([1, 2, 3]);
    config.platform = DiscoveryPlatform::Darwin {
        dns_sd: Some(script.clone()),
    };
    config.transport = DiscoveryTransport::Loopback {
        bind: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        target: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 45678),
    };
    let (native, mut events) = NativeDiscovery::new(
        library,
        config,
        NativeDiscovery::test_inputs(snapshot(), [1, 2, 3]),
    )
    .unwrap();
    std::fs::remove_file(&script).unwrap();
    let run = native
        .begin(&context, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        .await
        .unwrap();
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint = listener.local_addr().unwrap();
    assert_eq!(
        native
            .started_port(&run, DiscoveryName::Remote, endpoint)
            .await
            .unwrap_err()
            .code,
        "dns_sd_spawn"
    );
    assert_eq!(native.status()["service"]["children"], 0);
    assert!(native.status()["binding"]["actualPorts"]
        .as_object()
        .unwrap()
        .is_empty());
    assert!(events.try_recv().is_err());
    std::fs::write(&script, "#!/bin/sh\nexec /bin/sleep 60\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    native
        .started_port(&run, DiscoveryName::Remote, endpoint)
        .await
        .unwrap();
    let event = timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event.event.kind, DiscoveryEventKind::Spawned));
    native.stop(&run).await.unwrap();
    native.shutdown().await.unwrap();
    assert_eq!(native.status()["service"]["children"], 0);
}
fn bounded_node(mut command: Command) -> Value {
    use std::{io::Read, process::Stdio};
    let stdout = tempfile::NamedTempFile::new().unwrap();
    let stderr = tempfile::NamedTempFile::new().unwrap();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout.reopen().unwrap())
        .stderr(stderr.reopen().unwrap())
        .spawn()
        .unwrap();
    let end = std::time::Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= end
            || stdout.as_file().metadata().unwrap().len() > 1024 * 1024
            || stderr.as_file().metadata().unwrap().len() > 1024 * 1024
        {
            let _ = child.kill();
            let _ = child.wait();
            panic!("controlled Node oracle exceeded its time/output bound");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(stdout.as_file().metadata().unwrap().len() <= 1024 * 1024);
    assert!(stderr.as_file().metadata().unwrap().len() <= 1024 * 1024);
    let mut out = String::new();
    stdout.as_file().read_to_string(&mut out).unwrap();
    let mut err = String::new();
    stderr.as_file().read_to_string(&mut err).unwrap();
    assert!(status.success(), "{err}");
    serde_json::from_str(&out).unwrap()
}
#[tokio::test]
async fn frozen_actual_os_candidates_and_hostname_match_node_native_order_and_source_get_local_ips()
{
    // No publication or socket bind: compare native OS reads on this actual OS.
    let node = std::env::var_os("FREESHOW_TEST_NODE").unwrap_or_else(|| "node".into());
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/electron/data/bonjour.ts");
    // The independent check manifest is outside src-tauri; use the source module
    // path relative to this actual test source for the local standalone harness.
    let source = if source.exists() {
        source
    } else {
        std::path::PathBuf::from(file!())
            .parent()
            .unwrap()
            .join("../../src/electron/data/bonjour.ts")
    };
    let script = r#"const fs=require('fs'),ts=require('typescript'),os=require('os'),vm=require('vm'),crypto=require('crypto');const text=fs.readFileSync(process.argv[1],'utf8').replaceAll('\r\n','\n');if(crypto.createHash('sha256').update(text).digest('hex')!=='6ff78829b6f8a383d7584fdbec69e134892bb3299fbd7189901e41bcd4c5e869')throw Error('immutable Bonjour source changed'); const ast=ts.createSourceFile('bonjour.ts',text,ts.ScriptTarget.Latest,true);const fn=ast.statements.find(n=>ts.isFunctionDeclaration(n)&&n.name.text==='getLocalIPs'); const interfaces=os.networkInterfaces();const context=vm.createContext({os:{networkInterfaces:()=>interfaces}});const code=ts.transpileModule(fn.getText(ast).replace(/^export\s+/,'' )+';globalThis.ips=getLocalIPs()', {compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;new vm.Script(code).runInContext(context);console.log(JSON.stringify({runtime:{node:process.version,uv:process.versions.uv},hostname:os.hostname(),interfaces:Object.entries(interfaces).map(([name,addresses])=>({name,addresses:addresses.map(({address,family,internal,mac})=>({address,family,internal,mac}))})),ips:context.ips}));"#;
    let root = source
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let capture_node = || {
        let mut command = Command::new(&node);
        command.args(["-e", script]).arg(&source).current_dir(root);
        bounded_node(command)
    };
    let snapshots =
        network_snapshot::NetworkSnapshots::start(network_snapshot::SnapshotLimits::default())
            .unwrap();
    let mut stable = false;
    for _ in 0..3 {
        let before = capture_node();
        eprintln!("Native discovery source runtime: {}", before["runtime"]);
        let inputs = FrozenDiscoveryInputs::capture(&snapshots).await.unwrap();
        let after = capture_node();
        if before != after {
            continue;
        }
        assert_eq!(
            serde_json::to_value(inputs.snapshot()).unwrap(),
            json!({"hostname":before["hostname"],"interfaces":before["interfaces"]})
        );
        assert_eq!(
            json!(freeshow_discovery::get_local_ips(inputs.snapshot()).unwrap()),
            before["ips"]
        );
        stable = true;
        break;
    }
    assert!(
        stable,
        "Native interfaces changed across all three bounded source/native comparisons"
    );
    // Production picks a fixed native mac executable, never a renderer path or
    // PATH lookup. Constructing this config does not create any socket/process.
    snapshots.shutdown().await.unwrap();
    let config = NativeDiscovery::production_config();
    if cfg!(target_os = "macos") {
        assert!(
            matches!(config.platform,DiscoveryPlatform::Darwin {dns_sd:Some(path)} if path==std::path::Path::new("/usr/bin/dns-sd"))
        );
    }
}
