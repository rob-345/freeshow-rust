use super::*;

fn object(fields: &[(&str, CommonValue)]) -> CommonValue {
    CommonValue::Object(
        fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    )
}

#[test]
fn own_undefined_all_special_numbers_and_order_are_explicit() {
    let common = object(&[
        ("missing", CommonValue::Undefined),
        ("year", CommonValue::Number(f64::NAN)),
        ("negativeZero", CommonValue::Number(-0.0)),
        ("pos", CommonValue::Number(f64::INFINITY)),
        ("neg", CommonValue::Number(f64::NEG_INFINITY)),
        (
            "array",
            CommonValue::Array(vec![CommonValue::Undefined, CommonValue::Null]),
        ),
    ]);
    let wire = build("/音楽 🎵.mp3", &common, &[]).unwrap();
    assert_eq!(wire.data["filePath"], "/音楽 🎵.mp3");
    assert_eq!(wire.data["metadata"]["missing"], Value::Null);
    assert_eq!(wire.data["metadata"]["negativeZero"], 0);
    assert_eq!(
        wire.hydration.undefined_paths,
        ["/metadata/missing", "/metadata/array/0"]
    );
    assert_eq!(wire.hydration.special_numbers.len(), 4);
    assert_eq!(wire.hydration.special_numbers[0].kind.name(), "NaN");
    assert_eq!(
        wire.hydration.object_keys[0].keys,
        ["missing", "year", "negativeZero", "pos", "neg", "array"]
    );
    assert!(wire.hydration.byte_buffers.is_empty());
    assert!(matches!(common.get("year"),CommonValue::Number(n) if n.is_nan()));
}

#[test]
fn borrowed_picture_bytes_and_fields_have_complete_canonical_wire_receipts() {
    let common = object(&[("title", CommonValue::String("Title".into()))]);
    let fields = object(&[
        ("format", CommonValue::String("image/png".into())),
        ("type", CommonValue::Undefined),
        ("description", CommonValue::String("Café".into())),
    ]);
    let bytes = [0, 255, 1, 2];
    let wire = build(
        "path",
        &common,
        &[PictureRef {
            fields: &fields,
            bytes: &bytes,
        }],
    )
    .unwrap();
    assert_eq!(wire.data["metadata"]["picture"][0]["data"], "AP8BAg==");
    assert_eq!(wire.hydration.byte_buffers, ["/metadata/picture/0/data"]);
    assert_eq!(wire.hydration.undefined_paths, ["/metadata/picture/0/type"]);
    assert_eq!(wire.hydration.object_keys[0].keys, ["title", "picture"]);
    assert_eq!(
        wire.hydration.object_keys[1].keys,
        ["format", "type", "description", "data"]
    );
    assert_eq!(bytes, [0, 255, 1, 2]);
    assert!(matches!(fields.get("type"), CommonValue::Undefined));
}

#[test]
fn full_decoded_quota_is_checked_before_base64_materialization() {
    let common = object(&[]);
    let fields = object(&[]);
    let bytes = vec![211; MAX_DECODED_BYTES];
    let wire = build(
        "path",
        &common,
        &[PictureRef {
            fields: &fields,
            bytes: &bytes,
        }],
    )
    .unwrap();
    assert_eq!(
        wire.data["metadata"]["picture"][0]["data"]
            .as_str()
            .unwrap()
            .len(),
        5_592_408
    );
    let extra = [0];
    assert_eq!(
        build(
            "path",
            &common,
            &[
                PictureRef {
                    fields: &fields,
                    bytes: &bytes
                },
                PictureRef {
                    fields: &fields,
                    bytes: &extra
                }
            ]
        )
        .unwrap_err(),
        WireError::Limit("decoded picture bytes")
    );
    let nine: Vec<_> = (0..9)
        .map(|_| PictureRef {
            fields: &fields,
            bytes: &[],
        })
        .collect();
    assert_eq!(
        build("path", &common, &nine).unwrap_err(),
        WireError::Limit("picture count")
    );
}

