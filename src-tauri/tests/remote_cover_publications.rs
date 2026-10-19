#[allow(dead_code)]
#[path = "../src/now_playing_cover_file.rs"]
mod now_playing_cover_file;
#[allow(
    dead_code,
    reason = "Independent native API awaits production HTTP integration"
)]
#[path = "../src/remote_cover_publications.rs"]
mod publications;
use freeshow_core::{protocol::Request, runtime::Runtime, service::LibraryService};
use now_playing_cover_file::{Control, CoverFiles, Observation};
use publications::Publications;
#[cfg(target_os = "linux")]
use publications::{Delivery, Receipt};
#[cfg(target_os = "linux")]
use std::future::Future;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Fixture {
    _temp: tempfile::TempDir,
    library: LibraryService,
    files: CoverFiles,
}
impl Fixture {
    fn new(blocked: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
        )
        .unwrap();
        if blocked {
            std::fs::write(
                library.context().unwrap().data_path.join("Audio"),
                b"private blocker",
            )
            .unwrap();
        }
        Self {
            _temp: temp,
            library,
            files: CoverFiles::default(),
        }
    }
    async fn observation(&self) -> Observation {
        let context = self.library.context().unwrap();
        let phase = self
            .library
            .begin_phase(
                Request {
                    channel: "MAIN".into(),
                    data: serde_json::Value::Null,
                    listener_id: None,
                    context: Some(context),
                },
                "main",
            )
            .await
            .unwrap();
        let value = self
            .files
            .submit(&phase, Control::default())
            .unwrap()
            .finish()
            .await
            .unwrap();
        phase.finish().await.unwrap();
        value.into_parts().0
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(3)
}

