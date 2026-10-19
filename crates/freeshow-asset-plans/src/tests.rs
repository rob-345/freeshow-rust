use super::*;
use serde_json::json;
use std::{cell::Cell, path::Path};
fn context() -> Context {
    Context {
        runtime_id: "test".into(),
        library_generation: 7,
        data_path: "/library".into(),
    }
}
fn plain(_: &str) -> Option<Protection> {
    Some(Protection::Plain)
}
fn root(name: &str) -> String {
    if cfg!(windows) {
        format!("C:/definitely-not-a-real-asset/{name}")
    } else {
        format!("/definitely-not-a-real-asset/{name}")
    }
}

#[test]
fn pure_planning_preserves_exact_source_keys_and_selected_native_percent_signs() {
    let context = context();
    let native = root("a%20b.png");
    let alias = root("a b.png");
    let paths = [
        AcceptedPath::new(&native, Path::new(&native), Some(Protection::Plain)),
        AcceptedPath::new(&alias, Path::new(&native), Some(Protection::Plain)),
    ];
    let plan = Planner::default()
        .plan(AcceptedNative::selection(&context, &paths))
        .unwrap();
    assert_eq!(plan.context, context);
    assert_eq!(plan.grants.len(), 2);
    assert_eq!(plan.grants[0].source_path, native);
    assert_eq!(plan.grants[0].path, Path::new(&native));
    assert_eq!(plan.grants[1].source_path, alias);
    assert_eq!(plan.grants[1].path, Path::new(&native));
}

#[test]
fn source_numeric_keys_precede_insertion_keys_without_treating_plus_as_an_index() {
    let mut media = serde_json::Map::new();
    for (key, value) in [
        ("z", root("z")),
        ("10", root("10")),
        ("+1", root("plus")),
        ("2", root("2")),
        ("a", root("a")),
    ] {
        media.insert(key.into(), json!({"path":value}));
    }
    let data = json!({"media":media});
    let context = context();
    let plan = Planner::default()
        .plan(Committed::show(&context, &data, &plain))
        .unwrap();
    assert_eq!(
        plan.grants
            .iter()
            .map(|grant| grant.source_path.as_str())
            .collect::<Vec<_>>(),
        [root("2"), root("10"), root("z"), root("plus"), root("a")]
    );
}

#[test]
fn known_container_and_work_limits_reject_before_the_classifier_runs() {
    let data =
        json!({"media":{"a":{"path":root("a")},"b":{"path":root("b")},"c":{"path":root("c")}}});
    let count = Cell::new(0);
    let classifier = |_: &str| {
        count.set(count.get() + 1);
        Some(Protection::Plain)
    };
    for limits in [
        Limits {
            container_entries: 2,
            ..Limits::default()
        },
        Limits {
            operations: 2,
            ..Limits::default()
        },
    ] {
        let planner = Planner::new(limits).unwrap();
        assert_eq!(
            planner
                .plan(Committed::show(&context(), &data, &classifier))
                .unwrap_err()
                .code,
            "asset_plan_limit"
        );
        assert_eq!(count.get(), 0);
    }
}

#[test]
fn diagnostic_count_bytes_and_omissions_are_bounded_without_cutting_utf8() {
    let data = json!({"media":(0..100).map(|i|(i.to_string(),json!({"path":format!("freeshow-protected://{}-{i}","é".repeat(300))}))).collect::<serde_json::Map<_,_>>()});
    let context = context();
    for (count, bytes) in [(2, 65536), (64, 600), (0, 0)] {
        let planner = Planner::new(Limits {
            diagnostics: count,
            diagnostic_bytes: bytes,
            ..Limits::default()
        })
        .unwrap();
        let plan = planner
            .plan(Committed::show(&context, &data, &plain))
            .unwrap();
        assert_eq!(plan.grants.len(), 0);
        assert_eq!(plan.stats.protected, 100);
        assert!(plan.diagnostics.len() <= count);
        assert!(plan.stats.diagnostic_bytes <= bytes);
        assert_eq!(plan.stats.diagnostics_omitted + plan.diagnostics.len(), 100);
        assert!(plan
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.source_path.len() <= 512));
        assert_eq!(
            plan.stats.classifier_calls, 0,
            "Protected URI overrides a plaintext classifier without invoking it"
        );
    }
}

