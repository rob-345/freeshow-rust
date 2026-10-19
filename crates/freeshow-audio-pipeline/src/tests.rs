use super::*;
#[test]
fn stereo_rounding_and_nonfinite() {
    let values = [
        -1.0f32,
        1.0,
        f32::NAN,
        f32::INFINITY,
        0.5,
        -0.5,
        f32::NEG_INFINITY,
        2.0,
    ];
    let bytes: Vec<_> = values.into_iter().flat_map(f32::to_le_bytes).collect();
    let out = convert_stereo(
        &bytes,
        &PipelineConfig::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(out.len(), 16);
    assert_eq!(i16::from_le_bytes(out[0..2].try_into().unwrap()), -32768);
}
#[test]
fn pure_cpu_limits_and_cancel_are_explicit_and_resampler_rejection_is_transactional() {
    let raw = vec![0; 1000];
    let config = PipelineConfig {
        operations: 1,
        ..Default::default()
    };
    assert_eq!(
        convert_stereo(&raw, &config, &CancellationToken::new())
            .err()
            .unwrap()
            .code,
        "audio_pipeline_resource_limit"
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        convert_stereo(&raw, &PipelineConfig::default(), &cancel)
            .err()
            .unwrap()
            .code,
        "cancelled"
    );
    let mut resampler = StereoResampler::default();
    resampler
        .process(
            &[1, 0, 2, 0],
            44100.0,
            &PipelineConfig::default(),
            &CancellationToken::new(),
        )
        .unwrap();
    let before = resampler.state_bits();
    assert_eq!(
        resampler
            .process(
                &[1, 0, 2, 0],
                1.0,
                &PipelineConfig::default(),
                &CancellationToken::new()
            )
            .err()
            .unwrap()
            .code,
        "unsupported_sample_rate"
    );
    assert_eq!(resampler.state_bits(), before);
}
#[tokio::test]
async fn metadata_and_aggregate_frame_limits_reject_instead_of_empty_audio() {
    use serde_json::json;
    let service = AudioPipelineService::new(
        PipelineConfig {
            max_frames: 1,
            ..Default::default()
        },
        true,
    )
    .unwrap();
    let sinks =
        SinkSnapshot::new(ContextId(1, 1), vec![], vec![], vec![], false, false, false).unwrap();
    let meta = AudioMetadata {
        icecast: Some(json!({"enabled":true})),
        ..Default::default()
    };
    assert_eq!(
        service
            .prepare(&vec![0; 1920 * 8], &meta, &sinks, CancellationToken::new())
            .await
            .err()
            .unwrap()
            .code,
        "audio_pipeline_resource_limit"
    );
    let huge = AudioMetadata {
        icecast: Some(json!({"unknown":"x".repeat(9000)})),
        ..Default::default()
    };
    assert_eq!(
        service
            .prepare(&[0; 8], &huge, &sinks, CancellationToken::new())
            .await
            .err()
            .unwrap()
            .code,
        "audio_pipeline_resource_limit"
    );
    let unsupported = AudioMetadata {
        target_id: JsInput::Json(json!({"toString":"not callable"})),
        ..Default::default()
    };
    assert_eq!(
        service
            .prepare(&[0; 8], &unsupported, &sinks, CancellationToken::new())
            .await
            .err()
            .unwrap()
            .code,
        "unsupported_metadata"
    );
    let p = service
        .prepare(
            &[0; 8],
            &AudioMetadata::default(),
            &sinks,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        p.opus_frames().is_empty(),
        "failed input must not latch Icecast config"
    );
    drop(p);
    service.shutdown().await.unwrap();
}
