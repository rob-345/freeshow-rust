#[path = "../src/assets.rs"]
#[allow(dead_code)]
mod assets;
#[path = "../src/audio_pipeline.rs"]
#[allow(dead_code)]
mod audio_pipeline;
#[path = "../src/now_playing.rs"]
#[allow(dead_code)]
mod now_playing;
#[path = "../src/now_playing_files.rs"]
#[allow(dead_code)]
mod now_playing_files;
#[path = "../src/now_playing_prepare.rs"]
#[allow(dead_code)]
mod now_playing_prepare;
#[path = "../src/now_playing_schedule.rs"]
#[allow(dead_code)]
mod now_playing_schedule;
use freeshow_audio::{AudioConfig, AudioService};
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_cover_images::CoverImageService;
use freeshow_opus::LibrarySource;
use now_playing::*;
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Fixture {
    temp: tempfile::TempDir,
    library: LibraryService,
    assets: assets::NativeAssets,
    audio: Arc<AudioService>,
    covers: Arc<CoverImageService>,
    pipeline: audio_pipeline::NativeAudioPipeline,
    status_task: tokio::task::JoinHandle<()>,
    connected: Arc<std::sync::atomic::AtomicBool>,
    host: NativeNowPlaying,
    budget: OperationBudget,
    events: mpsc::Receiver<Publication>,
    revealed: Arc<Mutex<Vec<PathBuf>>>,
    ctx: RuntimeContext,
    publish_mode: Arc<std::sync::atomic::AtomicU8>,
}
impl Fixture {
    async fn new(config: Config) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
        )
        .unwrap();
        let ctx = library.context().unwrap();
        let assets = assets::NativeAssets::start(&ctx, &temp.path().join("app"))
            .await
            .unwrap();
        let audio = Arc::new(AudioService::new(AudioConfig::default()).unwrap());
        let covers = Arc::new(CoverImageService::new().unwrap());
        let (status_send, mut status_receive) =
            mpsc::channel::<audio_pipeline::AudioStatusDelivery>(8);
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pipeline = audio_pipeline::NativeAudioPipeline::start(
            library.clone(),
            LibrarySource::system(),
            audio_pipeline::AudioLimits::default(),
            move |event| {
                status_send
                    .try_send(event)
                    .map_err(|_| "test status queue full".into())
            },
        )
        .unwrap();
        // Even an unavailable codec retains a genuine metadata-only Icecast
        // owner. No PCM, packet, server or hardware delivery is asserted here.
        let _ = pipeline.capability().await.unwrap();
        let saved = pipeline.clone();
        let signal = connected.clone();
        let status_task = tokio::spawn(async move {
            while let Some(event) = status_receive.recv().await {
                let data = event.event().message.data.as_ref().unwrap();
                if data["connected"] == true {
                    signal.store(true, std::sync::atomic::Ordering::Release);
                }
                let _ = saved.complete_status(
                    "main",
                    event.event().context.as_ref().unwrap(),
                    event.id(),
                    true,
                );
            }
        });
        let (send, events) = mpsc::channel(4);
        let publish_mode = Arc::new(std::sync::atomic::AtomicU8::new(0));
        let mode = publish_mode.clone();
        let revealed = Arc::new(Mutex::new(vec![]));
        let saved = revealed.clone();
        let budget = OperationBudget::default();
        let host = NativeNowPlaying::start_with_budget(
            Dependencies {
                library: library.clone(),
                assets: assets.clone(),
                audio: audio.clone(),
                covers: covers.clone(),
                pipeline: pipeline.clone(),
            },
            config,
            Callbacks {
                publish: Arc::new(move |event| {
                    match mode.load(std::sync::atomic::Ordering::Acquire) {
                        1 => {
                            return Box::pin(async {
                                Err(HostError::new(
                                    "test_publish",
                                    "Real callback rejected queue acceptance",
                                ))
                            })
                        }
                        2 => panic!("controlled trusted publication panic"),
                        _ => {}
                    }

                    let result = send.try_send(event).map_err(|_| {
                        HostError::new("test_publish", "Bounded renderer queue unavailable")
                    });
                    Box::pin(async move { result })
                }),
                reveal: Arc::new(move |path, _| {
                    saved.lock().unwrap().push(path);
                    Box::pin(async { Ok(()) })
                }),
            },
            budget.clone(),
        )
        .unwrap();
        Self {
            temp,
            library,
            assets,
            audio,
            covers,
            pipeline,
            status_task,
            connected,
            host,
            budget,
            events,
            revealed,
            ctx,
            publish_mode,
        }
    }
    fn submit(&self, name: &str, format: &str) -> Ticket {
        self.host.submit(&self.ctx,Command::Set{payload:json!({"filePath":name,"name":name,"unknownLang":["Artist","Title","Album"],"format":format,"duration":65.9}),duration_number:None},CancellationToken::new()).unwrap()
    }
    async fn source(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.temp.path().join(name);
        fs::write(&path, bytes).unwrap();
        self.assets
            .import_grants(&self.library, &self.ctx, std::slice::from_ref(&path))
            .await
            .unwrap();
        path
    }
    fn text(&self) -> PathBuf {
        self.ctx.data_path.join("Audio/NowPlaying.txt")
    }
    fn cover(&self) -> PathBuf {
        self.ctx.data_path.join("Audio/NowPlayingCover.png")
    }
    async fn event(&mut self) -> Publication {
        tokio::time::timeout(Duration::from_secs(10), self.events.recv())
            .await
            .unwrap()
            .unwrap()
    }
    fn ack(&self, event: &Publication, accepted: bool) {
        self.host
            .complete(event.id(), event.context(), accepted)
            .unwrap();
    }
    async fn close(&self) {
        self.host.shutdown().await.unwrap();
        assert_eq!(self.host.status()["complete"], true);
        self.pipeline.shutdown().await.unwrap();
        wait(|| self.status_task.is_finished()).await;
        self.audio.shutdown().await;
        self.covers.shutdown(Duration::from_secs(3)).await.unwrap();
        self.assets.shutdown().await.unwrap();
        self.library.finish_writes().await.unwrap();
        assert_eq!(self.covers.status().sessions, 0);
        assert_eq!(self.covers.status().finished_workers, 2);
        assert_eq!(self.assets.status()["fileWorkersJoined"], true);
    }
}
async fn wait(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
fn wav() -> Vec<u8> {
    let mut b=b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0data\x02\0\0\0\0\0".to_vec();
    let n = (b.len() - 8) as u32;
    b[4..8].copy_from_slice(&n.to_le_bytes());
    b
}
fn png() -> Vec<u8> {
    "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c63f8cfc0f01f00050001ff89993d1d0000000049454e44ae426082".as_bytes().as_chunks::<2>().0.iter().map(|s|u8::from_str_radix(std::str::from_utf8(s).unwrap(),16).unwrap()).collect()
}
fn safe(n: usize) -> [u8; 4] {
    [
        ((n >> 21) & 127) as u8,
        ((n >> 14) & 127) as u8,
        ((n >> 7) & 127) as u8,
        (n & 127) as u8,
    ]
}
fn mp3() -> Vec<u8> {
    let mut tag = vec![];
    for (key, body) in [
        (
            b"TIT2",
            [vec![3], "Café 音楽 🎵".as_bytes().to_vec()].concat(),
        ),
        (b"TPE1", b"\x03 Artist ".to_vec()),
        (b"TALB", b"\x03Album".to_vec()),
        (b"TYER", b"\x03invalid".to_vec()),
        (
            b"APIC",
            [b"\x03image/png\0\xffundefined\0".to_vec(), png()].concat(),
        ),
    ] {
        tag.extend(key);
        tag.extend(safe(body.len()));
        tag.extend([0, 0]);
        tag.extend(body);
    }
    let mut out = b"ID3\x04\0\0".to_vec();
    out.extend(safe(tag.len()));
    out.extend(tag);
    let mut frame = vec![0; 417];
    frame[..4].copy_from_slice(&[255, 251, 144, 100]);
    for _ in 0..8 {
        out.extend(&frame);
    }
    out
}
#[tokio::test]
async fn real_wav_publication_ack_does_not_gate_title_or_files_but_retains_result() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(Config::default()).await;
    let source = f.source("Ω.wav", &wav()).await;
    let ticket = f.submit(
        source.to_str().unwrap(),
        "{artist}|{year}|{duration}|{duration_s}",
    );
    let event = f.event().await;
    assert_eq!(event.event().message.channel, "AUDIO_METADATA");
    assert_eq!(event.event().context.as_ref(), Some(&f.ctx));
    wait(|| fs::read_to_string(f.text()).ok().as_deref() == Some("Artist|Unknown Year|01:05|65.9"))
        .await;
    assert_eq!(f.host.status()["retained"], 1);
    assert_eq!(f.host.status()["publications"], 1);
    let mut future = Box::pin(ticket.finish());
    assert!(tokio::time::timeout(Duration::from_millis(30), &mut future)
        .await
        .is_err());
    let mut foreign = f.ctx.clone();
    foreign.library_generation += 1;
    assert!(f.host.complete(event.id(), &foreign, true).is_err());
    f.ack(&event, false);
    assert!(f.host.complete(event.id(), &f.ctx, true).is_err());
    let receipt = future.await.unwrap();
    assert!(receipt.failure().is_none());
    assert!(receipt
        .diagnostics()
        .iter()
        .any(|e| e.code == "now_playing_delivery_rejected"));
    assert_eq!(
        receipt.mutated_input().unwrap()["format"],
        "Artist|Unknown Year|01:05|65.9"
    );
    drop(receipt);
    f.close().await;
}
#[tokio::test]
async fn real_mp3_png_preserves_native_year_undefined_picture_and_cleanup() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(Config::default()).await;
    let path = f.source("tagged.mp3", &mp3()).await;
    fs::create_dir_all(f.text().parent().unwrap()).unwrap();
    fs::write(f.cover(), vec![42; 1000]).unwrap();
    fs::write(f.text(), "a much longer old text").unwrap();
    let ticket = f.submit(
        path.to_str().unwrap(),
        "{artist}|{title}|{album}|{year}|{artwork_base64}",
    );
    let event = f.event().await;
    let Publication::Metadata(ref metadata) = event else {
        panic!("metadata")
    };
    let hydration = serde_json::to_value(metadata.hydration()).unwrap();
    assert!(hydration["specialNumbers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["path"] == "/metadata/year" && v["kind"] == "NaN"));
    assert!(hydration["undefinedPaths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "/metadata/picture/0/type"));
    assert_eq!(
        metadata.event().message.data.as_ref().unwrap()["metadata"]["picture"][0]["description"],
        "undefined"
    );
    f.ack(&event, true);
    let receipt = ticket.finish().await.unwrap();
    assert!(receipt.failure().is_none(), "{:?}", receipt.failure());
    let text = fs::read_to_string(f.text()).unwrap();
    assert!(text.starts_with("Artist|Café 音楽 🎵|Album|NaN|data:image/jpeg;base64,"));
    let raw = metadata.event().message.data.as_ref().unwrap()["metadata"]["picture"][0]["data"]
        .as_str()
        .unwrap();
    assert!(text.ends_with(raw));
    let installed = fs::read(f.cover()).unwrap();
    assert!(installed.starts_with(b"\x89PNG"));
    assert!(installed.len() < 1000);
    assert_eq!(f.covers.status().sessions, 0);
    drop(receipt);
    let unset = f
        .host
        .submit(&f.ctx, Command::Unset, CancellationToken::new())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert!(unset.failure().is_none());
    assert_eq!(fs::read(f.text()).unwrap(), b"");
    assert!(!f.cover().exists());
    drop(unset);
    f.close().await;
}
#[tokio::test]
async fn source_null_empty_and_real_parser_failure_continue_but_authority_does_not() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(Config::default()).await;
    let empty = f.submit("", "{year}:{duration_s}").finish().await.unwrap();
    assert!(empty.failure().is_none());
    assert_eq!(fs::read_to_string(f.text()).unwrap(), "Unknown Year:65.9");
    drop(empty);
    let corrupt = f.source("corrupt.wav", b"not audio").await;
    let bad = f
        .submit(corrupt.to_str().unwrap(), "{artist}")
        .finish()
        .await
        .unwrap();
    assert!(bad.failure().is_none());
    assert!(bad
        .diagnostics()
        .iter()
        .any(|e| e.code == "audio_input_rejected"));
    assert_eq!(f.covers.status().sessions, 0);
    drop(bad);
    let before = fs::read(f.text()).unwrap();
    let forbidden = f
        .submit("/unregistered.wav", "changed")
        .finish()
        .await
        .unwrap();
    assert_eq!(
        forbidden.failure().unwrap().code,
        "now_playing_authorization"
    );
    assert_eq!(fs::read(f.text()).unwrap(), before);
    drop(forbidden);
    f.close().await;
}
#[tokio::test]
async fn open_alert_is_source_string_then_actual_exists_reveals_without_ack() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(Config::default()).await;
    let open = f
        .host
        .submit(&f.ctx, Command::Open, CancellationToken::new())
        .unwrap();
    let event = f.event().await;
    assert!(matches!(event, Publication::Alert(_)));
    assert_eq!(
        event.event().message.data.as_ref(),
        Some(&json!("This does not exist!"))
    );
    f.ack(&event, true);
    drop(open.finish().await.unwrap());
    fs::write(f.text(), "exists").unwrap();
    let open = f
        .host
        .submit(&f.ctx, Command::Open, CancellationToken::new())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert!(open.failure().is_none());
    assert_eq!(*f.revealed.lock().unwrap(), [f.text()]);
    drop(open);
    assert!(f.events.try_recv().is_err());
    f.close().await;
}
#[tokio::test]
async fn overlap_uses_actual_preparation_completion_and_unset_does_not_cancel_set() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(Config::default()).await;
    let first = f.source("first.wav", &wav()).await;
    let second = f.source("second.wav", &wav()).await;
    let gate = f.host.test_hooks().hold(TestStage::Preparation);
    let a = f.submit(first.to_str().unwrap(), "first");
    gate.entered.notified().await;
    let unset = f
        .host
        .submit(&f.ctx, Command::Unset, CancellationToken::new())
        .unwrap();
    let b = f.submit(second.to_str().unwrap(), "second");
    let event = f.event().await;
    assert_eq!(
        event.event().message.data.as_ref().unwrap()["filePath"],
        second.to_str().unwrap()
    );
    f.ack(&event, true);
    drop(b.finish().await.unwrap());
    drop(unset.finish().await.unwrap());
    assert_eq!(fs::read_to_string(f.text()).unwrap(), "second");
    gate.resume.notify_one();
    let event = f.event().await;
    assert_eq!(
        event.event().message.data.as_ref().unwrap()["filePath"],
        first.to_str().unwrap()
    );
    f.ack(&event, true);
    drop(a.finish().await.unwrap());
    assert_eq!(fs::read_to_string(f.text()).unwrap(), "first");
    f.close().await;
}
#[tokio::test]
async fn caller_drop_and_four_whole_results_remain_bounded_through_private_ack() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(Config::default()).await;
    let mut tickets = vec![];
    for _ in 0..4 {
        tickets.push(
            f.host
                .submit(&f.ctx, Command::Open, CancellationToken::new())
                .unwrap(),
        );
    }
    assert_eq!(
        f.host
            .submit(&f.ctx, Command::Open, CancellationToken::new())
            .err()
            .unwrap()
            .code,
        "now_playing_busy"
    );
    drop(tickets.remove(0));
    let mut sequences = vec![];
    for _ in 0..4 {
        let e = f.event().await;
        sequences.push(e.sequence().parse::<u64>().unwrap());
        f.ack(&e, true);
    }
    assert!(sequences.windows(2).all(|s| s[0] < s[1]));
    let mut receipts = vec![];
    for t in tickets {
        receipts.push(t.finish().await.unwrap());
    }
    wait(|| f.host.status()["activeOperations"] == 0).await;
    assert_eq!(f.host.status()["retained"], 3);
    fs::write(f.text(), "exists").unwrap();
    let last = f
        .host
        .submit(&f.ctx, Command::Open, CancellationToken::new())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(f.host.status()["retained"], 4);
    assert!(f
        .host
        .submit(&f.ctx, Command::Open, CancellationToken::new())
        .is_err());
    drop(last);
    drop(receipts);
    f.close().await;
}
#[tokio::test]
async fn shutdown_retains_real_supervisor_join_across_aborted_waiter_and_caps_observers() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(Config {
        shutdown_timeout: Duration::from_millis(80),
        ..Config::default()
    })
    .await;
    let gate = f.host.test_hooks().hold(TestStage::SupervisorExit);
    let host = f.host.clone();
    let first = tokio::spawn(async move { host.shutdown().await });
    gate.entered.notified().await;
    first.abort();
    let _ = first.await;
    assert_eq!(f.host.status()["complete"], true);
    let mut observers = vec![];
    for _ in 0..4 {
        let host = f.host.clone();
        observers.push(tokio::spawn(async move { host.shutdown().await }));
    }
    wait(|| f.host.status()["lifecycleObservers"] == 4).await;
    assert_eq!(
        f.host.shutdown().await.unwrap_err().code,
        "now_playing_lifecycle_busy"
    );
    for observer in observers {
        assert_eq!(
            observer.await.unwrap().unwrap_err().code,
            "now_playing_drain_timeout"
        );
    }
    gate.resume.notify_one();
    f.host.shutdown().await.unwrap();
    f.close().await;
}
#[tokio::test]
async fn retirement_waits_for_real_phase_and_rejects_stale_publication() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(Config {
        shutdown_timeout: Duration::from_millis(50),
        ..Config::default()
    })
    .await;
    let source = f.source("retire.wav", &wav()).await;
    let gate = f.host.test_hooks().hold(TestStage::Phase(
        now_playing_schedule::PhaseKind::PostMetadata,
    ));
    let ticket = f.submit(source.to_str().unwrap(), "stale");
    let event = f.event().await;
    gate.entered.notified().await;
    assert_eq!(
        f.host.shutdown().await.unwrap_err().code,
        "now_playing_drain_timeout"
    );
    assert!(f.host.complete(event.id(), &f.ctx, true).is_err());
    drop(ticket);
    assert_eq!(f.covers.status().sessions, 1);
    gate.resume.notify_one();
    f.host.shutdown().await.unwrap();
    assert!(!f.text().exists());
    f.close().await;
}
#[tokio::test]
async fn changed_full_context_cannot_publish_old_source_and_typed_duration_keeps_nan() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(Config::default()).await;
    let receipt=f.host.submit(&f.ctx,Command::Set{payload:json!({"filePath":"","unknownLang":[],"format":"{duration}|{duration_s}","duration":null}),duration_number:Some(f64::NAN)},CancellationToken::new()).unwrap().finish().await.unwrap();
    assert!(receipt.failure().is_none());
    assert_eq!(fs::read_to_string(f.text()).unwrap(), "00:00|NaN");
    drop(receipt);
    let next = f.temp.path().join("new");
    fs::create_dir_all(&next).unwrap();
    let request:Request=serde_json::from_value(json!({"channel":"MAIN","context":f.ctx,"data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":f.ctx.data_path,"newPath":next}}})).unwrap();
    f.library.dispatch(request, "main").await.unwrap();
    assert_eq!(
        f.host
            .submit(&f.ctx, Command::Unset, CancellationToken::new())
            .err()
            .unwrap()
            .code,
        "stale_library"
    );
    f.close().await;
}

