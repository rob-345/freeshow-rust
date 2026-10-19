#[path = "../src/now_playing_schedule.rs"]
mod now_playing_schedule;

use freeshow_core::protocol::RuntimeContext;
use now_playing_schedule::{
    OperationId, OperationState, PhaseKind, Schedule, ScheduleError, MAX_OPERATIONS,
};
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf};

fn context() -> RuntimeContext {
    RuntimeContext {
        runtime_id: "native-schedule-音楽".into(),
        library_generation: 7,
        data_path: std::env::temp_dir().join("freeshow-schedule-native-only"),
    }
}
fn schedule() -> Schedule {
    Schedule::new(context()).unwrap()
}
fn drain(s: &mut Schedule, names: &HashMap<OperationId, &str>) -> Vec<Value> {
    let mut trace = Vec::new();
    while let Some(token) = s.take_next() {
        trace.push(json!([
            names[token.operation()],
            format!("{:?}", token.kind())
        ]));
        assert!(
            s.take_next().is_none(),
            "Current source phase must remain atomic"
        );
        s.complete(&token).unwrap();
    }
    trace
}

#[test]
fn actual_pinned_same_realm_source_matches_eight_documented_controlled_turn_cases() {
    let root = std::env::var_os("FREESHOW_NOW_PLAYING_ORACLE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_path_buf()
        });
    let output = std::process::Command::new("node")
        .current_dir(&root)
        .arg(root.join("tests/parity/fixtures/now-playing-overlap.mjs"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() < 64 * 1024);
    let original: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        original["sourceHash"],
        "0a203138d9c283b3e5fa37f0aa46cd33df596d8ee7183ac6cd73d3c59cf3bc7c"
    );
    assert_eq!(original["cases"].as_object().unwrap().len(), 8);
    eprintln!(
        "Same-realm original NOW_PLAYING oracle: {}",
        original["node"]
    );

    for name in [
        "same_turn_b_a",
        "same_turn_a_b",
        "unset_before_yield",
        "separate_turn",
    ] {
        let mut s = schedule();
        let a = s.admit_set().unwrap();
        let b = s.admit_set().unwrap();
        let mut names = HashMap::from([(a.clone(), "A"), (b.clone(), "B")]);
        if name == "unset_before_yield" {
            // The actual source calls UNSET synchronously after resolving the
            // parser promises but before yielding. Native incoming source jobs
            // are therefore observed before the ready outcomes in this case.
            let u = s.admit_unset().unwrap();
            names.insert(u, "U");
        }
        let mut trace = Vec::new();
        if name == "same_turn_a_b" {
            s.ready(&a, true).unwrap();
            s.ready(&b, true).unwrap();
        } else {
            s.ready(&b, true).unwrap();
            if name == "separate_turn" {
                trace.extend(drain(&mut s, &names));
            }
            s.ready(&a, true).unwrap();
        }
        trace.extend(drain(&mut s, &names));
        assert_eq!(json!(trace), original["cases"][name]["phases"], "{name}");
        for id in names.keys() {
            s.release(id).unwrap();
        }
        assert!(s.status().drained());
    }
    {
        let mut s = schedule();
        let a = s.admit_set().unwrap();
        let u = s.admit_unset().unwrap();
        let o = s.admit_open().unwrap();
        s.ready(&a, true).unwrap();
        let names = HashMap::from([(a, "A"), (u, "U"), (o, "O")]);
        assert_eq!(
            json!(drain(&mut s, &names)),
            original["cases"]["unset_open_pending"]["phases"]
        );
    }
    for name in ["parser_null", "missing_common"] {
        let mut s = schedule();
        let a = s.admit_set().unwrap();
        s.ready(&a, false).unwrap();
        assert_eq!(
            json!(drain(&mut s, &HashMap::from([(a, "A")]))),
            original["cases"][name]["phases"]
        );
    }
    {
        let mut s = schedule();
        let a = s.admit_set().unwrap();
        s.ready(&a, true).unwrap();
        let mut trace = Vec::new();
        let metadata = s.take_next().unwrap();
        trace.push(json!(["A", format!("{:?}", metadata.kind())]));
        s.complete(&metadata).unwrap();
        let post = s.take_next().unwrap();
        trace.push(json!(["A", format!("{:?}", post.kind())]));
        s.fail_current(&post).unwrap();
        assert!(s.take_next().is_none());
        assert_eq!(json!(trace), original["cases"]["post_failure"]["phases"]);
        // Failure happened after the source metadata event and title boundary,
        // before PNG/text. Source image failure is real rejection, not an ACK.
        let boundaries = original["cases"]["post_failure"]["boundaries"]
            .as_array()
            .unwrap();
        assert!(boundaries.iter().any(|row| row[0] == "image.create"));
        assert!(!boundaries.iter().any(|row| row[0] == "write"));
        s.release(&a).unwrap();
    }
}

