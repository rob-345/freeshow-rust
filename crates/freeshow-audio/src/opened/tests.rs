use super::*;
use crate::{AudioService, CommonValue};
use std::{
    future::Future,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    pin::pin,
    sync::{atomic::Ordering, mpsc},
    task::{Context, Poll, Wake, Waker},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWake(std::thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::park_timeout(Duration::from_secs(3)),
        }
    }
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "freeshow-audio-opened-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> File {
        let p = self.0.join(name);
        std::fs::write(&p, bytes).unwrap();
        File::open(p).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn wav(tags: &[(&str, &str)]) -> Vec<u8> {
    let mut out = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0data\x02\0\0\0\0\0".to_vec();
    let mut info = b"INFO".to_vec();
    for (key, value) in tags {
        info.extend(key.as_bytes());
        info.extend(((value.len() + 1) as u32).to_le_bytes());
        info.extend(value.as_bytes());
        info.push(0);
        if (value.len() + 1) % 2 != 0 {
            info.push(0);
        }
    }
    out.extend(b"LIST");
    out.extend((info.len() as u32).to_le_bytes());
    out.extend(info);
    let size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    out
}
fn submit(service: &AudioService, file: File) -> OpenedTicket {
    service
        .reserve_opened()
        .unwrap()
        .submit(
            file,
            "source 音楽.wav",
            FormatHint::FromSourceName,
            CancellationToken::new(),
        )
        .unwrap()
}
fn gated(
    service: &AudioService,
    file: File,
    after_read: bool,
) -> (OpenedTicket, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let permit = service.inner.acquire_job().unwrap();
    let (entered, event) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let (response, receive) = oneshot::channel();
    let mut work = Work {
        file,
        source_name: "fixture.wav".into(),
        hint: FormatHint::FromSourceName,
        cancel: CancellationToken::new(),
        shutdown: service.inner.cancel.clone(),
        response,
        permit,
        gate: None,
        after_read_gate: None,
    };
    if after_read {
        work.after_read_gate = Some((entered, resume));
    } else {
        work.gate = Some((entered, resume));
    }
    service
        .inner
        .sender
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .try_send(crate::Work::Opened(work))
        .ok()
        .unwrap();
    (OpenedTicket { receive }, event, release)
}

#[test]
fn held_descriptor_survives_path_replacement_and_is_reusable_sequentially() {
    let fixture = Fixture::new();
    let mut file = fixture.file("音楽.wav", &wav(&[("INAM", "Original")]));
    file.seek(SeekFrom::End(0)).unwrap();
    std::fs::rename(fixture.0.join("音楽.wav"), fixture.0.join("old.wav")).unwrap();
    let replacement = fixture.file("音楽.wav", &wav(&[("INAM", "Replacement")]));
    let service = AudioService::new(AudioConfig::default()).unwrap();
    let read = block_on(submit(&service, file).finish()).unwrap();
    assert_eq!(
        read.common().unwrap().json_projection()["title"],
        "Original"
    );
    assert_eq!(read.common().unwrap().covers(), CoverKnowledge::Unknown);
    assert_eq!(read.observations().unchanged(), Some(true));
    assert_ne!(
        read.observations().before.as_ref().unwrap().identity,
        observe(&replacement).unwrap().identity
    );
    let (common, mut file, observed) = read.into_parts();
    assert_eq!(service.inner.permits.available_permits(), 2);
    assert_eq!(common.unwrap().json_projection()["title"], "Original");
    assert_eq!(observed.before, observed.after);
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut riff = [0; 4];
    file.read_exact(&mut riff).unwrap();
    assert_eq!(&riff, b"RIFF");
    // Reuse the same File after another sequential reader moved its cursor.
    file.seek(SeekFrom::End(0)).unwrap();
    let read = block_on(submit(&service, file).finish()).unwrap();
    assert_eq!(
        read.common().unwrap().json_projection()["title"],
        "Original"
    );
    drop(read);
    block_on(service.shutdown());
}

#[test]
fn actual_invalid_year_retains_nan_before_compatibility_projection() {
    let fixture = Fixture::new();
    let service = AudioService::new(AudioConfig::default()).unwrap();
    let read =
        block_on(submit(&service, fixture.file("bad.wav", &wav(&[("YEAR", "nope")]))).finish())
            .unwrap();
    let common = read.common().unwrap();
    assert!(matches!(common.value().get("year"), CommonValue::Number(n) if n.is_nan()));
    assert_eq!(common.json_projection()["year"], serde_json::Value::Null);
    assert_eq!(common.covers(), CoverKnowledge::Unknown);
    let (common, file, _) = read.into_parts();
    assert!(
        matches!(common.unwrap().into_value().get("year"), CommonValue::Number(n) if n.is_nan())
    );
    drop(file);
    block_on(service.shutdown());
}