#[tokio::test]
async fn actual_blocked_library_owner_phase_survives_caller_drop_and_timeout() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(Config {
        shutdown_timeout: Duration::from_millis(60),
        ..Config::default()
    })
    .await;
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saved = gate.clone();
    let signaled = entered.clone();
    let library = f.library.clone();
    let request = Request {
        channel: "MAIN".into(),
        data: json!({}),
        context: Some(f.ctx.clone()),
        listener_id: None,
    };
    let next_request = Request {
        channel: "MAIN".into(),
        data: json!({}),
        context: Some(f.ctx.clone()),
        listener_id: None,
    };
    let earlier = tokio::spawn(async move {
        library
            .commit(request, "main", move |_| {
                signaled.store(true, std::sync::atomic::Ordering::Release);
                let (lock, changed) = &*saved;
                let mut done = lock.lock().unwrap();
                while !*done {
                    done = changed.wait(done).unwrap();
                }
                Ok(vec![])
            })
            .await
    });
    wait(|| entered.load(std::sync::atomic::Ordering::Acquire)).await;
    let ticket = f.submit("", "after");
    wait(|| f.host.status()["currentPhase"] == true).await;
    let later_entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saved = later_entered.clone();
    let library = f.library.clone();
    let later = tokio::spawn(async move {
        library
            .commit(next_request, "main", move |_| {
                saved.store(true, std::sync::atomic::Ordering::Release);
                Ok(vec![])
            })
            .await
    });
    drop(ticket);
    assert_eq!(
        f.host.shutdown().await.unwrap_err().code,
        "now_playing_drain_timeout"
    );
    assert_eq!(f.host.status()["drained"], false);
    assert!(!later_entered.load(std::sync::atomic::Ordering::Acquire));
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    earlier.await.unwrap().unwrap();
    f.host.shutdown().await.unwrap();
    later.await.unwrap().unwrap();
    assert_eq!(f.host.status()["supervisorJoined"], true);
    assert!(!f.text().exists());
    f.close().await;
}

