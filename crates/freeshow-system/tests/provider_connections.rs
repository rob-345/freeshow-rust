use freeshow_system::provider_connections::{
    project, ProjectionError, MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_DEPTH, MAX_SNAPSHOT_NODES,
};
use serde_json::{json, Value};

#[test]
fn registration_order_and_combined_scope_keys_are_preserved() {
    let snapshot = json!({"contentProviders":{
        "canva":{"folder:read design:content:read design:meta:read":[]},
        "onstage":{"presenter":{"byTeam":{}}},
        "amazinglife":{"openid profile email":"expired fixture"},
        "planningcenter":{"services":{}},"churchApps":{"plans":true}
    },"secrets":{"fixture":"never published"}});
    let before = snapshot.clone();
    let result = project(&snapshot).unwrap();
    assert_eq!(
        result
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "churchApps",
            "planningcenter",
            "amazinglife",
            "onstage",
            "canva"
        ]
    );
    assert!(result
        .as_object()
        .unwrap()
        .values()
        .all(|value| value == true));
    assert_eq!(snapshot, before);
    assert_eq!(
        project(
            &json!({"contentProviders":{"amazinglife":{"openid":{}},"canva":{"folder:read":{}}}})
        )
        .unwrap(),
        json!({})
    );
}

#[test]
fn js_truthiness_and_nested_wrong_shapes_do_not_validate_tokens() {
    for value in [json!(null), json!(false), json!(0), json!(-0.0), json!("")] {
        assert_eq!(
            project(&json!({"contentProviders":{"onstage":{"presenter":value}}})).unwrap(),
            json!({})
        );
    }
    for value in [
        json!({}),
        json!([]),
        json!(1),
        json!(-1),
        json!(" "),
        json!({"expires_in":0}),
    ] {
        assert_eq!(
            project(&json!({"contentProviders":{"onstage":{"presenter":value}}})).unwrap(),
            json!({"onstage":true})
        );
    }
    for value in [
        json!(null),
        json!(false),
        json!(1),
        json!("presenter"),
        json!([]),
    ] {
        assert_eq!(
            project(&json!({"contentProviders":value})).unwrap(),
            json!({})
        );
        assert_eq!(
            project(&json!({"contentProviders":{"onstage":value}})).unwrap(),
            json!({})
        );
    }
    assert_eq!(project(&json!({})).unwrap(), json!({}));
}

#[test]
fn invalid_root_is_not_a_successful_empty_store() {
    for value in [json!(null), json!(false), json!(1), json!(""), json!([])] {
        let error = project(&value).unwrap_err();
        assert_eq!(error, ProjectionError::InvalidSnapshot);
        assert_eq!(error.code(), "provider_connections_invalid");
    }
}

#[test]
fn json_byte_limit_counts_unknown_secrets_keys_unicode_and_escaping() {
    let prefix = serde_json::to_vec(&json!({"secrets":""})).unwrap().len();
    let at_limit = json!({"secrets":"x".repeat(MAX_SNAPSHOT_BYTES-prefix)});
    assert_eq!(
        serde_json::to_vec(&at_limit).unwrap().len(),
        MAX_SNAPSHOT_BYTES
    );
    assert_eq!(project(&at_limit).unwrap(), json!({}));
    assert_eq!(
        project(&json!({"secrets":"x".repeat(MAX_SNAPSHOT_BYTES-prefix+1)})),
        Err(ProjectionError::ByteLimit)
    );
    let escaped = json!({"secrets":"\0".repeat(MAX_SNAPSHOT_BYTES/6)});
    assert!(serde_json::to_vec(&escaped).unwrap().len() > MAX_SNAPSHOT_BYTES);
    assert_eq!(project(&escaped), Err(ProjectionError::ByteLimit));
    let unicode = json!({"secrets":"🌍".repeat(MAX_SNAPSHOT_BYTES/4)});
    assert_eq!(project(&unicode), Err(ProjectionError::ByteLimit));
    assert_eq!(
        project(&json!({"unknown":{"k".repeat(MAX_SNAPSHOT_BYTES):null}})),
        Err(ProjectionError::ByteLimit)
    );
    let error = project(&escaped).unwrap_err().to_string();
    assert!(!error.contains("secrets"));
    assert!(!error.contains('\0'));
}

#[test]
fn node_and_depth_limits_cover_unused_data_at_exact_boundaries() {
    // Root + array + N null elements are N+2 value nodes.
    assert_eq!(
        project(&json!({"secrets":vec![Value::Null; MAX_SNAPSHOT_NODES-2]})).unwrap(),
        json!({})
    );
    assert_eq!(
        project(&json!({"secrets":vec![Value::Null; MAX_SNAPSHOT_NODES-1]})),
        Err(ProjectionError::NodeLimit)
    );
    let mut nested = Value::Null;
    for _ in 1..MAX_SNAPSHOT_DEPTH {
        nested = json!([nested]);
    }
    assert_eq!(
        project(&json!({"secrets":nested.clone()})).unwrap(),
        json!({})
    );
    assert_eq!(
        project(&json!({"secrets":[nested]})),
        Err(ProjectionError::DepthLimit)
    );
}