#[test]
fn four_whole_slots_include_pending_current_and_finished_unreleased_records() {
    let mut s = schedule();
    let ids = (0..MAX_OPERATIONS)
        .map(|_| s.admit_set().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(s.admit_open().unwrap_err(), ScheduleError::Full);
    assert_eq!(s.status().pending_preparations, 4);
    for id in &ids {
        s.ready(id, true).unwrap();
    }
    assert_eq!(s.status().queued, 4);
    let names = ids.iter().map(|id| (id.clone(), "A")).collect();
    assert_eq!(drain(&mut s, &names).len(), 12);
    assert_eq!(s.status().finished_unreleased, 4);
    assert!(!s.status().drained());
    assert_eq!(s.admit_set().unwrap_err(), ScheduleError::Full);
    s.release(&ids[0]).unwrap();
    let next = s.admit_open().unwrap();
    assert!(next.sequence() > ids[3].sequence());
    assert_eq!(s.ready(&ids[0], true), Err(ScheduleError::UnknownOperation));
    assert_eq!(s.release(&ids[0]), Err(ScheduleError::UnknownOperation));
    let token = s.take_next().unwrap();
    assert_eq!(token.operation(), &next);
    s.complete(&token).unwrap();
    s.release(&next).unwrap();
    for id in &ids[1..] {
        s.release(id).unwrap();
    }
    assert!(s.status().drained());
}

#[test]
fn current_phase_is_atomic_even_when_ready_results_and_source_jobs_arrive() {
    let mut s = schedule();
    let a = s.admit_set().unwrap();
    s.ready(&a, true).unwrap();
    let publish = s.take_next().unwrap();
    assert_eq!(publish.kind(), PhaseKind::PublishMetadata);
    let b = s.admit_set().unwrap();
    s.ready(&b, true).unwrap();
    let u = s.admit_unset().unwrap();
    let o = s.admit_open().unwrap();
    assert!(s.take_next().is_none());
    assert_eq!(s.status().active, 1);
    assert_eq!(s.status().queued, 3);
    assert_eq!(s.release(&a), Err(ScheduleError::InvalidState));
    s.complete(&publish).unwrap();
    assert_eq!(s.status().queued, 4);
    let names = HashMap::from([(a.clone(), "A"), (b, "B"), (u, "U"), (o, "O")]);
    assert_eq!(
        drain(&mut s, &names),
        vec![
            json!(["B", "PublishMetadata"]),
            json!(["U", "Unset"]),
            json!(["O", "Open"]),
            json!(["A", "PostMetadata"]),
            json!(["B", "PostMetadata"]),
            json!(["A", "Text"]),
            json!(["B", "Text"]),
        ]
    );
}

#[test]
fn duplicate_ready_and_replayed_tokens_never_advance_another_phase() {
    let mut s = schedule();
    let a = s.admit_set().unwrap();
    assert_eq!(s.release(&a), Err(ScheduleError::InvalidState));
    s.ready(&a, true).unwrap();
    assert_eq!(s.ready(&a, false), Err(ScheduleError::InvalidState));
    assert_eq!(s.preparation_failed(&a), Err(ScheduleError::InvalidState));
    let metadata = s.take_next().unwrap();
    s.complete(&metadata).unwrap();
    assert_eq!(s.complete(&metadata), Err(ScheduleError::StalePhase));
    let post = s.take_next().unwrap();
    assert_eq!(s.fail_current(&metadata), Err(ScheduleError::StalePhase));
    assert_eq!(
        s.state(&a).unwrap(),
        OperationState::Active(PhaseKind::PostMetadata)
    );
    s.complete(&post).unwrap();
    let text = s.take_next().unwrap();
    s.complete(&text).unwrap();
    assert_eq!(s.complete(&text), Err(ScheduleError::StalePhase));
    s.release(&a).unwrap();
    assert_eq!(s.complete(&text), Err(ScheduleError::UnknownOperation));
}

#[test]
fn native_context_and_run_seals_cannot_be_replaced_by_equal_or_foreign_ids() {
    let mut captured = context();
    let mut s = Schedule::new(captured.clone()).unwrap();
    let id = s.admit_set().unwrap();
    let mut values = HashMap::from([(id.clone(), "retained native operation")]);
    assert_eq!(values.remove(&id), Some("retained native operation"));
    assert_eq!(id.context(), &captured);
    assert_eq!(s.context(), &captured);
    captured.library_generation += 1;
    captured.runtime_id = "different".into();
    captured.data_path = std::env::temp_dir().join("different-root");
    assert_ne!(id.context(), &captured);
    assert_eq!(id.context(), &context());
    for foreign_context in [
        context(),
        captured,
        RuntimeContext {
            runtime_id: "other".into(),
            ..context()
        },
        RuntimeContext {
            library_generation: 8,
            ..context()
        },
        RuntimeContext {
            data_path: std::env::temp_dir().join("another"),
            ..context()
        },
    ] {
        let mut foreign = Schedule::new(foreign_context).unwrap();
        let foreign_id = foreign.admit_unset().unwrap();
        assert_ne!(id, foreign_id);
        assert_eq!(s.ready(&foreign_id, true), Err(ScheduleError::ForeignRun));
        assert_eq!(s.release(&foreign_id), Err(ScheduleError::ForeignRun));
        let token = foreign.take_next().unwrap();
        assert_eq!(s.complete(&token), Err(ScheduleError::ForeignRun));
        assert_eq!(token.context(), foreign.context());
        foreign.complete(&token).unwrap();
    }
    assert_eq!(s.status().pending_preparations, 1);
}

#[test]
fn retirement_keeps_actual_current_and_pending_preparation_until_host_settles_them() {
    let mut s = schedule();
    let pending = s.admit_set().unwrap();
    let preparing = s.admit_set().unwrap();
    s.ready(&preparing, true).unwrap();
    let current = s.take_next().unwrap();
    let unset = s.admit_unset().unwrap();
    let open = s.admit_open().unwrap();
    s.retire();
    s.retire();
    assert!(s.status().retiring);
    assert_eq!(s.status().queued, 0);
    assert_eq!(s.status().pending_preparations, 1);
    assert_eq!(s.status().active, 1);
    assert_eq!(s.status().finished_unreleased, 2);
    assert!(!s.status().drained());
    assert!(s.take_next().is_none());
    assert_eq!(s.admit_open().unwrap_err(), ScheduleError::Retired);
    assert_eq!(s.release(&pending), Err(ScheduleError::InvalidState));
    assert_eq!(s.release(&preparing), Err(ScheduleError::InvalidState));
    s.release(&unset).unwrap();
    s.release(&open).unwrap();
    // Only a genuinely returned prep future may perform this transition.
    s.ready(&pending, true).unwrap();
    assert_eq!(s.status().pending_preparations, 0);
    assert!(s.take_next().is_none());
    s.release(&pending).unwrap();
    assert_eq!(s.status().active, 1);
    assert!(!s.status().drained());
    // Only an actual effect/cleanup receipt may perform this transition.
    s.complete(&current).unwrap();
    assert_eq!(s.state(&preparing).unwrap(), OperationState::Finished);
    assert!(s.take_next().is_none());
    s.release(&preparing).unwrap();
    assert!(s.status().drained());
}

#[test]
fn failed_preparations_and_phases_stop_only_their_own_future_work() {
    let mut s = schedule();
    let failed = s.admit_set().unwrap();
    let good = s.admit_set().unwrap();
    s.preparation_failed(&failed).unwrap();
    assert_eq!(s.state(&failed).unwrap(), OperationState::Finished);
    assert_eq!(
        s.preparation_failed(&failed),
        Err(ScheduleError::InvalidState)
    );
    s.ready(&good, true).unwrap();
    let metadata = s.take_next().unwrap();
    assert_eq!(metadata.operation(), &good);
    s.complete(&metadata).unwrap();
    let post = s.take_next().unwrap();
    s.fail_current(&post).unwrap();
    assert_eq!(s.state(&good).unwrap(), OperationState::Finished);
    assert!(s.take_next().is_none());
    assert!(!s.status().drained());
    s.release(&failed).unwrap();
    s.release(&good).unwrap();
    let still_pending = s.admit_set().unwrap();
    s.retire();
    assert_eq!(s.status().pending_preparations, 1);
    s.preparation_failed(&still_pending).unwrap();
    s.release(&still_pending).unwrap();
    assert!(s.status().drained());
}

#[test]
fn retirement_does_not_forge_completion_when_dispatch_token_is_dropped() {
    let mut s = schedule();
    let id = s.admit_open().unwrap();
    let actual = s.take_next().unwrap();
    drop(actual); // The real host must retain it; token Drop is never a receipt.
    s.retire();
    assert_eq!(s.status().active, 1);
    assert_eq!(s.release(&id), Err(ScheduleError::InvalidState));
    assert!(!s.status().drained());
}

#[test]
fn released_slots_reuse_capacity_without_reusing_native_identity_or_growing_fifo() {
    let mut s = schedule();
    let mut previous = 0;
    for _ in 0..64 {
        let ids = (0..4).map(|_| s.admit_set().unwrap()).collect::<Vec<_>>();
        for id in ids.iter().rev() {
            assert!(id.sequence() > previous);
            s.ready(id, true).unwrap();
        }
        previous = ids[3].sequence();
        while let Some(token) = s.take_next() {
            let status = s.status();
            assert!(status.records <= 4 && status.queued + status.active <= 4);
            s.complete(&token).unwrap();
        }
        for id in &ids {
            s.release(id).unwrap();
        }
        assert!(s.status().drained());
    }
}

#[test]
fn context_limits_reject_native_invalid_spelling_before_creating_records() {
    let invalid = vec![
        RuntimeContext {
            runtime_id: String::new(),
            ..context()
        },
        RuntimeContext {
            runtime_id: "é".repeat(129),
            ..context()
        },
        RuntimeContext {
            runtime_id: "native\0id".into(),
            ..context()
        },
        RuntimeContext {
            data_path: PathBuf::new(),
            ..context()
        },
        RuntimeContext {
            data_path: PathBuf::from("relative"),
            ..context()
        },
        RuntimeContext {
            data_path: std::env::temp_dir().join("a\0b"),
            ..context()
        },
        RuntimeContext {
            data_path: std::env::temp_dir().join("x".repeat(32769)),
            ..context()
        },
        RuntimeContext {
            library_generation: 9_007_199_254_740_992,
            ..context()
        },
    ];
    for value in invalid {
        assert_eq!(
            Schedule::new(value).err(),
            Some(ScheduleError::InvalidContext)
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let value = RuntimeContext {
            data_path: PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 255])),
            ..context()
        };
        assert_eq!(
            Schedule::new(value).err(),
            Some(ScheduleError::InvalidContext)
        );
    }
}
