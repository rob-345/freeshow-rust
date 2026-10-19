#[path = "../src/provider_connections.rs"]
mod provider_connections;
use freeshow_core::{
    protocol::{Event, Request},
    runtime::Runtime,
    service::LibraryService,
};
use provider_connections::ProviderConnections;
use serde_json::json;
use std::{
    future::Future,
    task::{Context, Poll, Waker},
    time::Duration,
};

fn fixture() -> (tempfile::TempDir, LibraryService, ProviderConnections) {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    (temp, library, ProviderConnections::new())
}
fn request(library: &LibraryService) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"GET_CONTENT_PROVIDERS","data":{"path":"ignored","providers":[{"providerId":"untrusted"}]}},"context":library.context().unwrap(),"listenerId":"catalogue-敬拜"})).unwrap()
}
fn ready(provider: &ProviderConnections, library: &LibraryService, request: Request) -> Vec<Event> {
    let mut cx = Context::from_waker(Waker::noop());
    match std::pin::pin!(provider.dispatch(library, request, "main"))
        .as_mut()
        .poll(&mut cx)
    {
        Poll::Ready(result) => result.unwrap(),
        Poll::Pending => panic!("Fixed provider declarations must not wait for async work"),
    }
}
fn check(events: &[Event], library: &LibraryService) {
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.channel, "MAIN");
    assert_eq!(event.message.channel, "GET_CONTENT_PROVIDERS");
    assert_eq!(event.listener_id.as_deref(), Some("catalogue-敬拜"));
    assert_eq!(event.context.as_ref(), Some(&library.context().unwrap()));
    assert!(event.native_lifetime.is_some());
    let rows = event.message.data.as_ref().unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(
        rows.iter()
            .map(|r| r["providerId"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "churchApps",
            "planningcenter",
            "amazinglife",
            "onstage",
            "canva"
        ]
    );
    for row in rows {
        assert_eq!(
            row.as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["providerId", "displayName", "hasContentLibrary"]
        );
    }
    let encoded = serde_json::to_value(event).unwrap();
    assert_eq!(
        encoded
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["channel", "message", "listenerId", "context"]
    );
    assert!(serde_json::to_vec(&encoded).unwrap().len() < 2048);
}
fn finish(library: &LibraryService, provider: &ProviderConnections) {
    provider.seal();
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(library.finish_writes())
        .unwrap();
}

#[test]
fn fixed_catalogue_completes_without_async_runtime_and_ignores_corrupt_global_access() {
    let (temp, library, provider) = fixture();
    let path = temp.path().join("app/ACCESS.json");
    let corrupt = b"{invalid credentials must never be inspected";
    std::fs::write(&path, corrupt).unwrap();
    let events = ready(&provider, &library, request(&library));
    check(&events, &library);
    assert_eq!(std::fs::read(path).unwrap(), corrupt);
    assert_eq!(provider.retained(), 1);
    drop(events);
    assert_eq!(provider.retained(), 0);
    finish(&library, &provider);
}

#[tokio::test]
async fn catalogue_ready_events_share_four_slots_through_clones_encoding_and_seal() {
    let (_temp, library, provider) = fixture();
    let mut held = Vec::new();
    for _ in 0..4 {
        let events = provider
            .dispatch(&library, request(&library), "main")
            .await
            .unwrap();
        check(&events, &library);
        held.push(events);
    }
    assert_eq!(provider.retained(), 4);
    let copy = held[0].clone();
    let encoded = serde_json::to_value(&held).unwrap();
    assert_eq!(encoded.as_array().unwrap().len(), 4);
    assert_eq!(
        provider.retained(),
        4,
        "Serialization does not release actual event guards"
    );
    for command in ["GET_CONTENT_PROVIDERS", "PROVIDER_CONNECTIONS"] {
        let mut q = request(&library);
        q.data["channel"] = json!(command);
        assert_eq!(
            provider
                .dispatch(&library, q, "main")
                .await
                .unwrap_err()
                .code,
            "provider_connections_busy"
        );
    }
    provider.seal();
    assert_eq!(
        provider
            .dispatch(&library, request(&library), "main")
            .await
            .unwrap_err()
            .code,
        "provider_connections_shutdown"
    );
    library.finish_writes().await.unwrap();
    assert_eq!(
        provider.retained(),
        4,
        "Library terminal fence is separate from ready-event disposal"
    );
    drop(held);
    assert_eq!(provider.retained(), 1);
    drop(copy);
    assert_eq!(provider.retained(), 0);
}

