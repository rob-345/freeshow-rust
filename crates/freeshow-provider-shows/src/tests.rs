use super::*;
fn s(value: &str) -> SourceValue {
    SourceValue::String(value.into())
}
fn json(v: serde_json::Value) -> SourceValue {
    SourceValue::from_json(&v).unwrap()
}
fn env<'a>(uids: &'a [&'a str], clock: &'a [f64]) -> Environment<'a> {
    Environment {
        uids,
        now_millis: clock,
    }
}
fn field<'a>(v: &'a SourceValue, k: &str) -> &'a SourceValue {
    get(v, k).unwrap()
}
#[test]
fn undefined_is_own_typed_data_and_projection_is_separate() {
    let result = create_empty_show(
        &SourceValue::Undefined,
        &SourceValue::Undefined,
        &SourceValue::Undefined,
        &SourceValue::Undefined,
        env(&["aaaaaaaaaaa"], &[-0.0]),
    )
    .unwrap();
    assert_eq!(result.uid_calls, 1);
    assert_eq!(result.now_calls, 1);
    assert_eq!(result.date_calls, 0);
    let show = field(&result.value, "show");
    assert!(matches!(
        field(field(show, "meta"), "title"),
        SourceValue::Undefined
    ));
    let SourceValue::Number(created) = field(field(show, "timestamps"), "created") else {
        panic!()
    };
    assert_eq!(created.to_bits(), (-0.0f64).to_bits());
    let p = result.value.json_projection().unwrap();
    assert!(!p["show"]["meta"].as_object().unwrap().contains_key("title"));
    assert!(result.work_bytes <= MAX_WORK_BYTES && result.operations <= MAX_OPERATIONS);
}
#[test]
fn ordinary_object_index_order_and_duplicate_validation() {
    let v = json(
        serde_json::from_str(r#"{"z":0,"2":"two","01":1,"1":"one","4294967295":5,"4294967294":4}"#)
            .unwrap(),
    );
    let SourceValue::Object(fields) = &v else {
        panic!()
    };
    assert_eq!(
        fields.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        ["1", "2", "4294967294", "z", "01", "4294967295"]
    );
    let invalid = SourceValue::Object(vec![("x".into(), s("a")), ("x".into(), s("b"))]);
    assert_eq!(
        create_empty_show(
            &s("id"),
            &invalid,
            &s(""),
            &SourceValue::Null,
            env(&[], &[])
        )
        .unwrap_err()
        .code,
        "provider_shows_unsupported"
    );
}
#[test]
fn global_lyric_admission_and_ecmascript_whitespace() {
    let sections = parse_lyrics(&s("[A 12]\n\u{85}\n\u{feff}\nX")).unwrap();
    assert_eq!(sections[0].label, "A");
    assert_eq!(sections[0].lyrics, ["\u{85}", "X"]);
    let text = "[A]\nx\n\nx\n\n".repeat(2049);
    assert_eq!(
        parse_lyrics(&s(&text)).unwrap_err().code,
        "provider_shows_limit"
    );
}
#[test]
fn aggregate_input_is_checked_before_any_uid_or_output() {
    let item = json(serde_json::json!({"ignored":"x".repeat(MAX_INPUT_BYTES-10)}));
    let files = [json(serde_json::json!({"ignored":"y".repeat(32)}))];
    let e = create_media_show(&item, &files, env(&[], &[])).unwrap_err();
    assert_eq!(e.code, "provider_shows_limit");
    let flat = SourceValue::Array(vec![SourceValue::Null; MAX_NODES]);
    assert_eq!(
        create_empty_show(&flat, &s(""), &s(""), &SourceValue::Null, env(&[], &[]))
            .unwrap_err()
            .code,
        "provider_shows_limit"
    );
}
#[test]
fn copied_depth_and_output_expansion_reject_before_large_clone() {
    let mut nested = SourceValue::Null;
    for _ in 0..MAX_COPIED_DEPTH + 1 {
        nested = SourceValue::Array(vec![nested]);
    }
    assert_eq!(
        create_empty_show(
            &s("id"),
            &nested,
            &s(""),
            &SourceValue::Null,
            env(&["aaaaaaaaaaa"], &[0.0])
        )
        .unwrap_err()
        .code,
        "provider_shows_limit"
    );
    assert_eq!(
        create_empty_show(
            &s("id"),
            &s(&"x".repeat(MAX_OUTPUT_BYTES / 6 + 1)),
            &s(""),
            &SourceValue::Null,
            env(&["aaaaaaaaaaa"], &[0.0])
        )
        .unwrap_err()
        .code,
        "provider_shows_limit"
    );
}
#[test]
fn source_boundary_consumption_only_uses_entered_calls() {
    let result = create_media_show(
        &json(serde_json::json!({"label":"X"})),
        &[],
        env(
            &["aaaaaaaaaaa", "unused nonuid"],
            &[f64::INFINITY, f64::NAN],
        ),
    )
    .unwrap();
    assert_eq!(result.uid_calls, 1);
    assert_eq!(result.now_calls, 1);
    assert!(
        matches!(field(field(field(&result.value,"show"),"timestamps"),"created"),SourceValue::Number(n) if n.is_infinite())
    );
    assert_eq!(
        create_media_show(
            &json(serde_json::json!({})),
            &[],
            env(&["__proto__"], &[1.0])
        )
        .unwrap_err()
        .code,
        "provider_shows_unsupported"
    );
}
#[test]
fn timeclip_full_iso_range_and_explicit_unsupported_legacy() {
    for (v, n) in [(0.9, 0.0), (-0.9, 0.0), (1.9, 1.0), (-1.9, -1.0)] {
        assert_eq!(
            date::parse(&SourceValue::Number(v)).unwrap().to_bits(),
            f64::to_bits(n)
        );
    }
    for input in ["+275760-09-13T00:00:00Z", "-271821-04-20T00:00:00Z"] {
        assert_eq!(
            date::parse(&s(input)).unwrap().abs(),
            8_640_000_000_000_000.0
        );
    }
    assert!(date::parse(&s("+275760-09-13T00:00:00.001Z"))
        .unwrap()
        .is_nan());
    for input in ["2024-01-01T01:00:00", "01/02/2024", "-000000-01-01"] {
        assert_eq!(
            date::parse(&s(input)).unwrap_err().code,
            "provider_shows_unsupported"
        );
    }
}
#[test]
fn operation_budget_covers_repeated_long_section_labels() {
    let text = format!(
        "[{}]\n{}",
        "X".repeat(16000),
        (0..1000)
            .map(|i| format!("line{i}\n\n"))
            .collect::<String>()
    );
    let key = json(serde_json::json!({}));
    let arrangement = json(serde_json::json!({"lyrics":text}));
    assert_eq!(
        create_song_show(
            &key,
            &arrangement,
            &key,
            &key,
            env(&["aaaaaaaaaaa"; 1024], &[1.0])
        )
        .unwrap_err()
        .code,
        "provider_shows_limit"
    );
}
