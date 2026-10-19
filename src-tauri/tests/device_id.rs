#[path = "../src/device_id.rs"]
mod device_id;
use device_id::DeviceIdentity;
use freeshow_core::{protocol::Request, runtime::Runtime, service::LibraryService};
use serde_json::{json, Value};
use std::{fs, sync::mpsc, time::Duration};
use tokio::{sync::oneshot, time::timeout};

fn fixture() -> (tempfile::TempDir, LibraryService, DeviceIdentity) {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    (temp, library, DeviceIdentity::new())
}
fn request(library: &LibraryService) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"DEVICE_ID","data":{"path":"untrusted ignored payload"}},"context":library.context().unwrap(),"listenerId":"identity-context"})).unwrap()
}
async fn get(library: &LibraryService, device: &DeviceIdentity) -> Value {
    let context = library.context().unwrap();
    let events = device
        .dispatch(library, request(library), "main")
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].channel, "MAIN");
    assert_eq!(events[0].message.channel, "DEVICE_ID");
    assert_eq!(events[0].listener_id.as_deref(), Some("identity-context"));
    assert_eq!(events[0].context.as_ref(), Some(&context));
    events[0].message.data.clone().unwrap()
}
// Avoid formatting an unexpected successful response containing a native ID.
trait TakeError<E> {
    fn take_error(self) -> E;
}
impl<T, E> TakeError<E> for Result<T, E> {
    fn take_error(self) -> E {
        match self {
            Err(error) => error,
            Ok(_) => panic!("Expected controlled error"),
        }
    }
}
fn same_js_value(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .zip(b.as_f64())
            .is_some_and(|(a, b)| a.to_bits() == b.to_bits()),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_js_value(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|((ak, av), (bk, bv))| ak == bk && same_js_value(av, bv))
        }
        _ => actual == expected,
    }
}
async fn held(library: &LibraryService) -> (mpsc::Sender<()>, tokio::task::JoinHandle<()>) {
    let (entered, ready) = oneshot::channel();
    let (release, gate) = mpsc::channel();
    let owner = library.clone();
    let request = request(library);
    let task = tokio::spawn(async move {
        owner
            .commit(request, "main", move |_| {
                let _ = entered.send(());
                let _ = gate.recv();
                Ok(vec![])
            })
            .await
            .unwrap();
    });
    timeout(Duration::from_secs(5), ready)
        .await
        .unwrap()
        .unwrap();
    (release, task)
}
async fn queued(library: &LibraryService, count: usize) {
    timeout(Duration::from_secs(5), async {
        while library.status().unwrap().queued_jobs != count {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn global_identity_is_persisted_once_preserves_unknown_config_and_survives_library_selection()
{
    let (temp, library, device) = fixture();
    let path = temp.path().join("app/config.json");
    let mut initial: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    initial["unknown-host-key"] = json!({"raw":[true,17,"保持"]});
    fs::write(&path, serde_json::to_vec(&initial).unwrap()).unwrap();
    let id = get(&library, &device).await;
    assert!(id
        .as_str()
        .is_some_and(|v| !v.is_empty() && v != "fallback-machine-id"));
    let first = fs::read(&path).unwrap();
    let persisted: Value = serde_json::from_slice(&first).unwrap();
    assert!(persisted["machineId"] == id, "Persisted identity differs");
    assert_eq!(persisted["unknown-host-key"], initial["unknown-host-key"]);
    assert!(
        get(&library, &device).await == id,
        "Stored identity changed"
    );
    assert!(
        fs::read(&path).unwrap() == first,
        "Repeated identity query changed config bytes"
    );
    let old = library.context().unwrap();
    let next = temp.path().join("next-library");
    fs::create_dir(&next).unwrap();
    let change:Request=serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":old.data_path,"newPath":next}},"context":old})).unwrap();
    library.dispatch(change, "main").await.unwrap();
    let selected = fs::read(&path).unwrap();
    assert!(
        get(&library, &device).await == id,
        "Stored identity changed"
    );
    assert!(
        fs::read(&path).unwrap() == selected,
        "Selected-library identity query changed global config"
    );
    assert!(!next.join("Config/config.json").exists());
    assert_eq!(device.retained(), 0);
    device.seal();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn actual_truthy_json_ids_and_fractional_number_bits_return_unchanged_without_writes() {
    let (temp, library, device) = fixture();
    let path = temp.path().join("app/config.json");
    for value in [
        json!(true),
        json!(42),
        json!([]),
        json!({}),
        json!([null,false,{"nested":"value"}]),
        json!("existing-legacy-id"),
    ] {
        let bytes = serde_json::to_vec(&json!({"machineId":value,"preserve":"unchanged"})).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(
            same_js_value(&get(&library, &device).await, &value),
            "Truthy identity fixture changed type, order or Number bits"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    let raw=br#"{"machineId":{"fraction":1792415704000.0015,"negativeZero":-0.0,"duplicates":1,"duplicates":2},"preserve":"original bytes"}"#;
    fs::write(&path, raw).unwrap();
    let value = get(&library, &device).await;
    assert_eq!(
        value["fraction"].as_f64().unwrap().to_bits(),
        0x427a1544d23c0006
    );
    assert_eq!(
        value["negativeZero"].as_f64().unwrap().to_bits(),
        (-0.0f64).to_bits()
    );
    assert_eq!(value["duplicates"].as_f64(), Some(2.0));
    assert_eq!(fs::read(&path).unwrap(), raw);
    device.seal();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn stale_role_resource_and_real_config_read_failures_are_distinct() {
    let (temp, library, device) = fixture();
    let path = temp.path().join("app/config.json");
    assert_eq!(
        device
            .dispatch(&library, request(&library), "output-1")
            .await
            .take_error()
            .code,
        "device_id_role"
    );
    let mut stale = request(&library);
    stale.context.as_mut().unwrap().library_generation += 1;
    assert_eq!(
        device
            .dispatch(&library, stale, "main")
            .await
            .take_error()
            .code,
        "stale_library"
    );
    fs::write(&path, b"{invalid actual config bytes").unwrap();
    assert!(
        get(&library, &device).await == json!("fallback-machine-id"),
        "Read failure did not preserve source fallback"
    );
    assert_eq!(fs::read(&path).unwrap(), b"{invalid actual config bytes");
    fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .unwrap()
        .set_len(8 * 1024 * 1024 + 1)
        .unwrap();
    assert_eq!(
        device
            .dispatch(&library, request(&library), "main")
            .await
            .take_error()
            .code,
        "device_id_resource_limit"
    );
    assert_eq!(device.retained(), 0);
    device.seal();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn valid_deep_configuration_is_an_explicit_resource_error_instead_of_a_legacy_fallback() {
    let (temp, library, device) = fixture();
    let path = temp.path().join("app/config.json");
    // This is valid JSON accepted by the actual Conf/JSON.parse source getter.
    let bytes = format!(
        "{{\"machineId\":\"synthetic-retained-id\",\"nested\":{}0{}}}",
        "[".repeat(130),
        "]".repeat(130)
    );
    fs::write(&path, bytes.as_bytes()).unwrap();
    assert_eq!(
        device
            .dispatch(&library, request(&library), "main")
            .await
            .take_error()
            .code,
        "device_id_resource_limit"
    );
    assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
    device.seal();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn pretty_persistence_limit_rejects_before_write_and_preserves_near_cap_input() {
    let (temp, library, device) = fixture();
    let path = temp.path().join("app/config.json");
    let mut nested = json!(0);
    for _ in 0..120 {
        nested = json!([nested]);
    }
    let mut value = json!({"nested":nested,"pad":"x".repeat(8 * 1024 * 1024 - 2048)});
    let original = serde_json::to_vec(&value).unwrap();
    assert!(original.len() < 8 * 1024 * 1024);
    value["machineId"] = json!("synthetic-size-witness".repeat(4));
    assert!(serde_json::to_vec(&value).unwrap().len() < 8 * 1024 * 1024);
    assert!(serde_json::to_vec_pretty(&value).unwrap().len() > 8 * 1024 * 1024);
    fs::write(&path, &original).unwrap();
    assert_eq!(
        device
            .dispatch(&library, request(&library), "main")
            .await
            .take_error()
            .code,
        "device_id_resource_limit"
    );
    assert!(
        fs::read(&path).unwrap() == original,
        "Over-cap persistence changed input bytes"
    );
    assert_eq!(device.retained(), 0);
    device.seal();
    library.finish_writes().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn actual_readonly_config_write_failure_returns_original_outer_fallback_and_preserves_bytes()
{
    use std::os::unix::fs::PermissionsExt;
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "real readonly failure requires non-root"
    );
    let (temp, library, device) = fixture();
    let path = temp.path().join("app/config.json");
    let bytes = fs::read(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    let value = get(&library, &device).await;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        value == json!("fallback-machine-id"),
        "Write failure did not preserve source fallback"
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    device.seal();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn accepted_aborted_callers_keep_four_real_queued_leases_until_owner_return_and_seal_fences_them(
) {
    let (_temp, library, device) = fixture();
    let (release, owner) = held(&library).await;
    let mut jobs = Vec::new();
    for _ in 0..4 {
        let library = library.clone();
        let device = device.clone();
        let request = request(&library);
        jobs.push(tokio::spawn(async move {
            device.dispatch(&library, request, "main").await
        }));
    }
    queued(&library, 4).await;
    assert_eq!(device.retained(), 4);
    let dropped = jobs.pop().unwrap();
    dropped.abort();
    assert!(dropped.await.take_error().is_cancelled());
    assert_eq!(
        device.retained(),
        4,
        "accepted closure owns its lease after observer Drop"
    );
    assert_eq!(
        device
            .dispatch(&library, request(&library), "main")
            .await
            .take_error()
            .code,
        "device_id_busy"
    );
    device.seal();
    assert_eq!(
        device
            .dispatch(&library, request(&library), "main")
            .await
            .take_error()
            .code,
        "device_id_shutdown"
    );
    let fence = library.finish_writes();
    tokio::pin!(fence);
    assert!(timeout(Duration::from_millis(20), fence.as_mut())
        .await
        .is_err());
    assert_eq!(device.retained(), 4);
    drop(release);
    owner.await.unwrap();
    for job in jobs {
        assert_eq!(job.await.unwrap().take_error().code, "device_id_cancelled");
    }
    fence.await.unwrap();
    assert_eq!(device.retained(), 0);
}

#[tokio::test]
async fn actual_observer_deadline_keeps_accepted_queued_job_until_the_ordered_fence() {
    let (_temp, library, device) = fixture();
    let (release, owner) = held(&library).await;
    let native = device.clone();
    let config = library.clone();
    let request = request(&library);
    let job = tokio::spawn(async move { native.dispatch(&config, request, "main").await });
    queued(&library, 1).await;
    assert_eq!(
        timeout(Duration::from_secs(3), job)
            .await
            .unwrap()
            .unwrap()
            .take_error()
            .code,
        "device_id_deadline"
    );
    assert_eq!(
        device.retained(),
        1,
        "deadline is not actual accepted-work disposal"
    );
    device.seal();
    drop(release);
    owner.await.unwrap();
    library.finish_writes().await.unwrap();
    assert_eq!(device.retained(), 0);
}
