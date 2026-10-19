#[allow(dead_code)]
#[path = "../src/audio_ingress.rs"]
mod audio_ingress;
#[allow(dead_code)]
#[path = "../src/audio_pipeline.rs"]
mod audio_pipeline;
use audio_ingress::*;
use audio_pipeline::*;
use freeshow_audio_pipeline::{AudioMetadata, JsInput};
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_opus::{Capability, LibrarySource};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    net::TcpListener,
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Setup {
    _temp: tempfile::TempDir,
    library: LibraryService,
    native: NativeAudioPipeline,
    ingress: AudioIngress,
    deliveries: tokio::task::JoinHandle<()>,
    events: Arc<Mutex<Vec<Value>>>,
}
fn setup(real: bool, timeout: Duration) -> Setup {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    let (send, mut receive) = mpsc::channel::<AudioStatusDelivery>(1);
    let source = if real {
        LibrarySource::system()
    } else {
        unsafe { LibrarySource::trusted_path(&temp.path().join("explicitly-absent-opus")) }.unwrap()
    };
    let native =
        NativeAudioPipeline::start(library.clone(), source, AudioLimits::default(), move |e| {
            send.blocking_send(e)
                .map_err(|_| "test receiver closed".into())
        })
        .unwrap();
    let saved = Arc::new(Mutex::new(vec![]));
    let events = saved.clone();
    let host = native.clone();
    let deliveries = tokio::spawn(async move {
        while let Some(event) = receive.recv().await {
            let context = event.event().context.as_ref().unwrap();
            events
                .lock()
                .unwrap()
                .push(event.event().message.data.clone().unwrap());
            // Private completion is deliberately independent of ingress FIFO.
            let _ = host.complete_status("main", context, event.id(), true);
        }
    });
    let ingress = AudioIngress::with_timeout(library.clone(), native.clone(), timeout).unwrap();
    Setup {
        _temp: temp,
        library,
        native,
        ingress,
        deliveries,
        events: saved,
    }
}
fn token() -> CancellationToken {
    CancellationToken::new()
}
fn submit(setup: &Setup, context: &RuntimeContext) -> Ticket {
    setup
        .ingress
        .submit(
            context,
            &[0],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token(),
        )
        .unwrap()
}
async fn wait(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
async fn done(setup: Setup) {
    let a = setup.ingress.shutdown().await;
    let b = setup.native.shutdown().await;
    a.unwrap();
    b.unwrap();
    setup.deliveries.await.unwrap();
    setup.library.finish_writes().await.unwrap();
}
async fn switch(setup: &Setup, old: &RuntimeContext) -> RuntimeContext {
    let path = setup
        ._temp
        .path()
        .join(format!("next-{}", old.library_generation));
    std::fs::create_dir_all(&path).unwrap();
    let request:Request=serde_json::from_value(json!({"channel":"MAIN","context":old,"data":{"channel":"UPDATE_DATA_PATH","data":{"newPath":path,"oldPath":old.data_path}}})).unwrap();
    setup.library.dispatch(request, "main").await.unwrap();
    // Coordinator contract: revoke old native status first, never reset DSP.
    setup.native.context_changed().unwrap();
    setup.library.context().unwrap()
}
fn gate(setup: &Setup, accepted: bool) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
    let (entered, ready) = oneshot::channel();
    let (release, held) = oneshot::channel();
    setup.native.test_gate(accepted, entered, held);
    (ready, release)
}
fn code<T>(result: std::result::Result<T, IngressError>) -> &'static str {
    match result {
        Ok(_) => panic!("expected explicit error"),
        Err(e) => e.code,
    }
}
#[tokio::test]
async fn unused_library_retirement_and_late_cleanup_do_not_close_the_current_audio_group() {
    let _serial = SERIAL.lock().await;
    let setup = setup(false, Duration::from_secs(2));
    let old = setup.library.context().unwrap();
    let current = switch(&setup, &old).await;
    setup.ingress.context_changed().await.unwrap();
    setup.ingress.close_context(&old).await.unwrap();
    submit(&setup, &current)
        .finish()
        .await
        .unwrap()
        .into_void()
        .unwrap();
    assert_eq!(setup.ingress.status().accepted_packets, 1);
    assert_eq!(setup.ingress.status().completed_packets, 1);
    setup.ingress.context_changed().await.unwrap();
    setup.ingress.close_context(&old).await.unwrap();
    let mut foreign = current.clone();
    foreign.data_path = setup._temp.path().join("not-the-current-library");
    assert_eq!(
        code(setup.ingress.close_context(&foreign).await),
        "stale_library"
    );
    submit(&setup, &current)
        .finish()
        .await
        .unwrap()
        .into_void()
        .unwrap();
    let next = switch(&setup, &current).await;
    setup.ingress.context_changed().await.unwrap();
    submit(&setup, &next)
        .finish()
        .await
        .unwrap()
        .into_void()
        .unwrap();
    setup.ingress.close_context(&old).await.unwrap();
    assert!(!setup.ingress.status().group_closed);
    assert_eq!(setup.ingress.status().completed_packets, 3);
    done(setup).await;
}
#[tokio::test]
async fn whole_raw_job_and_ready_observation_quota_and_actual_disposal_are_four() {
    let _serial = SERIAL.lock().await;
    let setup = setup(false, Duration::from_secs(2));
    assert_eq!(
        code(AudioIngress::new(
            setup.library.clone(),
            setup.native.clone()
        )),
        "audio_ingress_owner_exists"
    );
    assert!(!setup.native.capability().await.unwrap().is_available());
    let ctx = setup.library.context().unwrap();
    let tickets = (0..CAPACITY)
        .map(|_| submit(&setup, &ctx))
        .collect::<Vec<_>>();
    assert_eq!(
        code(setup.ingress.submit(
            &ctx,
            &[0],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token()
        )),
        "audio_ingress_busy"
    );
    let mut observations = vec![];
    for t in tickets {
        observations.push(t.finish().await.unwrap());
    }
    assert_eq!(
        observations
            .iter()
            .map(|o| o.inspect(|r| r.sequence()).unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert_eq!(setup.ingress.status().retained, 4);
    assert_eq!(setup.ingress.status().group_outstanding, 4);
    assert_eq!(setup.native.status()["retained"], 4);
    assert_eq!(
        code(setup.ingress.submit(
            &ctx,
            &[0],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token()
        )),
        "audio_ingress_busy"
    );
    observations.pop().unwrap().into_void().unwrap();
    assert_eq!(setup.ingress.status().retained, 3);
    submit(&setup, &ctx)
        .finish()
        .await
        .unwrap()
        .into_void()
        .unwrap();
    setup.ingress.close_context(&ctx).await.unwrap();
    assert_eq!(setup.ingress.status().retained, 0);
    assert_eq!(setup.ingress.status().group_outstanding, 0);
    assert_eq!(setup.native.status()["retained"], 0);
    for o in observations {
        assert_eq!(code(o.into_void()), "audio_ingress_cancelled");
    }
    assert_eq!(
        code(setup.ingress.submit(
            &ctx,
            &[],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token()
        )),
        "audio_ingress_context_closed"
    );
    done(setup).await;
}
#[tokio::test]
async fn close_cancels_at_call_before_poll_and_ready_results_do_not_wait_on_blocked_fifo() {
    let _serial = SERIAL.lock().await;
    let setup = setup(false, Duration::from_millis(30));
    setup.native.capability().await.unwrap();
    let ctx = setup.library.context().unwrap();
    let ready = submit(&setup, &ctx).finish().await.unwrap();
    let (entered, release) = gate(&setup, true);
    let accepted = submit(&setup, &ctx);
    entered.await.unwrap();
    let queued = submit(&setup, &ctx);
    let drain = setup.ingress.close_context(&ctx);
    assert!(setup.ingress.status().group_closed);
    assert_eq!(setup.ingress.status().queued, 0);
    assert_eq!(setup.ingress.status().group_outstanding, 1);
    assert_eq!(setup.native.status()["retained"], 1);
    assert_eq!(code(ready.into_void()), "audio_ingress_cancelled");
    assert_eq!(
        code(queued.finish().await.unwrap().into_void()),
        "audio_ingress_cancelled"
    );
    assert_eq!(
        code(setup.ingress.submit(
            &ctx,
            &[],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token()
        )),
        "audio_ingress_context_closed"
    );
    drop(drain); // Same group/active native future remains owned without a waiter.
    assert_eq!(
        code(setup.ingress.close_context(&ctx).await),
        "audio_ingress_drain_timeout"
    );
    assert!(setup.ingress.status().active);
    release.send(()).unwrap();
    setup.ingress.close_context(&ctx).await.unwrap();
    assert_eq!(
        code(accepted.finish().await.unwrap().into_void()),
        "audio_ingress_cancelled"
    );
    assert_eq!(setup.ingress.status().retained, 0);
    done(setup).await;
}
#[tokio::test]
async fn accepted_caller_drop_keeps_native_future_and_queued_drop_never_calls_source() {
    let _serial = SERIAL.lock().await;
    let setup = setup(false, Duration::from_secs(2));
    setup.native.capability().await.unwrap();
    let ctx = setup.library.context().unwrap();
    let (entered, release) = gate(&setup, true);
    let accepted = submit(&setup, &ctx);
    entered.await.unwrap();
    drop(accepted);
    let queued = submit(&setup, &ctx);
    drop(queued);
    let cancelled = submit(&setup, &ctx);
    cancelled.cancel();
    assert_eq!(
        code(cancelled.finish().await.unwrap().into_void()),
        "audio_ingress_cancelled"
    );
    assert_eq!(setup.ingress.status().retained, 1);
    assert_eq!(setup.ingress.status().group_outstanding, 1);
    assert_eq!(setup.native.status()["retained"], 1);
    release.send(()).unwrap();
    wait(|| setup.ingress.status().retained == 0).await;
    let next = submit(&setup, &ctx).finish().await.unwrap();
    assert_eq!(
        next.inspect(|r| r.sequence()).unwrap(),
        2,
        "queued/cancelled packets never entered source host"
    );
    next.into_void().unwrap();
    done(setup).await;
}
#[tokio::test]
async fn context_switch_waits_real_guard_then_new_group_and_stale_receipts_are_rejected() {
    let _serial = SERIAL.lock().await;
    let setup = setup(false, Duration::from_millis(30));
    setup.native.capability().await.unwrap();
    let old = setup.library.context().unwrap();
    let (entered, release) = gate(&setup, true);
    let first = submit(&setup, &old);
    entered.await.unwrap();
    let queued = submit(&setup, &old);
    let new = switch(&setup, &old).await;
    assert_eq!(
        code(setup.ingress.submit(
            &new,
            &[],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token()
        )),
        "audio_ingress_context_drain"
    );
    let drain = setup.ingress.close_context(&old);
    assert_eq!(
        code(queued.finish().await.unwrap().into_void()),
        "stale_library"
    );
    assert_eq!(
        code(setup.ingress.submit(
            &new,
            &[],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token()
        )),
        "audio_ingress_context_drain"
    );
    assert_eq!(code(drain.await), "audio_ingress_drain_timeout");
    release.send(()).unwrap();
    setup.ingress.close_context(&old).await.unwrap();
    assert_eq!(
        code(first.finish().await.unwrap().into_void()),
        "stale_library"
    );
    submit(&setup, &new)
        .finish()
        .await
        .unwrap()
        .into_void()
        .unwrap();
    assert_eq!(
        code(setup.ingress.submit(
            &old,
            &[],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token()
        )),
        "stale_library"
    );
    setup.ingress.close_context(&old).await.unwrap(); // Bounded previous-group retry.
    assert!(!setup.ingress.status().group_closed);
    done(setup).await;
}
#[tokio::test]
async fn explicit_cancel_preaccept_and_full_context_metadata_sink_bounds_are_real() {
    let _serial = SERIAL.lock().await;
    let setup = setup(false, Duration::from_secs(2));
    setup.native.capability().await.unwrap();
    let ctx = setup.library.context().unwrap();
    let mut wrong = ctx.clone();
    wrong.runtime_id.push('x');
    assert_eq!(
        code(setup.ingress.submit(
            &wrong,
            &[],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token()
        )),
        "stale_library"
    );
    let mut wrong = ctx.clone();
    wrong.library_generation += 1;
    assert_eq!(
        code(setup.ingress.submit(
            &wrong,
            &[],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token()
        )),
        "stale_library"
    );
    let mut wrong = ctx.clone();
    wrong.data_path = setup._temp.path().join("other");
    assert_eq!(
        code(setup.ingress.submit(
            &wrong,
            &[],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token()
        )),
        "stale_library"
    );
    assert_eq!(
        code(setup.ingress.submit(
            &ctx,
            &vec![0; MAX_PCM_BYTES + 1],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token()
        )),
        "audio_ingress_limit"
    );
    let huge = AudioMetadata {
        icecast: Some(json!({"s":"文".repeat(3000)})),
        ..AudioMetadata::default()
    };
    assert_eq!(
        code(
            setup
                .ingress
                .submit(&ctx, &[], &huge, &SinkAvailability::default(), token())
        ),
        "audio_ingress_limit"
    );
    let mut deep = json!(null);
    for _ in 0..10 {
        deep = json!([deep]);
    }
    let huge = AudioMetadata {
        icecast: Some(deep),
        ..AudioMetadata::default()
    };
    assert_eq!(
        code(
            setup
                .ingress
                .submit(&ctx, &[], &huge, &SinkAvailability::default(), token())
        ),
        "audio_ingress_limit"
    );
    let sinks = SinkAvailability {
        ndi: vec!["x".into(); 65],
        ..SinkAvailability::default()
    };
    assert_eq!(
        code(
            setup
                .ingress
                .submit(&ctx, &[], &AudioMetadata::default(), &sinks, token())
        ),
        "audio_ingress_limit"
    );
    assert_eq!(setup.ingress.status().retained, 0);
    let (entered, release) = gate(&setup, false);
    let cancel = token();
    let ticket = setup
        .ingress
        .submit(
            &ctx,
            &[0],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            cancel.clone(),
        )
        .unwrap();
    entered.await.unwrap();
    cancel.cancel();
    release.send(()).unwrap();
    assert_eq!(
        code(ticket.finish().await.unwrap().into_void()),
        "cancelled"
    );
    assert_eq!(setup.ingress.status().retained, 0);
    done(setup).await;
}
#[tokio::test]
async fn terminal_shutdown_is_synchronous_bounded_retry_and_joins_actual_fixed_pump() {
    let _serial = SERIAL.lock().await;
    let setup = setup(false, Duration::from_millis(30));
    setup.native.capability().await.unwrap();
    let ctx = setup.library.context().unwrap();
    let (entered, release) = gate(&setup, true);
    let held = submit(&setup, &ctx);
    entered.await.unwrap();
    let queued = submit(&setup, &ctx);
    let shutdown = setup.ingress.shutdown();
    assert!(setup.ingress.status().closed);
    assert_eq!(
        code(setup.ingress.submit(
            &ctx,
            &[],
            &AudioMetadata::default(),
            &SinkAvailability::default(),
            token()
        )),
        "audio_ingress_closed"
    );
    assert_eq!(
        code(queued.finish().await.unwrap().into_void()),
        "audio_ingress_cancelled"
    );
    assert_eq!(code(shutdown.await), "audio_ingress_drain_timeout");
    assert!(!setup.ingress.status().pump_joined);
    assert_eq!(setup.ingress.status().group_outstanding, 1);
    release.send(()).unwrap();
    setup.ingress.shutdown().await.unwrap();
    assert!(setup.ingress.status().pump_joined);
    assert!(
        !setup.native.status()["joined"].as_bool().unwrap(),
        "ingress never claims to close actual native pipeline"
    );
    assert_eq!(
        code(held.finish().await.unwrap().into_void()),
        "audio_ingress_cancelled"
    );
    setup.ingress.shutdown().await.unwrap();
    done(setup).await;
}
#[tokio::test]
async fn actual_source_host_receipts_stay_ordered_and_ordinary_disconnect_keeps_group_open() {
    let _serial = SERIAL.lock().await;
    let setup = setup(false, Duration::from_secs(2));
    setup.native.capability().await.unwrap();
    let ctx = setup.library.context().unwrap();
    let first = submit(&setup, &ctx);
    let disconnect = setup.ingress.disconnect_icecast(&ctx, token()).unwrap();
    let last = submit(&setup, &ctx);
    let mut seq = vec![];
    for t in [first, disconnect, last] {
        let o = t.finish().await.unwrap();
        seq.push(o.inspect(|r| r.sequence()).unwrap());
        o.into_void().unwrap();
    }
    assert_eq!(seq, vec![1, 2, 3]);
    assert!(!setup.ingress.status().group_closed);
    assert!(!setup.native.status()["closing"].as_bool().unwrap());
    done(setup).await;
}
#[tokio::test]
async fn real_opus_icecast_bytes_and_status_completion_continue_after_ipc_caller_drop() {
    let _serial = SERIAL.lock().await;
    let setup = setup(true, Duration::from_secs(3));
    if let Capability::Unavailable(e) = setup.native.capability().await.unwrap() {
        eprintln!("REAL INGRESS OPUS UNAVAILABLE: {e}; byte delivery not claimed");
        assert!(std::env::var_os("FREESHOW_REQUIRE_OPUS").is_none());
        done(setup).await;
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let wire = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = vec![];
        socket.read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    let ctx = setup.library.context().unwrap();
    let mut meta = AudioMetadata {
        sample_rate: JsInput::Number(48000.0),
        target_id: JsInput::Json(json!("icecast")),
        icecast: Some(
            json!({"enabled":true,"host":"127.0.0.1","port":port,"mount":"audio","password":"秘密"}),
        ),
    };
    let mut bytes = vec![];
    for channel in 0..2 {
        for sample in 0..960 {
            bytes.extend_from_slice(
                &((sample as f32 * 0.03 + channel as f32).sin() * 0.4).to_le_bytes(),
            );
        }
    }
    // Original Icecast connect consumes its triggering packet without writing
    // it. Hold that real call while the next input is still in ingress, proving
    // that its bytes/metadata were snapshotted before native admission.
    let (first_entered, first_release) = gate(&setup, true);
    let first = setup
        .ingress
        .submit(&ctx, &bytes, &meta, &SinkAvailability::default(), token())
        .unwrap();
    first_entered.await.unwrap();
    let (entered, release) = gate(&setup, true);
    let held = setup
        .ingress
        .submit(&ctx, &bytes, &meta, &SinkAvailability::default(), token())
        .unwrap();
    assert_eq!(setup.ingress.status().queued, 1);
    bytes.fill(255);
    meta.target_id = JsInput::Json(json!("mutated-away-from-icecast"));
    meta.icecast = Some(json!({"enabled":false}));
    assert!(matches!(meta.target_id,JsInput::Json(ref v) if v=="mutated-away-from-icecast"));
    first_release.send(()).unwrap();
    first.finish().await.unwrap().into_void().unwrap();
    entered.await.unwrap();
    drop(held);
    assert_eq!(setup.ingress.status().group_outstanding, 1);
    release.send(()).unwrap();
    wait(|| setup.ingress.status().retained == 0).await;
    setup
        .ingress
        .disconnect_icecast(&ctx, token())
        .unwrap()
        .finish()
        .await
        .unwrap()
        .into_void()
        .unwrap();
    wait(|| setup.native.status()["deliveredStatuses"].as_u64().unwrap() >= 2).await;
    assert!(setup
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| e["connected"] == true));
    done(setup).await;
    let bytes = wire.await.unwrap();
    assert!(bytes.windows(8).any(|b| b == b"OpusHead"));
    assert!(bytes.starts_with(b"PUT /audio HTTP/1.1\r\n"));
    let mut offset = bytes.windows(4).position(|b| b == b"\r\n\r\n").unwrap() + 4;
    let mut audio_packets = 0;
    let mut pages = 0;
    while offset < bytes.len() {
        assert_eq!(&bytes[offset..offset + 4], b"OggS");
        let count = bytes[offset + 26] as usize;
        let payload = offset + 27 + count;
        let end = payload
            + bytes[offset + 27..payload]
                .iter()
                .map(|n| *n as usize)
                .sum::<usize>();
        let mut page = bytes[offset..end].to_vec();
        let crc = u32::from_le_bytes(page[22..26].try_into().unwrap());
        page[22..26].fill(0);
        assert_eq!(freeshow_icecast::crc32(&page), crc);
        if payload < end && !bytes[payload..end].starts_with(b"Opus") {
            audio_packets += 1;
        }
        pages += 1;
        offset = end;
    }
    assert_eq!(pages, 4, "head, tags, caller-dropped actual audio, EOS");
    assert_eq!(audio_packets, 1);
}
#[tokio::test]
async fn sticky_pump_failure_never_releases_actual_accepted_guard_until_native_join() {
    let _serial = SERIAL.lock().await;
    let setup = setup(false, Duration::from_millis(30));
    setup.native.capability().await.unwrap();
    let ctx = setup.library.context().unwrap();
    let (entered, release) = gate(&setup, true);
    let held = submit(&setup, &ctx);
    entered.await.unwrap();
    setup.ingress.test_panic_pump();
    wait(|| setup.ingress.status().terminal.is_some()).await;
    assert_eq!(setup.ingress.status().group_outstanding, 1);
    assert_eq!(setup.native.status()["retained"], 1);
    assert_eq!(
        code(setup.ingress.shutdown().await),
        "audio_ingress_drain_timeout"
    );
    let native = setup.native.clone();
    let shutdown = tokio::spawn(async move { native.shutdown().await });
    assert_eq!(setup.ingress.status().group_outstanding, 1);
    release.send(()).unwrap();
    shutdown.await.unwrap().unwrap();
    assert_eq!(code(setup.ingress.shutdown().await), "audio_ingress_pump");
    assert_eq!(setup.ingress.status().group_outstanding, 0);
    assert_eq!(
        code(held.finish().await.unwrap().into_void()),
        "audio_ingress_pump"
    );
    assert_eq!(code(setup.ingress.shutdown().await), "audio_ingress_pump");
    setup.deliveries.await.unwrap();
    setup.library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn lifecycle_waiters_are_bounded_and_cancel_before_busy_rejection() {
    let _serial = SERIAL.lock().await;
    let setup = setup(false, Duration::from_secs(2));
    setup.native.capability().await.unwrap();
    let ctx = setup.library.context().unwrap();
    let (entered, release) = gate(&setup, true);
    let active = submit(&setup, &ctx);
    entered.await.unwrap();
    let drains: Vec<_> = (0..4).map(|_| setup.ingress.close_context(&ctx)).collect();
    assert_eq!(setup.ingress.status().retained_controls, 4);
    assert_eq!(
        code(setup.ingress.close_context(&ctx).await),
        "audio_ingress_control_busy"
    );
    assert_eq!(
        code(setup.ingress.shutdown().await),
        "audio_ingress_control_busy"
    );
    assert!(
        setup.ingress.status().closed,
        "terminal admission closes even when drain waiter capacity is full"
    );
    assert!(setup.ingress.status().group_closed);
    assert_eq!(
        setup.ingress.status().group_outstanding,
        1,
        "entered native work remains actually retained"
    );
    drop(drains);
    assert_eq!(setup.ingress.status().retained_controls, 0);
    assert_eq!(setup.ingress.status().group_outstanding, 1);
    release.send(()).unwrap();
    assert_eq!(
        code(active.finish().await.unwrap().into_void()),
        "audio_ingress_cancelled"
    );
    done(setup).await;
}