#[test]
fn native_value_keeps_own_undefined_nonfinite_and_negative_zero_separate() {
    let common = NativeCommon {
        value: Js::Object(vec![
            ("own".into(), Js::Undefined),
            (
                "numbers".into(),
                Js::Array(vec![
                    Js::Number(f64::NAN),
                    Js::Number(f64::INFINITY),
                    Js::Number(f64::NEG_INFINITY),
                    Js::Number(-0.0),
                ]),
            ),
        ]),
    };
    assert!(
        matches!(common.value(), Js::Object(fields) if fields[0].0 == "own" && matches!(fields[0].1, Js::Undefined))
    );
    assert!(
        matches!(common.value().get("numbers"), Js::Array(v) if matches!(v[3], Js::Number(n) if n.is_sign_negative()))
    );
    assert_eq!(
        common.json_projection(),
        serde_json::json!({"numbers":[null,null,null,-0.0]})
    );
    assert!(!raw_within_limit(&common.value, 1, 100));
    assert!(!raw_within_limit(&common.value, 10000, 1));
}

#[test]
fn format_hint_and_source_name_are_not_paths_or_permission_grants() {
    let fixture = Fixture::new();
    let service = AudioService::new(AudioConfig::default()).unwrap();
    let read = block_on(
        service
            .reserve_opened()
            .unwrap()
            .submit(
                fixture.file("ok.wav", &wav(&[])),
                "/this/does/not/exist.mp3",
                FormatHint::FromSourceName,
                CancellationToken::new(),
            )
            .unwrap()
            .finish(),
    )
    .unwrap();
    assert!(matches!(
        read.common(),
        Err(AudioError::UnsupportedVariant(_))
    ));
    drop(read);
    for name in ["", "a\0.wav", &"a".repeat(32769)] {
        assert!(matches!(
            service.reserve_opened().unwrap().submit(
                File::open(fixture.0.join("ok.wav")).unwrap(),
                name,
                FormatHint::Detect,
                CancellationToken::new()
            ),
            Err(AudioError::Invalid)
        ));
    }
    let read = block_on(
        service
            .reserve_opened()
            .unwrap()
            .submit(
                File::open(fixture.0.join("ok.wav")).unwrap(),
                "no-such-file.bin",
                FormatHint::FromSourceName,
                CancellationToken::new(),
            )
            .unwrap()
            .finish(),
    )
    .unwrap();
    assert!(read.common().is_ok());
    drop(read);
    block_on(service.shutdown());
}

#[test]
fn ready_results_hold_shared_capacity_until_actual_consumption_or_disposal() {
    let fixture = Fixture::new();
    let service = AudioService::new(AudioConfig {
        max_jobs: 1,
        ..AudioConfig::default()
    })
    .unwrap();
    let read = block_on(submit(&service, fixture.file("ok.wav", &wav(&[]))).finish()).unwrap();
    assert!(matches!(service.reserve_opened(), Err(AudioError::Busy)));
    assert!(matches!(
        block_on(service.dispatch(
            "READ_AUDIO_METADATA",
            serde_json::json!({"filePath":"missing"})
        )),
        Err(AudioError::Busy)
    ));
    let (common, file, _) = read.into_parts();
    assert!(common.is_ok());
    // Actual handoff means external ownership, not a claim that File was closed.
    assert!(file.metadata().unwrap().is_file());
    assert!(service.reserve_opened().is_ok());
    drop(file);
    block_on(service.shutdown());
}

#[tokio::test]
async fn caller_drop_and_shutdown_retain_blocked_accepted_descriptor_until_real_return() {
    let fixture = Fixture::new();
    let service = AudioService::new(AudioConfig {
        max_jobs: 1,
        ..AudioConfig::default()
    })
    .unwrap();
    let file = fixture.file("ok.wav", &wav(&[]));
    #[cfg(target_os = "linux")]
    let descriptor = {
        use std::os::fd::AsRawFd;
        format!("/proc/self/fd/{}", file.as_raw_fd())
    };
    let (ticket, entered, release) = gated(&service, file, false);
    entered.recv_timeout(Duration::from_secs(3)).unwrap();
    drop(ticket);
    let mut shutdown = Box::pin(service.shutdown());
    tokio::select! { biased; _ = &mut shutdown => panic!("fake drain"), _ = tokio::task::yield_now() => {} }
    assert_eq!(service.inner.permits.available_permits(), 0);
    #[cfg(target_os = "linux")]
    assert!(std::fs::metadata(&descriptor).is_ok());
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap();
    assert_eq!(
        service.inner.completion.remaining.load(Ordering::Acquire),
        0
    );
    #[cfg(target_os = "linux")]
    assert_ne!(
        std::fs::read_link(&descriptor).ok(),
        Some(fixture.0.join("ok.wav"))
    );
}

