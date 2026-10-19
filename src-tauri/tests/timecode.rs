#[allow(dead_code)]
#[path = "../../crates/freeshow-midi-ports/src/controlled.rs"]
mod controlled;
#[path = "../src/timecode.rs"]
mod timecode;
use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_midi::Number;
use freeshow_midi_ports::{Backend, Device, Direction, Error, Input, InputCallback, Output};
use freeshow_timecode::{Command, Data, Scalar};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use timecode::*;
use tokio::{sync::mpsc, time::timeout};
use tokio_util::sync::CancellationToken;
type Result<T> = freeshow_midi_ports::Result<T>;
fn lock<T>(v: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    v.lock().unwrap_or_else(|e| e.into_inner())
}
#[derive(Default)]
struct Policy {
    block_next: Option<&'static str>,
    controls: Vec<controlled::Controlled>,
}
struct BackendObserved {
    control: controlled::Controlled,
    active: Arc<AtomicUsize>,
}
impl Drop for BackendObserved {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Backend for BackendObserved {
    fn devices(&mut self, d: Direction) -> Result<Vec<Device>> {
        self.control.devices(d)
    }
    fn input(&mut self, d: &Device, c: InputCallback) -> Result<Box<dyn Input>> {
        self.control.input(d, c)
    }
    fn output(&mut self, d: &Device) -> Result<Box<dyn Output>> {
        self.control.output(d)
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    library: LibraryService,
    native: NativeTimecode,
    policy: Arc<Mutex<Policy>>,
    active: Arc<AtomicUsize>,
    events: mpsc::Receiver<NativeTimecodePublication>,
}
impl Fixture {
    fn new(config: TimecodeHostConfig, real: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let library = LibraryService::start(
            Runtime::new(temp.path().join("app"), temp.path().join("data")).unwrap(),
        )
        .unwrap();
        let policy = Arc::new(Mutex::new(Policy::default()));
        let p = policy.clone();
        let active = Arc::new(AtomicUsize::new(0));
        let a = active.clone();
        let (send, events) = mpsc::channel(1);
        let root = std::path::PathBuf::from(file!());
        let root = if root.is_absolute() {
            root
        } else {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(root)
        };
        let root = root.parent().unwrap().parent().unwrap().parent().unwrap();
        let source = if real {
            root.join("node_modules/libltc-wrapper/libltc/bin/libltc.so.11")
        } else {
            temp.path().join("absent-ltc.so")
        };
        let native = NativeTimecode::new(
            library.clone(),
            config,
            freeshow_ltc::TrustedLibrary::from_host(source).unwrap(),
            move || {
                let control = controlled::Controlled::new(
                    vec![Device {
                        id: "in".into(),
                        name: "Input 日本語".into(),
                    }],
                    vec![Device {
                        id: "out".into(),
                        name: "Output 🎹".into(),
                    }],
                );
                let mut policy = lock(&p);
                if let Some(kind) = policy.block_next.take() {
                    control.block(kind);
                }
                policy.controls.push(control.clone());
                a.fetch_add(1, Ordering::AcqRel);
                Ok(Box::new(BackendObserved {
                    control,
                    active: a.clone(),
                }))
            },
            move |e| send.try_send(e).map_err(|e| e.to_string()),
        )
        .unwrap();
        Self {
            temp,
            library,
            native,
            policy,
            active,
            events,
        }
    }
    fn context(&self) -> RuntimeContext {
        self.library.context().unwrap()
    }
    fn controls(&self) -> Vec<controlled::Controlled> {
        lock(&self.policy).controls.clone()
    }
    fn block_next(&self, kind: &'static str) {
        lock(&self.policy).block_next = Some(kind);
    }
    async fn command(&self, c: Command) -> TimecodeReceipt {
        self.native
            .command(&self.context(), c, CancellationToken::new())
            .await
            .unwrap()
    }
    async fn start(&self, kind: &str, mode: &str, fps: f64) -> TimecodeReceipt {
        self.command(Command::Start {
            kind: kind.into(),
            mode: mode.into(),
            framerate: Scalar::number(fps),
            data: Data::Json(json!({"midiInput":"Input 日本語","midiOutput":"Output 🎹"})),
        })
        .await
    }
    async fn event(&mut self) -> NativeTimecodePublication {
        timeout(Duration::from_secs(5), self.events.recv())
            .await
            .unwrap()
            .unwrap()
    }
    fn ack(&self, e: &NativeTimecodePublication) {
        self.native
            .complete(
                "main",
                e.event.context.as_ref().unwrap(),
                &e.delivery.id,
                true,
            )
            .unwrap();
    }
    async fn drain(&mut self) -> std::result::Result<(), freeshow_core::WireError> {
        let native = self.native.clone();
        let join = tokio::spawn(async move { native.shutdown().await });
        tokio::pin!(join);
        loop {
            tokio::select! {result=&mut join=>return result.unwrap(),event=self.events.recv()=>{if let Some(e)=event{let _=self.native.complete("main",e.event.context.as_ref().unwrap(),&e.delivery.id,true);}}}
        }
    }
    async fn switch(&self) -> RuntimeContext {
        let old = self.context();
        let path = self
            .temp
            .path()
            .join(format!("new-{}", old.library_generation));
        std::fs::create_dir_all(&path).unwrap();
        let request:Request=serde_json::from_value(json!({"channel":"MAIN","context":old,"data":{"channel":"UPDATE_DATA_PATH","data":{"newPath":path,"oldPath":old.data_path}}})).unwrap();
        self.library.dispatch(request, "main").await.unwrap();
        self.library.context().unwrap()
    }
}
async fn wait(mut predicate: impl FnMut() -> bool) {
    timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn actual_controlled_mtc_callbacks_packets_and_reopenable_stop() {
    let mut f = Fixture::new(TimecodeHostConfig::default(), false);
    f.start("receive", "MTC", 25.0).await;
    wait(|| f.native.status()["router"]["ownedListeners"] == 1).await;
    let control = f.controls()[0].clone();
    control.emit("in", &[0xf2, 8, 0]);
    control.emit("in", &[0xfa]);
    let value = f.event().await;
    assert_eq!(value.event.message.channel, "TIMECODE_VALUE");
    assert_eq!(value.event.message.data, Some(json!(1000.0)));
    assert!(value.bytes.is_none());
    assert!(value.hydration.numbers.is_empty());
    f.ack(&value);
    let repeated = f.event().await;
    assert_eq!(repeated.event.message.data, Some(json!(1000.0)));
    f.ack(&repeated);
    let status = f.event().await;
    assert_eq!(status.event.message.channel, "TIMECODE_STATUS");
    assert_eq!(status.event.message.data, Some(json!("play")));
    f.ack(&status);
    f.start("send", "MTC", 29.97).await;
    let first = f
        .native
        .value(
            &f.context(),
            Number::new(3723040.0),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(first.context(), &f.context());
    assert!(!first.observations().is_empty());
    wait(|| {
        f.controls().len() == 2
            && f.controls()[1]
                .trace()
                .iter()
                .filter(|v| v[0] == "send")
                .count()
                == 8
    })
    .await;
    let sent = f.controls()[1]
        .trace()
        .into_iter()
        .filter(|v| v[0] == "send")
        .map(|v| v[2].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        sent,
        freeshow_midi::mtc_packets(3723040.0, 29.97)
            .iter()
            .map(|v| json!(v))
            .collect::<Vec<_>>()
    );
    f.command(Command::Stop).await;
    assert_eq!(f.active.load(Ordering::Acquire), 0);
    assert_eq!(f.native.status()["router"]["currentMode"], "MTC");
    f.native
        .value(&f.context(), Number::new(0.0), CancellationToken::new())
        .await
        .unwrap();
    wait(|| f.controls().len() == 3 && f.controls()[2].trace().len() >= 9).await;
    f.drain().await.unwrap();
    assert_eq!(f.active.load(Ordering::Acquire), 0);
}
#[tokio::test]
async fn unavailable_ltc_has_exact_sticky_constructor_alerts_and_preserves_flags_after_library_change(
) {
    let mut f = Fixture::new(TimecodeHostConfig::default(), false);
    f.start("receive", "LTC", 25.0).await;
    let alert = f.event().await;
    assert_eq!(alert.event.message.channel, "ALERT");
    let mut expected = "Could not initialize LTC Decoder!".to_owned();
    if cfg!(target_os = "windows") {
        expected.push_str("\n\nThis is likely because the required Visual C++ Redistributable is not installed.\nPlease install it from https://aka.ms/vs/17/release/vc_redist.x64.exe");
    }
    assert_eq!(alert.event.message.data, Some(json!(expected)));
    f.ack(&alert);
    f.start("receive", "LTC", 30.0).await;
    assert!(f.events.try_recv().is_err());
    assert_eq!(f.native.status()["router"]["decoderFailed"], true);
    f.start("send", "LTC", 25.0).await;
    f.native
        .value(&f.context(), Number::new(0.0), CancellationToken::new())
        .await
        .unwrap();
    let alert = f.event().await;
    assert!(alert
        .event
        .message
        .data
        .as_ref()
        .unwrap()
        .as_str()
        .unwrap()
        .starts_with("Could not initialize LTC Encoder!"));
    f.ack(&alert);
    f.command(Command::Stop).await;
    f.switch().await;
    f.native.context_changed().await.unwrap();
    assert_eq!(f.native.status()["router"]["decoderFailed"], true);
    assert_eq!(f.native.status()["router"]["encoderFailed"], true);
    assert_eq!(f.native.status()["router"]["currentMode"], "LTC");
    f.native
        .value(&f.context(), Number::new(1000.0), CancellationToken::new())
        .await
        .unwrap();
    assert!(f.events.try_recv().is_err());
    f.drain().await.unwrap();
}
#[tokio::test]
async fn pending_opens_survive_stop_and_caller_drop_and_overwrite_current_map_without_losing_orphans(
) {
    let mut f = Fixture::new(TimecodeHostConfig::default(), false);
    f.block_next("open");
    f.start("receive", "MTC", 25.0).await;
    wait(|| f.controls()[0].entered()).await;
    f.block_next("open");
    f.start("receive", "MTC", 30.0).await;
    wait(|| f.controls().len() == 2 && f.controls()[1].entered()).await;
    assert_eq!(f.native.status()["asyncEffects"], 2);
    assert_eq!(f.native.status()["router"]["receivers"], json!([]));
    f.command(Command::Stop).await;
    f.controls()[1].release();
    wait(|| f.native.status()["router"]["ownedListeners"] == 1).await;
    f.controls()[0].release();
    wait(|| f.native.status()["router"]["ownedListeners"] == 2).await;
    assert_eq!(f.native.status()["router"]["receivers"], json!(["MTC"]));
    f.command(Command::Stop).await;
    assert_eq!(f.active.load(Ordering::Acquire), 1);
    assert!(f.controls()[0].trace().iter().any(|v| v[0] == "close"));
    assert!(!f.controls()[1].trace().iter().any(|v| v[0] == "close"));
    f.drain().await.unwrap();
    assert_eq!(f.active.load(Ordering::Acquire), 0);
    assert!(f.controls()[1].trace().iter().any(|v| v[0] == "close"));
}

#[tokio::test]
async fn input_and_completed_result_quotas_raw_bounds_and_foreign_completions_are_explicit() {
    let mut f = Fixture::new(TimecodeHostConfig::default(), false);
    assert_eq!(COMMANDS.len(), 5);
    let c = f.context();
    let mut bad = c.clone();
    bad.runtime_id.push('x');
    assert_eq!(
        f.native
            .audio(&bad, "LTC", &[], 0, CancellationToken::new())
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    assert_eq!(
        f.native
            .audio(&c, "LTC", &[0], 2, CancellationToken::new())
            .await
            .unwrap_err()
            .code,
        "timecode_raw_length"
    );
    assert_eq!(
        f.native
            .audio(&c, "LTC", &vec![0; 65537], 65537, CancellationToken::new())
            .await
            .unwrap_err()
            .code,
        "timecode_limit"
    );
    let request: Request = serde_json::from_value(
        json!({"channel":"MAIN","context":c,"data":{"channel":"TIMECODE_STATUS"}}),
    )
    .unwrap();
    let receipt = f
        .native
        .dispatch(&request, "main", CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(
        receipt.observations(),
        [Observation::LogStatus {
            data: Data::Undefined
        }]
    ));
    drop(receipt);
    assert_eq!(
        f.native
            .dispatch(&request, "output", CancellationToken::new())
            .await
            .unwrap_err()
            .code,
        "timecode_unsupported"
    );
    let mut retained = Vec::new();
    for _ in 0..16 {
        retained.push(f.command(Command::Status(Data::Undefined)).await);
    }
    assert_eq!(
        f.native
            .command(&c, Command::Stop, CancellationToken::new())
            .await
            .unwrap_err()
            .code,
        "timecode_busy"
    );
    drop(retained.pop());
    f.start("receive", "LTC", 25.0).await;
    let event = f.event().await;
    assert_eq!(
        f.native
            .complete("output", &c, &event.delivery.id, true)
            .unwrap_err()
            .code,
        "timecode_completion"
    );
    assert_eq!(
        f.native
            .complete("main", &bad, &event.delivery.id, true)
            .unwrap_err()
            .code,
        "timecode_completion"
    );
    assert_eq!(
        f.native
            .complete("main", &c, "foreign", true)
            .unwrap_err()
            .code,
        "timecode_completion"
    );
    f.ack(&event);
    assert_eq!(
        f.native
            .complete("main", &c, &event.delivery.id, true)
            .unwrap_err()
            .code,
        "timecode_completion"
    );
    f.drain().await.unwrap();
    assert_eq!(f.native.status()["retained"], 15);
    drop(retained);
    assert_eq!(f.native.status()["retained"], 0);
}
#[tokio::test]
async fn caller_drop_and_full_async_quota_retain_actual_open_ownership_through_terminal() {
    let mut f = Fixture::new(TimecodeHostConfig::default(), false);
    for _ in 0..8 {
        f.block_next("open");
        f.start("receive", "MTC", 25.0).await;
    }
    wait(|| f.controls().len() == 8 && f.controls().iter().all(|c| c.entered())).await;
    assert_eq!(f.native.status()["asyncEffects"], 8);
    assert_eq!(f.active.load(Ordering::Acquire), 8);
    assert!(f
        .native
        .command(
            &f.context(),
            Command::Start {
                kind: "receive".into(),
                mode: "MTC".into(),
                framerate: Scalar::number(25.0),
                data: Data::Json(json!({}))
            },
            CancellationToken::new()
        )
        .await
        .is_err());
    let native = f.native.clone();
    let drain = tokio::spawn(async move { native.shutdown().await });
    wait(|| f.native.status()["retiring"] == true).await;
    assert!(!drain.is_finished());
    for c in f.controls() {
        c.release();
    }
    drain.await.unwrap().unwrap();
    assert_eq!(f.active.load(Ordering::Acquire), 0);
    assert!(f
        .controls()
        .iter()
        .all(|c| c.trace().iter().any(|v| v[0] == "close")));
    f.drain().await.unwrap();
}
#[tokio::test]
async fn blocked_close_times_out_without_renewal_and_actual_retry_joins_then_preserves_globals() {
    let config = TimecodeHostConfig {
        lifecycle_timeout: Duration::from_millis(30),
        ports: freeshow_midi_ports::Config {
            shutdown_timeout: Duration::from_millis(20),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut f = Fixture::new(config, false);
    f.start("receive", "MTC", 30.0).await;
    wait(|| f.native.status()["router"]["ownedListeners"] == 1).await;
    f.start("send", "ableton", 30.0).await;
    let control = f.controls()[0].clone();
    control.block("close");
    let before = f.native.status()["run"].clone();
    f.switch().await;
    let result = f.native.context_changed().await;
    assert!(result.is_err());
    assert_eq!(f.active.load(Ordering::Acquire), 1);
    assert_eq!(f.native.status()["run"], before);
    assert_eq!(f.native.status()["retiring"], true);
    control.release();
    f.native.context_changed().await.unwrap();
    assert_eq!(f.active.load(Ordering::Acquire), 0);
    assert_ne!(f.native.status()["run"], before);
    assert_eq!(
        f.native.status()["router"]["framerate"]["value"]["value"],
        30.0
    );
    assert_eq!(f.native.status()["router"]["currentMode"], "ableton");
    f.drain().await.unwrap();
}
#[tokio::test]
async fn source_close_failure_keeps_current_map_and_orphan_until_later_actual_retirement() {
    let mut f = Fixture::new(TimecodeHostConfig::default(), false);
    f.start("receive", "MTC", 25.0).await;
    wait(|| f.native.status()["router"]["ownedListeners"] == 1).await;
    f.controls()[0].fail_close("Input 日本語");
    let receipt = f.command(Command::Stop).await;
    assert!(receipt.observations().iter().any(|e| matches!(
        e,
        Observation::Diagnostic {
            code: "controlled_close",
            ..
        }
    )));
    assert_eq!(f.active.load(Ordering::Acquire), 1);
    assert_eq!(f.native.status()["router"]["receivers"], json!(["MTC"]));
    f.start("receive", "MTC", 30.0).await;
    assert_eq!(f.controls().len(), 1);
    f.drain().await.unwrap();
    assert_eq!(f.active.load(Ordering::Acquire), 0);
}
#[tokio::test]
async fn real_ltc_encode_decode_and_actual_join_or_explicit_platform_unavailable() {
    let mut f = Fixture::new(TimecodeHostConfig::default(), true);
    f.start("send", "LTC", 25.0).await;
    f.native
        .value(
            &f.context(),
            Number::new(3723040.0),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let event = f.event().await;
    if event.event.message.channel == "ALERT" {
        eprintln!(
            "ACTUAL NATIVE LTC UNAVAILABLE: {:?}; no waveform coverage claimed",
            f.native.status()
        );
        assert!(std::env::var_os("FREESHOW_REQUIRE_LTC").is_none());
        f.ack(&event);
        f.drain().await.unwrap();
        return;
    }
    assert_eq!(event.event.message.channel, "TIMECODE_AUDIO_DATA");
    let first = event.bytes.clone().unwrap();
    assert_eq!(first.len(), 1920);
    assert_eq!(event.event.message.data, Some(Value::Null));
    f.ack(&event);
    assert_eq!(f.native.status()["ltc"]["loaded"], true);
    assert_eq!(f.native.status()["ltc"]["encoderCreated"], 1);
    f.start("receive", "LTC", 25.0).await;
    let mut decoded = Vec::new();
    for i in 0..8 {
        f.native
            .value(
                &f.context(),
                Number::new(3723040.0 + i as f64 * 40.0),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let bytes = loop {
            let event = f.event().await;
            if let Some(bytes) = event.bytes.clone() {
                f.ack(&event);
                break bytes;
            }
            if event.event.message.channel == "TIMECODE_VALUE" {
                decoded.push(event.event.message.data.clone().unwrap());
            }
            f.ack(&event);
        };
        f.native
            .audio(
                &f.context(),
                "LTC",
                &bytes,
                bytes.len(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        while let Ok(event) = f.events.try_recv() {
            if event.event.message.channel == "TIMECODE_VALUE" {
                decoded.push(event.event.message.data.clone().unwrap());
            }
            f.ack(&event);
        }
    }
    wait(|| f.native.status()["ltc"]["samples"].as_u64().unwrap() > 0).await;
    assert!(f.native.status()["ltc"]["decoderCreated"] == 1);
    f.command(Command::Stop).await;
    f.drain().await.unwrap();
    assert_eq!(
        f.native.status()["ltc"]["encoderCreated"],
        f.native.status()["ltc"]["encoderFreed"]
    );
    assert_eq!(
        f.native.status()["ltc"]["decoderCreated"],
        f.native.status()["ltc"]["decoderFreed"]
    );
    assert_eq!(f.native.status()["ltcJoined"], true);
    assert!(!decoded.is_empty());
    assert!(decoded.iter().all(|v| v.as_f64().unwrap() == 3723000.0));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn js_value(value: &Value) -> Value {
    match value {
        Value::Number(n) => json!(n.as_f64().unwrap()),
        Value::Array(a) => Value::Array(a.iter().map(js_value).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), js_value(v))).collect())
        }
        v => v.clone(),
    }
}
fn assert_events(actual: &[Value], expected: &[Value]) {
    assert_eq!(actual.len(), expected.len(), "source event counts");
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        if actual.get("hex").is_some() {
            assert!(
                actual["hex"] == expected["hex"],
                "actual source PCM bytes differ at event {index}"
            );
            assert_eq!(actual["channel"], expected["channel"]);
        } else {
            assert_eq!(js_value(actual), js_value(expected), "source event {index}");
        }
    }
}
fn source(actions: Vec<Value>, addon: Option<std::path::PathBuf>) -> Value {
    use std::{
        io::Write,
        process::{Command as Process, Stdio},
    };
    let source = std::path::PathBuf::from(file!());
    let source = if source.is_absolute() {
        source
    } else {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(source)
    };
    let helper = source.parent().unwrap().join("support/timecode_oracle.mjs");
    let mut child =
        Process::new(std::env::var_os("FREESHOW_TIMECODE_NODE").unwrap_or_else(|| "node".into()))
            .arg(helper)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    let request = json!({"actions":actions,"addon":addon});
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
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["revision"],
        "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
    );
    for (file, digest) in [
        (
            "timecode.ts",
            "18506a959deaf9053f5bae84235963e2dbf3779688d5a3d646a4995a65820a79",
        ),
        (
            "LTC.ts",
            "f4cbf2b19e2f6a8e22967c2409c2acca72b03e1f027dcead85dec318f12aaacf",
        ),
        (
            "MTC.ts",
            "d0127f1074686d169e7deeae708a2f4ef281bac2f028f2d75b0bc400a7ebab2c",
        ),
    ] {
        assert_eq!(
            result["sourceHashes"][format!("src/electron/timecode/{file}")],
            digest
        );
    }
    result
}
#[tokio::test]
async fn actual_immutable_source_mtc_methods_and_native_controlled_handles_match_packets_and_notifications(
) {
    let mut f = Fixture::new(TimecodeHostConfig::default(), false);
    let original = source(
        vec![
            json!({"op":"start","data":{"type":"receive","mode":"MTC","framerate":25,"data":{"midiInput":"Input 日本語"}}}),
            json!({"op":"emit","bytes":[242,8,0]}),
            json!({"op":"emit","bytes":[250]}),
            json!({"op":"emit","bytes":[252]}),
            json!({"op":"start","data":{"type":"send","mode":"MTC","framerate":29.97,"data":{"midiOutput":"Output 🎹"}}}),
            json!({"op":"value","value":3723040}),
            json!({"op":"value","value":{"kind":"NaN"}}),
            json!({"op":"stop"}),
        ],
        None,
    );
    f.start("receive", "MTC", 25.0).await;
    wait(|| f.native.status()["router"]["ownedListeners"] == 1).await;
    for packet in [&[242, 8, 0][..], &[250][..], &[252][..]] {
        f.controls()[0].emit("in", packet);
    }
    let mut events = Vec::new();
    for _ in 0..4 {
        let e = f.event().await;
        events.push(json!({"channel":e.event.message.channel,"data":e.event.message.data}));
        f.ack(&e);
    }
    f.start("send", "MTC", 29.97).await;
    f.native
        .value(
            &f.context(),
            Number::new(3723040.0),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    wait(|| f.controls().len() == 2 && f.controls()[1].trace().len() >= 9).await;
    f.native
        .value(&f.context(), Number::NaN, CancellationToken::new())
        .await
        .unwrap();
    f.command(Command::Stop).await;
    f.drain().await.unwrap();
    assert_events(&events, original["events"].as_array().unwrap());
    let original_sends = original["trace"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v[0] == "send")
        .cloned()
        .collect::<Vec<_>>();
    let actual = f
        .controls()
        .into_iter()
        .flat_map(|c| c.trace())
        .filter(|v| v[0] == "send")
        .collect::<Vec<_>>();
    assert_eq!(actual, original_sends);
    assert_eq!(actual.len(), 16);
    assert_eq!(
        original["revision"],
        "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
    );
    eprintln!(
        "actual source timecode MTC hashes: {}",
        original["sourceHashes"]
    );
}
#[tokio::test]
async fn actual_source_ltc_full_router_and_original_addon_match_native_host_audio_bytes() {
    let Some(addon) = std::env::var_os("FREESHOW_LTC_ORACLE_ADDON").map(std::path::PathBuf::from)
    else {
        eprintln!("ACTUAL LTC SOURCE ADDON UNAVAILABLE; no byte comparison claimed");
        assert!(std::env::var_os("FREESHOW_REQUIRE_TIMECODE_ORACLE").is_none());
        return;
    };
    let mut f = Fixture::new(TimecodeHostConfig::default(), true);
    let mut actions = vec![
        json!({"op":"start","data":{"type":"send","mode":"LTC","framerate":25}}),
        json!({"op":"start","data":{"type":"receive","mode":"LTC","framerate":25}}),
    ];
    for t in [0.0, 40.0, 80.0, 120.0, 60.0, 1000.0] {
        actions.push(json!({"op":"value","value":t}));
        actions.push(json!({"op":"feedLast"}));
    }
    actions.push(json!({"op":"stop"}));
    let original = source(actions, Some(addon));
    f.start("send", "LTC", 25.0).await;
    f.start("receive", "LTC", 25.0).await;
    let mut events = Vec::new();
    for t in [0.0, 40.0, 80.0, 120.0, 60.0, 1000.0] {
        f.native
            .value(&f.context(), Number::new(t), CancellationToken::new())
            .await
            .unwrap();
        let audio = loop {
            let e = f.event().await;
            let is_audio = e.event.message.channel == "TIMECODE_AUDIO_DATA";
            events.push(if let Some(bytes) = &e.bytes {
                json!({"channel":e.event.message.channel,"hex":hex(bytes)})
            } else {
                json!({"channel":e.event.message.channel,"data":e.event.message.data})
            });
            f.ack(&e);
            if is_audio {
                break e.bytes.unwrap();
            }
        };
        f.native
            .audio(
                &f.context(),
                "LTC",
                &audio,
                audio.len(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
    f.command(Command::Stop).await;
    while events.len() < original["events"].as_array().unwrap().len() {
        let e = f.event().await;
        events.push(json!({"channel":e.event.message.channel,"data":e.event.message.data}));
        f.ack(&e);
    }
    f.drain().await.unwrap();
    assert_events(&events, original["events"].as_array().unwrap());
    eprintln!(
        "actual source LTC host hashes: {}",
        original["sourceHashes"]
    );
}

#[tokio::test]
async fn aggregate_sixteen_owner_cap_preserves_all_accepted_inputs_outputs_and_joins_them() {
    let mut f = Fixture::new(TimecodeHostConfig::default(), false);
    for _ in 0..8 {
        f.block_next("open");
        f.start("receive", "MTC", 25.0).await;
    }
    wait(|| f.controls().len() == 8 && f.controls().iter().all(|c| c.entered())).await;
    for (i, c) in f.controls().iter().enumerate() {
        c.release();
        wait(|| f.native.status()["router"]["ownedListeners"] == i + 1).await;
    }
    f.start("send", "MTC", 25.0).await;
    for i in 0..8 {
        f.block_next("open");
        f.native
            .value(
                &f.context(),
                Number::new(i as f64 * 40.0),
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
    wait(|| f.controls().len() == 16 && f.controls()[8..].iter().all(|c| c.entered())).await;
    assert_eq!(f.native.status()["portOwners"], 16);
    assert_eq!(f.active.load(Ordering::Acquire), 16);
    assert!(f
        .native
        .value(&f.context(), Number::new(360.0), CancellationToken::new())
        .await
        .is_err());
    assert_eq!(f.controls().len(), 16);
    let host = f.native.clone();
    let closing = tokio::spawn(async move { host.shutdown().await });
    wait(|| f.native.status()["retiring"] == true).await;
    for c in &f.controls()[8..] {
        c.release();
    }
    closing.await.unwrap().unwrap();
    assert_eq!(f.active.load(Ordering::Acquire), 0);
    assert!(f
        .controls()
        .iter()
        .all(|c| c.trace().iter().any(|v| v[0] == "close")));
    assert_eq!(
        f.native.status()["lastRetired"]["allPortOwnersJoined"],
        true
    );
    f.drain().await.unwrap();
}
#[tokio::test]
async fn accepted_current_sender_survives_caller_future_drop_and_source_stop_waits_for_packet_batch(
) {
    let mut f = Fixture::new(TimecodeHostConfig::default(), false);
    f.start("send", "MTC", 25.0).await;
    f.native
        .value(&f.context(), Number::new(0.0), CancellationToken::new())
        .await
        .unwrap();
    wait(|| f.controls().len() == 1 && f.controls()[0].trace().len() >= 9).await;
    let c = f.controls()[0].clone();
    c.block("send");
    let host = f.native.clone();
    let context = f.context();
    let sending = tokio::spawn(async move {
        host.value(&context, Number::new(1000.0), CancellationToken::new())
            .await
    });
    wait(|| c.entered()).await;
    sending.abort();
    let _ = sending.await;
    let host = f.native.clone();
    let context = f.context();
    let stopping = tokio::spawn(async move {
        host.command(&context, Command::Stop, CancellationToken::new())
            .await
    });
    tokio::task::yield_now().await;
    assert!(!stopping.is_finished());
    assert!(!c.trace().iter().any(|v| v[0] == "close"));
    c.release();
    stopping.await.unwrap().unwrap();
    assert_eq!(c.trace().iter().filter(|v| v[0] == "send").count(), 16);
    assert_eq!(f.active.load(Ordering::Acquire), 0);
    f.drain().await.unwrap();
}
#[tokio::test]
async fn preaccept_cancellation_and_source_stop_sender_close_error_do_not_lose_receivers() {
    let mut f = Fixture::new(TimecodeHostConfig::default(), false);
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        f.native
            .value(&f.context(), Number::new(0.0), token)
            .await
            .unwrap_err()
            .code,
        "cancelled"
    );
    assert!(f.controls().is_empty());
    f.start("receive", "MTC", 25.0).await;
    wait(|| f.native.status()["router"]["ownedListeners"] == 1).await;
    f.start("send", "MTC", 25.0).await;
    f.native
        .value(&f.context(), Number::new(0.0), CancellationToken::new())
        .await
        .unwrap();
    wait(|| f.controls().len() == 2 && f.controls()[1].trace().len() >= 9).await;
    f.controls()[1].fail_close("Output 🎹");
    let result = f.command(Command::Stop).await;
    assert!(result.observations().iter().any(|v| matches!(
        v,
        Observation::Diagnostic {
            code: "controlled_close",
            ..
        }
    )));
    assert!(!f.controls()[0].trace().iter().any(|v| v[0] == "close"));
    assert_eq!(f.native.status()["router"]["receivers"], json!(["MTC"]));
    f.drain().await.unwrap();
    assert_eq!(f.active.load(Ordering::Acquire), 0);
}
#[tokio::test]
async fn actual_ltc_resource_rejection_is_not_a_constructor_failure_and_terminal_disposal_bypasses_work_quota(
) {
    let mut config = TimecodeHostConfig::default();
    config.ltc.max_operations = 1;
    let mut f = Fixture::new(config, true);
    f.start("send", "LTC", 25.0).await;
    let first = f
        .native
        .value(&f.context(), Number::new(1000.0), CancellationToken::new())
        .await;
    if f.native.status()["router"]["encoderFailed"] == true {
        eprintln!("Actual LTC unavailable for resource test");
        assert!(std::env::var_os("FREESHOW_REQUIRE_LTC").is_none());
        let e = f.event().await;
        f.ack(&e);
        f.drain().await.unwrap();
        return;
    }
    assert_eq!(first.unwrap_err().code, "ltc_resource_limit");
    assert_eq!(f.native.status()["ltc"]["encoderCreated"], 1);
    assert!(f.events.try_recv().is_err());
    assert_eq!(
        f.native
            .value(&f.context(), Number::new(500.0), CancellationToken::new())
            .await
            .unwrap_err()
            .code,
        "ltc_resource_limit"
    );
    wait(|| f.native.status()["retiredRuns"] == 1).await;
    assert_eq!(f.native.status()["router"]["encoderFailed"], false);
    assert_eq!(f.native.status()["lastRetired"]["ltc"]["encoderCreated"], 1);
    assert_eq!(f.native.status()["lastRetired"]["ltc"]["encoderFreed"], 1);
    assert_eq!(f.native.status()["lastRetired"]["ltcJoined"], true);
    assert!(f.events.try_recv().is_err());
    f.drain().await.unwrap();
}

#[tokio::test]
async fn global_status_gate_survives_context_renewal_and_old_private_callbacks_are_rejected() {
    let mut f = Fixture::new(TimecodeHostConfig::default(), false);
    f.start("receive", "MTC", 25.0).await;
    wait(|| f.native.status()["router"]["ownedListeners"] == 1).await;
    f.controls()[0].emit("in", &[250]);
    let value = f.event().await;
    f.ack(&value);
    let status = f.event().await;
    assert_eq!(status.event.message.data, Some(json!("play")));
    let old = f.context();
    f.switch().await;
    f.native.context_changed().await.unwrap();
    assert!(f
        .native
        .complete("main", &old, &status.delivery.id, true)
        .is_err());
    f.start("receive", "MTC", 25.0).await;
    wait(|| f.controls().len() == 2 && f.native.status()["router"]["ownedListeners"] == 1).await;
    f.controls()[1].emit("in", &[250]);
    let value = f.event().await;
    assert_eq!(value.event.message.channel, "TIMECODE_VALUE");
    f.ack(&value);
    // The original shared status remains play; native queue is fully observed after
    // an ordered STATUS/log barrier rather than an arbitrary timing sleep.
    f.command(Command::Status(Data::Undefined)).await;
    tokio::task::yield_now().await;
    assert!(f.events.try_recv().is_err());
    f.drain().await.unwrap();
}
#[tokio::test]
async fn missing_private_completion_fences_admission_and_actual_owners_still_join() {
    let config = TimecodeHostConfig {
        delivery_timeout: Duration::from_millis(20),
        ..Default::default()
    };
    let mut f = Fixture::new(config, false);
    f.start("receive", "MTC", 25.0).await;
    wait(|| f.native.status()["router"]["ownedListeners"] == 1).await;
    f.controls()[0].emit("in", &[250]);
    let event = f.event().await;
    wait(|| f.native.status()["closing"] == true).await;
    assert_eq!(
        f.native.shutdown().await.unwrap_err().code,
        "timecode_delivery_timeout"
    );
    assert_eq!(f.active.load(Ordering::Acquire), 0);
    assert_eq!(
        f.native.status()["lastRetired"]["allPortOwnersJoined"],
        true
    );
    assert!(f
        .native
        .complete("main", &f.context(), &event.delivery.id, true)
        .is_err());
    assert_eq!(
        f.native.shutdown().await.unwrap_err().code,
        "timecode_delivery_timeout"
    );
}
