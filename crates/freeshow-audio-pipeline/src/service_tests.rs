use super::*;
use serde_json::json;
fn bytes(frames: usize, value: f32) -> Vec<u8> {
    (0..frames * 2).flat_map(|_| value.to_le_bytes()).collect()
}
fn meta() -> AudioMetadata {
    AudioMetadata {
        icecast: Some(json!({"enabled":true,"unknown":"kept"})),
        ..Default::default()
    }
}
fn sinks() -> SinkSnapshot {
    SinkSnapshot::new(ContextId(7, 9), vec![], vec![], vec![], true, false, true).unwrap()
}
fn service() -> AudioPipelineService {
    AudioPipelineService::new(PipelineConfig::default(), true).unwrap()
}
async fn prepare(service: &AudioPipelineService, frames: usize) -> PreparedAudio {
    service
        .prepare(
            &bytes(frames, 0.5),
            &meta(),
            &sinks(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
}
fn task(
    service: &AudioPipelineService,
    frames: usize,
    metadata: AudioMetadata,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<Result<PreparedAudio>> {
    let service = service.clone();
    tokio::spawn(async move {
        service
            .prepare(&bytes(frames, 0.5), &metadata, &sinks(), cancel)
            .await
    })
}
#[tokio::test]
async fn exact_failure_transition_retains_full_frames_and_legacy_continuation() {
    let s = service();
    let p = prepare(&s, 2880).await;
    assert_eq!(p.opus_frames().len(), 3);
    assert_eq!(p.planar_intents().len(), 2);
    assert_eq!(p.icecast_config().unwrap()["unknown"], "kept");
    let mut a = p.accept(sinks().context()).unwrap();
    let finished = s
        .finish(
            &mut a,
            sinks().context(),
            EncodingOutcome::Failed {
                index: 1,
                stage: FailureStage::Encode,
                message: "controlled".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(finished.debug_accumulator().len(), 3840);
    assert_eq!(finished.legacy_intents().len(), 3);
    assert_eq!(finished.diagnostics()[0].code, "opus_encode_failed");
    drop((finished, a));
    let p = s
        .prepare(
            &[1],
            &AudioMetadata::default(),
            &sinks(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(p.opus_frames().len(), 1);
    let mut a = p.accept(sinks().context()).unwrap();
    let v = s
        .finish(&mut a, sinks().context(), EncodingOutcome::AllSucceeded)
        .await
        .unwrap();
    assert!(v.debug_accumulator().is_empty());
    assert!(v.legacy_intents().iter().all(|i| i.buffer().is_empty()));
    drop((v, a));
    s.shutdown().await.unwrap();
}
#[tokio::test]
async fn foreign_stale_invalid_and_duplicate_completions_do_not_advance() {
    let s = service();
    let other = service();
    let mut a = prepare(&s, 960).await.accept(sinks().context()).unwrap();
    assert_eq!(
        other
            .finish(&mut a, sinks().context(), EncodingOutcome::AllSucceeded)
            .await
            .err()
            .unwrap()
            .code,
        "foreign_audio_plan"
    );
    assert_eq!(
        s.finish(&mut a, ContextId(8, 9), EncodingOutcome::AllSucceeded)
            .await
            .err()
            .unwrap()
            .code,
        "stale_audio_context"
    );
    assert_eq!(
        s.finish(
            &mut a,
            sinks().context(),
            EncodingOutcome::Failed {
                index: 1,
                stage: FailureStage::Send,
                message: "wrong".into()
            }
        )
        .await
        .err()
        .unwrap()
        .code,
        "invalid_completion"
    );
    let result = s
        .finish(&mut a, sinks().context(), EncodingOutcome::AllSucceeded)
        .await
        .unwrap();
    assert!(result.debug_accumulator().is_empty());
    assert_eq!(
        s.finish(&mut a, sinks().context(), EncodingOutcome::AllSucceeded)
            .await
            .err()
            .unwrap()
            .code,
        "duplicate_completion"
    );
    drop((result, a));
    s.shutdown().await.unwrap();
    other.shutdown().await.unwrap();
}
#[tokio::test]
async fn stale_accept_and_dropped_preparation_leave_global_latched_config_unchanged() {
    let s = service();
    assert_eq!(
        prepare(&s, 480)
            .await
            .accept(ContextId(0, 0))
            .err()
            .unwrap()
            .code,
        "stale_audio_context"
    );
    drop(prepare(&s, 480).await);
    let p = s
        .prepare(
            &bytes(960, 0.3),
            &AudioMetadata::default(),
            &sinks(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(p.opus_frames().is_empty());
    assert!(p.icecast_config().is_none());
    drop(p);
    s.shutdown().await.unwrap();
}
#[tokio::test]
async fn fifo_and_admission_are_bounded_while_one_global_transition_is_pending() {
    let s = AudioPipelineService::new(
        PipelineConfig {
            queued_jobs: 2,
            ..Default::default()
        },
        true,
    )
    .unwrap();
    let first = prepare(&s, 480).await;
    let second = task(&s, 480, meta(), CancellationToken::new());
    tokio::task::yield_now().await;
    assert_eq!(s.inner.jobs.available_permits(), 1);
    let third = task(&s, 480, meta(), CancellationToken::new());
    tokio::task::yield_now().await;
    assert_eq!(s.inner.jobs.available_permits(), 0);
    assert_eq!(
        s.prepare(&[], &meta(), &sinks(), CancellationToken::new())
            .await
            .err()
            .unwrap()
            .code,
        "audio_pipeline_busy"
    );
    assert!(!second.is_finished());
    assert!(!third.is_finished());
    drop(first);
    let p = second.await.unwrap().unwrap();
    assert_eq!(p.sequence(), 2);
    assert!(p.opus_frames().is_empty());
    let mut a = p.accept(sinks().context()).unwrap();
    drop(
        s.finish(&mut a, sinks().context(), EncodingOutcome::AllSucceeded)
            .await
            .unwrap(),
    );
    drop(a);
    let p = third.await.unwrap().unwrap();
    assert_eq!(p.sequence(), 3);
    assert_eq!(p.opus_frames().len(), 1);
    let mut a = p.accept(sinks().context()).unwrap();
    drop(
        s.finish(&mut a, sinks().context(), EncodingOutcome::AllSucceeded)
            .await
            .unwrap(),
    );
    drop(a);
    s.shutdown().await.unwrap();
}
#[tokio::test]
async fn queued_external_cancel_and_dropped_caller_cannot_latch_config() {
    let s = service();
    let first = prepare(&s, 480).await;
    let cancel = CancellationToken::new();
    let external = task(&s, 480, meta(), cancel.clone());
    tokio::task::yield_now().await;
    cancel.cancel();
    assert_eq!(external.await.unwrap().err().unwrap().code, "cancelled");
    let dropped = task(&s, 480, meta(), CancellationToken::new());
    tokio::task::yield_now().await;
    dropped.abort();
    let _ = dropped.await;
    drop(first);
    let p = s
        .prepare(
            &bytes(960, 0.5),
            &AudioMetadata::default(),
            &sinks(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(p.opus_frames().is_empty());
    drop(p);
    s.shutdown().await.unwrap();
}
#[tokio::test]
async fn shutdown_has_both_admission_orderings_and_no_late_accept() {
    for after in [false, true] {
        let s = service();
        let owner = Arc::downgrade(&s.inner);
        let hook: Box<dyn FnOnce() + Send> = Box::new(move || {
            let inner = owner.upgrade().unwrap();
            inner.closing.store(true, Ordering::Release);
            inner.cancel.cancel();
        });
        if after {
            *s.inner.hooks.after.lock().unwrap() = Some(hook);
        } else {
            *s.inner.hooks.before.lock().unwrap() = Some(hook);
        }
        assert_eq!(
            s.prepare(&[], &meta(), &sinks(), CancellationToken::new())
                .await
                .err()
                .unwrap()
                .code,
            "audio_pipeline_shutdown"
        );
        s.shutdown().await.unwrap();
    }
    let s = service();
    let p = prepare(&s, 960).await;
    tokio::time::timeout(Duration::from_millis(500), s.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        p.accept(sinks().context()).err().unwrap().code,
        "audio_pipeline_shutdown"
    );
    s.shutdown().await.unwrap();
}
#[tokio::test]
async fn accepted_work_survives_caller_cancel_and_shutdown_returns_drain_obligation() {
    let s = service();
    let cancel = CancellationToken::new();
    let p = s
        .prepare(&bytes(960, 0.5), &meta(), &sinks(), cancel.clone())
        .await
        .unwrap();
    let mut a = p.accept(sinks().context()).unwrap();
    cancel.cancel();
    assert_eq!(
        s.shutdown().await.err().unwrap().code,
        "audio_commit_in_progress"
    );
    a.ensure_current(sinks().context()).unwrap();
    let result = s
        .finish(&mut a, sinks().context(), EncodingOutcome::AllSucceeded)
        .await
        .unwrap();
    assert!(result.debug_accumulator().is_empty());
    s.shutdown().await.unwrap();
    drop((result, a));
}
#[tokio::test]
async fn accepted_drop_is_terminal_because_partial_outputs_are_unknown() {
    let s = service();
    let a = prepare(&s, 960).await.accept(sinks().context()).unwrap();
    drop(a);
    for _ in 0..2 {
        assert_eq!(
            s.shutdown().await.unwrap_err().code,
            "audio_owner_abandoned"
        );
        assert!(s.inner.worker.lock().unwrap().is_none());
    }
    assert_eq!(
        s.prepare(&[], &meta(), &sinks(), CancellationToken::new())
            .await
            .err()
            .unwrap()
            .code,
        "audio_owner_abandoned"
    );
}
#[tokio::test]
async fn pending_expiry_retains_buffer_quota_without_blocking_shutdown() {
    let s = AudioPipelineService::new(
        PipelineConfig {
            retained_plans: 1,
            cpu_timeout: Duration::from_millis(100),
            prepared_ttl: Duration::from_millis(150),
            ..Default::default()
        },
        true,
    )
    .unwrap();
    let p = prepare(&s, 960).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        s.prepare(&[], &meta(), &sinks(), CancellationToken::new())
            .await
            .err()
            .unwrap()
            .code,
        "audio_pipeline_busy"
    );
    assert_eq!(
        p.accept(sinks().context()).err().unwrap().code,
        "audio_plan_expired"
    );
    let p = prepare(&s, 960).await;
    drop(p);
    s.shutdown().await.unwrap();
}
#[tokio::test]
async fn accepted_expiry_terminates_owner_and_rejects_late_completion() {
    let s = AudioPipelineService::new(
        PipelineConfig {
            cpu_timeout: Duration::from_millis(100),
            prepared_ttl: Duration::from_millis(150),
            ..Default::default()
        },
        true,
    )
    .unwrap();
    let mut a = prepare(&s, 960).await.accept(sinks().context()).unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        s.finish(&mut a, sinks().context(), EncodingOutcome::AllSucceeded)
            .await
            .err()
            .unwrap()
            .code,
        "audio_owner_timeout"
    );
    assert_eq!(s.shutdown().await.unwrap_err().code, "audio_owner_timeout");
    assert_eq!(
        s.prepare(&[], &meta(), &sinks(), CancellationToken::new())
            .await
            .err()
            .unwrap()
            .code,
        "audio_owner_timeout"
    );
}
#[tokio::test]
async fn retained_finished_output_holds_separate_capacity() {
    let s = AudioPipelineService::new(
        PipelineConfig {
            retained_plans: 1,
            ..Default::default()
        },
        true,
    )
    .unwrap();
    let mut a = prepare(&s, 960).await.accept(sinks().context()).unwrap();
    let v = s
        .finish(&mut a, sinks().context(), EncodingOutcome::AllSucceeded)
        .await
        .unwrap();
    drop(a);
    assert_eq!(
        s.prepare(&[], &meta(), &sinks(), CancellationToken::new())
            .await
            .err()
            .unwrap()
            .code,
        "audio_pipeline_busy"
    );
    drop(v);
    drop(prepare(&s, 960).await);
    s.shutdown().await.unwrap();
}
#[test]
fn runtime_independent_constructor_and_explicit_no_runtime_shutdown() {
    let s = service();
    {
        let mut future = std::pin::pin!(s.shutdown());
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match std::future::Future::poll(future.as_mut(), &mut cx) {
            std::task::Poll::Ready(v) => assert_eq!(v.err().unwrap().code, "runtime_required"),
            _ => panic!("no-runtime shutdown must not wait"),
        }
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(s.shutdown())
        .unwrap();
}
#[tokio::test]
async fn unexpected_worker_exit_fences_admission_and_terminal_drain() {
    let s = service();
    *s.inner.hooks.worker.lock().unwrap() = Some(Box::new(|| panic!("controlled worker panic")));
    let error = tokio::time::timeout(
        Duration::from_millis(500),
        s.prepare(&[], &meta(), &sinks(), CancellationToken::new()),
    )
    .await
    .unwrap()
    .err()
    .unwrap();
    assert!(matches!(
        error.code,
        "audio_worker_error" | "audio_pipeline_shutdown"
    ));
    let result = tokio::time::timeout(Duration::from_millis(500), s.shutdown())
        .await
        .unwrap();
    assert_eq!(result.err().unwrap().code, "audio_worker_error");
    assert_eq!(
        s.prepare(&[], &meta(), &sinks(), CancellationToken::new())
            .await
            .err()
            .unwrap()
            .code,
        "audio_worker_error"
    );
    assert_eq!(s.shutdown().await.unwrap_err().code, "audio_worker_error");
}
#[tokio::test]
async fn submitted_finish_survives_dropped_awaiting_caller() {
    let s = service();
    let mut a = prepare(&s, 480).await.accept(sinks().context()).unwrap();
    {
        let mut future =
            std::pin::pin!(s.finish(&mut a, sinks().context(), EncodingOutcome::AllSucceeded));
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        if let std::task::Poll::Ready(result) = std::future::Future::poll(future.as_mut(), &mut cx)
        {
            drop(result.unwrap());
        }
    }
    assert!(a.pending.is_none());
    drop(a);
    let p = s
        .prepare(
            &bytes(480, 0.5),
            &AudioMetadata::default(),
            &sinks(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(p.opus_frames().len(), 1);
    drop(p);
    s.shutdown().await.unwrap();
}