#[tokio::test]
async fn completed_uncollected_ticket_retains_file_and_quota_until_disposed() {
    struct ReadyWake(mpsc::Sender<()>);
    impl Wake for ReadyWake {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            let _ = self.0.send(());
        }
    }
    let fixture = Fixture::new();
    let service = AudioService::new(AudioConfig {
        max_jobs: 1,
        ..AudioConfig::default()
    })
    .unwrap();
    let (mut ticket, entered, release) = gated(&service, fixture.file("ok.wav", &wav(&[])), false);
    entered.recv_timeout(Duration::from_secs(3)).unwrap();
    let (ready, ready_event) = mpsc::channel();
    let waker = Waker::from(Arc::new(ReadyWake(ready)));
    assert!(std::pin::Pin::new(&mut ticket.receive)
        .poll(&mut Context::from_waker(&waker))
        .is_pending());
    release.send(()).unwrap();
    ready_event.recv_timeout(Duration::from_secs(3)).unwrap();
    // Worker sent the actual result, but nobody has consumed the oneshot.
    assert_eq!(service.inner.permits.available_permits(), 0);
    let mut shutdown = Box::pin(service.shutdown());
    tokio::select! { biased; _ = &mut shutdown => panic!("uncollected file escaped drain"), _ = tokio::task::yield_now() => {} }
    drop(ticket); // Actually disposes the completed private descriptor/result.
    tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap();
}

#[test]
fn cancellation_after_actual_parse_rejects_publication_but_returns_the_owned_file() {
    let fixture = Fixture::new();
    let service = AudioService::new(AudioConfig::default()).unwrap();
    let (ticket, parsed, release) = gated(&service, fixture.file("ok.wav", &wav(&[])), true);
    parsed.recv_timeout(Duration::from_secs(3)).unwrap();
    // This is global retirement after actual parsing and before ready publication.
    service.inner.cancel.cancel();
    release.send(()).unwrap();
    let read = block_on(ticket.finish()).unwrap();
    assert!(matches!(read.common(), Err(AudioError::Cancelled)));
    let (_, file, observed) = read.into_parts();
    assert_eq!(observed.unchanged(), Some(true));
    assert!(file.metadata().unwrap().is_file());
    drop(file);
    block_on(service.shutdown());
}

#[tokio::test]
async fn shutdown_waits_owned_ready_result_and_admission_cannot_restart() {
    let fixture = Fixture::new();
    let service = AudioService::new(AudioConfig::default()).unwrap();
    let read = submit(&service, fixture.file("ok.wav", &wav(&[])))
        .finish()
        .await
        .unwrap();
    let mut shutdown = Box::pin(service.shutdown());
    tokio::select! { biased; _ = &mut shutdown => panic!("ready result was discarded"), _ = tokio::task::yield_now() => {} }
    assert!(matches!(
        service.reserve_opened(),
        Err(AudioError::Shutdown)
    ));
    drop(read);
    tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap();
    service.shutdown().await;
}

#[test]
fn native_changes_are_reported_without_certifying_an_immutable_snapshot() {
    let fixture = Fixture::new();
    let service = AudioService::new(AudioConfig::default()).unwrap();
    let (ticket, parsed, release) = gated(
        &service,
        fixture.file("ok.wav", &wav(&[("INAM", "Before")])),
        true,
    );
    parsed.recv_timeout(Duration::from_secs(3)).unwrap();
    std::fs::write(
        fixture.0.join("ok.wav"),
        wav(&[("INAM", "Longer after content")]),
    )
    .unwrap();
    release.send(()).unwrap();
    let read = block_on(ticket.finish()).unwrap();
    assert!(matches!(read.common(), Err(AudioError::Changed)));
    let observed = read.observations();
    assert_eq!(observed.unchanged(), Some(false));
    assert_eq!(
        observed.before.as_ref().unwrap().identity,
        observed.after.as_ref().unwrap().identity
    );
    assert_ne!(
        observed.before.as_ref().unwrap().length,
        observed.after.as_ref().unwrap().length
    );
    drop(read);
    block_on(service.shutdown());
}

#[test]
fn malformed_resources_and_cancelled_inputs_return_owned_receipts() {
    let fixture = Fixture::new();
    let service = AudioService::new(AudioConfig {
        output_limit: 32,
        ..AudioConfig::default()
    })
    .unwrap();
    let read = block_on(submit(&service, fixture.file("ok.wav", &wav(&[]))).finish()).unwrap();
    assert!(matches!(read.common(), Err(AudioError::Resource(_))));
    drop(read);
    let read = block_on(submit(&service, fixture.file("bad.wav", b"corrupt")).finish()).unwrap();
    assert!(matches!(read.common(), Err(AudioError::Input)));
    assert_eq!(read.observations().unchanged(), Some(true));
    let (_, file, _) = read.into_parts();
    assert!(file.metadata().unwrap().is_file());
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        service
            .reserve_opened()
            .unwrap()
            .submit(file, "fixture.wav", FormatHint::Detect, cancel),
        Err(AudioError::Cancelled)
    ));
    block_on(service.shutdown());
}
