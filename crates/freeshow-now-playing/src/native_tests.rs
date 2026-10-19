use super::*;
use freeshow_audio::CommonValue as C;
use serde_json::json;

fn router() -> Router {
    let context = Context::from_host("native", 1, "/library").unwrap();
    Router::new(
        context.clone(),
        TrustedPaths::from_host(&context, Platform::Posix, "/library/Audio").unwrap(),
    )
    .unwrap()
}
fn started(router: &mut Router) -> Started {
    router
        .begin_set(
            SourceData::from_payload(
                json!({"filePath":"/input.wav","duration":1,"unknownLang":[],"format":"{year}"}),
            )
            .unwrap(),
            TrustedMediaPath::from_host(&router.context, "/input.wav").unwrap(),
        )
        .unwrap()
}
fn next(router: &mut Router, issued: Issued, receipt: Receipt) -> Option<Issued> {
    let accepted = router.accept(issued.token).unwrap();
    router.complete(&accepted, receipt).unwrap()
}

#[test]
fn native_receipt_survives_cover_transition_and_compatibility_json_cannot_drive_year() {
    let mut router = router();
    let start = started(&mut router);
    let metadata = Metadata::from_common_value(
        C::Object(vec![
            ("year".into(), C::Number(f64::NAN)),
            ("own".into(), C::Undefined),
        ]),
        Covers::Unknown,
    )
    .unwrap();
    let Common::Native(raw) = &metadata.common else {
        panic!()
    };
    let address = raw.value() as *const C;
    let read = next(
        &mut router,
        start.effect.unwrap(),
        Receipt::Metadata(Some(metadata)),
    )
    .unwrap();
    assert!(matches!(read.effect, Effect::ReadCovers { .. }));
    let publication = next(&mut router, read, Receipt::NativeCovers(vec![])).unwrap();
    let raw = publication.effect.native_metadata().unwrap().clone();
    assert_eq!(raw.common() as *const C, address);
    assert!(
        raw.parsed_common().is_none(),
        "host fixtures are not parser evidence"
    );
    assert!(matches!(raw.common().get("year"), C::Number(n) if n.is_nan()));
    assert!(
        matches!(raw.common(), C::Object(fields) if fields[1].0 == "own" && matches!(fields[1].1, C::Undefined))
    );
    let json = serde_json::to_value(&publication.effect).unwrap();
    assert_eq!(json["common"]["year"], Value::Null);
    assert!(json["common"].get("own").is_none());
    assert!(json.get("native_receipt").is_none());
    assert_eq!(json["json_projection_only"], true);
    let mut pending = next(&mut router, publication, Receipt::Done { success: true });
    let mut text = None;
    while let Some(issued) = pending {
        if let Effect::WriteText { text: value, .. } = &issued.effect {
            text = Some(value.clone());
        }
        let receipt = if matches!(issued.effect, Effect::Exists { .. }) {
            Receipt::Exists(false)
        } else {
            Receipt::Done { success: true }
        };
        pending = next(&mut router, issued, receipt);
    }
    assert_eq!(text.as_deref(), Some("NaN"));
    assert!(router.take_result(start.operation).unwrap().error.is_none());
    // The actual immutable observation remains alive after router collection.
    assert!(matches!(raw.common().get("year"), C::Number(n) if n.is_nan()));
}

