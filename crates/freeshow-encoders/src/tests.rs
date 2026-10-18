use super::*;
use serde_json::json;
fn input(cache: Option<Vec<u8>>) -> FrozenDetectionInput {
    FrozenDetectionInput {
        generation: 42,
        executable: Some(FrozenExecutable {
            path: std::env::current_exe().unwrap(),
            source_path: "ffmpeg".into(),
            mtime_ms: 0.0,
        }),
        platform: "linux".into(),
        arch: "x64".into(),
        vaapi_device: profiles::DEFAULT_VAAPI_DEVICE.into(),
        cache,
    }
}
#[tokio::test]
async fn cache_intent_is_native_guarded_and_shutdown_is_idempotent() {
    let intent = CacheWriteIntent {
        generation: 42,
        key: "1|ffmpeg|0|linux|x64".into(),
        bytes: b"prepared bytes".to_vec(),
    };
    intent.ensure_current(42, "1|ffmpeg|0|linux|x64").unwrap();
    assert_eq!(
        intent.ensure_current(43, intent.key()).unwrap_err().code,
        "stale_generation"
    );
    assert_eq!(
        intent
            .ensure_current(42, "other binary key")
            .unwrap_err()
            .code,
        "stale_generation"
    );
    let service = EncoderService::new(EncoderConfig::default()).unwrap();
    let (first, second) = tokio::join!(service.shutdown(), service.shutdown());
    first.unwrap();
    second.unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(
        service.detect(input(None), false).await.unwrap_err().code,
        "shutdown"
    );
    assert_eq!(service.stats().active_children, 0);
}
#[tokio::test]
async fn raw_cache_and_resource_errors_do_not_turn_into_successful_normalized_detection() {
    let service = EncoderService::new(EncoderConfig {
        cache_limit: 4096,
        ..Default::default()
    })
    .unwrap();
    let cached = json!({"version":1,"key":"1|ffmpeg|0|linux|x64","detection":{"encoders":null,"unknown":["preserved",true]}});
    let result = service
        .detect(input(Some(serde_json::to_vec(&cached).unwrap())), false)
        .await
        .unwrap();
    assert_eq!(result.detection, cached["detection"]);
    assert!(result.cache_write.is_none());
    assert_eq!(
        resolve_detection(&result.detection, Some("nvenc"))
            .unwrap_err()
            .code,
        "invalid_cached_detection"
    );
    assert_eq!(
        service
            .detect(input(Some(vec![b'x'; 4097])), false)
            .await
            .unwrap_err()
            .code,
        "input_limit"
    );
    let overflow =
        br#"{"version":1,"key":"1|ffmpeg|0|linux|x64","detection":{"n":1e309}}"#.to_vec();
    assert_eq!(
        service
            .detect(input(Some(overflow)), false)
            .await
            .unwrap_err()
            .code,
        "json_number_overflow"
    );
    assert_eq!(service.stats().children_started, 0);
    service.shutdown().await.unwrap();
}
#[test]
fn explicit_resolve_uses_first_matching_cached_id_and_preserves_truthy_available() {
    let detection = json!({"recommended":"future","encoders":[{"id":"nvenc","available":false},{"id":"nvenc","available":true},null]});
    assert_eq!(
        resolve_detection(&detection, Some("auto")).unwrap(),
        "future"
    );
    assert_eq!(
        resolve_detection(&detection, Some("nvenc")).unwrap(),
        "x264"
    );
    assert_eq!(
        resolve_detection(&detection, Some("unknown"))
            .unwrap_err()
            .code,
        "invalid_cached_detection"
    );
    assert_eq!(
        resolve_detection(
            &json!({"encoders":[{"id":"x264","available":[]}]}),
            Some("x264")
        )
        .unwrap(),
        "x264"
    );
}
#[test]
fn listing_regex_can_cross_line_whitespace_and_requires_trailing_separator() {
    assert_eq!(
        profiles::parse_available_encoders("V\nlibx264\nV h264_nvenc\u{2028}"),
        ["x264", "nvenc"]
    );
    assert!(profiles::parse_available_encoders("V libx264").is_empty());
    assert!(profiles::parse_available_encoders("prefix V libx264 listed").is_empty());
}
#[test]
fn options_and_arguments_reject_nonfinite_values_and_prototype_profiles() {
    assert!(force_requested(Some(&json!({"force":[]}))));
    assert!(force_requested(Some(&json!({"force":"false"}))));
    assert!(!force_requested(Some(&json!({"force":0}))));
    assert!(!force_requested(None));
    assert_eq!(
        crate::json::parse(r#""\ud800""#).unwrap_err().code,
        "unsupported_utf16"
    );
    assert_eq!(
        profiles::encoder_args("x264", f64::MAX, 60.0, Some("vbr"))
            .unwrap_err()
            .code,
        "nonfinite_number"
    );
    assert!(profiles::encoder_args("x264", f64::MAX, 60.0, Some("cbr")).is_ok());
    assert_eq!(
        profiles::test_encode_command("constructor", "")
            .unwrap_err()
            .code,
        "unsupported_profile_key"
    );
    assert_eq!(
        first_error_line("\n\u{feff} unavailable\r\nsecond", true, None),
        "unavailable"
    );
    assert_eq!(first_error_line("", true, Some("other")), "Timed out");
}
