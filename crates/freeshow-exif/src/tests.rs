use super::*;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Condvar, Mutex,
};

pub(crate) struct Gate {
    entered: AtomicUsize,
    released: Mutex<bool>,
    changed: Condvar,
    cooperative: bool,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: AtomicUsize::new(0),
            released: Mutex::new(false),
            changed: Condvar::new(),
            cooperative: true,
        })
    }
    pub(crate) fn wait(&self, control: &Control, closing: &AtomicBool) -> Result<(), Error> {
        self.entered.fetch_add(1, Ordering::AcqRel);
        let mut released = self.released.lock().unwrap();
        while !*released {
            if self.cooperative {
                control.check(closing)?;
            }
            released = self
                .changed
                .wait_timeout(released, Duration::from_millis(2))
                .unwrap()
                .0;
        }
        control.check(closing)
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
fn context() -> Context {
    Context {
        runtime_id: "test-runtime".into(),
        library_generation: 7,
        data_path: "/selected/日本語".into(),
    }
}
fn fixture() -> &'static [u8] {
    include_bytes!("../fixtures/full-II.jpg")
}
fn parse(data: &[u8], limits: Limits) -> Result<ExifOutcome, Error> {
    parse_bytes(
        "/image.jpg".into(),
        context(),
        data,
        &limits,
        Control::default(),
    )
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn image_file() -> (tempfile::TempDir, String) {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("日本語 é.jpg");
    std::fs::write(&path, fixture()).unwrap();
    (folder, path.to_str().unwrap().to_owned())
}

#[test]
fn source_values_keep_six_groups_timestamp_bytes_and_nonfinite_hydration() {
    let out = parse(fixture(), Limits::default()).unwrap();
    assert_eq!(out.context, context());
    assert!(out.source_compatible);
    assert_eq!(out.reply["exif"].as_object().unwrap().len(), 6);
    assert_eq!(out.reply["exif"]["image"]["Make"], "Écamera\0");
    assert_eq!(
        out.reply["exif"]["exif"]["DateTimeOriginal"],
        "2001:02:03 04:05:06"
    );
    assert_eq!(
        out.hydration
            .special_numbers
            .iter()
            .map(|x| x.kind.clone())
            .collect::<Vec<_>>(),
        vec![
            NumberKind::PositiveInfinity,
            NumberKind::NaN,
            NumberKind::NegativeZero,
            NumberKind::NegativeInfinity
        ]
    );
    assert!(out
        .hydration
        .byte_buffers
        .contains(&"/exif/exif/ExifVersion".into()));
    assert!(out.stats.input_bytes == fixture().len() && out.stats.output_bytes > 0);
    assert_eq!(
        serde_json::to_vec(&out).unwrap().len(),
        out.stats.output_bytes
    );
    for data in [
        b"not a JPEG".as_slice(),
        include_bytes!("../fixtures/raw-II.tiff"),
    ] {
        let out = parse(data, Limits::default()).unwrap();
        assert!(out.reply.get("exif").is_none());
        assert_eq!(out.hydration.undefined_paths, vec!["/exif"]);
        assert!(out.source_compatible);
    }
}
#[test]
fn every_resource_budget_rejects_before_unbounded_work_or_allocation() {
    let limits = Limits::default();
    for limits in [
        Limits {
            input_bytes: fixture().len() - 1,
            ..limits.clone()
        },
        Limits {
            tags: 1,
            ..limits.clone()
        },
        Limits {
            components: 1,
            ..limits.clone()
        },
        Limits {
            depth: 1,
            ..limits.clone()
        },
        Limits {
            allocation_bytes: 128,
            ..limits.clone()
        },
        Limits {
            output_bytes: 128,
            ..limits.clone()
        },
        Limits {
            operations: 2,
            ..limits.clone()
        },
    ] {
        assert_eq!(parse(fixture(), limits).unwrap_err().code, "exif_limit");
    }
    assert_eq!(
        parse(
            b"invalid",
            Limits {
                output_bytes: 1,
                ..limits
            }
        )
        .unwrap_err()
        .code,
        "exif_limit"
    );
    let mut data = fixture().to_vec();
    // First IFD0 tag is recognized ImageWidth. Source would attempt billions
    // of iterations; the native bound rejects the header before Vec allocation.
    data[26..30].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        parse(&data, Limits::default()).unwrap_err().code,
        "exif_limit"
    );
    assert_eq!(
        ExifService::start(Limits {
            queue_capacity: 65,
            ..Limits::default()
        })
        .err()
        .unwrap()
        .code,
        "exif_invalid"
    );
}
#[test]
fn truncation_offsets_and_deterministic_mutations_never_panic_or_escape_limits() {
    for end in 0..fixture().len() {
        let out = parse(&fixture()[..end], Limits::default());
        assert!(out.is_ok() || out.unwrap_err().code == "exif_limit");
    }
    let mut seed = 0x65786966_u64;
    for _ in 0..1000 {
        let mut bytes = fixture().to_vec();
        for _ in 0..4 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let index = seed as usize % bytes.len();
            bytes[index] = (seed >> 32) as u8;
        }
        match parse(&bytes, Limits::default()) {
            Ok(out) => assert!(out.stats.allocated_bytes <= Limits::default().allocation_bytes),
            Err(e) => assert!(matches!(e.code.as_str(), "exif_limit" | "exif_deadline")),
        }
    }
}
#[tokio::test]
async fn fixed_two_workers_bounded_admission_cancelled_queue_and_recovery() {
    let (_folder, id) = image_file();
    let service = ExifService::start(Limits {
        queue_capacity: 1,
        ..Limits::default()
    })
    .unwrap();
    let gate = Gate::new();
    service.gate(gate.clone());
    let first = {
        let service = service.clone();
        let id = id.clone();
        tokio::spawn(async move { service.read(id, context(), Control::default()).await })
    };
    until(|| gate.entered.load(Ordering::Acquire) == 1).await;
    let second = {
        let service = service.clone();
        let id = id.clone();
        tokio::spawn(async move { service.read(id, context(), Control::default()).await })
    };
    until(|| gate.entered.load(Ordering::Acquire) == 2).await;
    assert_eq!(service.status().active, 2);
    assert_eq!(service.status().workers, 2);
    let cancel = CancellationToken::new();
    let control = Control {
        cancel: cancel.clone(),
        ..Control::default()
    };
    let third = {
        let service = service.clone();
        let id = id.clone();
        tokio::spawn(async move { service.read(id, context(), control).await })
    };
    until(|| service.status().queued == 1).await;
    assert_eq!(
        service
            .read(id.clone(), context(), Control::default())
            .await
            .unwrap_err()
            .code,
        "exif_busy"
    );
    cancel.cancel();
    assert_eq!(third.await.unwrap().unwrap_err().code, "exif_cancelled");
    gate.release();
    assert!(first.await.unwrap().unwrap().reply.get("exif").is_some());
    assert!(second.await.unwrap().unwrap().reply.get("exif").is_some());
    until(|| {
        service.status().active == 0
            && service.status().queued == 0
            && service.status().admitted == 0
    })
    .await;
    assert!(
        service
            .read(id, context(), Control::default())
            .await
            .unwrap()
            .source_compatible
    );
    service.shutdown().await.unwrap();
    assert_eq!(service.status().workers, 0);
}
#[tokio::test]
async fn cancellation_deadline_dropped_futures_and_shutdown_drain_real_worker_boundary() {
    let (_folder, id) = image_file();
    let service = ExifService::start(Limits::default()).unwrap();
    let gate = Gate::new();
    service.gate(gate.clone());
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        service
            .read(
                id.clone(),
                context(),
                Control {
                    cancel,
                    ..Control::default()
                }
            )
            .await
            .unwrap_err()
            .code,
        "exif_cancelled"
    );
    assert_eq!(
        service
            .read(
                id.clone(),
                context(),
                Control {
                    deadline: Instant::now(),
                    ..Control::default()
                }
            )
            .await
            .unwrap_err()
            .code,
        "exif_deadline"
    );
    let timed = {
        let service = service.clone();
        let id = id.clone();
        tokio::spawn(async move {
            service
                .read(
                    id,
                    context(),
                    Control {
                        deadline: Instant::now() + Duration::from_millis(30),
                        ..Control::default()
                    },
                )
                .await
        })
    };
    until(|| service.status().active == 1).await;
    assert_eq!(timed.await.unwrap().unwrap_err().code, "exif_deadline");
    until(|| service.status().active == 0).await;
    let dropped = {
        let service = service.clone();
        let id = id.clone();
        tokio::spawn(async move { service.read(id, context(), Control::default()).await })
    };
    until(|| service.status().active == 1).await;
    dropped.abort();
    let _ = dropped.await;
    until(|| service.status().active == 0 && service.status().admitted == 0).await;
    let pending = {
        let service = service.clone();
        let id = id.clone();
        tokio::spawn(async move { service.read(id, context(), Control::default()).await })
    };
    until(|| service.status().active == 1).await;
    service.shutdown().await.unwrap();
    assert_eq!(pending.await.unwrap().unwrap_err().code, "exif_shutdown");
    assert_eq!(service.status().workers, 0);
    assert_eq!(
        service
            .read(id, context(), Control::default())
            .await
            .unwrap_err()
            .code,
        "exif_shutdown"
    );
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn contexts_are_captured_not_silently_replaced_and_invalid_contexts_reject() {
    let (_folder, id) = image_file();
    let service = ExifService::start(Limits::default()).unwrap();
    let first = service
        .read(id.clone(), context(), Control::default())
        .await
        .unwrap();
    let mut newer = context();
    newer.library_generation += 1;
    newer.data_path = "/new-library".into();
    let second = service
        .read(id.clone(), newer.clone(), Control::default())
        .await
        .unwrap();
    assert_eq!(first.context, context());
    assert_eq!(second.context, newer);
    let mut invalid = context();
    invalid.library_generation = u64::MAX;
    assert_eq!(
        service
            .read(id, invalid, Control::default())
            .await
            .unwrap_err()
            .code,
        "exif_invalid"
    );
    service.shutdown().await.unwrap();
}
#[cfg(unix)]
#[tokio::test]
async fn real_permission_symlink_loop_and_fifo_inputs_do_not_hold_workers() {
    use std::{
        ffi::CString,
        os::unix::fs::{symlink, PermissionsExt},
    };
    let (folder, id) = image_file();
    let service = ExifService::start(Limits::default()).unwrap();
    std::fs::set_permissions(&id, std::fs::Permissions::from_mode(0o0)).unwrap();
    let result = service
        .read(id.clone(), context(), Control::default())
        .await
        .unwrap();
    // This runner is unprivileged. If run as root, mode bits do not prevent read.
    if unsafe { libc::geteuid() } != 0 {
        assert!(result.reply.get("exif").is_none());
        assert_eq!(result.hydration.undefined_paths, vec!["/exif"]);
    }
    std::fs::set_permissions(&id, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = folder.path().join("loop");
    symlink(&link, &link).unwrap();
    assert!(service
        .read(link.to_str().unwrap().into(), context(), Control::default())
        .await
        .unwrap()
        .reply
        .get("exif")
        .is_none());
    let fifo = folder.path().join("fifo");
    let c = CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let result = service
        .read(fifo.to_str().unwrap().into(), context(), Control::default())
        .await
        .unwrap();
    assert!(!result.source_compatible);
    assert_eq!(result.diagnostics[0].code, "exif_unsupported");
    assert!(service
        .read(id, context(), Control::default())
        .await
        .unwrap()
        .reply
        .get("exif")
        .is_some());
    service.shutdown().await.unwrap();
}

#[test]
fn parser_checks_cancellation_and_deadline_during_large_bounded_field_work() {
    let mut bytes = fixture().to_vec();
    bytes[26..30].copy_from_slice(&65536_u32.to_le_bytes());
    bytes[30..34].copy_from_slice(&0xfffffff0_u32.to_le_bytes());
    let result = parse_bytes(
        "/large.jpg".into(),
        context(),
        &bytes,
        &Limits::default(),
        Control {
            deadline: Instant::now() + Duration::from_millis(1),
            ..Control::default()
        },
    );
    assert_eq!(result.unwrap_err().code, "exif_deadline");
    let cancel = CancellationToken::new();
    let control = Control {
        cancel: cancel.clone(),
        ..Control::default()
    };
    let worker = std::thread::spawn(move || {
        parse_bytes(
            "/large.jpg".into(),
            context(),
            &bytes,
            &Limits::default(),
            control,
        )
    });
    std::thread::sleep(Duration::from_millis(1));
    cancel.cancel();
    assert_eq!(worker.join().unwrap().unwrap_err().code, "exif_cancelled");
}

#[tokio::test]
async fn shutdown_timeout_bounds_nonreturning_os_boundary_without_spawning_replacements() {
    let (_folder, id) = image_file();
    let service = ExifService::start(Limits {
        shutdown_timeout: Duration::from_millis(20),
        ..Limits::default()
    })
    .unwrap();
    // A controlled worker boundary models an OS open/read that cannot observe
    // cancellation. No platform-specific mount or external process is needed.
    let gate = Arc::new(Gate {
        entered: AtomicUsize::new(0),
        released: Mutex::new(false),
        changed: Condvar::new(),
        cooperative: false,
    });
    service.gate(gate.clone());
    let pending = {
        let service = service.clone();
        let id = id.clone();
        tokio::spawn(async move { service.read(id, context(), Control::default()).await })
    };
    until(|| gate.entered.load(Ordering::Acquire) == 1).await;
    let result = tokio::time::timeout(Duration::from_secs(1), service.shutdown())
        .await
        .unwrap();
    assert_eq!(result.unwrap_err().code, "exif_shutdown_timeout");
    assert!(service.status().closing);
    assert_eq!(service.status().active, 1);
    assert!(service.status().workers <= 2);
    assert_eq!(
        service
            .read(id, context(), Control::default())
            .await
            .unwrap_err()
            .code,
        "exif_shutdown"
    );
    assert_eq!(pending.await.unwrap().unwrap_err().code, "exif_shutdown");
    gate.release();
    service.shutdown().await.unwrap();
    assert_eq!(service.status().workers, 0);
}