#[tokio::test]
async fn catalogue_rejects_role_context_listener_and_command_before_admission() {
    let (_temp, library, provider) = fixture();
    for role in ["output-1", "stage", "main-other"] {
        assert_eq!(
            provider
                .dispatch(&library, request(&library), role)
                .await
                .unwrap_err()
                .code,
            "provider_connections_role"
        );
    }
    for field in ["missing", "runtime", "generation", "path"] {
        let mut q = request(&library);
        match field {
            "missing" => q.context = None,
            "runtime" => q.context.as_mut().unwrap().runtime_id.push('x'),
            "generation" => q.context.as_mut().unwrap().library_generation += 1,
            "path" => q.context.as_mut().unwrap().data_path.push("foreign"),
            _ => unreachable!(),
        }
        assert_eq!(
            provider
                .dispatch(&library, q, "main")
                .await
                .unwrap_err()
                .code,
            "stale_library"
        );
    }
    let mut q = request(&library);
    q.listener_id = Some("界".repeat(2731));
    assert_eq!(
        provider
            .dispatch(&library, q, "main")
            .await
            .unwrap_err()
            .code,
        "provider_connections_resource_limit"
    );
    let mut q = request(&library);
    q.channel = "OUTPUT".into();
    assert_eq!(
        provider
            .dispatch(&library, q, "main")
            .await
            .unwrap_err()
            .code,
        "unsupported_command"
    );
    assert_eq!(provider.retained(), 0);
    provider.seal();
    library.finish_writes().await.unwrap();
}

#[tokio::test]
async fn catalogue_finishes_while_real_library_commit_is_held_then_fences_old_selection() {
    use futures_util::FutureExt;
    use std::{panic::AssertUnwindSafe, sync::mpsc};
    let (temp, library, provider) = fixture();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, held) = mpsc::sync_channel::<()>(1);
    let owner = library.clone();
    let q = request(&library);
    let commit = tokio::spawn(async move {
        owner
            .commit(q, "main", move |_| {
                let _ = entered.send(std::thread::current().name().unwrap_or("").to_string());
                let _ = held.recv();
                Ok(vec![])
            })
            .await
    });
    let result = AssertUnwindSafe(async {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), ready)
                .await
                .unwrap()
                .unwrap(),
            "freeshow-library"
        );
        let events = tokio::time::timeout(
            Duration::from_millis(500),
            provider.dispatch(&library, request(&library), "main"),
        )
        .await
        .unwrap()
        .unwrap();
        check(&events, &library);
        assert!(!commit.is_finished());
        drop(events);
    })
    .catch_unwind()
    .await;
    drop(release);
    let committed = tokio::time::timeout(Duration::from_secs(5), commit).await;
    if let Err(panic) = result {
        provider.seal();
        let _ = library.finish_writes().await;
        std::panic::resume_unwind(panic);
    }
    committed.unwrap().unwrap().unwrap();
    let old = request(&library);
    let previous = library.context().unwrap();
    let next = temp.path().join("next-library");
    std::fs::create_dir(&next).unwrap();
    let change=serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":previous.data_path,"newPath":next}},"context":previous})).unwrap();
    library.dispatch(change, "main").await.unwrap();
    assert_eq!(
        provider
            .dispatch(&library, old, "main")
            .await
            .unwrap_err()
            .code,
        "stale_library"
    );
    let events = provider
        .dispatch(&library, request(&library), "main")
        .await
        .unwrap();
    check(&events, &library);
    drop(events);
    provider.seal();
    library.finish_writes().await.unwrap();
    assert_eq!(provider.retained(), 0);
}
