use freeshow_network::{encode_message, EmitOutcome, OscErrorKind, OscSender, OscSenderConfig};
use serde_json::json;
use std::{net::Ipv4Addr, time::Duration};
use tokio::{net::UdpSocket, time::timeout};

#[tokio::test]
async fn sends_all_reachable_tags_and_preserves_fifo_to_loopback() {
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = receiver.local_addr().unwrap().port();
    let sender = OscSender::start(OscSenderConfig::default()).unwrap();
    let messages = [
        "/日本語/🙌🏾 true false 1 2.5 hi",
        "/space  a  ",
        "/edge -0 2147483648 -2147483649 4294967295 4294967296 9007199254740993",
        "/float Infinity -Infinity 1e309 -1e-400 1e-45 3.4028236e38",
        "/numbers 0x20000000000001 0o377 0b101 1. 1e-5 .5 +2 -0x1 NaN inf",
        "/whitespace \t \u{a0} \u{feff} \n123\r",
        "/",
    ];
    let tickets = messages
        .iter()
        .map(|data| {
            sender
                .try_emit(&json!({
                    "signal":{"host":"127.0.0.1","port":port}, "data":data,
                }))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut buffer = [0u8; 65535];
    for (data, ticket) in messages.iter().zip(tickets) {
        assert!(matches!(
            ticket.wait().await.unwrap(),
            EmitOutcome::Sent { .. }
        ));
        let (length, source) = timeout(Duration::from_secs(2), receiver.recv_from(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert!(source.ip().is_loopback());
        assert_ne!(source.port(), 0);
        assert_eq!(&buffer[..length], encode_message(data).unwrap().packet());
    }
    sender.shutdown().await;
}

#[tokio::test]
async fn hostname_resolution_is_ipv4_and_handles_repeated_sends() {
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = receiver.local_addr().unwrap().port();
    let sender = OscSender::start(OscSenderConfig::default()).unwrap();
    for index in 0..8 {
        sender.emit(&json!({"signal":{"host":"localhost","port":port.to_string()},"data":format!("/dns {index}")})).await.unwrap();
        let mut buffer = [0; 128];
        let (length, _) = timeout(Duration::from_secs(2), receiver.recv_from(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            &buffer[..length],
            encode_message(&format!("/dns {index}")).unwrap().packet()
        );
    }
    sender.shutdown().await;
}

#[tokio::test]
async fn unavailable_udp_receiver_is_not_misreported_as_delivery_failure() {
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = receiver.local_addr().unwrap().port();
    drop(receiver);
    let sender = OscSender::start(OscSenderConfig::default()).unwrap();
    // An unconnected UDP socket accepts this send; neither Node nor Rust has a
    // receiver acknowledgement. The outcome only promises local OS acceptance.
    assert!(matches!(
        sender
            .emit(&json!({"signal":{"host":"127.0.0.1","port":port},"data":"/absent"}))
            .await
            .unwrap(),
        EmitOutcome::Sent { .. }
    ));
    sender.shutdown().await;
}

#[tokio::test]
async fn send_error_recovers_and_shutdown_closes_admission_for_all_clones() {
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = receiver.local_addr().unwrap().port();
    let sender = OscSender::start(OscSenderConfig::default()).unwrap();
    let clone = sender.clone();
    let error = sender
        .emit(&json!({"signal":{"host":"127.0.0.1","port":"0"},"data":"/zero"}))
        .await
        .unwrap_err();
    assert_eq!(error.kind, OscErrorKind::SendFailed);
    let error = sender
        .emit(&json!({"signal":{"host":"::1","port":port},"data":"/ipv6"}))
        .await
        .unwrap_err();
    assert_eq!(error.kind, OscErrorKind::ResolveFailed);
    clone
        .emit(&json!({"signal":{"host":"127.0.0.1","port":port},"data":"/recovered"}))
        .await
        .unwrap();
    let mut buffer = [0; 128];
    let (length, _) = timeout(Duration::from_secs(2), receiver.recv_from(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        &buffer[..length],
        encode_message("/recovered").unwrap().packet()
    );
    tokio::join!(sender.shutdown(), clone.shutdown());
    sender.shutdown().await;
    assert_eq!(
        clone.try_emit(&json!({"data":"/closed"})).unwrap_err().kind,
        OscErrorKind::Closed
    );
}

#[tokio::test]
async fn cancelled_queued_ticket_never_opens_a_socket_or_sends() {
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = receiver.local_addr().unwrap().port();
    let sender = OscSender::start(OscSenderConfig::default()).unwrap();
    let cancelled = sender
        .try_emit(&json!({"signal":{"host":"127.0.0.1","port":port},"data":"/cancelled"}))
        .unwrap();
    drop(cancelled);
    sender
        .emit(&json!({"signal":{"host":"127.0.0.1","port":port},"data":"/kept"}))
        .await
        .unwrap();
    let mut buffer = [0; 128];
    let (length, _) = timeout(Duration::from_secs(2), receiver.recv_from(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buffer[..length], encode_message("/kept").unwrap().packet());
    assert!(
        timeout(Duration::from_millis(30), receiver.recv_from(&mut buffer))
            .await
            .is_err()
    );
    sender.shutdown().await;
}

#[tokio::test]
async fn maximum_datagrams_complete_and_sender_recreation_releases_socket_ports() {
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = receiver.local_addr().unwrap().port();
    let data = format!("/x {}", "a".repeat(65_492));
    let expected = encode_message(&data).unwrap();
    assert_eq!(expected.packet().len(), 65_504);
    assert_eq!(
        encode_message(&format!("{data}aaaa")).unwrap_err().kind,
        OscErrorKind::PacketTooLarge
    );
    for _ in 0..4 {
        let sender = OscSender::start(OscSenderConfig::default()).unwrap();
        let outcome = sender
            .emit(&json!({"signal":{"host":"127.0.0.1","port":port},"data":data}))
            .await
            .unwrap();
        assert!(matches!(outcome, EmitOutcome::Sent { bytes: 65_504, .. }));
        let mut buffer = [0u8; 65_535];
        let (length, source) = timeout(Duration::from_secs(2), receiver.recv_from(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buffer[..length], expected.packet());
        // An ephemeral socket still bound to 0.0.0.0 would prevent rebinding
        // its source address. Send completion must have dropped that socket.
        let reused = std::net::UdpSocket::bind(source).unwrap();
        drop(reused);
        sender.shutdown().await;
    }
}