#[test]
fn native_pictures_keep_order_own_undefined_all_bytes_and_no_null_guessing() {
    let fields = C::Object(vec![
        ("format".into(), C::String("image/png".into())),
        ("type".into(), C::Undefined),
        ("description".into(), C::String("音".into())),
    ]);
    let first = NativePicture::from_host(fields, vec![1, 2, 3]).unwrap();
    let second =
        NativePicture::from_host(C::Object(vec![("type".into(), C::Null)]), vec![9]).unwrap();
    let mut router = router();
    let start = started(&mut router);
    let publication = next(
        &mut router,
        start.effect.unwrap(),
        Receipt::Metadata(Some(
            Metadata::from_common_value(
                C::Object(vec![]),
                Covers::NativeComplete(vec![first, second]),
            )
            .unwrap(),
        )),
    )
    .unwrap();
    let raw = publication.effect.native_metadata().unwrap();
    let C::Object(fields) = raw.pictures()[0].fields() else {
        panic!()
    };
    assert_eq!(
        fields
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        ["format", "type", "description"]
    );
    assert!(matches!(fields[1].1, C::Undefined));
    assert!(matches!(raw.pictures()[1].fields().get("type"), C::Null));
    assert_eq!(raw.pictures()[0].bytes(), [1, 2, 3]);
    assert_eq!(raw.pictures()[1].bytes(), [9]);
    // Accepted publication cancellation retains the existing effect token.
    let accepted = router.accept(publication.token).unwrap();
    router.cancel(start.operation).unwrap();
    assert_eq!(router.accepted(), 1);
    assert!(router
        .complete(&accepted, Receipt::Done { success: true })
        .unwrap()
        .is_none());
    assert_eq!(
        router
            .take_result(start.operation)
            .unwrap()
            .error
            .unwrap()
            .code,
        "now_playing_cancelled"
    );
}

#[test]
fn native_bounds_precede_lossy_projection_and_reject_ambiguous_object_variants() {
    let code = |value| {
        Metadata::from_common_value(value, Covers::Unknown)
            .err()
            .unwrap()
            .code
    };
    assert_eq!(
        code(C::Object(vec![
            ("name".into(), C::Null),
            ("2".into(), C::Undefined)
        ])),
        "now_playing_unsupported_common"
    );
    assert_eq!(
        code(C::Object(vec![("year".into(), C::Undefined); 1024])),
        "now_playing_input_limit"
    );
    assert_eq!(
        code(C::Object(vec![
            ("one".into(), C::Null),
            ("one".into(), C::Undefined)
        ])),
        "now_playing_unsupported_common"
    );
    assert_eq!(
        code(C::Object(vec![(
            "year".into(),
            C::Object(vec![("toString".into(), C::Null)])
        )])),
        "now_playing_unsupported_common"
    );
    assert_eq!(
        code(C::Object(vec![("picture".into(), C::Undefined)])),
        "now_playing_metadata"
    );
    assert_eq!(
        code(C::Object(vec![(
            "large".into(),
            C::String("x".repeat(native::COMMON_LIMIT))
        )])),
        "now_playing_input_limit"
    );
    let mut deep = C::Number(f64::NAN);
    for _ in 0..9 {
        deep = C::Array(vec![deep]);
    }
    assert_eq!(
        code(C::Object(vec![("nested".into(), deep)])),
        "now_playing_input_limit"
    );
    assert!(
        NativePicture::from_host(C::Object(vec![("data".into(), C::Undefined)]), vec![]).is_err()
    );
    let picture = NativePicture::from_host(C::Object(vec![]), vec![0; MAX_COVER / 2 + 1]).unwrap();
    assert_eq!(
        Metadata::from_common_value(
            C::Object(vec![]),
            Covers::NativeComplete(vec![picture.clone(), picture])
        )
        .err()
        .unwrap()
        .code,
        "now_playing_picture"
    );
}

#[test]
fn compatibility_json_path_has_no_raw_parser_observation() {
    let mut router = router();
    let start = started(&mut router);
    let publication = next(
        &mut router,
        start.effect.unwrap(),
        Receipt::Metadata(Some(
            Metadata::from_host(json!({"year":0}), Covers::Complete(vec![])).unwrap(),
        )),
    )
    .unwrap();
    assert!(publication.effect.native_metadata().is_none());
    let json = serde_json::to_value(publication.effect).unwrap();
    assert_eq!(json["common"], json!({"year":0}));
    assert_eq!(json["json_projection_only"], false);
}
