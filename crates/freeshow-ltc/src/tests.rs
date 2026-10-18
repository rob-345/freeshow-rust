use super::*;
#[test]
fn scope_and_limits_are_explicit() {
    assert!(TrustedLibrary::from_host("relative.so".into()).is_err());
    assert!(Owner::new("", 0, "/tmp").is_err());
    assert!(supported_fps(f64::NAN).is_err());
    assert!(supported_fps(0.0).is_err());
    assert!(supported_fps(60.0).is_err());
    assert!(supported_fps(29.97).is_ok());
}
#[derive(Default)]
pub(crate) struct Gate {
    state: Mutex<(bool, bool)>,
    wake: std::sync::Condvar,
}
impl Gate {
    pub fn wait(&self) {
        let mut state = lock(&self.state);
        state.0 = true;
        self.wake.notify_all();
        while !state.1 {
            state = self.wake.wait(state).unwrap();
        }
    }
    fn release(&self) {
        lock(&self.state).1 = true;
        self.wake.notify_all();
    }
    fn entered(&self) -> bool {
        lock(&self.state).0
    }
}
fn owner() -> Owner {
    Owner::new(
        "native-test",
        1,
        if cfg!(windows) {
            "C:\\library"
        } else {
            "/library"
        },
    )
    .unwrap()
}
fn library() -> TrustedLibrary {
    TrustedLibrary::from_host(
        std::env::current_dir()
            .unwrap()
            .join("../../node_modules/libltc-wrapper/libltc/bin/libltc.so.11"),
    )
    .unwrap()
}
async fn wait(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
async fn run(service: &LtcService, owner: &Owner, operation: Operation) -> Result<Reply> {
    service
        .begin(owner, operation, Instant::now() + Duration::from_secs(5))?
        .finish()
        .await?
        .into_reply()
}
#[tokio::test]
async fn unavailable_is_explicit_and_does_not_allocate_fake_codecs() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("wrong-library");
    std::fs::write(&path, b"not an ELF").unwrap();
    let o = owner();
    let s = LtcService::start(
        o.clone(),
        TrustedLibrary::from_host(path).unwrap(),
        Config::default(),
    )
    .unwrap();
    let Reply::Capability(cap) = run(&s, &o, Operation::Capability).await.unwrap() else {
        panic!()
    };
    assert!(!cap.available);
    assert_eq!(cap.error.unwrap().code, "ltc_backend_unavailable");
    assert_eq!(
        run(&s, &o, Operation::CreateEncoder { fps: 25.0 })
            .await
            .unwrap_err()
            .code,
        "ltc_backend_unavailable"
    );
    s.shutdown().await.unwrap();
    assert_eq!(s.status().encoder_created, 0);
    assert!(!s.status().loaded);
}
#[tokio::test]
async fn complete_uncollected_results_hold_eight_leases_and_shutdown_revokes_them() {
    let o = owner();
    let s = LtcService::start(o.clone(), library(), Config::default()).unwrap();
    let mut tickets = vec![];
    for _ in 0..MAX_LEASES {
        tickets.push(
            s.begin(
                &o,
                Operation::Capability,
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap(),
        );
    }
    let mut ready = vec![];
    for ticket in tickets {
        ready.push(ticket.finish().await.unwrap());
    }
    assert_eq!(s.status().admitted, MAX_LEASES);
    assert!(
        matches!(s.begin(&o,Operation::Capability,Instant::now()+Duration::from_secs(5)),Err(e)if e.code=="ltc_busy")
    );
    ready.pop().unwrap().into_reply().unwrap();
    assert_eq!(s.status().admitted, MAX_LEASES - 1);
    s.shutdown().await.unwrap();
    assert_eq!(s.status().admitted, 0);
    for r in ready {
        assert_eq!(r.into_reply().unwrap_err().code, "ltc_shutdown");
    }
}
#[tokio::test]
async fn stale_identity_and_deadline_do_not_accept_native_work() {
    let o = owner();
    let s = LtcService::start(o.clone(), library(), Config::default()).unwrap();
    let other = owner();
    assert!(
        matches!(s.begin(&other,Operation::Capability,Instant::now()),Err(e)if e.code=="ltc_stale_owner")
    );
    let deadline = s
        .begin(
            &o,
            Operation::Capability,
            Instant::now() - Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(
        deadline
            .finish()
            .await
            .unwrap()
            .into_reply()
            .unwrap_err()
            .code,
        "ltc_cancelled"
    );
    o.retire();
    assert!(s.begin(&o, Operation::Capability, Instant::now()).is_err());
    s.shutdown().await.unwrap();
    assert_eq!(s.status().admitted, 0);
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn real_codecs_dispose_replace_reset_and_enforce_input_sample_operation_bounds() {
    let o = owner();
    let s = LtcService::start(
        o.clone(),
        library(),
        Config {
            max_session_samples: 10,
            ..Config::default()
        },
    )
    .unwrap();
    run(&s, &o, Operation::CreateEncoder { fps: 25.0 })
        .await
        .unwrap();
    run(&s, &o, Operation::CreateEncoder { fps: 24.0 })
        .await
        .unwrap();
    run(&s, &o, Operation::CreateDecoder { fps: 25.0 })
        .await
        .unwrap();
    run(
        &s,
        &o,
        Operation::Decode {
            bytes: vec![128; 10],
        },
    )
    .await
    .unwrap();
    assert_eq!(
        run(&s, &o, Operation::Decode { bytes: vec![128] })
            .await
            .unwrap_err()
            .code,
        "ltc_resource_limit"
    );
    assert_eq!(s.status().samples, 10);
    run(&s, &o, Operation::CreateDecoder { fps: 24.0 })
        .await
        .unwrap();
    assert_eq!(s.status().samples, 0);
    assert_eq!(s.status().backlog, 0);
    assert!(s
        .begin(
            &o,
            Operation::Decode {
                bytes: vec![128; MAX_AUDIO_BYTES + 1]
            },
            Instant::now() + Duration::from_secs(5)
        )
        .is_err());
    run(&s, &o, Operation::StopDecoder).await.unwrap();
    run(&s, &o, Operation::CreateDecoder { fps: 30.0 })
        .await
        .unwrap();
    assert_eq!(s.status().samples, 0);
    s.shutdown().await.unwrap();
    let status = s.status();
    assert_eq!((status.encoder_created, status.encoder_freed), (2, 2));
    assert_eq!((status.decoder_created, status.decoder_freed), (3, 3));
    assert!(!status.loaded);
    assert!(status.worker_finished);
    let s = LtcService::start(
        owner(),
        library(),
        Config {
            max_operations: 1,
            ..Config::default()
        },
    )
    .unwrap();
    let o = s.inner.shared.owner.clone();
    run(&s, &o, Operation::Capability).await.unwrap();
    assert_eq!(
        run(&s, &o, Operation::CreateEncoder { fps: 25.0 })
            .await
            .unwrap_err()
            .code,
        "ltc_resource_limit"
    );
    s.shutdown().await.unwrap();
    assert_eq!(s.status().encoder_created, 0);
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn accepted_caller_drop_and_close_timeout_keep_the_real_owner_until_join() {
    let gate = Arc::new(Gate::default());
    let o = owner();
    let s = LtcService::start(
        o.clone(),
        library(),
        Config {
            shutdown_timeout: Duration::from_millis(20),
            gate: Some(gate.clone()),
            ..Config::default()
        },
    )
    .unwrap();
    let ticket = s
        .begin(
            &o,
            Operation::CreateEncoder { fps: 25.0 },
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    wait(|| gate.entered()).await;
    drop(ticket);
    assert_eq!(s.status().admitted, 1);
    assert_eq!(s.shutdown().await.unwrap_err().code, "ltc_shutdown_timeout");
    assert!(!s.status().worker_finished);
    assert_eq!(s.status().admitted, 1);
    gate.release();
    s.shutdown().await.unwrap();
    assert_eq!(s.status().admitted, 0);
    assert_eq!(
        (s.status().encoder_created, s.status().encoder_freed),
        (1, 1)
    );
    assert!(!s.status().loaded);
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn queued_caller_drop_and_cancel_do_not_invoke_codec_after_accepted_barrier() {
    let gate = Arc::new(Gate::default());
    let o = owner();
    let s = LtcService::start(
        o.clone(),
        library(),
        Config {
            gate: Some(gate.clone()),
            ..Config::default()
        },
    )
    .unwrap();
    let accepted = s
        .begin(
            &o,
            Operation::CreateEncoder { fps: 25.0 },
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    wait(|| gate.entered()).await;
    let abandoned = s
        .begin(
            &o,
            Operation::CreateDecoder { fps: 25.0 },
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    drop(abandoned);
    let cancel = s
        .begin(
            &o,
            Operation::CreateDecoder { fps: 30.0 },
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    cancel.cancel();
    gate.release();
    accepted.finish().await.unwrap().into_reply().unwrap();
    assert_eq!(
        cancel
            .finish()
            .await
            .unwrap()
            .into_reply()
            .unwrap_err()
            .code,
        "ltc_cancelled"
    );
    wait(|| s.status().admitted == 0).await;
    assert_eq!(s.status().decoder_created, 0);
    s.shutdown().await.unwrap();
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn sealed_loaded_bytes_survive_in_place_source_mutation_and_final_symlink_rejects() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let copy = temp.path().join("libltc.so.11");
    std::fs::copy(library().path, &copy).unwrap();
    let o = owner();
    let s = LtcService::start(
        o.clone(),
        TrustedLibrary::from_host(copy.clone()).unwrap(),
        Config::default(),
    )
    .unwrap();
    let Reply::Capability(c) = run(&s, &o, Operation::Capability).await.unwrap() else {
        panic!()
    };
    assert!(c.available);
    std::fs::write(&copy, b"in-place changed").unwrap();
    run(&s, &o, Operation::CreateEncoder { fps: 25.0 })
        .await
        .unwrap();
    let Reply::Audio(bytes) = run(
        &s,
        &o,
        Operation::Encode {
            hours: 1.0,
            minutes: 2.0,
            seconds: 3.0,
            frames: f64::NAN,
        },
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert_eq!(bytes.len(), 1920);
    s.shutdown().await.unwrap();
    let link = temp.path().join("redirect");
    symlink(library().path, &link).unwrap();
    let o = owner();
    let s = LtcService::start(
        o.clone(),
        TrustedLibrary::from_host(link).unwrap(),
        Config::default(),
    )
    .unwrap();
    let Reply::Capability(c) = run(&s, &o, Operation::Capability).await.unwrap() else {
        panic!()
    };
    assert!(!c.available);
    s.shutdown().await.unwrap();
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn dropping_service_joins_native_thread_and_frees_accepted_resources() {
    let o = owner();
    let s = LtcService::start(o.clone(), library(), Config::default()).unwrap();
    run(&s, &o, Operation::CreateEncoder { fps: 25.0 })
        .await
        .unwrap();
    run(&s, &o, Operation::CreateDecoder { fps: 25.0 })
        .await
        .unwrap();
    let state = s.inner.shared.clone();
    drop(s);
    assert!(!state.loaded.load(Ordering::Acquire));
    assert_eq!(state.encoder_freed.load(Ordering::Acquire), 1);
    assert_eq!(state.decoder_freed.load(Ordering::Acquire), 1);
}