#[tokio::test]
async fn actual_pinned_source_wav_format_and_mutation_match_native_host() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(Config::default()).await;
    let source = f.source("same-source.wav", &wav()).await;
    let payload = json!({"filePath":source,"name":"Café 🎵","unknownLang":["Singer","Title","Album"],"format":"{artist}|{title}|{album}|{year}|{duration}|{duration_s}","duration":125.9});
    let ticket = f
        .host
        .submit(
            &f.ctx,
            Command::Set {
                payload: payload.clone(),
                duration_number: None,
            },
            CancellationToken::new(),
        )
        .unwrap();
    let event = f.event().await;
    f.ack(&event, true);
    let receipt = ticket.finish().await.unwrap();
    assert!(receipt.failure().is_none());
    let root = std::env::var_os("FREESHOW_NOW_PLAYING_ORACLE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_path_buf()
        });
    let output=std::process::Command::new("node").current_dir(root).args(["--input-type=module","-e",r#"
import ts from 'typescript';import fs from 'node:fs';import path from 'node:path';import {execFileSync} from 'node:child_process';import {parseFile} from 'music-metadata';
const source=execFileSync('git',['show','2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/audio/nowPlaying.ts'],{encoding:'utf8',timeout:10000,maxBuffer:1024*1024});
const ast=ts.createSourceFile('nowPlaying.ts',source,ts.ScriptTarget.Latest,true);
const names=['getNowPlayingTextPath','getNowPlayingMediaPath','setPlayingState','convertDynamicValues','getArtist','getAudioMetadata'];
const declarations=names.map(name=>ast.statements.find(n=>ts.isFunctionDeclaration(n)&&n.name?.text===name).getText(ast).replace(/^export\s+/,''));
const prelude=`const fileNameText='NowPlaying.txt',fileNameImage='NowPlayingCover.png',dynamicValues=['{artist}','{title}','{album}','{year}','{artwork_path}','{artwork_base64}','{duration}','{duration_s}'];`;
const js=ts.transpileModule(prelude+declarations.join('\n')+';return setPlayingState',{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
const titles=[],events=[],writes=[];const folder=process.argv[2];fs.mkdirSync(folder,{recursive:true});
const fn=new Function('parseFile','nativeImage','join','sendToMain','ToMain','IcecastSender','getDataFolderPath','doesPathExist','deleteFile','writeFile',js)(parseFile,{createFromBuffer(){throw Error('WAV image unexpected')}},path.join,(channel,data)=>events.push({channel,data}),{AUDIO_METADATA:'AUDIO_METADATA'},{updateMetadata(title){titles.push(title)}},()=>folder,p=>fs.existsSync(p),p=>{fs.unlinkSync(p);return true},(p,b)=>{fs.writeFileSync(p,b);writes.push([path.basename(p),String(b)]);return true});
const data=JSON.parse(process.argv[1]);const result=await fn(data);process.stdout.write(JSON.stringify({void:result===undefined,data,titles,writes,events}));
"#]).arg(payload.to_string()).arg(f.temp.path().join("source-oracle")).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(original["void"], true);
    assert_eq!(original["data"], receipt.mutated_input().unwrap().clone());
    assert_eq!(
        original["writes"][0][1],
        fs::read_to_string(f.text()).unwrap()
    );
    assert_eq!(
        original["events"][0]["channel"],
        event.event().message.channel
    );
    assert_eq!(
        original["events"][0]["data"]["filePath"],
        event.event().message.data.as_ref().unwrap()["filePath"]
    );
    assert_eq!(original["titles"], json!(["Singer - Café 🎵"]));
    drop(receipt);
    f.close().await;
}

#[tokio::test]
async fn rejected_and_panicking_publishers_clean_real_sessions_without_waiting_for_phantom_ack() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(Config::default()).await;
    let path = f.source("publisher.wav", &wav()).await;
    for (mode, expected) in [(1, "test_publish"), (2, "now_playing_callback_panic")] {
        f.publish_mode
            .store(mode, std::sync::atomic::Ordering::Release);
        let receipt = f
            .submit(path.to_str().unwrap(), "never-written")
            .finish()
            .await
            .unwrap();
        assert_eq!(receipt.failure().unwrap().code, expected);
        assert_eq!(f.covers.status().sessions, 0);
        assert_eq!(f.host.status()["publications"], 0);
        assert!(!f.text().exists());
        drop(receipt);
    }
    f.publish_mode
        .store(0, std::sync::atomic::Ordering::Release);
    f.close().await;
}

#[tokio::test]
async fn explicit_cancel_bounds_and_publication_timeout_do_not_release_unfinished_io() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(Config {
        operation_timeout: Duration::from_millis(250),
        ..Config::default()
    })
    .await;
    let canceled = CancellationToken::new();
    canceled.cancel();
    assert_eq!(
        f.host
            .submit(&f.ctx, Command::Unset, canceled)
            .err()
            .unwrap()
            .code,
        "cancelled"
    );
    for payload in [
        json!({"filePath":"x".repeat(32769)}),
        json!({"filePath":"","format":"x".repeat(65537)}),
        json!({"filePath":"\u{0000}"}),
    ] {
        assert!(f
            .host
            .submit(
                &f.ctx,
                Command::Set {
                    payload,
                    duration_number: None
                },
                CancellationToken::new()
            )
            .is_err());
    }
    assert_eq!(f.host.status()["retained"], 0);
    let path = f.source("timeout.wav", &wav()).await;
    let ticket = f.submit(path.to_str().unwrap(), "real-before-timeout");
    let event = f.event().await;
    wait(|| fs::read_to_string(f.text()).ok().as_deref() == Some("real-before-timeout")).await;
    let receipt = ticket.finish().await.unwrap();
    assert_eq!(
        receipt.failure().unwrap().code,
        "now_playing_delivery_timeout"
    );
    assert!(f.host.complete(event.id(), &f.ctx, true).is_err());
    drop(receipt);
    assert_eq!(
        f.host.shutdown().await.unwrap_err().code,
        "now_playing_delivery_timeout"
    );
    assert_eq!(f.host.status()["drained"], true);
    // Teardown still proceeds after proven host drain despite its sticky error.
    f.pipeline.shutdown().await.unwrap();
    f.audio.shutdown().await;
    f.covers.shutdown(Duration::from_secs(2)).await.unwrap();
    f.assets.shutdown().await.unwrap();
    f.library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn explicit_cancellation_at_actual_preparation_boundary_settles_session_before_receipt() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(Config::default()).await;
    let path = f.source("cancel.wav", &wav()).await;
    let gate = f.host.test_hooks().hold(TestStage::Preparation);
    let cancel = CancellationToken::new();
    let ticket = f
        .host
        .submit(
            &f.ctx,
            Command::Set {
                payload: json!({"filePath":path,"format":"cancelled","duration":0}),
                duration_number: None,
            },
            cancel.clone(),
        )
        .unwrap();
    gate.entered.notified().await;
    cancel.cancel();
    gate.resume.notify_one();
    let receipt = ticket.finish().await.unwrap();
    assert!(receipt.failure().is_some());
    assert_eq!(f.covers.status().sessions, 0);
    assert!(!f.text().exists());
    drop(receipt);
    f.close().await;
}

#[tokio::test]
async fn actual_icecast_title_pages_follow_source_metadata_before_native_text() {
    use freeshow_audio_pipeline::{AudioMetadata, JsInput};
    use tokio::io::AsyncReadExt;
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(Config::default()).await;
    if !f.pipeline.capability().await.unwrap().is_available() {
        eprintln!("REAL OPUS UNAVAILABLE: no loopback codec/title-page coverage claimed");
        assert!(std::env::var_os("FREESHOW_REQUIRE_OPUS").is_none());
        f.close().await;
        return;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut bytes = vec![];
        socket.take(262144).read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    let raw = vec![0; 960 * 2 * 4];
    let receipt=f.pipeline.pcm(&f.ctx,&raw,raw.len(),&AudioMetadata{sample_rate:JsInput::Number(48000.0),target_id:JsInput::Json(json!("icecast")),icecast:Some(json!({"enabled":true,"host":"127.0.0.1","port":port,"mount":"audio","password":"test"}))},&audio_pipeline::SinkAvailability::default(),CancellationToken::new()).await.unwrap();
    assert!(receipt.failure().is_none());
    drop(receipt);
    wait(|| f.connected.load(std::sync::atomic::Ordering::Acquire)).await;
    let source = f.source("network.wav", &wav()).await;
    let ticket=f.host.submit(&f.ctx,Command::Set{payload:json!({"filePath":source,"name":"Real title","unknownLang":["Singer"],"format":"{artist} - {title}","duration":1}),duration_number:None},CancellationToken::new()).unwrap();
    let event = f.event().await;
    f.ack(&event, true);
    let receipt = ticket.finish().await.unwrap();
    assert!(receipt.failure().is_none());
    assert_eq!(fs::read_to_string(f.text()).unwrap(), "Singer - Real title");
    drop(receipt);
    drop(
        f.pipeline
            .disconnect_icecast(&f.ctx, CancellationToken::new())
            .await
            .unwrap(),
    );
    let bytes = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert!(bytes.starts_with(b"PUT /audio HTTP/1.1\r\n"));
    let start = bytes.windows(4).position(|p| p == b"\r\n\r\n").unwrap() + 4;
    let mut at = start;
    let mut titles = 0;
    while at < bytes.len() {
        assert_eq!(&bytes[at..at + 4], b"OggS");
        let segments = bytes[at + 26] as usize;
        let body = at + 27 + segments;
        let end = body
            + bytes[at + 27..body]
                .iter()
                .map(|b| *b as usize)
                .sum::<usize>();
        let raw = &bytes[at..end];
        let expected = u32::from_le_bytes(raw[22..26].try_into().unwrap());
        let mut crc_input = raw.to_vec();
        crc_input[22..26].fill(0);
        assert_eq!(
            freeshow_icecast::crc32(&crc_input),
            expected,
            "actual raw on-wire Ogg CRC"
        );
        if bytes[body..end]
            .windows(b"TITLE=Real title".len())
            .any(|b| b == b"TITLE=Real title")
        {
            titles += 1;
        }
        at = end;
    }
    assert_eq!(at, bytes.len());
    f.close().await;
    assert_eq!(titles, 1);
    assert!(bytes
        .windows(b"ARTIST=Singer".len())
        .any(|p| p == b"ARTIST=Singer"));
}

#[tokio::test]
async fn shared_sealed_budget_keeps_completed_uncollected_results_charged_across_hosts() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(Config::default()).await;
    let mut receipts = vec![];
    for _ in 0..4 {
        let ticket = f
            .host
            .submit(&f.ctx, Command::Open, CancellationToken::new())
            .unwrap();
        let event = f.event().await;
        f.ack(&event, true);
        receipts.push(ticket.finish().await.unwrap());
    }
    assert_eq!(f.host.status()["admitted"], "4");
    assert_eq!(f.host.status()["completed"], "4");
    f.host.shutdown().await.unwrap();
    assert_eq!(f.host.status()["drained"], true);
    let next = NativeNowPlaying::start_with_budget(
        Dependencies {
            library: f.library.clone(),
            assets: f.assets.clone(),
            audio: f.audio.clone(),
            covers: f.covers.clone(),
            pipeline: f.pipeline.clone(),
        },
        Config::default(),
        Callbacks {
            publish: Arc::new(|_| Box::pin(async { Ok(()) })),
            reveal: Arc::new(|_, _| Box::pin(async { Ok(()) })),
        },
        f.budget.clone(),
    )
    .unwrap();
    assert_eq!(next.status()["admitted"], "0");
    assert_eq!(
        next.submit(&f.ctx, Command::Open, CancellationToken::new())
            .err()
            .unwrap()
            .code,
        "now_playing_busy"
    );
    drop(receipts.pop());
    fs::write(f.text(), "exists").unwrap();
    let receipt = next
        .submit(&f.ctx, Command::Open, CancellationToken::new())
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert!(receipt.failure().is_none());
    assert_eq!(next.status()["retained"], 4);
    drop(receipt);
    drop(receipts);
    next.shutdown().await.unwrap();
    f.close().await;
}