#[test]
fn source_key_target_and_protection_conflicts_return_no_partial_plan() {
    let context = context();
    let key = root("key");
    let other = root("other");
    for second in [
        AcceptedPath::new(&key, Path::new(&other), Some(Protection::Plain)),
        AcceptedPath::new(&key, Path::new(&key), Some(Protection::Protected)),
        AcceptedPath::new(&key, Path::new(&key), None),
    ] {
        let paths = [
            AcceptedPath::new(&key, Path::new(&key), Some(Protection::Plain)),
            second,
        ];
        assert_eq!(
            Planner::default()
                .plan(AcceptedNative::selection(&context, &paths))
                .unwrap_err()
                .code,
            "asset_plan_conflict"
        );
    }
}

#[test]
fn local_candidate_and_payload_limits_fail_before_an_excess_candidate_is_returned() {
    let context = context();
    let data = json!({"media":{"a":{"path":root("a")},"b":{"path":root("b")}}});
    for limits in [
        Limits {
            candidates: 1,
            ..Limits::default()
        },
        Limits {
            candidate_bytes: 1,
            ..Limits::default()
        },
        Limits {
            input_path_bytes: 1,
            ..Limits::default()
        },
        Limits {
            path_bytes: 1,
            ..Limits::default()
        },
        Limits {
            references: 1,
            ..Limits::default()
        },
        Limits {
            containers: 1,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            Planner::new(limits)
                .unwrap()
                .plan(Committed::show(&context, &data, &plain))
                .unwrap_err()
                .code,
            "asset_plan_limit"
        );
    }
}

#[test]
fn only_committed_full_show_fields_are_walked_and_callback_invocations_stay_bounded() {
    let path = root("visible");
    let secret = root("unknown");
    let context = context();
    let data = json!({"SHOWS":{"untrusted":{"media":{"x":{"path":secret}}}},"showsCache":{"s":{"media":{"x":{"path":path}},"unknown":{"src":secret,"path":secret,"deep":[{"media":{"path":secret}}]},"slides":{"s":{"items":[{"type":"text","src":secret},{"type":"media","src":path,"unknown":{"src":secret}}]}}}}});
    let count = Cell::new(0);
    let classifier = |_: &str| {
        count.set(count.get() + 1);
        Some(Protection::Plain)
    };
    let plan = Planner::default()
        .plan(Committed::save_snapshot(&context, &data, &classifier))
        .unwrap();
    assert_eq!(plan.grants.len(), 1);
    assert_eq!(plan.grants[0].source_path, path);
    assert_eq!(plan.stats.duplicates, 1);
    assert_eq!(count.get(), 2);
    let trimmed = json!({"SHOWS":data["SHOWS"]});
    assert!(Planner::default()
        .plan(Committed::save_snapshot(&context, &trimmed, &plain))
        .unwrap()
        .grants
        .is_empty());
}

#[test]
fn caller_classification_is_required_and_protected_never_yields_a_plain_grant() {
    let context = context();
    let path = root("opaque-cache-entry");
    let data = json!({"media":{"x":{"path":path,"encrypted":false}}});
    let unknown = |_: &str| None;
    let protected = |_: &str| Some(Protection::Protected);
    for (classify, code) in [
        (&unknown as Classifier<'_>, "asset_plan_unclassified"),
        (&protected as Classifier<'_>, "asset_plan_protected"),
    ] {
        let plan = Planner::default()
            .plan(Committed::show(&context, &data, classify))
            .unwrap();
        assert!(plan.grants.is_empty());
        assert_eq!(plan.diagnostics[0].code, code);
    }
}
