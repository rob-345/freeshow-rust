#[path = "../src/system.rs"]
mod system;

use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    service::LibraryService,
};
use freeshow_system::{Limits, SystemService};
use serde_json::json;
use std::{sync::mpsc, time::Duration};
use tokio::{sync::oneshot, time::timeout};

fn request(command: &str, context: Option<&RuntimeContext>) -> Request {
    serde_json::from_value(json!({"channel":"MAIN", "data":{"channel":command}, "listenerId":"native-system-test", "context":context})).unwrap()
}
fn services() -> (tempfile::TempDir, LibraryService, SystemService) {
    let temp = tempfile::tempdir().unwrap();
    let library = LibraryService::start(
        Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap(),
    )
    .unwrap();
    (
        temp,
        library,
        SystemService::start(Limits::default()).unwrap(),
    )
}

#[tokio::test]
async fn native_system_replies_retain_contract_context_and_correlation_while_library_owner_is_blocked(
) {
    let (_temp, library, native) = services();
    let context = library.context().unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, gate) = mpsc::channel::<()>();
    let blocked = library.clone();
    let commit = request("trusted-host-test", Some(&context));
    let owner = tokio::spawn(async move {
        blocked
            .commit(commit, "main", move |_| {
                let _ = entered.send(());
                let _ = gate.recv();
                Ok(vec![])
            })
            .await
    });
    ready.await.unwrap();
    for command in system::COMMANDS {
        let events = timeout(
            Duration::from_secs(3),
            system::dispatch(&library, &native, request(command, Some(&context))),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.channel, "MAIN");
        assert_eq!(event.message.channel, *command);
        assert_eq!(event.context.as_ref(), Some(&context));
        assert_eq!(event.listener_id.as_deref(), Some("native-system-test"));
        let value = event.message.data.as_ref().unwrap();
        match *command {
            "GET_OS" => {
                assert_eq!(
                    value["platform"],
                    freeshow_system::platform_alias(std::env::consts::OS).unwrap()
                );
                assert_eq!(value.as_object().unwrap().len(), 3);
                assert!(value["name"].is_string() && value["arch"].is_string());
            }
            "GET_DEVICE_NAME" => assert!(value.is_string()),
            "CHECK_RAM_USAGE" => {
                assert!(value["total"].as_f64().unwrap() > 0.0);
                assert!(value["free"].as_f64().unwrap() >= 0.0);
                assert!(value["performanceMode"].is_boolean());
                assert_eq!(value.as_object().unwrap().len(), 3);
            }
            _ => unreachable!(),
        }
    }
    drop(release);
    owner.await.unwrap().unwrap();
    native.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_system_host_rejects_missing_or_obsolete_context_before_reading_the_os() {
    let (_temp, library, native) = services();
    let mut obsolete = library.context().unwrap();
    obsolete.library_generation += 1;
    for context in [None, Some(&obsolete)] {
        let error = system::dispatch(&library, &native, request("GET_OS", context))
            .await
            .unwrap_err();
        assert_eq!(error.code, "stale_library");
    }
    assert_eq!(native.status().active_jobs, 0);
    assert_eq!(native.status().queued_jobs, 0);
    native.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_system_shutdown_is_terminal_and_never_returns_fabricated_os_values() {
    let (_temp, library, native) = services();
    let context = library.context().unwrap();
    native.shutdown().await.unwrap();
    for command in system::COMMANDS {
        let error = system::dispatch(&library, &native, request(command, Some(&context)))
            .await
            .unwrap_err();
        assert_eq!(error.code, "system_shutdown");
    }
    let error = system::dispatch(&library, &native, request("UNSUPPORTED", Some(&context)))
        .await
        .unwrap_err();
    assert_eq!(error.code, "unsupported_command");
}