#[test]
fn generated_envelope_and_metadata_counts_match_known_shapes() {
    let common = object(&[("x", CommonValue::Array(vec![CommonValue::Null; 65523]))]);
    let mut c = Counts::start().unwrap();
    let mut path = "/metadata".into();
    let CommonValue::Object(fields) = &common else {
        unreachable!()
    };
    c.object(fields, Extra::None, &mut path, 0).unwrap();
    assert_eq!(c.nodes, MAX_NODES);
    let wire = build("path", &common, &[]).unwrap();
    assert_eq!(
        serde_json::to_vec(&wire.hydration).unwrap().len(),
        c.envelope
    );
    assert_eq!(
        serde_json::to_vec(&wire.data["metadata"]).unwrap().len(),
        c.metadata
    );
    let oversized = object(&[("x", CommonValue::Array(vec![CommonValue::Null; 65524]))]);
    assert_eq!(
        build("path", &oversized, &[]).unwrap_err(),
        WireError::Limit("shared nodes")
    );
}

#[test]
fn nested_escaped_pointers_and_depth_are_bounded_without_mutating_native_input() {
    let common = object(&[("a/b~c", object(&[("value", CommonValue::Undefined)]))]);
    let wire = build("path", &common, &[]).unwrap();
    assert_eq!(wire.hydration.undefined_paths, ["/metadata/a~1b~0c/value"]);
    let mut nested = CommonValue::Null;
    for _ in 0..16 {
        nested = object(&[("x", nested)]);
    }
    assert!(build("path", &nested, &[]).is_ok());
    let deeper = object(&[("x", nested)]);
    assert!(matches!(
        build("path", &deeper, &[]),
        Err(WireError::Limit(_))
    ));
    let long = "x".repeat(1024);
    let common = object(&[(
        &long,
        object(&[(
            &long,
            CommonValue::String("ordinary leaf needs no envelope path".into()),
        )]),
    )]);
    assert!(build("path", &common, &[]).is_ok());
}

#[test]
fn duplicate_dangerous_and_nonrepresentable_keys_and_raw_picture_fields_reject() {
    for common in [
        object(&[("constructor", CommonValue::Null)]),
        object(&[("__proto__", CommonValue::Null)]),
        object(&[("prototype", CommonValue::Null)]),
        object(&[("x", CommonValue::Null), ("x", CommonValue::Null)]),
        object(&[("a", CommonValue::Null), ("0", CommonValue::Null)]),
        object(&[("10", CommonValue::Null), ("2", CommonValue::Null)]),
        object(&[("picture", CommonValue::Null)]),
        CommonValue::Null,
    ] {
        assert!(matches!(
            build("path", &common, &[]),
            Err(WireError::Invalid(_))
        ));
    }
    assert!(build(
        "path",
        &object(&[
            ("2", CommonValue::Null),
            ("10", CommonValue::Null),
            ("z", CommonValue::Null)
        ]),
        &[]
    )
    .is_ok());
    let fields = object(&[("data", CommonValue::String("not bytes".into()))]);
    assert!(matches!(
        build(
            "path",
            &object(&[]),
            &[PictureRef {
                fields: &fields,
                bytes: &[]
            }]
        ),
        Err(WireError::Invalid(_))
    ));
}

#[test]
fn individual_strings_metadata_keys_paths_and_late_bad_fields_are_explicit_limits() {
    assert_eq!(
        build(&"x".repeat(32769), &object(&[]), &[]).unwrap_err(),
        WireError::Limit("filePath")
    );
    assert!(build(
        "path",
        &object(&[("x", CommonValue::String("x".repeat(MAX_STRING_BYTES)))]),
        &[]
    )
    .is_err());
    assert_eq!(
        build(
            "path",
            &object(&[(&"x".repeat(1025), CommonValue::Null)]),
            &[]
        )
        .unwrap_err(),
        WireError::Limit("individual key")
    );
    let common = object(&[
        ("a", CommonValue::String("x".repeat(4 * 1024 * 1024))),
        ("b", CommonValue::String("x".repeat(4 * 1024 * 1024))),
    ]);
    assert_eq!(
        build("path", &common, &[]).unwrap_err(),
        WireError::Limit("metadata JSON")
    );
    let long = "x".repeat(1024);
    let common = object(&[(&long, object(&[(&long, CommonValue::Undefined)]))]);
    assert_eq!(
        build("path", &common, &[]).unwrap_err(),
        WireError::Limit("individual pointer")
    );
    let fields = object(&[
        ("format", CommonValue::String("image/png".into())),
        ("constructor", CommonValue::Null),
    ]);
    let large = vec![0; MAX_DECODED_BYTES];
    assert!(matches!(
        build(
            "path",
            &object(&[]),
            &[PictureRef {
                fields: &fields,
                bytes: &large
            }]
        ),
        Err(WireError::Invalid(_))
    ));
}
