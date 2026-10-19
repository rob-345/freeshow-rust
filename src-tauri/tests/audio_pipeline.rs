#[path = "../src/audio_pipeline.rs"]
mod audio_pipeline;
use audio_pipeline::*;
use freeshow_audio_pipeline::{AudioMetadata, JsInput, SinkKind};
use freeshow_core::{
    protocol::{Event, Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_icecast::{crc32, Disposition};
use freeshow_opus::{Capability, LibrarySource};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
fn cancel() -> CancellationToken {
    CancellationToken::new()
}
fn setup_library() -> (tempfile::TempDir, LibraryService) {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    (temp, library)
}
struct Setup {
    _temp: tempfile::TempDir,
    library: LibraryService,
    native: NativeAudioPipeline,
    events: Arc<Mutex<Vec<Event>>>,
    delivery: tokio::task::JoinHandle<()>,
}
fn setup(source: LibrarySource, limits: AudioLimits) -> Setup {
    setup_with_ack(source, limits, true)
}
fn setup_with_ack(source: LibrarySource, limits: AudioLimits, accepted: bool) -> Setup {
    let (temp, library) = setup_library();
    let (send, mut receive) = mpsc::channel::<AudioStatusDelivery>(1);
    let events = Arc::new(Mutex::new(Vec::new()));
    let saved = events.clone();
    let native = NativeAudioPipeline::start(library.clone(), source, limits, move |e| {
        send.blocking_send(e)
            .map_err(|_| "test renderer closed".into())
    })
    .unwrap();
    let host = native.clone();
    let delivery = tokio::spawn(async move {
        while let Some(e) = receive.recv().await {
            assert!(e.run() > 0);
            assert_eq!(
                e.own_undefined_error(),
                e.event()
                    .message
                    .data
                    .as_ref()
                    .unwrap()
                    .get("error")
                    .is_none()
            );
            let _diagnostic = e.diagnostic();
            saved.lock().unwrap().push(e.event().clone());
            let context = e.event().context.as_ref().unwrap();
            let _ = host.complete_status("main", context, e.id(), accepted);
        }
    });
    Setup {
        _temp: temp,
        library,
        native,
        events,
        delivery,
    }
}
async fn wait_for(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
async fn require_codec(setup: &Setup) -> bool {
    match setup.native.capability().await.unwrap() {
        Capability::Available(_) => true,
        Capability::Unavailable(e) => {
            eprintln!(
                "REAL NATIVE AUDIO CODEC UNAVAILABLE: {} {}; real-path coverage not claimed",
                e.code, e.message
            );
            assert!(
                std::env::var_os("FREESHOW_REQUIRE_OPUS").is_none(),
                "strict native codec required"
            );
            false
        }
    }
}
fn frame(offset: usize) -> Vec<u8> {
    let mut bytes = vec![0; 960 * 2 * 4];
    for channel in 0..2 {
        for i in 0..960 {
            let value = (0.4
                * (2.0
                    * std::f64::consts::PI
                    * (if channel == 0 { 440.0 } else { 660.0 })
                    * (offset + i) as f64
                    / 48000.0)
                    .sin()) as f32;
            bytes[(channel * 960 + i) * 4..(channel * 960 + i + 1) * 4]
                .copy_from_slice(&value.to_le_bytes());
        }
    }
    bytes
}
fn meta(port: u16) -> AudioMetadata {
    AudioMetadata {
        sample_rate: JsInput::Number(48000.0),
        target_id: JsInput::Json(json!("icecast")),
        icecast: Some(
            json!({"enabled":true,"host":"127.0.0.1","port":port,"mount":"audio","password":"秘密"}),
        ),
    }
}
async fn send(
    setup: &Setup,
    context: &RuntimeContext,
    bytes: &[u8],
    meta: &AudioMetadata,
) -> AudioReceipt {
    setup
        .native
        .pcm(
            context,
            bytes,
            bytes.len(),
            meta,
            &SinkAvailability::default(),
            cancel(),
        )
        .await
        .unwrap()
}
struct Capture {
    port: u16,
    bytes: tokio::task::JoinHandle<Vec<u8>>,
}
async fn capture() -> Capture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let bytes = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    Capture { port, bytes }
}
// Two actual connections to the SAME endpoint/config expose accidental context
// retagging that a source config/port change would otherwise hide.
async fn capture_two() -> (
    u16,
    oneshot::Receiver<Vec<u8>>,
    tokio::task::JoinHandle<Vec<u8>>,
) {
    async fn read(socket: tokio::net::TcpStream) -> Vec<u8> {
        let mut bytes = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            socket.take(1024 * 1024 + 1).read_to_end(&mut bytes),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(bytes.len() <= 1024 * 1024);
        bytes
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (send_first, first) = oneshot::channel();
    let second = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let first = tokio::spawn(async move {
            let _ = send_first.send(read(socket).await);
        });
        let (socket, _) = listener.accept().await.unwrap();
        let bytes = read(socket).await;
        first.await.unwrap();
        bytes
    });
    (port, first, second)
}
fn parse(bytes: &[u8]) -> (String, Vec<Vec<u8>>) {
    let split = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let header = String::from_utf8(bytes[..split].to_vec()).unwrap();
    let mut cursor = split;
    let mut pages = Vec::new();
    while cursor < bytes.len() {
        assert_eq!(&bytes[cursor..cursor + 4], b"OggS");
        let segments = bytes[cursor + 26] as usize;
        let end = cursor
            + 27
            + segments
            + bytes[cursor + 27..cursor + 27 + segments]
                .iter()
                .map(|b| *b as usize)
                .sum::<usize>();
        let page = bytes[cursor..end].to_vec();
        let mut zero = page.clone();
        let stored = u32::from_le_bytes(zero[22..26].try_into().unwrap());
        zero[22..26].fill(0);
        assert_eq!(crc32(&zero), stored, "raw on-wire CRC before normalization");
        pages.push(page);
        cursor = end;
    }
    assert_eq!(cursor, bytes.len());
    let mut sequence = 0u32;
    let mut granule = 0u64;
    for p in &pages {
        if p[5] == 2 {
            sequence = 0;
            granule = 0;
        }
        assert_eq!(u32::from_le_bytes(p[18..22].try_into().unwrap()), sequence);
        sequence += 1;
        if !payload(p).starts_with(b"Opus") && !payload(p).is_empty() {
            assert_eq!(p[5], 0);
            granule += 960;
        }
        assert_eq!(u64::from_le_bytes(p[6..14].try_into().unwrap()), granule);
    }
    (header, pages)
}
fn payload(page: &[u8]) -> &[u8] {
    &page[27 + page[26] as usize..]
}
async fn switch(setup: &Setup, old: &RuntimeContext) -> RuntimeContext {
    let path = setup
        ._temp
        .path()
        .join(format!("other-library-{}", old.library_generation));
    std::fs::create_dir_all(&path).unwrap();
    let request:Request=serde_json::from_value(json!({"channel":"MAIN","context":old,"data":{"channel":"UPDATE_DATA_PATH","data":{"newPath":path,"oldPath":old.data_path}}})).unwrap();
    setup.library.dispatch(request, "main").await.unwrap();
    setup.native.context_changed().unwrap();
    setup.library.context().unwrap()
}

async fn connected(setup: &Setup, context: &RuntimeContext) {
    wait_for(|| {
        setup.events.lock().unwrap().iter().any(|e| {
            e.context.as_ref() == Some(context)
                && e.message.data.as_ref().unwrap()["connected"] == true
        })
    })
    .await;
}
fn stream_run(receipt: &AudioReceipt) -> u64 {
    receipt
        .effects()
        .iter()
        .find_map(|e| match e {
            Effect::Icecast { run, .. } => Some(*run),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn context_retirement_closes_idle_real_stream_despite_full_ordinary_receipts() {
    let _serial = SERIAL.lock().await;
    let setup = setup(LibrarySource::system(), AudioLimits::default());
    if !require_codec(&setup).await {
        setup.native.shutdown().await.unwrap();
        return;
    }
    let tcp = capture().await;
    let old = setup.library.context().unwrap();
    let first = send(&setup, &old, &frame(0), &meta(tcp.port)).await;
    connected(&setup, &old).await;
    let second = send(&setup, &old, &frame(960), &meta(tcp.port)).await;
    let run = stream_run(&second);
    let third = send(&setup, &old, &[], &AudioMetadata::default()).await;
    let fourth = send(&setup, &old, &[], &AudioMetadata::default()).await;
    assert_eq!(setup.native.status()["retained"], 4);
    let current = switch(&setup, &old).await;
    assert_ne!(old, current);
    // No PCM, shutdown or ordinary disconnect follows the library switch.
    let receipt = setup.native.retire_context(&old).await.unwrap();
    assert_eq!(receipt.context, old);
    assert_eq!(receipt.run, Some(run));
    assert!(receipt.disconnected);
    let bytes = tokio::time::timeout(Duration::from_secs(2), tcp.bytes)
        .await
        .unwrap()
        .unwrap();
    let (_, pages) = parse(&bytes);
    assert_eq!(
        pages.last().unwrap()[5],
        4,
        "actual EOS followed by peer EOF"
    );
    assert_eq!(setup.native.status()["retained"], 4);
    assert_eq!(setup.native.status()["retiring"], 1);
    assert_eq!(
        setup.native.retire_context(&old).await.unwrap_err().code,
        "audio_retirement_busy"
    );
    drop(receipt);
    let noop = setup.native.retire_context(&old).await.unwrap();
    assert!(!noop.disconnected);
    assert_eq!(noop.run, None);
    drop((noop, first, second, third, fourth));
    let mut foreign = old.clone();
    foreign.runtime_id.push('x');
    assert_eq!(
        setup
            .native
            .retire_context(&foreign)
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    let mut future = current.clone();
    future.library_generation += 1;
    assert_eq!(
        setup.native.retire_context(&future).await.unwrap_err().code,
        "stale_library"
    );
    let mut wrong_path = current.clone();
    wrong_path.data_path.push("wrong");
    assert_eq!(
        setup
            .native
            .retire_context(&wrong_path)
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    let mut oversized = old.clone();
    oversized.runtime_id = "x".repeat(257);
    assert_eq!(
        setup
            .native
            .retire_context(&oversized)
            .await
            .unwrap_err()
            .code,
        "audio_input_limit"
    );
    setup.native.shutdown().await.unwrap();
    assert_eq!(setup.native.status()["joined"], true);
    setup.delivery.await.unwrap();
}

#[tokio::test]
async fn new_context_pcm_racing_old_retirement_creates_new_run_and_preserves_global_codec() {
    let _serial = SERIAL.lock().await;
    let setup = setup(LibrarySource::system(), AudioLimits::default());
    if !require_codec(&setup).await {
        setup.native.shutdown().await.unwrap();
        return;
    }
    let (port, old_bytes, mut new_bytes) = capture_two().await;
    let old = setup.library.context().unwrap();
    send(&setup, &old, &frame(0), &meta(port)).await;
    connected(&setup, &old).await;
    let old_run = stream_run(&send(&setup, &old, &frame(960), &meta(port)).await);
    // Explicit disconnect preserves source's global three-second reconnect
    // cooldown. Let that real clock expire without changing the endpoint/config.
    tokio::time::sleep(Duration::from_millis(3010)).await;
    let new = switch(&setup, &old).await;
    let (entered, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    setup.native.test_gate(true, entered, gate);
    let host = setup.native.clone();
    let context = new.clone();
    let pcm = tokio::spawn(async move {
        let bytes = frame(1920);
        host.pcm(
            &context,
            &bytes,
            bytes.len(),
            &meta(port),
            &SinkAvailability::default(),
            cancel(),
        )
        .await
    });
    ready.await.unwrap();
    // Synchronously accepted control is behind the already accepted new PCM.
    let retire_old = setup.native.retire_context(&old);
    assert_eq!(setup.native.status()["retiring"], 1);
    release.send(()).unwrap();
    let first_new = pcm.await.unwrap().unwrap();
    let new_run = stream_run(&first_new);
    assert!(
        new_run > old_run,
        "new context cannot adopt/retag the old live run"
    );
    let receipt = retire_old.await.unwrap();
    assert!(
        !receipt.disconnected,
        "PCM already closed the old socket before genuine new connect"
    );
    drop(receipt);
    let bytes = tokio::time::timeout(Duration::from_secs(2), old_bytes)
        .await
        .unwrap()
        .unwrap();
    let (_, pages) = parse(&bytes);
    assert_eq!(pages.last().unwrap()[5], 4);
    connected(&setup, &new).await;
    let written = send(&setup, &new, &frame(2880), &meta(port)).await;
    assert!(matches!(
        written.effects(),
        [
            Effect::Encoded { sequence: 4, .. },
            Effect::Icecast {
                disposition: Disposition::Written,
                ..
            }
        ]
    ));
    assert_eq!(stream_run(&written), new_run);
    let late = setup.native.retire_context(&old).await.unwrap();
    assert!(!late.disconnected);
    drop(late);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut new_bytes)
            .await
            .is_err(),
        "delayed old cleanup leaves new real socket open"
    );
    let closed = setup.native.retire_context(&new).await.unwrap();
    assert_eq!(closed.run, Some(new_run));
    assert!(closed.disconnected);
    drop(closed);
    let bytes = tokio::time::timeout(Duration::from_secs(2), new_bytes)
        .await
        .unwrap()
        .unwrap();
    let (_, pages) = parse(&bytes);
    assert_eq!(pages.last().unwrap()[5], 4);
    setup.native.shutdown().await.unwrap();
    setup.delivery.await.unwrap();
}

#[tokio::test]
async fn retirement_is_owned_even_when_its_future_is_dropped_without_polling() {
    let _serial = SERIAL.lock().await;
    let absent = std::env::temp_dir().join("freeshow-native-audio-absent-opus-a3710.so");
    let setup = setup(
        unsafe { LibrarySource::trusted_path(&absent) }.unwrap(),
        AudioLimits::default(),
    );
    let context = setup.library.context().unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    setup.native.test_gate(false, entered, gate);
    let host = setup.native.clone();
    let c = context.clone();
    let pcm = tokio::spawn(async move {
        host.pcm(
            &c,
            &[0],
            1,
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            cancel(),
        )
        .await
    });
    ready.await.unwrap();
    drop(setup.native.retire_context(&context));
    assert_eq!(setup.native.status()["retiring"], 1);
    assert_eq!(
        setup
            .native
            .retire_context(&context)
            .await
            .unwrap_err()
            .code,
        "audio_retirement_busy"
    );
    release.send(()).unwrap();
    pcm.await.unwrap().unwrap();
    wait_for(|| setup.native.status()["retiring"] == 0).await;
    let proof = setup.native.retire_context(&context).await.unwrap();
    assert_eq!(proof.context, context);
    assert!(!proof.disconnected);
    drop(proof);
    setup.native.shutdown().await.unwrap();
    setup.delivery.await.unwrap();
}

#[tokio::test]
async fn dropped_timed_out_retirement_stays_owned_and_terminal_retry_joins_actual_cleanup() {
    let _serial = SERIAL.lock().await;
    let setup = setup(
        LibrarySource::system(),
        AudioLimits {
            shutdown_timeout: Duration::from_millis(80),
            ..AudioLimits::default()
        },
    );
    if !require_codec(&setup).await {
        setup.native.shutdown().await.unwrap();
        return;
    }
    let tcp = capture().await;
    let context = setup.library.context().unwrap();
    send(&setup, &context, &frame(0), &meta(tcp.port)).await;
    connected(&setup, &context).await;
    send(&setup, &context, &frame(960), &meta(tcp.port)).await;
    let (entered, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    setup.native.test_gate(true, entered, gate);
    let host = setup.native.clone();
    let old = context.clone();
    let port = tcp.port;
    let pcm = tokio::spawn(async move {
        let bytes = frame(1920);
        host.pcm(
            &old,
            &bytes,
            bytes.len(),
            &meta(port),
            &SinkAvailability::default(),
            cancel(),
        )
        .await
    });
    ready.await.unwrap();
    assert_eq!(
        setup
            .native
            .retire_context(&context)
            .await
            .unwrap_err()
            .code,
        "audio_retirement_timeout"
    );
    assert_eq!(
        setup.native.status()["retiring"],
        1,
        "timeout is not actual cleanup completion"
    );
    assert_eq!(
        setup
            .native
            .retire_context(&context)
            .await
            .unwrap_err()
            .code,
        "audio_retirement_busy"
    );
    assert_eq!(
        setup.native.shutdown().await.unwrap_err().code,
        "audio_shutdown_timeout"
    );
    assert_eq!(setup.native.status()["joined"], false);
    pcm.abort();
    release.send(()).unwrap();
    wait_for(|| setup.native.status()["done"] == true).await;
    setup.native.shutdown().await.unwrap();
    assert_eq!(setup.native.status()["retiring"], 0);
    assert_eq!(setup.native.status()["drainedOwners"], 15);
    assert_eq!(setup.native.status()["joined"], true);
    let bytes = tokio::time::timeout(Duration::from_secs(2), tcp.bytes)
        .await
        .unwrap()
        .unwrap();
    let (_, pages) = parse(&bytes);
    assert_eq!(pages.last().unwrap()[5], 4);
    setup.delivery.await.unwrap();
}

#[tokio::test]
async fn real_pcm_opus_ogg_status_metadata_and_eos_or_explicit_unavailable() {
    let _serial = SERIAL.lock().await;
    let setup = setup(LibrarySource::system(), AudioLimits::default());
    if !require_codec(&setup).await {
        setup.native.shutdown().await.unwrap();
        return;
    }
    let tcp = capture().await;
    let c = setup.library.context().unwrap();
    setup
        .native
        .metadata(&c, "演奏者 - One", cancel())
        .await
        .unwrap();
    let first = send(&setup, &c, &frame(0), &meta(tcp.port)).await;
    assert_eq!(first.context(), &c);
    assert!(first.sequence() > 0);
    assert!(first.diagnostics().is_empty());
    assert!(first.failure().is_none());
    assert!(matches!(
        first.effects(),
        [
            Effect::Encoded { sequence: 1, .. },
            Effect::Icecast {
                disposition: Disposition::ConnectingDropped,
                ..
            }
        ]
    ));
    wait_for(|| {
        setup
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.message.data.as_ref().unwrap()["connected"] == true)
    })
    .await;
    let second = send(&setup, &c, &frame(960), &meta(tcp.port)).await;
    assert!(matches!(
        second.effects(),
        [
            Effect::Encoded { sequence: 2, .. },
            Effect::Icecast {
                disposition: Disposition::Written,
                ..
            }
        ]
    ));
    setup
        .native
        .metadata(&c, "Artist - Two", cancel())
        .await
        .unwrap();
    send(&setup, &c, &frame(1920), &meta(tcp.port)).await;
    setup.native.disconnect_icecast(&c, cancel()).await.unwrap();
    setup.native.shutdown().await.unwrap();
    setup.delivery.await.unwrap();
    let bytes = tcp.bytes.await.unwrap();
    let (header, pages) = parse(&bytes);
    assert!(header.contains("Ice-Title: 演奏者 - One\r\n"));
    let audio = pages
        .iter()
        .filter(|p| {
            !payload(p).starts_with(b"Opus")
                && !payload(p).is_empty()
                && payload(p) != [0xf8, 0xff, 0xfe]
        })
        .count();
    assert_eq!(audio, 2);
    assert_eq!(pages.last().unwrap()[5], 4);
    assert_eq!(setup.native.status()["drainedOwners"], 15);
    assert_eq!(setup.native.status()["joined"], true);
}
#[tokio::test]
async fn unavailable_codec_and_unsupported_sinks_are_explicit_ordered_observations() {
    let _serial = SERIAL.lock().await;
    let absent = std::env::temp_dir().join("freeshow-native-audio-absent-opus-a3710.so");
    let setup = setup(
        unsafe { LibrarySource::trusted_path(&absent) }.unwrap(),
        AudioLimits::default(),
    );
    assert!(matches!(
        setup.native.capability().await.unwrap(),
        Capability::Unavailable(_)
    ));
    let c = setup.library.context().unwrap();
    let sinks = SinkAvailability {
        ndi: vec!["n".into()],
        omt: vec!["o".into()],
        blackmagic: vec!["b".into()],
        output_stream: true,
        webrtc: true,
        rtmp: true,
    };
    let bytes = frame(0);
    let result = setup
        .native
        .pcm(
            &c,
            &bytes,
            bytes.len(),
            &AudioMetadata::default(),
            &sinks,
            cancel(),
        )
        .await
        .unwrap();
    assert_eq!(result.effects().len(), 6);
    assert!(result
        .effects()
        .iter()
        .all(|e| matches!(e, Effect::Unsupported { .. })));
    assert!(matches!(
        result.effects(),
        [
            Effect::Unsupported {
                sink: SinkKind::NdiBroadcast
            },
            Effect::Unsupported {
                sink: SinkKind::OmtBroadcast
            },
            Effect::Unsupported {
                sink: SinkKind::BlackmagicBroadcast
            },
            Effect::Unsupported {
                sink: SinkKind::OutputServer
            },
            Effect::Unsupported {
                sink: SinkKind::WebRtc
            },
            Effect::Unsupported {
                sink: SinkKind::Rtmp
            }
        ]
    ));
    let noop = send(&setup, &c, &[1], &AudioMetadata::default()).await;
    assert!(matches!(
        noop.effects(),
        [
            Effect::SourceNoOp {
                sink: SinkKind::NdiBroadcast
            },
            Effect::SourceNoOp {
                sink: SinkKind::OmtBroadcast
            }
        ]
    ));
    setup.native.shutdown().await.unwrap();
    setup.delivery.await.unwrap();
}
#[tokio::test]
async fn raw_metadata_stale_and_whole_completed_leases_are_bounded() {
    let _serial = SERIAL.lock().await;
    let absent = std::env::temp_dir().join("freeshow-native-audio-absent-opus-a3710.so");
    let setup = setup(
        unsafe { LibrarySource::trusted_path(&absent) }.unwrap(),
        AudioLimits {
            retained: 2,
            queued: 1,
            ..AudioLimits::default()
        },
    );
    let c = setup.library.context().unwrap();
    let m = AudioMetadata::default();
    let s = SinkAvailability::default();
    assert_eq!(
        setup
            .native
            .pcm(&c, &[0], 2, &m, &s, cancel())
            .await
            .unwrap_err()
            .code,
        "audio_raw_length"
    );
    assert_eq!(
        setup
            .native
            .pcm(&c, &vec![0; 32769], 32769, &m, &s, cancel())
            .await
            .unwrap_err()
            .code,
        "audio_input_limit"
    );
    let mut bad = m.clone();
    bad.icecast = Some(json!({"ignored":"x".repeat(8193)}));
    assert_eq!(
        setup
            .native
            .pcm(&c, &[0], 1, &bad, &s, cancel())
            .await
            .unwrap_err()
            .code,
        "audio_input_limit"
    );
    let token = cancel();
    token.cancel();
    assert_eq!(
        setup
            .native
            .pcm(&c, &[0], 1, &m, &s, token)
            .await
            .unwrap_err()
            .code,
        "cancelled"
    );
    let mut forged = c.clone();
    forged.runtime_id.push('x');
    assert_eq!(
        setup
            .native
            .pcm(&forged, &[], 0, &m, &s, cancel())
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    let first = send(&setup, &c, &[0], &m).await;
    let second = send(&setup, &c, &[0], &m).await;
    assert_eq!(setup.native.status()["retained"], 2);
    assert_eq!(
        setup
            .native
            .pcm(&c, &[], 0, &m, &s, cancel())
            .await
            .unwrap_err()
            .code,
        "audio_host_busy"
    );
    drop(first);
    let third = send(&setup, &c, &[], &m).await;
    assert!(third.effects().is_empty());
    setup.native.shutdown().await.unwrap();
    assert_eq!(setup.native.status()["retained"], 2);
    drop((second, third));
    assert_eq!(setup.native.status()["retained"], 0);
    setup.delivery.await.unwrap();
}
#[tokio::test]
async fn queued_and_accepted_context_boundaries_and_caller_drop_preserve_owned_effects() {
    let _serial = SERIAL.lock().await;
    let setup = setup(LibrarySource::system(), AudioLimits::default());
    if !require_codec(&setup).await {
        setup.native.shutdown().await.unwrap();
        return;
    }
    let c = setup.library.context().unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    setup.native.test_gate(false, entered, gate);
    let host = setup.native.clone();
    let old = c.clone();
    let held = tokio::spawn(async move {
        let bytes = frame(0);
        host.pcm(
            &old,
            &bytes,
            bytes.len(),
            &meta(9),
            &SinkAvailability::default(),
            cancel(),
        )
        .await
    });
    ready.await.unwrap();
    let current = switch(&setup, &c).await;
    release.send(()).unwrap();
    assert_eq!(held.await.unwrap().unwrap_err().code, "stale_library");
    let tcp = capture().await;
    let (entered, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    setup.native.test_gate(true, entered, gate);
    let host = setup.native.clone();
    let ctx = current.clone();
    let port = tcp.port;
    let held = tokio::spawn(async move {
        let bytes = frame(960);
        host.pcm(
            &ctx,
            &bytes,
            bytes.len(),
            &meta(port),
            &SinkAvailability::default(),
            cancel(),
        )
        .await
    });
    ready.await.unwrap();
    let newest = switch(&setup, &current).await;
    held.abort();
    let _ = held.await;
    release.send(()).unwrap();
    wait_for(|| setup.native.status()["staleStatuses"].as_u64().unwrap() >= 2).await;
    let new_peer = capture().await;
    let next = send(&setup, &newest, &frame(1920), &meta(new_peer.port)).await;
    assert!(matches!(
        next.effects()[0],
        Effect::Encoded { sequence: 2, .. }
    ));
    // The accepted old callback still encoded/connected after caller drop, but
    // new-context audio now closes that old socket and makes a genuine new run.
    connected(&setup, &newest).await;
    send(&setup, &newest, &frame(2880), &meta(new_peer.port)).await;
    setup.native.shutdown().await.unwrap();
    setup.delivery.await.unwrap();
    let (_, pages) = parse(&tcp.bytes.await.unwrap());
    assert_eq!(pages.last().unwrap()[5], 4);
    let (_, pages) = parse(&new_peer.bytes.await.unwrap());
    assert!(pages
        .iter()
        .any(|p| !payload(p).starts_with(b"Opus") && !payload(p).is_empty()));
}

#[tokio::test]
async fn send_failure_consumes_failed_frame_and_keeps_later_accumulator_frames() {
    let _serial = SERIAL.lock().await;
    let setup = setup(LibrarySource::system(), AudioLimits::default());
    if !require_codec(&setup).await {
        setup.native.shutdown().await.unwrap();
        return;
    }
    let c = setup.library.context().unwrap();
    let mut bad = meta(9);
    bad.target_id = JsInput::Undefined;
    bad.icecast.as_mut().unwrap()["mount"] = json!(true);
    let sinks = SinkAvailability {
        output_stream: true,
        ..SinkAvailability::default()
    };
    let raw = vec![0; 960 * 3 * 2 * 4];
    let failed = setup
        .native
        .pcm(&c, &raw, raw.len(), &bad, &sinks, cancel())
        .await
        .unwrap();
    assert!(matches!(failed.effects(),[
        Effect::SourceNoOp { sink:SinkKind::NdiBroadcast },
        Effect::SourceNoOp { sink:SinkKind::OmtBroadcast },
        Effect::Encoded { sequence:1,.. },
        Effect::Failure { index:0, stage:freeshow_audio_pipeline::FailureStage::Send, error },
        Effect::SourceNoOp { sink:SinkKind::BlackmagicBroadcast },
        Effect::Unsupported { sink:SinkKind::OutputServer }
    ] if error.code=="unsupported_config"));
    assert!(!failed.diagnostics().is_empty());
    let mut disabled = meta(9);
    disabled.icecast.as_mut().unwrap()["enabled"] = json!(0);
    let next = send(&setup, &c, &frame(960), &disabled).await;
    let sequences = next
        .effects()
        .iter()
        .filter_map(|e| match e {
            Effect::Encoded { sequence, .. } => Some(*sequence),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(sequences, [2, 3, 4]);
    assert!(next
        .effects()
        .iter()
        .filter_map(|e| match e {
            Effect::Icecast { disposition, .. } => Some(disposition),
            _ => None,
        })
        .all(|d| *d == Disposition::Disabled));
    setup.native.shutdown().await.unwrap();
    setup.delivery.await.unwrap();
}
#[tokio::test]
async fn actual_codec_post_call_deadline_is_sticky_and_all_other_owners_drain() {
    let _serial = SERIAL.lock().await;
    let mut limits = AudioLimits::default();
    limits.opus.call_budget = Duration::from_nanos(1);
    let setup = setup(LibrarySource::system(), limits);
    if !require_codec(&setup).await {
        setup.native.shutdown().await.unwrap();
        return;
    }
    let c = setup.library.context().unwrap();
    let receipt = send(&setup, &c, &frame(0), &meta(9)).await;
    assert!(
        matches!(receipt.effects(),[Effect::Failure{index:0,stage:freeshow_audio_pipeline::FailureStage::Encode,error}] if error.code=="codec_deadline")
    );
    let first = setup.native.shutdown().await.unwrap_err();
    assert_eq!(first.code, "codec_deadline");
    assert_eq!(setup.native.shutdown().await.unwrap_err().code, first.code);
    assert_eq!(setup.native.status()["drainedOwners"], 15);
    assert_eq!(setup.native.status()["joined"], true);
    assert_eq!(
        setup.native.capability().await.unwrap_err().code,
        "codec_deadline"
    );
    setup.delivery.await.unwrap();
}
#[tokio::test]
async fn real_tcp_rejection_is_status_not_delivery_and_cooldown_still_encodes() {
    let _serial = SERIAL.lock().await;
    let setup = setup(LibrarySource::system(), AudioLimits::default());
    if !require_codec(&setup).await {
        setup.native.shutdown().await.unwrap();
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 8192];
        assert!(socket.read(&mut bytes).await.unwrap() > 0);
        socket
            .write_all(b"HTTP/1.1 401 Unauthorized\r\n\r\n")
            .await
            .unwrap();
        let mut rest = Vec::new();
        let _ = socket.read_to_end(&mut rest).await;
    });
    let c = setup.library.context().unwrap();
    send(&setup, &c, &frame(0), &meta(port)).await;
    wait_for(|| {
        setup
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.message.data.as_ref().unwrap()["error"] == "HTTP/1.1 401 Unauthorized")
    })
    .await;
    let next = send(&setup, &c, &frame(960), &meta(port)).await;
    assert!(matches!(
        next.effects(),
        [
            Effect::Encoded { sequence: 2, .. },
            Effect::Icecast {
                disposition: Disposition::CooldownDropped,
                ..
            }
        ]
    ));
    setup.native.shutdown().await.unwrap();
    setup.delivery.await.unwrap();
    peer.await.unwrap();
    assert_eq!(setup.native.status()["drainedOwners"], 15);
}
#[tokio::test]
async fn blocked_real_status_sink_retains_thread_after_timeout_and_private_ack_is_not_enough() {
    let _serial = SERIAL.lock().await;
    let (_temp, library) = setup_library();
    let c = library.context().unwrap();
    let (send_status, mut receive) = mpsc::channel::<AudioStatusDelivery>(1);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let held = gate.clone();
    let native = NativeAudioPipeline::start(
        library.clone(),
        LibrarySource::system(),
        AudioLimits {
            shutdown_timeout: Duration::from_millis(100),
            delivery_timeout: Duration::from_millis(60),
            ..AudioLimits::default()
        },
        move |e| {
            send_status
                .blocking_send(e)
                .map_err(|_| "test closed".to_owned())?;
            let (lock, changed) = &*held;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = changed.wait(released).unwrap();
            }
            Ok(())
        },
    )
    .unwrap();
    if !native.capability().await.unwrap().is_available() {
        assert!(std::env::var_os("FREESHOW_REQUIRE_OPUS").is_none());
        native.shutdown().await.unwrap();
        return;
    }
    let tcp = capture().await;
    let raw = frame(0);
    native
        .pcm(
            &c,
            &raw,
            raw.len(),
            &meta(tcp.port),
            &SinkAvailability::default(),
            cancel(),
        )
        .await
        .unwrap();
    let e = receive.recv().await.unwrap();
    assert_eq!(
        native
            .complete_status("output", &c, e.id(), true)
            .unwrap_err()
            .code,
        "audio_status_completion"
    );
    assert_eq!(
        native
            .complete_status("main", &c, "foreign", true)
            .unwrap_err()
            .code,
        "audio_status_completion"
    );
    let mut foreign = c.clone();
    foreign.library_generation += 1;
    assert_eq!(
        native
            .complete_status("main", &foreign, e.id(), true)
            .unwrap_err()
            .code,
        "audio_status_completion"
    );
    native.complete_status("main", &c, e.id(), true).unwrap();
    assert_eq!(
        native
            .complete_status("main", &c, e.id(), true)
            .unwrap_err()
            .code,
        "audio_status_completion"
    );
    assert_eq!(
        native.shutdown().await.unwrap_err().code,
        "audio_shutdown_timeout"
    );
    assert_eq!(native.status()["drainedOwners"], 7);
    assert_eq!(native.status()["joined"], false);
    assert!(
        matches!(NativeAudioPipeline::start(library,LibrarySource::system(),AudioLimits::default(),|_|Ok(())),Err(e) if e.code=="audio_owner_exists")
    );
    {
        let (lock, changed) = &*gate;
        *lock.lock().unwrap() = true;
        changed.notify_all();
    }
    assert_eq!(
        native.shutdown().await.unwrap_err().code,
        "audio_status_timeout"
    );
    assert_eq!(native.status()["drainedOwners"], 15);
    assert_eq!(native.status()["joined"], true);
    drop(e);
    let _ = tcp.bytes.await.unwrap();
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}
fn original_scenario(
    dir: &std::path::Path,
    frames: &[Vec<u8>],
    failure: Option<usize>,
) -> serde_json::Value {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let source = std::path::Path::new(file!());
    let source = if source.is_absolute() {
        source.to_path_buf()
    } else {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(source)
    };
    let helper = source
        .parent()
        .unwrap()
        .join("support/audio_pipeline_oracle.mjs");
    let mut child = Command::new(
        std::env::var_os("FREESHOW_NATIVE_AUDIO_NODE").unwrap_or_else(|| "node".into()),
    )
    .arg(helper)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    let request = json!({"addon":dir.join("bundled/opus.node"),"frames":frames.iter().map(|b|hex(b)).collect::<Vec<_>>(),"failEncode":failure});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() <= 2 * 1024 * 1024);
    serde_json::from_slice(&output.stdout).unwrap()
}
#[tokio::test]
async fn immutable_source_actual_original_addon_and_native_host_have_same_packets_and_raw_pages() {
    let _serial = SERIAL.lock().await;
    let Some(dir) = std::env::var_os("FREESHOW_OPUS_ORACLE_DIR").map(std::path::PathBuf::from)
    else {
        eprintln!("ACTUAL ORIGINAL ADDON E2E UNAVAILABLE: FREESHOW_OPUS_ORACLE_DIR absent; no same-version source/native packet claim");
        assert!(std::env::var_os("FREESHOW_REQUIRE_NATIVE_AUDIO_ORACLE").is_none());
        return;
    };
    let frames = vec![frame(0), frame(960), frame(1920)];
    let original = original_scenario(&dir, &frames, None);
    assert_eq!(
        original["revision"],
        "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
    );
    assert_eq!(
        original["sourceHashes"]["src/electron/audio/processAudio.ts"],
        "51fa05f893c126856ce60f23cda79c1784bfdfceb818ca184ed82da6426d9198"
    );
    assert_eq!(
        original["sourceHashes"]["src/electron/audio/IcecastSender.ts"],
        "0492c69db5c5ace4637e1a469e3e14a1cc3b48189f7ab78dc8abc64b3e274bc9"
    );
    assert!(original["errors"].as_array().unwrap().is_empty());
    assert_eq!(original["encoded"].as_array().unwrap().len(), 3);
    assert_eq!(original["pending"], "");
    let setup = setup(
        unsafe { LibrarySource::trusted_path(&dir.join("libopus.so.0")) }.unwrap(),
        AudioLimits::default(),
    );
    let cap = setup.native.capability().await.unwrap();
    assert!(matches!(cap,Capability::Available(ref cap) if cap.version=="libopus 1.3.1"));
    let tcp = capture().await;
    let c = setup.library.context().unwrap();
    setup
        .native
        .metadata(&c, "演奏者 - One", cancel())
        .await
        .unwrap();
    let first = send(&setup, &c, &frames[0], &meta(tcp.port)).await;
    assert!(matches!(
        first.effects()[1],
        Effect::Icecast {
            disposition: Disposition::ConnectingDropped,
            ..
        }
    ));
    wait_for(|| {
        setup
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.message.data.as_ref().unwrap()["connected"] == true)
    })
    .await;
    send(&setup, &c, &frames[1], &meta(tcp.port)).await;
    setup
        .native
        .metadata(&c, "Artist - Two", cancel())
        .await
        .unwrap();
    send(&setup, &c, &frames[2], &meta(tcp.port)).await;
    setup.native.disconnect_icecast(&c, cancel()).await.unwrap();
    setup.native.shutdown().await.unwrap();
    setup.delivery.await.unwrap();
    let raw = tcp.bytes.await.unwrap();
    let (header, pages) = parse(&raw);
    let (old_header, old_pages) = parse(&unhex(original["raw"].as_str().unwrap()));
    assert_eq!(
        header.replace(&format!(":{}\r\n", tcp.port), ":PORT\r\n"),
        old_header.replace(&format!(":{}\r\n", original["port"]), ":PORT\r\n")
    );
    // Timers are real in both runs. Compare real encoded packets and every header,
    // retaining raw-CRC checks above; independent idle insertion changes granules.
    let real = |pages: &[Vec<u8>]| {
        pages
            .iter()
            .filter(|p| {
                !payload(p).starts_with(b"Opus")
                    && !payload(p).is_empty()
                    && payload(p) != [0xf8, 0xff, 0xfe]
            })
            .map(|p| hex(payload(p)))
            .collect::<Vec<_>>()
    };
    let expected = original["encoded"].as_array().unwrap()[1..]
        .iter()
        .map(|p| p.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(real(&pages), expected);
    assert_eq!(real(&old_pages), expected);
    let headers = |pages: &[Vec<u8>]| {
        pages
            .iter()
            .filter(|p| payload(p).starts_with(b"Opus"))
            .map(|p| (p[5], hex(payload(p))))
            .collect::<Vec<_>>()
    };
    assert_eq!(headers(&pages), headers(&old_pages));
    assert_eq!(headers(&pages).len(), 4);
    assert_eq!(pages.last().unwrap()[5], 4);
    assert_eq!(old_pages.last().unwrap()[5], 4);
    assert_eq!(
        setup
            .events
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.message.data.clone().unwrap())
            .collect::<Vec<_>>(),
        original["statuses"].as_array().unwrap().clone()
    );
    eprintln!("actual source/native E2E: {}", original["sourceHashes"]);
    // A controlled source exception after an actual N-API encode consumes only
    // the failed frame; remaining accumulator frames survive for the next call.
    let failure = original_scenario(&dir, &[vec![0; 960 * 3 * 2 * 4], frame(960)], Some(1));
    assert_eq!(failure["encoded"].as_array().unwrap().len(), 4);
    assert_eq!(failure["pending"], "");
    assert_eq!(failure["errors"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn admission_cancellation_and_terminal_drain_are_controlled_without_any_codec_library() {
    let _serial = SERIAL.lock().await;
    let absent = std::env::temp_dir().join("freeshow-native-audio-absent-opus-a3710.so");
    let setup = setup(
        unsafe { LibrarySource::trusted_path(&absent) }.unwrap(),
        AudioLimits {
            queued: 1,
            retained: 3,
            ..AudioLimits::default()
        },
    );
    assert!(!setup.native.capability().await.unwrap().is_available());
    let c = setup.library.context().unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    setup.native.test_gate(false, entered, gate);
    let token = cancel();
    let token2 = token.clone();
    let host = setup.native.clone();
    let context = c.clone();
    let first = tokio::spawn(async move {
        host.pcm(
            &context,
            &[0],
            1,
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token2,
        )
        .await
    });
    ready.await.unwrap();
    let queued_cancel = cancel();
    let queued_token = queued_cancel.clone();
    let host = setup.native.clone();
    let context = c.clone();
    let queued = tokio::spawn(async move {
        host.pcm(
            &context,
            &[0],
            1,
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            queued_token,
        )
        .await
    });
    wait_for(|| setup.native.status()["retained"] == 2).await;
    assert_eq!(
        setup
            .native
            .pcm(
                &c,
                &[0],
                1,
                &AudioMetadata::default(),
                &SinkAvailability::default(),
                cancel()
            )
            .await
            .unwrap_err()
            .code,
        "audio_host_busy"
    );
    token.cancel();
    queued_cancel.cancel();
    release.send(()).unwrap();
    assert_eq!(first.await.unwrap().unwrap_err().code, "cancelled");
    assert_eq!(queued.await.unwrap().unwrap_err().code, "cancelled");
    let (entered, ready) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    setup.native.test_gate(true, entered, gate);
    let token = cancel();
    let accepted_cancel = token.clone();
    let host = setup.native.clone();
    let context = c.clone();
    let accepted = tokio::spawn(async move {
        host.pcm(
            &context,
            &[0],
            1,
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            accepted_cancel,
        )
        .await
    });
    ready.await.unwrap();
    token.cancel();
    let host = setup.native.clone();
    let shutdown = tokio::spawn(async move { host.shutdown().await });
    wait_for(|| setup.native.status()["closing"] == true).await;
    assert!(!shutdown.is_finished());
    assert_eq!(
        setup
            .native
            .pcm(
                &c,
                &[],
                0,
                &AudioMetadata::default(),
                &SinkAvailability::default(),
                cancel()
            )
            .await
            .unwrap_err()
            .code,
        "audio_host_closed"
    );
    release.send(()).unwrap();
    assert_eq!(accepted.await.unwrap().unwrap().effects().len(), 2);
    shutdown.await.unwrap().unwrap();
    assert_eq!(setup.native.status()["drainedOwners"], 15);
    assert_eq!(setup.native.status()["joined"], true);
    setup.native.shutdown().await.unwrap();
    setup.delivery.await.unwrap();
}
#[tokio::test]
async fn actual_peer_reset_and_eos_write_failure_remain_sticky_through_all_owner_drains() {
    let _serial = SERIAL.lock().await;
    let setup = setup(LibrarySource::system(), AudioLimits::default());
    if !require_codec(&setup).await {
        setup.native.shutdown().await.unwrap();
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (reset, trigger) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut b = [0; 8192];
        assert!(socket.read(&mut b).await.unwrap() > 0);
        trigger.await.unwrap();
        let socket = socket.into_std().unwrap();
        socket2::SockRef::from(&socket)
            .set_linger(Some(Duration::ZERO))
            .unwrap();
        drop(socket);
    });
    let c = setup.library.context().unwrap();
    send(&setup, &c, &frame(0), &meta(port)).await;
    wait_for(|| {
        setup
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.message.data.as_ref().unwrap()["connected"] == true)
    })
    .await;
    reset.send(()).unwrap();
    peer.await.unwrap();
    wait_for(|| setup.native.status()["terminal"].is_object()).await;
    let error = setup.native.shutdown().await.unwrap_err();
    assert!(
        matches!(error.code, "teardown_failed" | "teardown_timeout"),
        "{error:?}"
    );
    assert_eq!(setup.native.shutdown().await.unwrap_err().code, error.code);
    assert_eq!(setup.native.status()["drainedOwners"], 15);
    assert_eq!(setup.native.status()["joined"], true);
    setup.delivery.await.unwrap();
}

#[tokio::test]
async fn global_partial_resampling_accumulation_and_codec_survive_targets_disconnect_and_library_change(
) {
    let _serial = SERIAL.lock().await;
    let setup = setup(LibrarySource::system(), AudioLimits::default());
    if !require_codec(&setup).await {
        setup.native.shutdown().await.unwrap();
        return;
    }
    let c = setup.library.context().unwrap();
    let mut m = meta(9);
    m.sample_rate = JsInput::Number(24000.0);
    m.icecast.as_mut().unwrap()["enabled"] = json!(0);
    let first = send(&setup, &c, &vec![0; 240 * 2 * 4], &m).await;
    assert!(first.effects().is_empty());
    let mut other = m.clone();
    other.target_id = JsInput::Json(json!("unregistered-ndi"));
    assert!(send(&setup, &c, &frame(0), &other)
        .await
        .effects()
        .is_empty());
    setup.native.disconnect_icecast(&c, cancel()).await.unwrap();
    setup
        .native
        .metadata(&c, "Still same global encoder", cancel())
        .await
        .unwrap();
    let new = switch(&setup, &c).await;
    let second = send(&setup, &new, &vec![0; 240 * 2 * 4], &m).await;
    assert!(matches!(
        second.effects(),
        [
            Effect::Encoded { sequence: 1, .. },
            Effect::Icecast {
                disposition: Disposition::Disabled,
                ..
            }
        ]
    ));
    let mut disabled = m.clone();
    disabled.icecast.as_mut().unwrap()["enabled"] = json!(false);
    assert!(send(&setup, &new, &frame(0), &disabled)
        .await
        .effects()
        .is_empty());
    m.sample_rate = JsInput::Number(48000.0);
    let third = send(&setup, &new, &frame(960), &m).await;
    assert!(matches!(
        third.effects(),
        [
            Effect::Encoded { sequence: 2, .. },
            Effect::Icecast {
                disposition: Disposition::Disabled,
                ..
            }
        ]
    ));
    setup.native.shutdown().await.unwrap();
    setup.delivery.await.unwrap();
}

#[tokio::test]
async fn current_renderer_rejection_is_terminal_and_never_counted_as_stale_or_delivered() {
    let _serial = SERIAL.lock().await;
    let setup = setup_with_ack(LibrarySource::system(), AudioLimits::default(), false);
    if !require_codec(&setup).await {
        setup.native.shutdown().await.unwrap();
        return;
    }
    let tcp = capture().await;
    let c = setup.library.context().unwrap();
    send(&setup, &c, &frame(0), &meta(tcp.port)).await;
    wait_for(|| setup.native.status()["terminal"].is_object()).await;
    assert_eq!(
        setup.native.shutdown().await.unwrap_err().code,
        "audio_status_rejected"
    );
    assert_eq!(setup.native.status()["staleStatuses"], 0);
    assert_eq!(setup.native.status()["deliveredStatuses"], 0);
    assert_eq!(setup.native.status()["drainedOwners"], 15);
    setup.delivery.await.unwrap();
    tcp.bytes.abort();
    let _ = tcp.bytes.await;
}
#[tokio::test]
async fn no_runtime_shutdown_closes_admission_and_later_retry_joins_real_owners() {
    let _serial = SERIAL.lock().await;
    let setup = setup(
        unsafe {
            LibrarySource::trusted_path(
                &std::env::temp_dir().join("freeshow-native-audio-absent-opus-a3710.so"),
            )
        }
        .unwrap(),
        AudioLimits::default(),
    );
    let host = setup.native.clone();
    let error = std::thread::spawn(move || {
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        let mut future = std::pin::pin!(host.shutdown());
        let mut cx = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(Err(e)) => e,
            _ => panic!("no-runtime shutdown must return its explicit drain obligation"),
        }
    })
    .join()
    .unwrap();
    assert_eq!(error.code, "audio_runtime_required");
    assert_eq!(setup.native.status()["closing"], true);
    setup.native.shutdown().await.unwrap();
    assert_eq!(setup.native.status()["joined"], true);
    setup.delivery.await.unwrap();
}
