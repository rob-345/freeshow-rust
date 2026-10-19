#[path = "../src/provider_connections.rs"]
mod provider_connections;
use freeshow_core::{protocol::Request, runtime::Runtime, service::LibraryService};
use provider_connections::ProviderConnections;
use serde_json::{json, Value};
use std::{fs, sync::mpsc, time::Duration};
use tokio::{sync::oneshot, time::timeout};

fn fixture() -> (tempfile::TempDir, LibraryService, ProviderConnections) {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    (temp, library, ProviderConnections::new())
}
fn request(library: &LibraryService) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"PROVIDER_CONNECTIONS","data":{"path":"untrusted ignored payload","contentProviders":{"unknown":true}}},"context":library.context().unwrap(),"listenerId":"provider-context"})).unwrap()
}
async fn get(library: &LibraryService, provider: &ProviderConnections) -> Value {
    let context = library.context().unwrap();
    let events = provider
        .dispatch(library, request(library), "main")
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].channel, "MAIN");
    assert_eq!(events[0].message.channel, "PROVIDER_CONNECTIONS");
    assert_eq!(events[0].listener_id.as_deref(), Some("provider-context"));
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
async fn actual_global_access_preserves_source_scope_order_omits_secrets_and_survives_library_selection(
) {
    let (temp, library, provider) = fixture();
    let path = temp.path().join("app/ACCESS.json");
    let access = json!({"secrets":{"fixture":"never-published"},"contentProviders":{
        "canva":{"folder:read design:content:read design:meta:read":[]},
        "onstage":{"presenter":{}},"amazinglife":{"openid profile email":"expired"},
        "planningcenter":{"services":true},"churchApps":{"plans":{}}
    }});
    let bytes = serde_json::to_vec(&access).unwrap();
    fs::write(&path, &bytes).unwrap();
    fs::create_dir_all(temp.path().join("library/Config")).unwrap();
    fs::write(
        temp.path().join("library/Config/ACCESS.json"),
        b"{invalid selected library access}",
    )
    .unwrap();
    let expected = json!({"churchApps":true,"planningcenter":true,"amazinglife":true,"onstage":true,"canva":true});
    let value = get(&library, &provider).await;
    assert_eq!(value, expected);
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![
            "churchApps",
            "planningcenter",
            "amazinglife",
            "onstage",
            "canva"
        ]
    );
    assert!(!serde_json::to_string(&value)
        .unwrap()
        .contains("never-published"));
    assert!(fs::read(&path).unwrap() == bytes, "Query mutated ACCESS");
    let old = library.context().unwrap();
    let next = temp.path().join("next-library");
    fs::create_dir(&next).unwrap();
    let change:Request=serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":old.data_path,"newPath":next}},"context":old})).unwrap();
    library.dispatch(change, "main").await.unwrap();
    assert_eq!(get(&library, &provider).await, expected);
    assert!(fs::read(&path).unwrap() == bytes);
    fs::write(
        &path,
        br#"{"contentProviders":{"churchApps":{"plans":0},"onstage":{"presenter":[]}}}"#,
    )
    .unwrap();
    assert_eq!(get(&library, &provider).await, json!({"onstage":true}));
    fs::remove_file(&path).unwrap();
    assert_eq!(get(&library, &provider).await, json!({}));
    assert!(
        !path.exists(),
        "Read-only missing-store query created a file"
    );
    provider.seal();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn actual_parse_nonobject_role_stale_and_nonregular_failures_never_fabricate_empty_replies() {
    let (temp, library, provider) = fixture();
    let path = temp.path().join("app/ACCESS.json");
    assert_eq!(
        provider
            .dispatch(&library, request(&library), "output-1")
            .await
            .take_error()
            .code,
        "provider_connections_role"
    );
    let mut stale = request(&library);
    stale.context.as_mut().unwrap().library_generation += 1;
    assert_eq!(
        provider
            .dispatch(&library, stale, "main")
            .await
            .take_error()
            .code,
        "stale_library"
    );
    for (raw, code) in [
        ("{private malformed fixture", "provider_connections_parse"),
        ("null", "provider_connections_invalid"),
        ("[]", "provider_connections_invalid"),
    ] {
        fs::write(&path, raw).unwrap();
        let error = provider
            .dispatch(&library, request(&library), "main")
            .await
            .take_error();
        assert_eq!(error.code, code);
        assert!(!error.message.contains("private malformed fixture"));
        assert_eq!(fs::read(&path).unwrap(), raw.as_bytes());
    }
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let error = provider
        .dispatch(&library, request(&library), "main")
        .await
        .take_error();
    // Windows may reject opening a directory before the regular-file check.
    assert!(matches!(
        error.code,
        "provider_connections_unsupported" | "provider_connections_read"
    ));
    assert_eq!(provider.retained(), 0);
    provider.seal();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn actual_whole_snapshot_resource_limits_include_unknown_secrets_and_preserve_bytes() {
    let (temp, library, provider) = fixture();
    let path = temp.path().join("app/ACCESS.json");
    let mut depth = json!(null);
    for _ in 0..17 {
        depth = json!([depth]);
    }
    for (value, code) in [
        (
            json!({"secrets":"x".repeat(1024*1024)}),
            "provider_connections_resource_limit",
        ),
        (
            json!({"secrets":vec![Value::Null;16384]}),
            "provider_connections_limit",
        ),
        (json!({"secrets":depth}), "provider_connections_limit"),
    ] {
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            provider
                .dispatch(&library, request(&library), "main")
                .await
                .take_error()
                .code,
            code
        );
        assert!(fs::read(&path).unwrap() == bytes);
    }
    provider.seal();
    library.finish_writes().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn actual_permission_failure_is_an_error_without_secret_or_path_disclosure() {
    use std::os::unix::fs::PermissionsExt;
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "Real permission case requires non-root"
    );
    let (temp, library, provider) = fixture();
    let path = temp.path().join("app/ACCESS.json");
    let bytes = fs::read(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
    let result = provider.dispatch(&library, request(&library), "main").await;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let error = result.take_error();
    assert_eq!(error.code, "provider_connections_read");
    assert!(!error.message.contains(&temp.path().display().to_string()));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    provider.seal();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn accepted_aborted_callers_keep_four_real_queued_leases_until_owner_return_and_seal_fences_them(
) {
    let (_temp, library, provider) = fixture();
    let (release, owner) = held(&library).await;
    let mut jobs = Vec::new();
    for _ in 0..4 {
        let library = library.clone();
        let provider = provider.clone();
        let request = request(&library);
        jobs.push(tokio::spawn(async move {
            provider.dispatch(&library, request, "main").await
        }));
    }
    queued(&library, 4).await;
    assert_eq!(provider.retained(), 4);
    let dropped = jobs.pop().unwrap();
    dropped.abort();
    assert!(dropped.await.take_error().is_cancelled());
    assert_eq!(
        provider.retained(),
        4,
        "accepted closure owns its lease after observer Drop"
    );
    assert_eq!(
        provider
            .dispatch(&library, request(&library), "main")
            .await
            .take_error()
            .code,
        "provider_connections_busy"
    );
    provider.seal();
    assert_eq!(
        provider
            .dispatch(&library, request(&library), "main")
            .await
            .take_error()
            .code,
        "provider_connections_shutdown"
    );
    let fence = library.finish_writes();
    tokio::pin!(fence);
    assert!(timeout(Duration::from_millis(20), fence.as_mut())
        .await
        .is_err());
    assert_eq!(provider.retained(), 4);
    drop(release);
    owner.await.unwrap();
    for job in jobs {
        assert_eq!(
            job.await.unwrap().take_error().code,
            "provider_connections_cancelled"
        );
    }
    fence.await.unwrap();
    assert_eq!(provider.retained(), 0);
}

#[tokio::test]
async fn actual_observer_deadline_keeps_accepted_queued_job_until_the_ordered_fence() {
    let (_temp, library, provider) = fixture();
    let (release, owner) = held(&library).await;
    let native = provider.clone();
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
        "provider_connections_deadline"
    );
    assert_eq!(
        provider.retained(),
        1,
        "deadline is not actual accepted-work disposal"
    );
    provider.seal();
    drop(release);
    owner.await.unwrap();
    library.finish_writes().await.unwrap();
    assert_eq!(provider.retained(), 0);
}
