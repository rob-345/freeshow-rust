use freeshow_core::{
    location,
    protocol::Request,
    runtime::Runtime,
    storage::{self, Stores},
};
use serde_json::json;
use std::fs;

#[test]
fn selecting_a_library_preserves_both_show_folders_and_destination_settings() {
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("app");
    let old = temp.path().join("old");
    let new = temp.path().join("new");
    let mut stores = Stores::new(app.clone(), old.clone()).unwrap();
    stores
        .set(
            "PROJECTS",
            json!({"projects":{"old":{"name":"Old project"}}}),
        )
        .unwrap();
    stores
        .set(
            "TEMPLATES",
            json!({"from-old":{"unknown":[1,true,"日本語"]}}),
        )
        .unwrap();
    stores
        .set("SETTINGS", json!({"local":"stays local"}))
        .unwrap();
    let mut target = Stores::new(app.clone(), new.clone()).unwrap();
    target
        .set(
            "PROJECTS",
            json!({"projects":{"new":{"name":"New project"}},"folders":{},"projectTemplates":{},"future":true}),
        )
        .unwrap();
    fs::write(old.join("Shows/Old.show"), "old show bytes").unwrap();
    fs::write(new.join("Shows/New.show"), "new show bytes").unwrap();
    let before = fs::read(target.path("PROJECTS").unwrap()).unwrap();
    let local_settings = stores.get("SETTINGS").unwrap();
    stores
        .set("SHOWS", json!({"stale":{"name":"Old"}}))
        .unwrap();
    stores
        .set("HISTORY", json!({"undo":[{"library":"old"}]}))
        .unwrap();
    let values = location::select(&mut stores, &new, &old).unwrap();
    assert_eq!(stores.data, fs::canonicalize(&new).unwrap());
    assert_eq!(fs::read(stores.path("PROJECTS").unwrap()).unwrap(), before);
    assert!(values
        .iter()
        .any(|(id, value)| id == "PROJECTS" && value["future"] == true));
    assert_eq!(
        stores.get("TEMPLATES").unwrap()["from-old"]["unknown"],
        json!([1, true, "日本語"])
    );
    assert_eq!(stores.get("SETTINGS").unwrap(), local_settings);
    assert_eq!(
        stores.get("SHOWS").unwrap(),
        storage::defaults("SHOWS").unwrap()
    );
    assert_eq!(
        stores.get("HISTORY").unwrap(),
        storage::defaults("HISTORY").unwrap()
    );
    assert_eq!(
        fs::read_to_string(old.join("Shows/Old.show")).unwrap(),
        "old show bytes"
    );
    assert_eq!(
        fs::read_to_string(new.join("Shows/New.show")).unwrap(),
        "new show bytes"
    );
    assert!(!new.join("Shows/Old.show").exists());
    let reopened = Runtime::open(app.clone(), None).unwrap();
    assert_eq!(reopened.stores.data, fs::canonicalize(new).unwrap());
    let overridden = Runtime::open(app, Some(old.clone())).unwrap();
    assert_eq!(overridden.stores.data, old);
}

#[test]
fn invalid_destination_is_validated_before_any_store_or_config_write() {
    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old");
    let new = temp.path().join("new");
    let mut stores = Stores::new(temp.path().join("app"), old.clone()).unwrap();
    fs::create_dir_all(new.join("Config")).unwrap();
    // Put the invalid store after several missing stores in iteration order.
    fs::write(
        new.join("Config")
            .join(storage::filename("TEMPLATES").unwrap()),
        "{broken",
    )
    .unwrap();
    let config = fs::read(stores.path("config").unwrap()).unwrap();
    assert!(location::select(&mut stores, &new, &old).is_err());
    assert_eq!(stores.data, old);
    assert_eq!(fs::read(stores.path("config").unwrap()).unwrap(), config);
    assert_eq!(fs::read_dir(new.join("Config")).unwrap().count(), 1);
}

#[test]
fn stale_or_relative_requests_cannot_switch_the_library() {
    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old");
    let new = temp.path().join("new");
    fs::create_dir(&new).unwrap();
    let mut stores = Stores::new(temp.path().join("app"), old.clone()).unwrap();
    assert!(location::select(&mut stores, &new, &new).is_err());
    assert!(location::select(&mut stores, std::path::Path::new("relative"), &old).is_err());
    assert_eq!(stores.data, old);
    assert_eq!(fs::read_dir(new).unwrap().count(), 0);
}

#[test]
fn unavailable_saved_library_does_not_silently_create_a_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("app");
    let missing = temp.path().join("removed-volume");
    fs::create_dir(&app).unwrap();
    fs::write(
        app.join("config.json"),
        serde_json::to_vec(&json!({"dataPath":missing})).unwrap(),
    )
    .unwrap();
    assert!(Runtime::open(app, None).is_err());
    assert!(!missing.exists());
}

#[test]
fn ipc_reports_success_and_failure_with_the_active_path_and_correlation_id() {
    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old");
    let new = temp.path().join("new");
    fs::create_dir(&new).unwrap();
    let mut runtime = Runtime::new(temp.path().join("app"), old.clone()).unwrap();
    let request = || {
        serde_json::from_value::<Request>(json!({"channel":"MAIN", "listenerId":"picker-1", "data":{"channel":"UPDATE_DATA_PATH","data":{"oldPath":old,"newPath":new}}})).unwrap()
    };
    let events = runtime.dispatch(request(), "main").unwrap();
    assert!(events
        .iter()
        .any(|event| event.message.channel == "DATA_PATH"));
    assert_eq!(
        events.last().unwrap().listener_id.as_deref(),
        Some("picker-1")
    );
    assert_eq!(
        events.last().unwrap().message.data.as_ref().unwrap()["success"],
        true
    );
    let failed = runtime.dispatch(request(), "main").unwrap();
    assert!(!failed
        .iter()
        .any(|event| event.message.channel == "DATA_PATH"));
    assert_eq!(
        failed.last().unwrap().message.data.as_ref().unwrap()["success"],
        false
    );
    assert_eq!(runtime.stores.data, fs::canonicalize(new).unwrap());
}

#[cfg(unix)]
#[test]
fn a_read_only_profile_prevents_any_destination_file_writes() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old");
    let new = temp.path().join("new");
    fs::create_dir(&new).unwrap();
    let mut stores = Stores::new(temp.path().join("app"), old.clone()).unwrap();
    let config = stores.path("config").unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o400)).unwrap();
    let result = location::select(&mut stores, &new, &old);
    fs::set_permissions(config, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(result.is_err());
    assert_eq!(stores.data, old);
    assert_eq!(fs::read_dir(new.join("Config")).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn managed_symlinks_cannot_redirect_portable_store_writes() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old");
    let new = temp.path().join("new");
    let unrelated = temp.path().join("unrelated");
    fs::create_dir(&new).unwrap();
    fs::create_dir(&unrelated).unwrap();
    symlink(&unrelated, new.join("Config")).unwrap();
    let mut stores = Stores::new(temp.path().join("app"), old.clone()).unwrap();
    assert!(location::select(&mut stores, &new, &old).is_err());
    assert_eq!(fs::read_dir(unrelated).unwrap().count(), 0);
}
