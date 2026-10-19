//! Portable corruption controls for the shared Darwin source comparator.
#[path = "support/macos.rs"]
#[allow(
    dead_code,
    reason = "The actual source process runs in Darwin integration"
)]
mod evidence;
use serde_json::{json, Value};

fn row(modified: f64) -> Value {
    let data = json!({"stat":{"dev":7.0,"ino":11.0,"atimeMs":2.0,"mtimeMs":modified,"ctimeMs":3.0,"birthtimeMs":4.0,"size":0.0},"isFile":false});
    let mut bits = serde_json::Map::new();
    for (key, value) in data["stat"].as_object().unwrap() {
        bits.insert(
            format!("/stat/{key}"),
            json!(format!("{:016x}", value.as_f64().unwrap().to_bits())),
        );
    }
    json!({"data":data,"numberBits":bits})
}
fn rejects(actual: &Value, before: &Value, after: &Value) {
    assert!(
        std::panic::catch_unwind(|| evidence::assert_live_device(actual, before, after)).is_err()
    );
}
#[test]
fn changed_shared_device_time_accepts_only_a_bracketed_observation() {
    let before = row(1.25);
    let after = row(2.25);
    let native = row(1.75)["data"].clone();
    // The old exact-only fixture rejects a legitimate intervening observation.
    assert!(std::panic::catch_unwind(|| {
        evidence::assert_value(&native, &before["data"], &before["numberBits"])
    })
    .is_err());
    evidence::assert_live_device(&native, &before, &after);
    rejects(&row(2.5)["data"], &before, &after);
    rejects(&row(1.0)["data"], &before, &after);
}
#[test]
fn unchanged_device_time_still_requires_exact_binary64_bits() {
    let source = row(1.25);
    evidence::assert_live_device(&source["data"], &source, &source);
    let next = f64::from_bits(1.25_f64.to_bits() + 1);
    rejects(&row(next)["data"], &source, &source);
    let mut parsed_decimal = source.clone();
    parsed_decimal["data"]["stat"]["mtimeMs"] = json!(next);
    // The actual captured source bits remain authoritative even when its
    // transported decimal was rounded differently by a JSON parser.
    evidence::assert_live_device(&source["data"], &parsed_decimal, &parsed_decimal);
    let mut native = source["data"].clone();
    native["stat"]["ctimeMs"] = json!(f64::from_bits(3.0_f64.to_bits() + 1));
    rejects(&native, &source, &source);
}
#[test]
fn device_shape_stable_fields_replacement_and_source_bit_omission_still_fail() {
    let before = row(1.25);
    let after = row(2.25);
    let mut native = row(1.75)["data"].clone();
    native["stat"]["size"] = json!(1.0);
    rejects(&native, &before, &after);
    let mut replaced = row(2.25);
    replaced["data"]["isFile"] = json!(true);
    rejects(&row(1.75)["data"], &before, &replaced);
    let mut replaced_inode = row(2.25);
    replaced_inode["data"]["stat"]["ino"] = json!(12.0);
    replaced_inode["numberBits"]["/stat/ino"] = json!(format!("{:016x}", 12.0_f64.to_bits()));
    rejects(&row(1.75)["data"], &before, &replaced_inode);
    let mut missing = before.clone();
    missing["numberBits"]
        .as_object_mut()
        .unwrap()
        .remove("/stat/mtimeMs");
    rejects(&row(1.75)["data"], &missing, &after);
    let mut missing_field = before.clone();
    missing_field["data"]["stat"]
        .as_object_mut()
        .unwrap()
        .remove("mtimeMs");
    rejects(&row(1.75)["data"], &missing_field, &after);
    let mut wrong_type = after.clone();
    wrong_type["data"]["stat"]["mtimeMs"] = json!("not a timestamp");
    rejects(&row(1.75)["data"], &before, &wrong_type);
}