#[tokio::test]
async fn only_a_real_ordered_folder_failure_can_construct_a_source_notification() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(false);
    let owner = owner(f.library.context().unwrap());
    let error = owner
        .publish(
            f.observation().await,
            &CancellationToken::new(),
            deadline(),
            |_| panic!("No genuine failure to publish"),
        )
        .err()
        .unwrap();
    assert_eq!(error.code, "cover_publication_source");
    assert!(!error.message.is_empty());
    assert_eq!(owner.retained(), 0);
    assert_eq!(f.files.retained(), 0);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn source_payload_private_provenance_and_one_use_full_context_completion_are_separate() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(true);
    let context = f.library.context().unwrap();
    let owner = owner(context.clone());
    let cancel = CancellationToken::new();
    let delivery = owner
        .publish_for_test(f.observation().await, &cancel, deadline())
        .unwrap();
    let folder = context.data_path.join("Audio").to_str().unwrap().to_owned();
    assert_eq!(delivery.event.channel, "MAIN");
    assert_eq!(delivery.event.context.as_ref(), Some(&context));
    assert!(delivery.event.listener_id.is_none());
    assert_eq!(
        serde_json::to_value(&delivery.event.message).unwrap(),
        serde_json::json!({"channel":"ALERT","data":format!("Error: Could not create folder at: {folder}!")})
    );
    assert_eq!(delivery.metadata.folder, folder);
    assert_eq!(delivery.metadata.id.len(), 45);
    assert!(delivery.metadata.sequence.parse::<u64>().unwrap() > 0);
    assert_eq!(delivery.receipt.observation().context(), &context);
    let mut foreign = context.clone();
    foreign.runtime_id.push('x');
    assert!(owner
        .complete(&foreign, &delivery.metadata.id, true)
        .is_err());
    foreign = context.clone();
    foreign.data_path.push("foreign");
    assert!(owner
        .complete(&foreign, &delivery.metadata.id, true)
        .is_err());
    assert!(owner.complete(&context, "native-cover-Z", true).is_err());
    owner
        .complete(&context, &delivery.metadata.id, true)
        .unwrap();
    delivery.receipt.observe(&cancel, deadline()).await.unwrap();
    assert!(owner
        .complete(&context, &delivery.metadata.id, true)
        .is_err());
    assert_eq!(
        (owner.retained(), f.files.retained()),
        (1, 1),
        "ACK is not actual receipt/FD disposal"
    );
    drop(delivery);
    assert_eq!((owner.retained(), f.files.retained()), (0, 0));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn retirement_and_observer_timeout_keep_all_four_actual_observations_owned_until_drop() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(true);
    let context = f.library.context().unwrap();
    let owner = owner(context.clone());
    let cancel = CancellationToken::new();
    let mut deliveries = Vec::new();
    for _ in 0..4 {
        deliveries.push(
            owner
                .publish_for_test(f.observation().await, &cancel, deadline())
                .unwrap(),
        );
    }
    assert_eq!((owner.retained(), f.files.retained()), (4, 4));
    assert_eq!(
        deliveries[0]
            .receipt
            .observe(&cancel, Instant::now())
            .await
            .unwrap_err()
            .code,
        "cover_delivery_timeout"
    );
    assert_eq!((owner.retained(), f.files.retained()), (4, 4));
    owner.retire(&context, false);
    let mut next = context.clone();
    next.library_generation += 1;
    owner.ready(next).unwrap();
    for delivery in &deliveries {
        assert_eq!(
            delivery
                .receipt
                .observe(&cancel, deadline())
                .await
                .unwrap_err()
                .code,
            "cover_delivery_retired"
        );
        owner
            .complete(&context, &delivery.metadata.id, true)
            .unwrap();
        assert!(owner
            .complete(&context, &delivery.metadata.id, true)
            .is_err());
        assert_eq!(
            delivery
                .receipt
                .observe(&cancel, deadline())
                .await
                .unwrap_err()
                .code,
            "cover_delivery_retired",
            "late ACK does not undo retirement"
        );
    }
    assert_eq!((owner.retained(), f.files.retained()), (4, 4));
    let last_sequence = deliveries
        .last()
        .unwrap()
        .metadata
        .sequence
        .parse::<u64>()
        .unwrap();
    drop(deliveries.pop());
    assert_eq!((owner.retained(), f.files.retained()), (3, 3));
    owner.ready(context.clone()).unwrap();
    let fresh = owner
        .publish_for_test(f.observation().await, &cancel, deadline())
        .unwrap();
    assert!(fresh.metadata.sequence.parse::<u64>().unwrap() > last_sequence);
    assert_eq!((owner.retained(), f.files.retained()), (4, 4));
    let fresh_id = fresh.metadata.id.clone();
    drop(fresh);
    drop(deliveries);
    assert_eq!((owner.retained(), f.files.retained()), (1, 0));
    owner.retire(&context, false);
    assert_eq!((owner.retained(), f.files.retained()), (1, 0));
    owner.complete(&context, &fresh_id, false).unwrap();
    assert_eq!((owner.retained(), f.files.retained()), (0, 0));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropped_waiter_cancellation_rejection_and_retirement_never_forge_delivery() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(true);
    let context = f.library.context().unwrap();
    let owner = owner(context.clone());
    let cancel = CancellationToken::new();
    let delivery = owner
        .publish_for_test(f.observation().await, &cancel, deadline())
        .unwrap();
    {
        let mut observation = Box::pin(delivery.receipt.observe(&cancel, deadline()));
        std::future::poll_fn(|cx| {
            assert!(observation.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    assert_eq!((owner.retained(), f.files.retained()), (1, 1));
    cancel.cancel();
    assert_eq!(
        delivery
            .receipt
            .observe(&cancel, deadline())
            .await
            .unwrap_err()
            .code,
        "cover_delivery_cancelled"
    );
    delivery.receipt.reject();
    assert_eq!(
        delivery
            .receipt
            .observe(&CancellationToken::new(), deadline())
            .await
            .unwrap_err()
            .code,
        "cover_delivery_rejected"
    );
    let late = f.observation().await;
    owner.retire(&context, false);
    assert!(owner
        .publish(late, &CancellationToken::new(), deadline(), |_| panic!(
            "Terminal owner cannot emit"
        ))
        .is_err());
    assert_eq!((owner.retained(), f.files.retained()), (1, 1));
    drop(delivery);
    assert_eq!((owner.retained(), f.files.retained()), (0, 0));
}

#[cfg(target_os = "linux")]
struct TestDelivery {
    event: freeshow_core::protocol::Event,
    metadata: publications::Metadata,
    receipt: Receipt,
}
#[cfg(target_os = "linux")]
trait PublishForTest {
    fn publish_for_test(
        &self,
        observation: Observation,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> publications::Result<TestDelivery>;
}
#[cfg(target_os = "linux")]
impl PublishForTest for Publications {
    fn publish_for_test(
        &self,
        observation: Observation,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> publications::Result<TestDelivery> {
        let mut captured: Option<Delivery> = None;
        let receipt = self.publish(observation, cancel, deadline, |delivery| {
            captured = Some(delivery.clone());
            true
        })?;
        let Delivery { event, metadata } = captured.unwrap();
        Ok(TestDelivery {
            event,
            metadata,
            receipt,
        })
    }
}

fn owner(context: freeshow_core::protocol::RuntimeContext) -> Publications {
    let owner = Publications::new();
    owner.ready(context.clone()).unwrap();
    owner
}

#[test]
fn terminal_close_is_process_lifetime_and_replacement_cannot_reopen_it() {
    const CHILD: &str = "FREESHOW_COVER_PUBLICATION_TERMINAL_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "terminal_close_is_process_lifetime_and_replacement_cannot_reopen_it",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let f = Fixture::new(cfg!(target_os = "linux"));
    let context = f.library.context().unwrap();
    let first = owner(context.clone());
    #[cfg(target_os = "linux")]
    let delivery = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            first
                .publish_for_test(f.observation().await, &CancellationToken::new(), deadline())
                .unwrap()
        });
    #[cfg(target_os = "linux")]
    assert_eq!((first.retained(), f.files.retained()), (1, 1));
    first.retire(&context, true);
    #[cfg(target_os = "linux")]
    {
        assert_eq!((first.retained(), f.files.retained()), (1, 1));
        drop(delivery);
        assert_eq!((first.retained(), f.files.retained()), (0, 0));
    }
    assert!(first.ready(context.clone()).is_err());
    assert!(Publications::new().ready(context).is_err());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn actual_enqueue_order_is_global_across_replacement_handles_and_blocked_emitters() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(true);
    let context = f.library.context().unwrap();
    let owner = owner(context.clone());
    let first = f.observation().await;
    let second = f.observation().await;
    let events = std::sync::Mutex::new(Vec::new());
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let (attempt_tx, attempt_rx) = std::sync::mpsc::sync_channel(1);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    let (a, b) = std::thread::scope(|scope| {
        let owner_ref = &owner;
        let events_ref = &events;
        let a = scope.spawn(move || {
            owner_ref
                .publish(first, &CancellationToken::new(), deadline(), |delivery| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    events_ref.lock().unwrap().push(delivery.clone());
                    true
                })
                .unwrap()
        });
        entered_rx.recv().unwrap();
        let b = scope.spawn(|| {
            let replacement = Publications::new();
            attempt_tx.send(()).unwrap();
            let receipt = replacement
                .publish(second, &CancellationToken::new(), deadline(), |delivery| {
                    events.lock().unwrap().push(delivery.clone());
                    true
                })
                .unwrap();
            done_tx.send(()).unwrap();
            receipt
        });
        attempt_rx.recv().unwrap();
        let second_held = matches!(
            done_rx.recv_timeout(Duration::from_millis(30)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
        let enqueues_held = events.lock().unwrap().is_empty();
        release_tx.send(()).unwrap();
        let both = (a.join().unwrap(), b.join().unwrap());
        assert!(
            second_held,
            "second synchronous enqueue must remain behind the held first emitter"
        );
        assert!(enqueues_held, "first actual enqueue is still held");
        both
    });
    let events = events.into_inner().unwrap();
    assert_eq!(events.len(), 2);
    assert!(
        events[0].metadata.sequence.parse::<u64>().unwrap()
            < events[1].metadata.sequence.parse::<u64>().unwrap()
    );
    for delivery in events {
        assert_eq!(delivery.event.message.channel, "ALERT");
        owner
            .complete(&context, &delivery.metadata.id, true)
            .unwrap();
        assert!(owner
            .complete(&context, &delivery.metadata.id, true)
            .is_err());
    }
    a.observe(&CancellationToken::new(), deadline())
        .await
        .unwrap();
    b.observe(&CancellationToken::new(), deadline())
        .await
        .unwrap();
    drop((a, b));
    assert_eq!((owner.retained(), f.files.retained()), (0, 0));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn unconfirmed_ui_delivery_keeps_global_quota_after_actual_fd_disposal_and_handle_replacement(
) {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(true);
    let context = f.library.context().unwrap();
    let owner = owner(context.clone());
    let mut ids = Vec::new();
    for _ in 0..4 {
        let delivery = owner
            .publish_for_test(f.observation().await, &CancellationToken::new(), deadline())
            .unwrap();
        assert_eq!(
            delivery
                .receipt
                .observe(&CancellationToken::new(), Instant::now())
                .await
                .unwrap_err()
                .code,
            "cover_delivery_timeout"
        );
        ids.push(delivery.metadata.id.clone());
        drop(delivery);
    }
    assert_eq!(
        (owner.retained(), f.files.retained()),
        (4, 0),
        "only actual IO ownership has ended"
    );
    drop(owner);
    let replacement = Publications::new();
    assert_eq!(replacement.retained(), 4);
    let error = replacement
        .publish(
            f.observation().await,
            &CancellationToken::new(),
            deadline(),
            |_| panic!("Lost-response quota cannot be reused"),
        )
        .err()
        .unwrap();
    assert_eq!(error.code, "cover_publication_busy");
    assert_eq!(f.files.retained(), 0);
    replacement.complete(&context, &ids[0], true).unwrap();
    assert!(replacement.complete(&context, &ids[0], true).is_err());
    assert_eq!(replacement.retained(), 3);
    let rejected = replacement
        .publish(
            f.observation().await,
            &CancellationToken::new(),
            deadline(),
            |_| false,
        )
        .unwrap();
    assert_eq!(replacement.retained(), 4);
    assert_eq!(
        rejected
            .observe(&CancellationToken::new(), deadline())
            .await
            .unwrap_err()
            .code,
        "cover_delivery_rejected"
    );
    drop(rejected);
    assert_eq!(
        replacement.retained(),
        3,
        "known enqueue failure disposes its slot"
    );
    replacement.retire(&context, false);
    assert_eq!(
        replacement.retained(),
        3,
        "context retirement is not queued WebView disposal"
    );
    let mut next = context.clone();
    next.library_generation += 1;
    replacement.ready(next).unwrap();
    assert_eq!(
        replacement.retained(),
        3,
        "context replacement cannot multiply unknown queued deliveries"
    );
    for id in &ids[1..] {
        replacement.complete(&context, id, false).unwrap();
    }
    assert_eq!(
        replacement.retained(),
        0,
        "late exact-context private ACKs prove actual receipt disposal"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn retirement_then_late_ack_between_atomic_reads_cannot_revive_delivery() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(true);
    let context = f.library.context().unwrap();
    let owner = owner(context.clone());
    let delivery = owner
        .publish_for_test(f.observation().await, &CancellationToken::new(), deadline())
        .unwrap();
    let result = delivery
        .receipt
        .observe_state_interleaved(|| {
            owner.retire(&context, false);
            owner
                .complete(&context, &delivery.metadata.id, true)
                .unwrap();
        })
        .unwrap();
    assert_eq!(result.unwrap_err().code, "cover_delivery_retired");
    drop(delivery);
    assert_eq!((owner.retained(), f.files.retained()), (0, 0));
}
