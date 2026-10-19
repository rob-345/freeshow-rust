//! Registry tests compose the real native host/dependencies. Callback futures
//! alone are controlled; no Tauri app, physical audio or codec parity is claimed.
#[allow(dead_code)]
#[path = "../src/assets.rs"]
mod assets;
#[allow(dead_code)]
#[path = "../src/audio_pipeline.rs"]
mod audio_pipeline;
#[allow(dead_code)]
#[path = "../src/now_playing.rs"]
mod now_playing;
#[allow(dead_code)]
#[path = "../src/now_playing_files.rs"]
mod now_playing_files;
#[allow(dead_code)]
#[path = "../src/now_playing_owner.rs"]
mod now_playing_owner;
#[allow(dead_code)]
#[path = "../src/now_playing_prepare.rs"]
mod now_playing_prepare;
#[allow(dead_code)]
#[path = "../src/now_playing_schedule.rs"]
mod now_playing_schedule;

use freeshow_audio::{AudioConfig, AudioService};
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_cover_images::CoverImageService;
use freeshow_opus::LibrarySource;
use now_playing::{Callbacks, Command, Config, Dependencies, HostError, Publication, Ticket};
use now_playing_owner::Owner;
use serde_json::json;
use std::{
    fs,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Weak,
    },
    time::Duration,
};
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
#[derive(Default)]
struct RevealGate {
    entered: AtomicBool,
    release: Notify,
}
struct Fixture {
    temp: tempfile::TempDir,
    library: LibraryService,
    assets: assets::NativeAssets,
    audio: Arc<AudioService>,
    covers: Arc<CoverImageService>,
    pipeline: audio_pipeline::NativeAudioPipeline,
    status_task: tokio::task::JoinHandle<()>,
    owner: Arc<Owner>,
    send: mpsc::Sender<Publication>,
    events: mpsc::Receiver<Publication>,
    reveals: Arc<AtomicUsize>,
}
impl Fixture {
    async fn new() -> Self {
        Self::with_config(Config::default()).await
    }
    async fn with_config(config: Config) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
        )
        .unwrap();
        let context = library.context().unwrap();
        let assets = assets::NativeAssets::start(&context, &temp.path().join("app"))
            .await
            .unwrap();
        let audio = Arc::new(AudioService::new(AudioConfig::default()).unwrap());
        let covers = Arc::new(CoverImageService::new().unwrap());
        let (status_send, mut status_receive) =
            mpsc::channel::<audio_pipeline::AudioStatusDelivery>(2);
        // No PCM or Opus result is needed by Open. Make that absence explicit
        // rather than depending on the machine's installed codec/hardware.
        let source =
            unsafe { LibrarySource::trusted_path(&temp.path().join("explicitly-absent-opus")) }
                .unwrap();
        let pipeline = audio_pipeline::NativeAudioPipeline::start(
            library.clone(),
            source,
            audio_pipeline::AudioLimits::default(),
            move |event| {
                status_send
                    .try_send(event)
                    .map_err(|_| "controlled status receiver unavailable".into())
            },
        )
        .unwrap();
        let status_owner = pipeline.clone();
        let status_task = tokio::spawn(async move {
            while let Some(event) = status_receive.recv().await {
                let _ = status_owner.complete_status(
                    "main",
                    event.event().context.as_ref().unwrap(),
                    event.id(),
                    true,
                );
            }
        });
        let owner = Arc::new(
            Owner::with_config(
                Dependencies {
                    library: library.clone(),
                    assets: assets.clone(),
                    audio: audio.clone(),
                    covers: covers.clone(),
                    pipeline: pipeline.clone(),
                },
                config,
            )
            .unwrap(),
        );
        let (send, events) = mpsc::channel(4);
        Self {
            temp,
            library,
            assets,
            audio,
            covers,
            pipeline,
            status_task,
            owner,
            send,
            events,
            reveals: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn context(&self) -> RuntimeContext {
        self.library.context().unwrap()
    }
    fn existing_text(&self) {
        let path = self.context().data_path.join("Audio/NowPlaying.txt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "existing source text").unwrap();
    }
    /// A Weak marker observes real callback capture disposal, without pretending
    /// it is an actual Tauri AppHandle or testing an app's own strong cycles.
    fn callbacks(&self, gate: Option<Arc<RevealGate>>) -> (Callbacks, Weak<()>) {
        let guard = Arc::new(());
        let weak = Arc::downgrade(&guard);
        let publish_guard = guard.clone();
        let send = self.send.clone();
        let reveals = self.reveals.clone();
        let callbacks = Callbacks {
            publish: Arc::new(move |event| {
                let guard = publish_guard.clone();
                let result = send.try_send(event).map_err(|_| {
                    HostError::new(
                        "test_publication",
                        "Bounded publication receiver unavailable",
                    )
                });
                Box::pin(async move {
                    drop(guard);
                    result
                })
            }),
            reveal: Arc::new(move |_, _| {
                let guard = guard.clone();
                let gate = gate.clone();
                let reveals = reveals.clone();
                Box::pin(async move {
                    reveals.fetch_add(1, Ordering::AcqRel);
                    if let Some(gate) = gate {
                        gate.entered.store(true, Ordering::Release);
                        gate.release.notified().await;
                    }
                    drop(guard);
                    Ok(())
                })
            }),
        };
        (callbacks, weak)
    }
    fn open(&self, context: &RuntimeContext) -> Ticket {
        self.owner
            .submit(
                context,
                Command::Open,
                CancellationToken::new(),
                self.callbacks(None).0,
            )
            .unwrap()
    }
    async fn next_event(&mut self) -> Publication {
        tokio::time::timeout(Duration::from_secs(10), self.events.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn change_library(&self) -> RuntimeContext {
        let old = self.context();
        let next = self.temp.path().join("next-library");
        fs::create_dir_all(&next).unwrap();
        let request: Request = serde_json::from_value(json!({
            "channel":"MAIN", "context":old,
            "data":{"channel":"UPDATE_DATA_PATH", "data":{"newPath":next,"oldPath":old.data_path}}
        }))
        .unwrap();
        self.library.dispatch(request, "main").await.unwrap();
        self.context()
    }
    async fn finish(self) {
        self.finish_with_error(None).await;
    }
    async fn finish_with_error(self, expected: Option<HostError>) {
        let (drained, owner) = self.owner.retire(None, true).await;
        assert!(drained);
        match expected {
            Some(expected) => {
                let actual = error(owner);
                assert_eq!(actual.code, expected.code);
                assert_eq!(actual.message, expected.message);
            }
            None => owner.unwrap(),
        }
        // Each actual dependency is attempted before reporting one failure.
        let pipeline = self.pipeline.shutdown().await;
        self.audio.shutdown().await;
        let covers = self.covers.shutdown(Duration::from_secs(3)).await;
        let assets = self.assets.shutdown().await;
        self.status_task.await.unwrap();
        let library = self.library.finish_writes().await;
        pipeline.unwrap();
        covers.unwrap();
        assets.unwrap();
        library.unwrap();
    }
}
async fn wait(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
fn error<T>(result: Result<T, HostError>) -> HostError {
    match result {
        Ok(_) => panic!("expected explicit native admission error"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn lazy_owner_gates_full_context_and_releases_only_its_actual_supervisor_callbacks() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new().await;
    let current = f.context();
    assert!(f.owner.ready(&current));
    assert!(f.owner.status()["host"].is_null());
    let mut wrong_runtime = current.clone();
    wrong_runtime.runtime_id.push_str("-foreign");
    let mut wrong_generation = current.clone();
    wrong_generation.library_generation += 1;
    let mut wrong_path = current.clone();
    wrong_path.data_path = f.temp.path().join("foreign-path");
    for foreign in [wrong_runtime, wrong_generation, wrong_path] {
        let (callbacks, captured) = f.callbacks(None);
        assert_eq!(
            error(
                f.owner
                    .submit(&foreign, Command::Open, CancellationToken::new(), callbacks)
            )
            .code,
            "now_playing_context"
        );
        assert!(captured.upgrade().is_none());
        assert!(f.owner.status()["host"].is_null());
    }
    let (callbacks, capture) = f.callbacks(None);
    let first = f
        .owner
        .submit(&current, Command::Open, CancellationToken::new(), callbacks)
        .unwrap();
    let event = f.next_event().await;
    assert_eq!(event.event().message.channel, "ALERT");
    assert_eq!(
        event.event().message.data,
        Some(json!("This does not exist!"))
    );
    assert!(capture.upgrade().is_some());
    assert!(f.owner.context_ready(current.clone()).is_err());
    let mut foreign = current.clone();
    foreign.data_path = f.temp.path().join("wrong-completion");
    assert_eq!(
        error(f.owner.complete(&foreign, event.id(), true)).code,
        "now_playing_completion"
    );
    f.owner.complete(&current, event.id(), true).unwrap();
    assert!(f.owner.complete(&current, event.id(), true).is_err());
    let first = first.finish().await.unwrap();
    assert_eq!(first.context(), &current);
    assert!(first.failure().is_none());
    drop(first);
    let (unused_callbacks, unused_capture) = f.callbacks(None);
    let second = f
        .owner
        .submit(
            &current,
            Command::Open,
            CancellationToken::new(),
            unused_callbacks,
        )
        .unwrap();
    assert!(
        unused_capture.upgrade().is_none(),
        "an installed host must not retain per-request callbacks"
    );
    let event = f.next_event().await;
    f.owner.complete(&current, event.id(), true).unwrap();
    drop(second.finish().await.unwrap());
    let (drained, receipt) = f.owner.retire(Some(&current), false).await;
    assert!(drained);
    receipt.unwrap();
    assert!(
        capture.upgrade().is_none(),
        "real supervisor exit drops strong callback captures"
    );
    assert!(f.owner.status()["host"].is_null());
    assert_eq!(f.owner.status()["lastRetired"]["supervisorJoined"], true);
    assert_eq!(f.owner.status()["lastRetired"]["drained"], true);
    f.finish().await;
}

#[tokio::test]
async fn four_old_ready_tickets_hold_one_budget_across_joined_context_replacement_until_actual_receipt_drop(
) {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new().await;
    let old = f.context();
    f.existing_text();
    let mut tickets: Vec<_> = (0..4).map(|_| f.open(&old)).collect();
    wait(|| {
        f.reveals.load(Ordering::Acquire) == 4 && f.owner.status()["host"]["activeOperations"] == 0
    })
    .await;
    assert_eq!(f.owner.status()["host"]["retained"], 4);
    let (drained, receipt) = f.owner.retire(Some(&old), false).await;
    assert!(drained);
    receipt.unwrap();
    assert_eq!(f.owner.status()["lastRetired"]["supervisorJoined"], true);
    assert!(f.owner.status()["host"].is_null());
    let current = f.change_library().await;
    assert_ne!(current, old);
    assert!(!f.owner.ready(&current));
    assert_eq!(
        error(f.owner.submit(
            &current,
            Command::Open,
            CancellationToken::new(),
            f.callbacks(None).0
        ))
        .code,
        "now_playing_context"
    );
    assert!(f.owner.context_ready(old.clone()).is_err());
    // This is the production transition order after actual old-host join.
    f.pipeline.context_changed().unwrap();
    f.assets.context_changed(&current).unwrap();
    f.owner.context_ready(current.clone()).unwrap();
    f.existing_text();
    assert_eq!(
        error(f.owner.submit(
            &old,
            Command::Open,
            CancellationToken::new(),
            f.callbacks(None).0
        ))
        .code,
        "now_playing_context"
    );
    assert_eq!(
        error(f.owner.submit(
            &current,
            Command::Open,
            CancellationToken::new(),
            f.callbacks(None).0
        ))
        .code,
        "now_playing_busy"
    );
    // Consumption transfers the same lease into Receipt; it is not disposal.
    let consumed = tickets.remove(0).finish().await.unwrap();
    assert_eq!(consumed.context(), &old);
    assert_eq!(
        error(f.owner.submit(
            &current,
            Command::Open,
            CancellationToken::new(),
            f.callbacks(None).0
        ))
        .code,
        "now_playing_busy"
    );
    drop(consumed);
    let current_receipt = f.open(&current).finish().await.unwrap();
    assert!(current_receipt.failure().is_none());
    assert_eq!(current_receipt.context(), &current);
    assert_eq!(
        error(f.owner.submit(
            &current,
            Command::Open,
            CancellationToken::new(),
            f.callbacks(None).0
        ))
        .code,
        "now_playing_busy"
    );
    drop(current_receipt);
    // Dropping completed but uncollected receivers really disposes their results.
    drop(tickets);
    wait(|| f.owner.status()["host"]["retained"] == 0).await;
    drop(f.open(&current).finish().await.unwrap());
    f.finish().await;
}

#[tokio::test]
async fn terminal_retirement_keeps_dropped_callers_blocked_callback_and_dependencies_owned_after_observer_abort(
) {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new().await;
    let context = f.context();
    f.existing_text();
    let gate = Arc::new(RevealGate::default());
    let (callbacks, capture) = f.callbacks(Some(gate.clone()));
    let ticket = f
        .owner
        .submit(&context, Command::Open, CancellationToken::new(), callbacks)
        .unwrap();
    wait(|| gate.entered.load(Ordering::Acquire)).await;
    drop(ticket);
    assert_eq!(f.owner.status()["host"]["retained"], 1);
    let retained_owner = f.owner.clone();
    let observer = tokio::spawn(async move { retained_owner.retire(None, true).await });
    wait(|| f.owner.status()["closed"] == true && f.owner.status()["host"]["closing"] == true)
        .await;
    assert_eq!(f.owner.status()["host"]["drained"], false);
    assert!(capture.upgrade().is_some());
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert_eq!(f.owner.status()["host"]["drained"], false);
    assert!(f.owner.status()["lastRetired"].is_null());
    assert_eq!(
        error(f.owner.submit(
            &context,
            Command::Open,
            CancellationToken::new(),
            f.callbacks(None).0
        ))
        .code,
        "now_playing_shutdown"
    );
    assert!(f.owner.context_ready(context.clone()).is_err());
    assert_eq!(
        f.covers.status().finished_workers,
        0,
        "dependencies are still live while real callback work is blocked"
    );
    assert_eq!(f.assets.status()["closing"], false);
    assert_eq!(f.pipeline.status()["closing"], false);
    gate.release.notify_one();
    let (drained, result) = f.owner.retire(None, true).await;
    assert!(drained);
    result.unwrap();
    assert!(capture.upgrade().is_none());
    assert!(f.owner.status()["host"].is_null());
    assert_eq!(f.owner.status()["lastRetired"]["supervisorJoined"], true);
    assert_eq!(f.owner.status()["lastRetired"]["drained"], true);
    f.owner.context_ready(context.clone()).unwrap();
    assert_eq!(
        error(f.owner.submit(
            &context,
            Command::Open,
            CancellationToken::new(),
            f.callbacks(None).0
        ))
        .code,
        "now_playing_shutdown",
        "context readiness cannot reopen terminal admission"
    );
    f.finish().await;
}

#[tokio::test]
async fn joined_terminal_delivery_failure_survives_receipt_disposal_and_no_host_retry() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::with_config(Config {
        operation_timeout: Duration::from_millis(200),
        shutdown_timeout: Duration::from_secs(3),
    })
    .await;
    let context = f.context();
    let ticket = f.open(&context);
    let event = f.next_event().await;
    assert_eq!(event.event().message.channel, "ALERT");
    assert_eq!(
        event.event().message.data,
        Some(json!("This does not exist!"))
    );
    // Intentionally withhold the real private completion. Only the host's
    // actual delivery deadline can retire this operation and issue its receipt.
    let receipt = tokio::time::timeout(Duration::from_secs(10), ticket.finish())
        .await
        .unwrap()
        .unwrap();
    let observed = receipt.failure().unwrap();
    assert_eq!(observed.code, "now_playing_delivery_timeout");
    assert_eq!(receipt.context(), &context);
    assert!(f.owner.complete(&context, event.id(), true).is_err());
    let (drained, first) = f.owner.retire(Some(&context), true).await;
    assert!(
        drained,
        "terminal error is observed after the real supervisor join"
    );
    let first = error(first);
    assert_eq!(first.code, observed.code);
    assert_eq!(first.message, observed.message);
    assert!(f.owner.status()["host"].is_null());
    assert_eq!(f.owner.status()["lastRetired"]["drained"], true);
    assert_eq!(f.owner.status()["lastRetired"]["supervisorJoined"], true);
    drop(receipt);
    assert_eq!(f.owner.status()["retained"], 0);
    let (drained, retry) = f.owner.retire(None, true).await;
    assert!(drained);
    let retry = error(retry);
    assert_eq!(retry.code, first.code);
    assert_eq!(retry.message, first.message);
    assert!(f.owner.status()["host"].is_null());
    assert_eq!(f.owner.status()["closed"], true);
    assert_eq!(
        error(f.owner.submit(
            &context,
            Command::Open,
            CancellationToken::new(),
            f.callbacks(None).0,
        ))
        .code,
        "now_playing_shutdown",
    );
    // Sticky diagnostics never prevent attempting all actual dependency joins.
    f.finish_with_error(Some(first)).await;
}
