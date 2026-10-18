use super::*;
use serde_json::json;
use std::{
    fs,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
};
fn fixture() -> (tempfile::TempDir, ScriptureContext) {
    let root = tempfile::tempdir().unwrap();
    let context =
        ScriptureContext::new(root.path().join("selected"), root.path().join("documents")).unwrap();
    fs::create_dir_all(context.bibles_root()).unwrap();
    fs::create_dir_all(context.legacy_root()).unwrap();
    (root, context)
}
fn book(value: Value) -> Value {
    json!(["stored",{"books":[{"chapters":[{"verses":value}]}],"future":{"keep":[true,0.0,"é 🙌"]}}])
}
fn write(context: &ScriptureContext, name: &str, value: &Value) {
    fs::write(
        context.bibles_root().join(format!("{name}.fsb")),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
}
async fn load(
    service: &ScriptureService,
    context: &ScriptureContext,
    name: &str,
) -> PreparedScriptureReply {
    service
        .load(
            context,
            &json!({"id":"requested","name":name}),
            CancellationToken::new(),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn migration_gate_preserves_unknowns_falsy_text_and_source_values() {
    let (_root, context) = fixture();
    let service = ScriptureService::new(Limits::default()).unwrap();
    write(
        &context,
        "old",
        &book(
            json!([{"value":"first","extra":42},{"text":"existing","value":"retain"},{"text":false,"value":0},{"value":{"future":true}},{"text":[],"value":"retain2"}]),
        ),
    );
    let reply = load(&service, &context, "old").await;
    let verses = &reply.data.as_ref().unwrap()["content"][1]["books"][0]["chapters"][0]["verses"];
    assert_eq!(
        *verses,
        json!([{"extra":42.0,"text":"first"},{"text":"existing","value":"retain"},{"text":""},{"text":{"future":true}},{"text":[],"value":"retain2"}])
    );
    assert_eq!(reply.data.as_ref().unwrap()["content"][0], "requested");
    assert_eq!(reply.stats.migrated_verses, 3);
    assert_eq!(reply.stats.visited_verses, 5);
    assert_eq!(
        reply.data.as_ref().unwrap()["content"][1]["future"],
        json!({"keep":[true,0.0,"é 🙌"]})
    );
    let untouched = book(json!([{"value":0.0},{"value":"later"},null]));
    write(&context, "new", &untouched);
    assert_eq!(
        load(&service, &context, "new").await.data.unwrap()["content"][1],
        untouched[1]
    );
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn second_read_is_fresh_and_fallback_is_selected_only_by_first_result() {
    let (_root, context) = fixture();
    let service = ScriptureService::new(Limits::default()).unwrap();
    let primary = context.bibles_root().join("test.fsb");
    let legacy = context.legacy_root().join("test.fsb");
    fs::write(&legacy, "[\"legacy\",{\"from\":\"legacy\"}]").unwrap();
    fs::write(&primary, "[\"first\",{\"from\":\"first\"}]").unwrap();
    let path = primary.clone();
    *service.hooks().after_first.lock().unwrap() = Some(Arc::new(move || {
        fs::write(&path, "[\"second\",{\"from\":\"second\"}]").unwrap()
    }));
    let reply = load(&service, &context, "test").await;
    assert_eq!(reply.data.unwrap()["content"][1]["from"], "second");
    assert_eq!(reply.stats.load_paths, vec![primary.to_str().unwrap(); 2]);
    fs::write(&primary, "not json").unwrap();
    let path = primary.clone();
    *service.hooks().after_first.lock().unwrap() = Some(Arc::new(move || {
        fs::write(&path, "[\"new\",{\"from\":\"new\"}]").unwrap()
    }));
    let reply = load(&service, &context, "test").await;
    assert_eq!(reply.data.unwrap()["content"][1]["from"], "legacy");
    assert_eq!(
        reply.stats.load_paths,
        [primary.to_str().unwrap(), legacy.to_str().unwrap()]
    );
    let path = primary.clone();
    *service.hooks().after_first.lock().unwrap() =
        Some(Arc::new(move || fs::remove_file(&path).unwrap()));
    let reply = load(&service, &context, "test").await;
    assert_eq!(
        reply.data.unwrap(),
        json!({"error":"not_found","id":"requested"})
    );
    assert_eq!(reply.stats.load_paths, vec![primary.to_str().unwrap(); 2]);
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn preparation_has_no_writes_and_failed_host_mkdir_still_allows_legacy() {
    let root = tempfile::tempdir().unwrap();
    let context =
        ScriptureContext::new(root.path().join("missing"), root.path().join("docs")).unwrap();
    fs::create_dir_all(context.legacy_root()).unwrap();
    fs::write(context.legacy_root().join("test.fsb"), "[\"legacy\",{}]").unwrap();
    let service = ScriptureService::new(Limits::default()).unwrap();
    let reply = load(&service, &context, "test").await;
    assert!(!reply.is_complete());
    assert!(reply.data.is_none());
    assert_eq!(reply.folder_intents.len(), 1);
    assert!(reply.stats.load_paths.is_empty());
    assert!(!context.bibles_root().exists());
    let reply = load(&service, &context.after_directory_attempt(), "test").await;
    assert!(reply.is_complete());
    assert_eq!(reply.data.unwrap()["content"][0], "requested");
    assert!(!context.bibles_root().exists());
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn json_binary64_duplicates_utf8_repair_and_explicit_unsupported_shapes() {
    let (_root, context) = fixture();
    let service = ScriptureService::new(Limits::default()).unwrap();
    let path = context.bibles_root().join("test.fsb");
    fs::write(&path,b"[\"id\",{\"b\":1,\"a\":9007199254740993,\"z\":-0,\"precise\":8.3163996793004788882e-275,\"b\":2,\"badUtf8\":\"\xff\"}]").unwrap();
    let reply = load(&service, &context, "test").await.data.unwrap();
    let value = &reply["content"][1];
    assert_eq!(value["a"].as_f64(), Some(9007199254740992.0));
    assert!(value["z"].as_f64().unwrap().is_sign_negative());
    assert_eq!(
        value["precise"].as_f64(),
        Some("8.3163996793004788882e-275".parse().unwrap())
    );
    assert_eq!(value["badUtf8"], "�");
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["b", "a", "z", "precise", "badUtf8"]
    );
    fs::write(&path, "[\"id\",{\"a\":{}}] trailing").unwrap();
    assert!(load(&service, &context, "test")
        .await
        .data
        .unwrap()
        .get("content")
        .is_some());
    for text in [
        "[\"id\",{\"bad\":\"\\ud800\"}]",
        "[\"id\",{\"bad\":1e309}]",
        "true",
        "[\"id\",{\"books\":[{\"chapters\":[{\"verses\":[{\"value\":\"x\"},null]}]}]}]",
    ] {
        fs::write(&path, text).unwrap();
        assert_eq!(
            service
                .load(
                    &context,
                    &json!({"name":"test","id":"x"}),
                    CancellationToken::new()
                )
                .await
                .unwrap_err()
                .code,
            "scripture_unsupported"
        );
    }
    fs::write(&path, "[]junk").unwrap();
    assert_eq!(
        load(&service, &context, "test").await.data.unwrap()["content"],
        json!(["requested"])
    );
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn independent_hard_limits_fail_without_source_success() {
    let (_root, context) = fixture();
    write(
        &context,
        "test",
        &book(json!([{"value":"first"},{"value":"second"}])),
    );
    let base = Limits::default();
    let mut cases = Vec::new();
    let mut l = base.clone();
    l.read_bytes = 8;
    cases.push(l);
    let mut l = base.clone();
    l.read_bytes = fs::metadata(context.bibles_root().join("test.fsb"))
        .unwrap()
        .len() as usize
        + 1;
    cases.push(l); // A single read fits; the source's second read must be counted.
    let mut l = base.clone();
    l.nodes = 5;
    cases.push(l);
    let mut l = base.clone();
    l.depth = 2;
    cases.push(l);
    let mut l = base.clone();
    l.string_bytes = 3;
    cases.push(l);
    let mut l = base.clone();
    l.verses = 1;
    cases.push(l);
    let mut l = base.clone();
    l.allocation_bytes = 4096;
    cases.push(l);
    let mut l = base.clone();
    l.output_bytes = 10;
    cases.push(l);
    let mut l = base.clone();
    l.operations = 5;
    cases.push(l);
    for limits in cases {
        let service = ScriptureService::new(limits).unwrap();
        assert_eq!(
            service
                .load(
                    &context,
                    &json!({"name":"test","id":"id"}),
                    CancellationToken::new()
                )
                .await
                .unwrap_err()
                .code,
            "scripture_limit"
        );
        service.shutdown().await.unwrap();
    }
    let mut invalid = base;
    invalid.depth = 65;
    assert!(ScriptureService::new(invalid).is_err());
    let service = ScriptureService::new(Limits::default()).unwrap();
    assert!(service
        .load(&context, &json!({"name":17}), CancellationToken::new())
        .await
        .is_err());
    let mut id = Value::Null;
    for _ in 0..10 {
        id = json!([id]);
    }
    assert_eq!(
        service
            .load(
                &context,
                &json!({"name":"test","id":id}),
                CancellationToken::new()
            )
            .await
            .unwrap_err()
            .code,
        "scripture_limit"
    );
    service.shutdown().await.unwrap();
}
#[derive(Default)]
struct Gate {
    entered: AtomicUsize,
    open: Mutex<bool>,
    condition: Condvar,
}
impl Gate {
    fn hold(&self) {
        self.entered.fetch_add(1, Ordering::AcqRel);
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.condition.wait(open).unwrap();
        }
    }
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.condition.notify_all();
    }
}
async fn wait_for(mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(
            Instant::now() < until,
            "controlled worker barrier timed out"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}
#[tokio::test]
async fn admission_cancel_drop_and_shutdown_retain_work_until_workers_really_finish() {
    let (_root, context) = fixture();
    write(&context, "test", &json!(["id", {}]));
    let limits = Limits {
        queued_jobs: 1,
        ..Limits::default()
    };
    let service = ScriptureService::new(limits).unwrap();
    let gate = Arc::new(Gate::default());
    let g = gate.clone();
    *service.hooks().process.lock().unwrap() = Some(Arc::new(move || g.hold()));
    let start = |cancel: CancellationToken| {
        let service = service.clone();
        let context = context.clone();
        tokio::spawn(async move {
            service
                .load(&context, &json!({"name":"test","id":"id"}), cancel)
                .await
        })
    };
    let first = start(CancellationToken::new());
    let second = start(CancellationToken::new());
    wait_for(|| gate.entered.load(Ordering::Acquire) == 2).await;
    let cancel = CancellationToken::new();
    let third = start(cancel.clone());
    wait_for(|| service.status().admitted == 3).await;
    assert_eq!(
        service
            .load(&context, &json!({"name":"test"}), CancellationToken::new())
            .await
            .unwrap_err()
            .code,
        "scripture_busy"
    );
    cancel.cancel();
    assert_eq!(
        third.await.unwrap().unwrap_err().code,
        "scripture_cancelled"
    );
    first.abort();
    let _ = first.await;
    assert_eq!(
        service.status().admitted,
        3,
        "abandoned callers cannot free active work capacity"
    );
    gate.release();
    assert!(second.await.unwrap().is_ok());
    wait_for(|| service.status().admitted == 0).await;
    service.shutdown().await.unwrap();
    assert_eq!(service.status().workers, 0);
    assert_eq!(
        service
            .load(&context, &json!({"name":"test"}), CancellationToken::new())
            .await
            .unwrap_err()
            .code,
        "scripture_shutdown"
    );
}
#[tokio::test]
async fn deadlines_and_shutdown_timeouts_are_explicit_and_retry_can_join() {
    let (_root, context) = fixture();
    write(&context, "test", &json!(["id", {}]));
    let service = ScriptureService::new(Limits {
        shutdown_timeout: Duration::from_millis(20),
        ..Limits::default()
    })
    .unwrap();
    let gate = Arc::new(Gate::default());
    let g = gate.clone();
    *service.hooks().process.lock().unwrap() = Some(Arc::new(move || g.hold()));
    let s = service.clone();
    let c = context.clone();
    let held = tokio::spawn(async move {
        s.load_with_deadline(
            &c,
            &json!({"name":"test"}),
            CancellationToken::new(),
            Instant::now() + Duration::from_millis(20),
        )
        .await
    });
    wait_for(|| gate.entered.load(Ordering::Acquire) == 1).await;
    assert_eq!(held.await.unwrap().unwrap_err().code, "scripture_deadline");
    assert_eq!(service.status().active, 1);
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "scripture_shutdown_timeout"
    );
    gate.release();
    service.shutdown().await.unwrap();
    assert_eq!(service.status().workers, 0);
}

#[tokio::test]
async fn cancellation_before_admission_and_nonregular_files_never_claim_content() {
    let (_root, context) = fixture();
    let service = ScriptureService::new(Limits::default()).unwrap();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        service
            .load(&context, &json!({"name":"missing"}), cancelled)
            .await
            .unwrap_err()
            .code,
        "scripture_cancelled"
    );
    assert_eq!(
        service
            .load_with_deadline(
                &context,
                &json!({"name":"missing"}),
                CancellationToken::new(),
                Instant::now()
            )
            .await
            .unwrap_err()
            .code,
        "scripture_deadline"
    );
    assert_eq!(service.status().admitted, 0);
    #[cfg(unix)]
    {
        let fifo = std::ffi::CString::new(context.bibles_root().join("fifo.fsb").to_str().unwrap())
            .unwrap();
        // SAFETY: valid C path lives through mkfifo; test owns the private temp root.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(
            service
                .load(&context, &json!({"name":"fifo"}), CancellationToken::new())
                .await
                .unwrap_err()
                .code,
            "scripture_unsupported"
        );
    }
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn obstructing_root_requires_one_host_attempt_then_falls_back_without_writes() {
    let (_root, context) = fixture();
    fs::remove_dir(context.bibles_root()).unwrap();
    fs::write(context.bibles_root(), b"original obstruction").unwrap();
    fs::write(
        context.legacy_root().join("book.fsb"),
        b"[\"stored\",{\"from\":\"legacy\"}]",
    )
    .unwrap();
    let service = ScriptureService::new(Limits::default()).unwrap();
    let pending = load(&service, &context, "book").await;
    assert!(!pending.is_complete());
    assert_eq!(pending.folder_intents.len(), 1);
    assert!(pending.stats.load_paths.is_empty());
    let actual = load(&service, &context.after_directory_attempt(), "book").await;
    assert!(actual.is_complete());
    assert_eq!(actual.data.unwrap()["content"][1]["from"], "legacy");
    assert_eq!(
        fs::read(context.bibles_root()).unwrap(),
        b"original obstruction"
    );
    service.shutdown().await.unwrap();
}
