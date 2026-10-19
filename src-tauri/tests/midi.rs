#[path = "../src/midi.rs"]
mod midi;
// Existing bounded controlled-handle seam, never a product backend.
#[allow(dead_code)]
#[path = "../../crates/freeshow-midi-ports/src/controlled.rs"]
mod controlled;
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_midi_ports::{Backend, Device, Direction, Error, Input, InputCallback, Output};
use midi::{MidiHostConfig, NativeMidi, NativeMidiPublication};
use serde_json::{json, Value};
use std::{
    future::{poll_fn, Future},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::Poll,
    time::Duration,
};
use tokio::{sync::mpsc, time::timeout};
type Result<T> = freeshow_midi_ports::Result<T>;
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
#[derive(Default)]
struct Counts {
    created: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
    enumerations: AtomicUsize,
    unavailable: AtomicBool,
    panic: AtomicBool,
}
struct Observed {
    control: controlled::Controlled,
    counts: Arc<Counts>,
}
impl Drop for Observed {
    fn drop(&mut self) {
        self.counts.active.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Backend for Observed {
    fn devices(&mut self, direction: Direction) -> Result<Vec<Device>> {
        self.counts.enumerations.fetch_add(1, Ordering::AcqRel);
        assert!(
            !self.counts.panic.load(Ordering::Acquire),
            "controlled actual native MIDI worker panic"
        );
        if self.counts.unavailable.load(Ordering::Acquire) {
            return Err(Error::new(
                "midi_backend_unavailable",
                "controlled denied native MIDI backend",
            ));
        }
        self.control.devices(direction)
    }
    fn input(&mut self, device: &Device, callback: InputCallback) -> Result<Box<dyn Input>> {
        self.control.input(device, callback)
    }
    fn output(&mut self, device: &Device) -> Result<Box<dyn Output>> {
        self.control.output(device)
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    library: LibraryService,
    native: NativeMidi,
    control: controlled::Controlled,
    counts: Arc<Counts>,
    events: mpsc::Receiver<NativeMidiPublication>,
}
impl Fixture {
    fn new(config: MidiHostConfig) -> Self {
        Self::with_sink(config, None)
    }
    fn with_sink(config: MidiHostConfig, sink_error: Option<&'static str>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
        )
        .unwrap();
        let control = controlled::Controlled::new(
            vec![
                Device {
                    id: "in1".into(),
                    name: "Keys 日本語".into(),
                },
                Device {
                    id: "in2".into(),
                    name: "Pads".into(),
                },
            ],
            vec![
                Device {
                    id: "out1".into(),
                    name: "Output 🎹".into(),
                },
                Device {
                    id: "out2".into(),
                    name: "duplicate".into(),
                },
                Device {
                    id: "out3".into(),
                    name: "duplicate".into(),
                },
            ],
        );
        let counts = Arc::new(Counts::default());
        let provider_counts = counts.clone();
        let provider = control.clone();
        let (send, events) = mpsc::channel(1);
        let native = NativeMidi::new(
            library.clone(),
            config,
            move || {
                provider_counts.created.fetch_add(1, Ordering::AcqRel);
                let n = provider_counts.active.fetch_add(1, Ordering::AcqRel) + 1;
                provider_counts.max_active.fetch_max(n, Ordering::AcqRel);
                Ok(Box::new(Observed {
                    control: provider.clone(),
                    counts: provider_counts.clone(),
                }))
            },
            move |publication| {
                if let Some(error) = sink_error {
                    assert!(error != "panic", "controlled native MIDI publisher panic");
                    return Err(error.into());
                }
                send.try_send(publication).map_err(|e| e.to_string())
            },
        )
        .unwrap();
        Self {
            temp,
            library,
            native,
            control,
            counts,
            events,
        }
    }
    fn context(&self) -> RuntimeContext {
        self.library.context().unwrap()
    }
    async fn dispatch(
        &self,
        command: &str,
        payload: Value,
    ) -> std::result::Result<Vec<NativeMidiPublication>, freeshow_core::WireError> {
        self.native
            .dispatch(
                request(command, Some(payload), Some(&self.context())),
                "main",
            )
            .await
    }
    async fn listen(&self, id: Value, input: &str) {
        assert!(self
            .dispatch("RECEIVE_MIDI", json!({"id":id,"input":input}))
            .await
            .unwrap()
            .is_empty());
    }
    async fn event(&mut self) -> NativeMidiPublication {
        timeout(Duration::from_secs(3), self.events.recv())
            .await
            .unwrap()
            .unwrap()
    }
    fn complete(&self, event: &NativeMidiPublication) {
        self.native
            .complete("main", &event.delivery.as_ref().unwrap().id, true)
            .unwrap();
    }
    async fn switch(&self) -> RuntimeContext {
        let old = self.context();
        let path = self
            .temp
            .path()
            .join(format!("next-{}", old.library_generation));
        std::fs::create_dir(&path).unwrap();
        let request:Request=serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":old.data_path,"newPath":path}},"context":old})).unwrap();
        self.library.dispatch(request, "main").await.unwrap();
        assert_ne!(self.context(), old);
        self.context()
    }
}
fn request(command: &str, payload: Option<Value>, context: Option<&RuntimeContext>) -> Request {
    let mut message = json!({"channel":command});
    if let Some(payload) = payload {
        message["data"] = payload;
    }
    serde_json::from_value(json!({"channel":"MAIN","data":message,"listenerId":"midi-listener-敬拜","context":context})).unwrap()
}
async fn until(mut test: impl FnMut() -> bool) {
    timeout(Duration::from_secs(3), async {
        while !test() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
fn quick() -> MidiHostConfig {
    MidiHostConfig {
        lifecycle_timeout: Duration::from_millis(35),
        ports: freeshow_midi_ports::Config {
            shutdown_timeout: Duration::from_millis(25),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn source_getters_void_commands_and_callback_hydration_remain_separate() {
    let mut fixture = Fixture::new(Default::default());
    let context = fixture.context();
    assert_eq!(fixture.counts.created.load(Ordering::Acquire), 0);
    let replies = fixture
        .dispatch("GET_MIDI_OUTPUTS", json!({"ignored":"path/port/device"}))
        .await
        .unwrap();
    assert_eq!(replies.len(), 1);
    let reply = &replies[0];
    assert_eq!(reply.event.channel, "MAIN");
    assert_eq!(reply.event.message.channel, "GET_MIDI_OUTPUTS");
    assert_eq!(
        reply.event.listener_id.as_deref(),
        Some("midi-listener-敬拜")
    );
    assert_eq!(reply.event.context.as_ref(), Some(&context));
    assert!(reply.delivery.is_none());
    assert_eq!(
        reply.event.message.data,
        Some(json!([{"name":"Output 🎹"},{"name":"duplicate"},{"name":"duplicate"}]))
    );
    assert!(reply.hydration.undefined_paths.is_empty());
    assert!(reply.event.native_exif.is_none());
    assert_eq!(
        fixture
            .dispatch("GET_MIDI_INPUTS", Value::Null)
            .await
            .unwrap()[0]
            .event
            .message
            .data,
        Some(json!([{"name":"Keys 日本語"},{"name":"Pads"}]))
    );
    assert!(fixture
        .dispatch(
            "SEND_MIDI",
            json!({"output":"Output 🎹","values":{"channel":1,"note":60,"velocity":64}})
        )
        .await
        .unwrap()
        .is_empty());
    assert!(fixture
        .control
        .trace()
        .contains(&json!(["send", "Output 🎹", [144, 60, 64]])));
    assert!(fixture
        .dispatch("RECEIVE_MIDI", json!({"input":"Keys 日本語"}))
        .await
        .unwrap()
        .is_empty());
    fixture.control.emit("in1", &[0x90, 60, 1]);
    let received = fixture.event().await;
    assert_eq!(received.event.message.channel, "RECEIVE_MIDI2");
    assert_eq!(received.event.listener_id, None);
    assert_eq!(received.event.context.as_ref(), Some(&context));
    assert!(received
        .event
        .message
        .data
        .as_ref()
        .unwrap()
        .get("id")
        .is_none());
    assert_eq!(received.hydration.undefined_paths, vec!["/id"]);
    assert_eq!(
        received.event.message.data.as_ref().unwrap()["values"]["channel"],
        1
    );
    assert_eq!(received.delivery.as_ref().unwrap().context, context);
    assert!(received.delivery.as_ref().unwrap().listener > 0);
    assert_eq!(
        fixture
            .native
            .complete("output", &received.delivery.as_ref().unwrap().id, true)
            .unwrap_err()
            .code,
        "window_error"
    );
    assert_eq!(
        fixture
            .native
            .complete("main", "wrong", true)
            .unwrap_err()
            .code,
        "invalid_midi_completion"
    );
    fixture.complete(&received);
    assert_eq!(
        fixture
            .native
            .complete("main", &received.delivery.as_ref().unwrap().id, true)
            .unwrap_err()
            .code,
        "invalid_midi_completion"
    );
    let old_callback = fixture.control.callback("in1").unwrap();
    assert!(fixture
        .dispatch("CLOSE_MIDI", json!({}))
        .await
        .unwrap()
        .is_empty());
    old_callback(1, &[0x90, 60, 1]);
    assert!(fixture.events.try_recv().is_err());
    let enumerations = fixture.counts.enumerations.load(Ordering::Acquire);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        fixture.counts.enumerations.load(Ordering::Acquire),
        enumerations,
        "host adds no backend enumeration timer"
    );
    fixture.native.shutdown().await.unwrap();
    assert_eq!(fixture.counts.active.load(Ordering::Acquire), 0);
}
#[tokio::test]
async fn deferred_real_handle_open_survives_caller_drop_and_close_fences_private_delivery() {
    let mut fixture = Fixture::new(Default::default());
    fixture.control.block("open");
    let native = fixture.native.clone();
    let context = fixture.context();
    let caller = tokio::spawn(async move {
        native
            .dispatch(
                request(
                    "RECEIVE_MIDI",
                    Some(json!({"id":"listener","input":"Keys 日本語"})),
                    Some(&context),
                ),
                "main",
            )
            .await
    });
    until(|| fixture.control.entered()).await;
    let early = fixture.control.callback("in1").unwrap();
    early(1, &[0x90, 60, 1]);
    caller.abort();
    let _ = caller.await;
    assert_eq!(fixture.native.status()["admittedRequests"], 1);
    fixture.control.release();
    until(|| fixture.native.status()["admittedRequests"] == 0).await;
    assert!(fixture.events.try_recv().is_err());
    fixture.control.emit("in1", &[0x90, 60, 1]);
    let received = fixture.event().await;
    let native = fixture.native.clone();
    let context = fixture.context();
    let close = tokio::spawn(async move {
        native
            .dispatch(
                request("CLOSE_MIDI", Some(json!({"id":"listener"})), Some(&context)),
                "main",
            )
            .await
    });
    until(|| fixture.native.status()["binding"]["service"]["inputs"] == 0).await;
    assert!(
        !close.is_finished(),
        "CLOSE's internal completion fences accepted callback dispatch"
    );
    fixture.complete(&received);
    assert!(close.await.unwrap().unwrap().is_empty());
    early(1, &[0x90, 60, 1]);
    assert!(fixture.events.try_recv().is_err());
    fixture.native.shutdown().await.unwrap();
}
#[tokio::test]
async fn post_open_send_deadline_retains_actual_effect_and_terminal_retry_joins_it() {
    let fixture = Fixture::new(MidiHostConfig {
        request_timeout: Duration::from_millis(45),
        ..quick()
    });
    fixture.control.block("send");
    let native = fixture.native.clone();
    let context = fixture.context();
    let caller = tokio::spawn(async move {
        native
            .dispatch(
                request(
                    "SEND_MIDI",
                    Some(
                        json!({"output":"Output 🎹","values":{"channel":1,"note":60,"velocity":1}}),
                    ),
                    Some(&context),
                ),
                "main",
            )
            .await
    });
    until(|| fixture.control.entered()).await;
    assert_eq!(caller.await.unwrap().unwrap_err().code, "midi_deadline");
    assert_eq!(fixture.native.status()["admittedRequests"], 1);
    assert_eq!(
        fixture.native.shutdown().await.unwrap_err().code,
        "midi_shutdown_timeout"
    );
    assert_eq!(fixture.counts.active.load(Ordering::Acquire), 1);
    assert_eq!(fixture.native.status()["teardown"]["complete"], false);
    fixture.control.release();
    until(|| fixture.counts.active.load(Ordering::Acquire) == 0).await;
    fixture.native.shutdown().await.unwrap();
    assert_eq!(fixture.counts.created.load(Ordering::Acquire), 1);
    assert!(
        fixture
            .control
            .trace()
            .contains(&json!(["send", "Output 🎹", [144, 60, 1]])),
        "already-entered native write cannot be undone"
    );
    assert_eq!(fixture.native.status()["admittedRequests"], 0);
}
#[tokio::test]
async fn stale_context_closes_and_drains_old_stream_before_any_replacement() {
    let mut fixture = Fixture::new(quick());
    fixture.listen(json!("old"), "Keys 日本語").await;
    fixture.control.emit("in1", &[0x90, 60, 1]);
    let old_event = fixture.event().await;
    let old_context = fixture.context();
    let old_run = fixture.native.status()["binding"]["run"].clone();
    let old_callback = fixture.control.callback("in1").unwrap();
    fixture.control.block("close");
    let next = fixture.switch().await;
    assert_eq!(
        fixture.native.context_changed().await.unwrap_err().code,
        "midi_shutdown_timeout"
    );
    until(|| fixture.control.entered()).await;
    assert_eq!(fixture.counts.created.load(Ordering::Acquire), 1);
    assert_eq!(
        fixture.native.status()["eventStreamRetained"],
        true,
        "retirement must retain the consumer while its real producer is blocked"
    );
    assert_eq!(
        fixture
            .native
            .complete("main", &old_event.delivery.unwrap().id, true)
            .unwrap_err()
            .code,
        "invalid_midi_completion"
    );
    old_callback(2, &[0x90, 61, 1]);
    assert!(fixture.events.try_recv().is_err());
    assert_eq!(
        fixture
            .native
            .dispatch(
                request("GET_MIDI_INPUTS", Some(Value::Null), Some(&old_context)),
                "main"
            )
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    let native = fixture.native.clone();
    let fresh = next.clone();
    let waiting = tokio::spawn(async move {
        native
            .dispatch(
                request("GET_MIDI_INPUTS", Some(Value::Null), Some(&fresh)),
                "main",
            )
            .await
    });
    tokio::task::yield_now().await;
    assert_eq!(fixture.counts.created.load(Ordering::Acquire), 1);
    fixture.control.release();
    let replies = waiting.await.unwrap().unwrap();
    assert_eq!(replies[0].event.context.as_ref(), Some(&next));
    assert_ne!(fixture.native.status()["binding"]["run"], old_run);
    assert_eq!(fixture.counts.max_active.load(Ordering::Acquire), 1);
    let current_run = fixture.native.status()["binding"]["run"].clone();
    fixture.native.context_changed().await.unwrap();
    assert_eq!(
        fixture.native.status()["binding"]["run"],
        current_run,
        "a delayed context hook cannot retire a current run"
    );
    // Bridge rejection of an already-emitted old-context callback can race the
    // native context hook. It must not fault the new library's MIDI capability.
    fixture.listen(json!("second-context"), "Keys 日本語").await;
    fixture.control.emit("in1", &[0x90, 62, 1]);
    let retiring_event = fixture.event().await;
    fixture.switch().await;
    fixture
        .native
        .complete("main", &retiring_event.delivery.unwrap().id, false)
        .unwrap();
    until(|| fixture.native.status()["pendingDeliveries"] == 0).await;
    assert_eq!(fixture.native.status()["closing"], false);
    fixture.native.context_changed().await.unwrap();
    assert_eq!(fixture.native.status()["eventStreamRetained"], false);
    fixture.native.shutdown().await.unwrap();
}
#[tokio::test]
async fn input_window_context_and_shape_errors_cannot_touch_backend() {
    let fixture = Fixture::new(Default::default());
    let current = fixture.context();
    let mut old = current.clone();
    old.runtime_id.push('x');
    for context in [None, Some(&old)] {
        assert_eq!(
            fixture
                .native
                .dispatch(
                    request("GET_MIDI_INPUTS", Some(Value::Null), context),
                    "main"
                )
                .await
                .unwrap_err()
                .code,
            "stale_library"
        );
    }
    assert_eq!(
        fixture
            .native
            .dispatch(request("GET_MIDI_INPUTS", None, Some(&current)), "output")
            .await
            .unwrap_err()
            .code,
        "window_error"
    );
    assert_eq!(
        fixture
            .native
            .dispatch(request("MTC", None, Some(&current)), "main")
            .await
            .unwrap_err()
            .code,
        "unsupported_command"
    );
    assert_eq!(
        fixture
            .dispatch("GET_MIDI_INPUTS", json!("x".repeat(65537)))
            .await
            .unwrap_err()
            .code,
        "midi_host_limit"
    );
    let mut value = Value::Null;
    for _ in 0..12 {
        value = json!([value]);
    }
    assert_eq!(
        fixture
            .dispatch("GET_MIDI_INPUTS", value)
            .await
            .unwrap_err()
            .code,
        "midi_host_limit"
    );
    for payload in [None, Some(Value::Null)] {
        assert_eq!(
            fixture
                .native
                .dispatch(request("CLOSE_MIDI", payload, Some(&current)), "main")
                .await
                .unwrap_err()
                .code,
            "midi_invalid"
        );
    }
    assert_eq!(fixture.counts.created.load(Ordering::Acquire), 0);
    fixture.native.shutdown().await.unwrap();
}
#[tokio::test]
async fn ready_uncollected_replies_keep_all_sixteen_host_admissions() {
    let fixture = Fixture::new(Default::default());
    let mut pending = Vec::new();
    for _ in 0..16 {
        let native = fixture.native.clone();
        let context = fixture.context();
        let mut future = Box::pin(async move {
            native
                .dispatch(
                    request("GET_MIDI_INPUTS", Some(Value::Null), Some(&context)),
                    "main",
                )
                .await
        });
        assert!(poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx)))
            .await
            .is_pending());
        pending.push(future);
    }
    until(|| fixture.counts.enumerations.load(Ordering::Acquire) == 16).await;
    assert_eq!(fixture.native.status()["admittedRequests"], 16);
    assert_eq!(
        fixture
            .dispatch("GET_MIDI_INPUTS", Value::Null)
            .await
            .unwrap_err()
            .code,
        "midi_host_busy"
    );
    drop(pending.pop());
    until(|| fixture.native.status()["admittedRequests"] == 15).await;
    assert_eq!(pending.pop().unwrap().await.unwrap().len(), 1);
    assert_eq!(fixture.native.status()["admittedRequests"], 14);
    fixture.switch().await;
    assert_eq!(
        pending.pop().unwrap().await.unwrap_err().code,
        "stale_library",
        "a ready old-context reply is rejected when finally collected"
    );
    fixture.native.context_changed().await.unwrap();
    drop(pending);
    fixture.native.shutdown().await.unwrap();
}
#[tokio::test]
async fn one_private_delivery_bounds_callback_queue_and_terminal_cancellation_bypasses_it() {
    let mut fixture = Fixture::new(Default::default());
    fixture.listen(json!(7), "Keys 日本語").await;
    fixture.control.emit("in1", &[0x90, 60, 1]);
    let first = fixture.event().await;
    for _ in 0..16 {
        fixture.control.emit("in1", &[0x90, 61, 1]);
    }
    assert_eq!(
        fixture
            .dispatch("GET_MIDI_OUTPUTS", Value::Null)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(fixture.native.status()["pendingDeliveries"], 1);
    assert!(fixture.events.try_recv().is_err());
    fixture.native.shutdown().await.unwrap();
    assert_eq!(fixture.native.status()["pendingDeliveries"], 0);
    assert_eq!(
        fixture
            .native
            .complete("main", &first.delivery.unwrap().id, true)
            .unwrap_err()
            .code,
        "invalid_midi_completion"
    );
    assert!(fixture.events.try_recv().is_err());
    assert_eq!(fixture.counts.active.load(Ordering::Acquire), 0);
}
#[tokio::test]
async fn ready_private_rejection_cannot_be_starved_by_queued_native_effects() {
    let mut fixture = Fixture::new(Default::default());
    fixture
        .listen(json!("receipt-priority"), "Keys 日本語")
        .await;
    fixture.control.emit("in1", &[0x90, 60, 1]);
    let callback = fixture.event().await;
    // Admit a real write without yielding this current-thread Tokio task to the
    // owner. Both branches are ready when it next runs; receipt must win.
    let mut queued = Box::pin(fixture.dispatch(
        "SEND_MIDI",
        json!({"output":"Output 🎹","values":{"channel":1,"note":60,"velocity":1}}),
    ));
    poll_fn(|cx| {
        assert!(queued.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    fixture
        .native
        .complete("main", &callback.delivery.unwrap().id, false)
        .unwrap();
    assert!(queued.await.is_err());
    assert_eq!(
        fixture.native.shutdown().await.unwrap_err().code,
        "midi_delivery_rejected"
    );
    assert!(fixture
        .control
        .trace()
        .iter()
        .all(|row| row[0] != "send" && !(row[0] == "open" && row[1] == "out")));
    assert_eq!(fixture.counts.active.load(Ordering::Acquire), 0);
}
#[tokio::test]
async fn callback_overflow_is_explicit_sticky_and_drains_native_handles() {
    let mut fixture = Fixture::new(Default::default());
    fixture.listen(json!("overflow"), "Keys 日本語").await;
    fixture.control.emit("in1", &[0x90, 60, 1]);
    let _first = fixture.event().await;
    let callback = fixture.control.callback("in1").unwrap();
    for _ in 0..2000 {
        callback(1, &[0x90, 61, 1]);
    }
    until(|| fixture.native.status()["closing"] == true).await;
    let first = fixture.native.shutdown().await.unwrap_err();
    assert!(matches!(
        first.code,
        "midi_input_overflow" | "midi_event_overflow"
    ));
    assert_eq!(
        fixture.native.shutdown().await.unwrap_err().message,
        first.message
    );
    assert_eq!(fixture.counts.active.load(Ordering::Acquire), 0);
    assert_eq!(fixture.counts.created.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn private_delivery_rejection_timeout_and_sink_failure_all_reclaim_handles() {
    for mode in 0..4 {
        let mut fixture = Fixture::with_sink(
            MidiHostConfig {
                delivery_timeout: Duration::from_millis(35),
                ..Default::default()
            },
            match mode {
                2 => Some("controlled bounded publisher full"),
                3 => Some("panic"),
                _ => None,
            },
        );
        fixture.listen(json!("listener"), "Keys 日本語").await;
        fixture.control.emit("in1", &[0x90, 60, 1]);
        if mode < 2 {
            let event = fixture.event().await;
            if mode == 0 {
                fixture
                    .native
                    .complete("main", &event.delivery.unwrap().id, false)
                    .unwrap();
            }
        }
        until(|| fixture.native.status()["closing"] == true).await;
        let first = fixture.native.shutdown().await.unwrap_err();
        assert_eq!(
            first.code,
            [
                "midi_delivery_rejected",
                "midi_delivery_timeout",
                "midi_publication",
                "midi_host_worker"
            ][mode]
        );
        assert_eq!(fixture.counts.active.load(Ordering::Acquire), 0);
        assert_eq!(fixture.native.status()["pendingDeliveries"], 0);
    }
}
#[tokio::test]
async fn unavailable_backend_is_error_and_specific_close_quirks_use_existing_ports_contract() {
    let fixture = Fixture::new(Default::default());
    fixture.counts.unavailable.store(true, Ordering::Release);
    assert_eq!(
        fixture
            .dispatch("GET_MIDI_INPUTS", Value::Null)
            .await
            .unwrap_err()
            .code,
        "midi_backend_unavailable"
    );
    fixture.counts.unavailable.store(false, Ordering::Release);
    assert_eq!(
        fixture
            .dispatch("GET_MIDI_INPUTS", Value::Null)
            .await
            .unwrap()[0]
            .event
            .message
            .data
            .as_ref()
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    fixture.listen(json!(7), "Keys 日本語").await;
    fixture.listen(json!(8), "Pads").await;
    fixture
        .dispatch("CLOSE_MIDI", json!({"id":7}))
        .await
        .unwrap();
    assert_eq!(fixture.native.status()["binding"]["service"]["inputs"], 1);
    fixture
        .dispatch("CLOSE_MIDI", json!({"id":"unknown-closes-all"}))
        .await
        .unwrap();
    assert_eq!(fixture.native.status()["binding"]["service"]["inputs"], 0);
    fixture.native.shutdown().await.unwrap();
}
#[tokio::test]
async fn terminal_close_failure_is_sticky_but_other_handles_and_owner_still_drain() {
    let fixture = Fixture::new(Default::default());
    fixture.listen(json!("first"), "Keys 日本語").await;
    fixture.listen(json!("second"), "Pads").await;
    fixture
        .dispatch("SEND_MIDI", json!({"output":"Output 🎹"}))
        .await
        .unwrap();
    fixture.control.fail_close("Keys 日本語");
    assert_eq!(
        fixture
            .dispatch("CLOSE_MIDI", json!({}))
            .await
            .unwrap_err()
            .code,
        "controlled_close"
    );
    let first = fixture.native.shutdown().await.unwrap_err();
    assert_eq!(first.code, "controlled_close");
    assert_eq!(
        fixture.native.shutdown().await.unwrap_err().message,
        first.message
    );
    assert_eq!(
        fixture.native.context_changed().await.unwrap_err().code,
        first.code
    );
    assert_eq!(fixture.counts.active.load(Ordering::Acquire), 0);
    assert_eq!(fixture.counts.created.load(Ordering::Acquire), 1);
    assert!(fixture
        .control
        .trace()
        .contains(&json!(["close", "in", "Pads"])));
    assert!(fixture
        .control
        .trace()
        .contains(&json!(["close", "out", "Output 🎹"])));
}
#[tokio::test]
async fn actual_backend_worker_panic_never_reopens_or_turns_retry_into_success() {
    let fixture = Fixture::new(Default::default());
    fixture.listen(json!("listener"), "Keys 日本語").await;
    fixture.counts.panic.store(true, Ordering::Release);
    assert_eq!(
        fixture
            .dispatch("GET_MIDI_OUTPUTS", Value::Null)
            .await
            .unwrap_err()
            .code,
        "midi_worker"
    );
    let first = fixture.native.shutdown().await.unwrap_err();
    assert_eq!(first.code, "midi_worker");
    assert_eq!(
        fixture.native.shutdown().await.unwrap_err().message,
        first.message
    );
    assert_eq!(fixture.counts.active.load(Ordering::Acquire), 0);
    assert_eq!(fixture.counts.created.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn supervisor_keeps_timed_out_real_close_owned_until_explicit_shutdown_retry() {
    let fixture = Fixture::new(quick());
    fixture.listen(json!("held-close"), "Keys 日本語").await;
    fixture.control.block("close");
    let native = fixture.native.clone();
    let caller = tokio::spawn(async move { native.shutdown().await });
    until(|| fixture.control.entered()).await;
    caller.abort();
    let _ = caller.await;
    until(|| fixture.native.status()["teardown"]["error"].is_array()).await;
    assert_eq!(fixture.native.status()["teardown"]["complete"], false);
    assert_eq!(fixture.native.status()["eventStreamRetained"], true);
    assert_eq!(fixture.counts.active.load(Ordering::Acquire), 1);
    assert!(fixture.native.status()["binding"].is_object());
    assert_eq!(
        fixture.native.shutdown().await.unwrap_err().code,
        "midi_shutdown_timeout"
    );
    fixture.control.release();
    fixture.native.shutdown().await.unwrap();
    assert_eq!(fixture.native.status()["teardown"]["complete"], true);
    assert_eq!(fixture.native.status()["eventStreamRetained"], false);
    assert_eq!(fixture.counts.active.load(Ordering::Acquire), 0);
    assert_eq!(fixture.counts.created.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn output_counting_rejects_escaped_device_names_without_allocating_an_extra_wire_buffer() {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    let context = library.context().unwrap();
    let control = controlled::Controlled::new(
        (0..15)
            .map(|i| Device {
                id: i.to_string(),
                name: "\u{1}".repeat(4000),
            })
            .collect(),
        vec![],
    );
    let native = NativeMidi::new(
        library,
        Default::default(),
        move || Ok(Box::new(control.clone())),
        |_| Err("no callback is expected".into()),
    )
    .unwrap();
    assert_eq!(
        native
            .dispatch(
                request("GET_MIDI_INPUTS", Some(Value::Null), Some(&context)),
                "main"
            )
            .await
            .unwrap_err()
            .code,
        "midi_host_limit"
    );
    native.shutdown().await.unwrap();
}
