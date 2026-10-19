use super::*;
use serde_json::json;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Condvar;
use tempfile::TempDir;
fn context(dir: &TempDir) -> DocumentContext {
    DocumentContext::new(dir.path()).unwrap()
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}
fn release(gate: &Arc<(Mutex<bool>, Condvar)>) {
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}
fn wait(gate: &Arc<(Mutex<bool>, Condvar)>) {
    let mut open = gate.0.lock().unwrap();
    while !*open {
        open = gate.1.wait(open).unwrap();
    }
}
fn park_hook(
    service: &DocumentService,
) -> (
    Arc<(Mutex<bool>, Condvar)>,
    tokio::sync::mpsc::UnboundedReceiver<()>,
) {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (send, receive) = tokio::sync::mpsc::unbounded_channel();
    let pause = gate.clone();
    *service.inner.hooks.process.lock().unwrap() = Some(Arc::new(move || {
        send.send(()).unwrap();
        wait(&pause);
    }));
    (gate, receive)
}
fn file_task(
    service: &DocumentService,
    context: &DocumentContext,
    path: &Path,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<Result<PreparedDocumentReply>> {
    let service = service.clone();
    let context = context.clone();
    let data = json!({"path":path});
    tokio::spawn(async move {
        service
            .dispatch_with_cancel("READ_FILE", &data, &context, cancel)
            .await
    })
}
#[tokio::test]
async fn construction_validation_and_terminal_shutdown() {
    let dir = TempDir::new().unwrap();
    let context = context(&dir);
    let config = DocumentConfig {
        queued_jobs: 0,
        ..Default::default()
    };
    assert_eq!(
        DocumentService::new(config).err().unwrap().code,
        "invalid_configuration"
    );
    assert_eq!(
        DocumentContext::new("relative").unwrap_err().code,
        "invalid_context"
    );
    let service = DocumentService::new(DocumentConfig::default()).unwrap();
    assert_eq!(
        service
            .dispatch("OTHER", &Value::Null, &context)
            .await
            .unwrap_err()
            .code,
        "unsupported_command"
    );
    for data in [
        Value::Null,
        json!({}),
        json!({"path":3}),
        json!({"path":[]}),
    ] {
        assert_eq!(
            service
                .dispatch("READ_FILE", &data, &context)
                .await
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        service
            .dispatch_with_cancel("READ_FILE", &json!({"path":""}), &context, cancel)
            .await
            .unwrap_err()
            .code,
        "cancelled"
    );
    service.shutdown().await.unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(
        service
            .dispatch("READ_FILE", &json!({"path":""}), &context)
            .await
            .unwrap_err()
            .code,
        "documents_shutdown"
    );
    assert!(service.inner.workers.lock().unwrap().is_empty());
}
#[test]
fn shutdown_without_runtime_is_explicit_and_can_be_drained_later() {
    let service = DocumentService::new(DocumentConfig::default()).unwrap();
    {
        let mut shutdown = std::pin::pin!(service.shutdown());
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        match std::future::Future::poll(shutdown.as_mut(), &mut cx) {
            std::task::Poll::Ready(result) => {
                assert_eq!(result.unwrap_err().code, "runtime_required")
            }
            _ => panic!("No-runtime shutdown must not wait"),
        }
    }
    runtime().block_on(service.shutdown()).unwrap();
}
#[tokio::test]
async fn shutdown_both_admission_orderings_cannot_spawn_late_work() {
    let dir = TempDir::new().unwrap();
    let context = context(&dir);
    for after in [false, true] {
        let service = DocumentService::new(DocumentConfig::default()).unwrap();
        let cancel = service.inner.cancel.clone();
        let hook: Box<dyn FnOnce() + Send> = Box::new(move || cancel.cancel());
        if after {
            *service.inner.hooks.after_permit.lock().unwrap() = Some(hook);
        } else {
            *service.inner.hooks.before_permit.lock().unwrap() = Some(hook);
        }
        assert_eq!(
            service
                .dispatch("READ_FILE", &json!({"path":""}), &context)
                .await
                .unwrap_err()
                .code,
            "documents_shutdown"
        );
        assert_eq!(
            service.inner.permits.available_permits(),
            WORKERS + service.inner.config.queued_jobs
        );
        service.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn exactly_two_workers_bounded_queue_and_dropped_queued_callers_hold_permits_until_drained() {
    let dir = TempDir::new().unwrap();
    let context = context(&dir);
    let service = DocumentService::new(DocumentConfig {
        queued_jobs: 1,
        ..Default::default()
    })
    .unwrap();
    let (gate, mut started) = park_hook(&service);
    let first = file_task(&service, &context, dir.path(), CancellationToken::new());
    let second = file_task(&service, &context, dir.path(), CancellationToken::new());
    started.recv().await.unwrap();
    started.recv().await.unwrap();
    let third = file_task(&service, &context, dir.path(), CancellationToken::new());
    tokio::task::yield_now().await;
    assert_eq!(service.inner.permits.available_permits(), 0);
    assert_eq!(
        service
            .dispatch("READ_FILE", &json!({"path":""}), &context)
            .await
            .unwrap_err()
            .code,
        "documents_busy"
    );
    third.abort();
    let _ = third.await;
    assert_eq!(service.inner.permits.available_permits(), 0);
    release(&gate);
    let _ = first.await.unwrap();
    let _ = second.await.unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(service.inner.permits.available_permits(), 3);
}
#[tokio::test]
async fn shutdown_waits_for_workers_after_caller_cancellation_and_closes_admission() {
    let dir = TempDir::new().unwrap();
    let context = context(&dir);
    let service = DocumentService::new(DocumentConfig::default()).unwrap();
    let (gate, mut started) = park_hook(&service);
    let cancel = CancellationToken::new();
    let task = file_task(&service, &context, dir.path(), cancel.clone());
    started.recv().await.unwrap();
    cancel.cancel();
    assert_eq!(task.await.unwrap().unwrap_err().code, "cancelled");
    let shutdown_service = service.clone();
    let shutdown = tokio::spawn(async move { shutdown_service.shutdown().await });
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished());
    assert_eq!(
        service
            .dispatch("READ_FILE", &json!({"path":""}), &context)
            .await
            .unwrap_err()
            .code,
        "documents_shutdown"
    );
    release(&gate);
    shutdown.await.unwrap().unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn chunk_and_directory_entry_cancellation_are_cooperative_and_reaped() {
    for directory in [false, true] {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        std::fs::create_dir(context.bibles_root()).unwrap();
        let path = dir.path().join("read.txt");
        std::fs::write(&path, vec![b'a'; 100000]).unwrap();
        for name in ["a.fsb", "b.FSB", "c.json"] {
            std::fs::write(context.bibles_root().join(name), b"x").unwrap();
        }
        let service = DocumentService::new(DocumentConfig::default()).unwrap();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            token.cancel();
        });
        if directory {
            *service.inner.hooks.entry.lock().unwrap() = Some(hook);
        } else {
            *service.inner.hooks.chunk.lock().unwrap() = Some(hook);
        }
        let result = if directory {
            service
                .dispatch_with_cancel("READ_BIBLES_FOLDER", &Value::Null, &context, cancel)
                .await
        } else {
            service
                .dispatch_with_cancel("READ_FILE", &json!({"path":path}), &context, cancel)
                .await
        };
        assert_eq!(result.unwrap_err().code, "cancelled");
        service.shutdown().await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn context_roots_are_frozen_per_request_and_read_file_can_use_selected_paths_elsewhere() {
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    let left = context(&first);
    let right = context(&second);
    std::fs::create_dir(left.bibles_root()).unwrap();
    std::fs::create_dir(right.bibles_root()).unwrap();
    std::fs::write(left.bibles_root().join("left.fsb"), b"one").unwrap();
    std::fs::write(right.bibles_root().join("right.FSB"), b"two").unwrap();
    let service = DocumentService::new(DocumentConfig::default()).unwrap();
    let left_payload = json!({"path":right.bibles_root()});
    let right_payload = json!({"path":left.bibles_root()});
    let (a, b) = tokio::join!(
        service.dispatch("READ_BIBLES_FOLDER", &left_payload, &left),
        service.dispatch("READ_BIBLES_FOLDER", &right_payload, &right)
    );
    assert_eq!(
        a.unwrap().data,
        json!([{"path":left.bibles_root().join("left.fsb"),"name":"left"}])
    );
    assert_eq!(
        b.unwrap().data,
        json!([{"path":right.bibles_root().join("right.FSB"),"name":"right"}])
    );
    let selected = right.bibles_root().join("right.FSB");
    assert_eq!(
        service
            .dispatch("READ_FILE", &json!({"path":selected}), &left)
            .await
            .unwrap()
            .data,
        json!({"content":"two"})
    );
    service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn missing_and_file_bibles_roots_require_host_preparation_without_writes() {
    let dir = TempDir::new().unwrap();
    let context = context(&dir);
    let service = DocumentService::new(DocumentConfig::default()).unwrap();
    for file in [false, true] {
        if file {
            std::fs::write(context.bibles_root(), b"keep").unwrap();
        }
        let result = service
            .dispatch("READ_BIBLES_FOLDER", &Value::Null, &context)
            .await
            .unwrap();
        assert_eq!(result.data, json!([]));
        assert!(!result.is_complete());
        assert_eq!(result.folder_intents.len(), 1);
        let FolderIntent::EnsureBiblesDirectory { path } = &result.folder_intents[0];
        assert_eq!(path, context.bibles_root());
        if file {
            assert_eq!(std::fs::read(context.bibles_root()).unwrap(), b"keep");
        } else {
            assert!(!context.bibles_root().exists());
        }
    }
    service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn resource_limits_never_turn_into_successful_empty_replies() {
    let dir = TempDir::new().unwrap();
    let context = context(&dir);
    let path = dir.path().join("read");
    std::fs::write(&path, b"12345678").unwrap();
    std::fs::create_dir(context.bibles_root()).unwrap();
    std::fs::write(context.bibles_root().join("a"), b"").unwrap();
    std::fs::write(context.bibles_root().join("b"), b"").unwrap();
    for (config, command, data) in [
        (
            DocumentConfig {
                read_bytes: 7,
                ..Default::default()
            },
            "READ_FILE",
            json!({"path":path}),
        ),
        (
            DocumentConfig {
                output_bytes: 2,
                ..Default::default()
            },
            "READ_FILE",
            json!({"path":path}),
        ),
        (
            DocumentConfig {
                operations: 1,
                ..Default::default()
            },
            "READ_FILE",
            json!({"path":path}),
        ),
        (
            DocumentConfig {
                path_bytes: 1,
                ..Default::default()
            },
            "READ_FILE",
            json!({"path":path}),
        ),
        (
            DocumentConfig {
                entries: 1,
                ..Default::default()
            },
            "READ_BIBLES_FOLDER",
            Value::Null,
        ),
        (
            DocumentConfig {
                output_bytes: 2,
                ..Default::default()
            },
            "READ_BIBLES_FOLDER",
            Value::Null,
        ),
    ] {
        let service = DocumentService::new(config).unwrap();
        assert_eq!(
            service
                .dispatch(command, &data, &context)
                .await
                .unwrap_err()
                .code,
            "document_resource_limit"
        );
        service.shutdown().await.unwrap();
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn queued_deadline_expires_before_filesystem_work() {
    let dir = TempDir::new().unwrap();
    let context = context(&dir);
    let service = DocumentService::new(DocumentConfig {
        timeout: Duration::from_millis(5),
        ..Default::default()
    })
    .unwrap();
    let (gate, mut started) = park_hook(&service);
    let first = file_task(&service, &context, dir.path(), CancellationToken::new());
    let second = file_task(&service, &context, dir.path(), CancellationToken::new());
    started.recv().await.unwrap();
    started.recv().await.unwrap();
    let queued = file_task(&service, &context, dir.path(), CancellationToken::new());
    tokio::task::yield_now().await;
    assert_eq!(service.inner.permits.available_permits(), 7);
    tokio::time::sleep(Duration::from_millis(10)).await;
    release(&gate);
    for task in [first, second, queued] {
        assert_eq!(task.await.unwrap().unwrap_err().code, "document_timeout");
    }
    service.shutdown().await.unwrap();
}
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn other_platforms_are_explicitly_unsupported() {
    let dir = TempDir::new().unwrap();
    let context = context(&dir);
    let service = DocumentService::new(DocumentConfig::default()).unwrap();
    for (command, payload) in [
        ("READ_FILE", json!({"path":dir.path()})),
        ("READ_BIBLES_FOLDER", Value::Null),
    ] {
        assert_eq!(
            service
                .dispatch(command, &payload, &context)
                .await
                .unwrap_err()
                .code,
            "unsupported_platform"
        );
    }
    service.shutdown().await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn special_fifo_is_rejected_without_opening_and_growth_cannot_exceed_read_limit() {
    use std::os::unix::ffi::OsStrExt;
    let dir = TempDir::new().unwrap();
    let context = context(&dir);
    let fifo = dir.path().join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: valid NUL-terminated temporary fixture path; no file is opened.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let service = DocumentService::new(DocumentConfig::default()).unwrap();
    assert_eq!(
        service
            .dispatch("READ_FILE", &json!({"path":fifo}), &context)
            .await
            .unwrap_err()
            .code,
        "unsupported_file_type"
    );
    service.shutdown().await.unwrap();
    let path = dir.path().join("growing");
    std::fs::write(&path, vec![b'a'; 32768]).unwrap();
    let service = DocumentService::new(DocumentConfig {
        read_bytes: 40000,
        ..Default::default()
    })
    .unwrap();
    let growing = path.clone();
    *service.inner.hooks.chunk.lock().unwrap() = Some(Arc::new(move || {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&growing)
            .unwrap()
            .write_all(&[b'b'; 20000])
            .unwrap();
    }));
    assert_eq!(
        service
            .dispatch("READ_FILE", &json!({"path":path}), &context)
            .await
            .unwrap_err()
            .code,
        "document_resource_limit"
    );
    service.shutdown().await.unwrap();
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn encoded_output_expansion_is_counted_without_allocating_a_second_json_copy() {
    let dir = TempDir::new().unwrap();
    let context = context(&dir);
    let path = dir.path().join("nul");
    std::fs::write(&path, [0; 300]).unwrap();
    let service = DocumentService::new(DocumentConfig {
        output_bytes: 1000,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        service
            .dispatch("READ_FILE", &json!({"path":path}), &context)
            .await
            .unwrap_err()
            .code,
        "document_resource_limit"
    );
    service.shutdown().await.unwrap();
}
