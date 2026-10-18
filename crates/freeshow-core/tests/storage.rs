use freeshow_core::{
    protocol::Request,
    runtime::Runtime,
    storage::{atomic_write, Stores},
};
use serde_json::json;
use std::fs;

#[test]
fn corrupt_store_is_reported_without_deleting_user_data() {
    let root = tempfile::tempdir().unwrap();
    let app = root.path().join("app");
    fs::create_dir_all(&app).unwrap();
    fs::write(app.join("settings.json"), "{broken").unwrap();
    assert!(Stores::new(app.clone(), root.path().join("data")).is_err());
    assert_eq!(
        fs::read_to_string(app.join("settings.json")).unwrap(),
        "{broken"
    );
}

#[cfg(unix)]
#[test]
fn atomic_replace_preserves_old_file_when_parent_is_unwritable() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("protected");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("song.show");
    atomic_write(&path, &json!(["song", {"name":"original"}]), false).unwrap();
    let previous = fs::read(&path).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
    let result = atomic_write(&path, &json!(["song", {"name":"changed"}]), false);
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    // This test is intended for unprivileged CI, matching a desktop user.
    if result.is_err() {
        assert_eq!(fs::read(path).unwrap(), previous);
    } else {
        panic!("Run file-permission tests without root privileges");
    }
}

#[test]
fn failed_store_write_reports_the_disk_failure_without_hiding_it_in_a_cache() {
    let root = tempfile::tempdir().unwrap();
    let mut stores = Stores::new(root.path().join("app"), root.path().join("data")).unwrap();
    stores.set("CACHE", json!({"original":true})).unwrap();
    // Replace the destination with a directory, forcing atomic persist to fail.
    let previous = stores.app_data.join("cache.previous.json");
    fs::rename(stores.path("CACHE").unwrap(), &previous).unwrap();
    fs::create_dir(stores.path("CACHE").unwrap()).unwrap();
    assert!(stores.set("CACHE", json!({"changed":true})).is_err());
    assert!(stores.get("CACHE").is_err());
    assert_eq!(fs::read_to_string(previous).unwrap(), "{\"original\":true}");
}

#[test]
fn duplicate_show_ids_get_distinct_persisted_ids() {
    let root = tempfile::tempdir().unwrap();
    let app = root.path().join("app");
    let data = root.path().join("data");
    let mut runtime = Runtime::new(app, data.clone()).unwrap();
    for name in ["First", "Second"] {
        fs::write(
            data.join("Shows").join(format!("{name}.show")),
            serde_json::to_vec(&json!(["same", {"name":name,"slides":{}}])).unwrap(),
        )
        .unwrap();
    }
    let request =
        serde_json::from_value::<Request>(json!({"channel":"MAIN","data":{"channel":"SHOWS"}}))
            .unwrap();
    let events = runtime.dispatch(request, "main").unwrap();
    let shows = events
        .last()
        .unwrap()
        .message
        .data
        .as_ref()
        .unwrap()
        .as_object()
        .unwrap();
    assert_eq!(shows.len(), 2);
    let first: serde_json::Value =
        serde_json::from_slice(&fs::read(data.join("Shows/First.show")).unwrap()).unwrap();
    let second: serde_json::Value =
        serde_json::from_slice(&fs::read(data.join("Shows/Second.show")).unwrap()).unwrap();
    assert_ne!(first[0], second[0]);
    assert!(shows.contains_key(first[0].as_str().unwrap()));
    assert!(shows.contains_key(second[0].as_str().unwrap()));
}
