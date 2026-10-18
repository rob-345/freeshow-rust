use freeshow_core::{
    protocol::{Request, RuntimeContext},
    runtime::Runtime,
    WireError,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

fn request(command: &str, data: Value, context: Option<RuntimeContext>) -> Request {
    serde_json::from_value(
        json!({"channel":"MAIN","context":context,"data":{"channel":command,"data":data}}),
    )
    .unwrap()
}

fn select(runtime: &mut Runtime, path: &Path) {
    let events = runtime
        .dispatch(
            request(
                "UPDATE_DATA_PATH",
                json!({"oldPath":runtime.stores.data,"newPath":path}),
                Some(runtime.context()),
            ),
            "main",
        )
        .unwrap();
    assert_eq!(
        events.last().unwrap().message.data.as_ref().unwrap()["success"],
        true
    );
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                if entry.file_name() != "Recovery" {
                    visit(root, &entry.path(), result);
                }
            } else {
                result.insert(
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn saves_from_a_previous_library_are_preserved_without_changing_either_library() {
    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("A");
    let new = temp.path().join("B");
    fs::create_dir(&new).unwrap();
    let mut runtime = Runtime::new(temp.path().join("app"), old.clone()).unwrap();
    let context = runtime.context();
    select(&mut runtime, &new);
    let before = files(temp.path());
    for triggers in [json!({}), json!({"backup":true,"isAutoBackup":true})] {
        let payload = json!({"SETTINGS":{"shouldNotWrite":true},"PROJECTS":{"shouldNotWrite":true},"showsCache":{"old-id":{"name":"Never write this"}},"renamedShows":[{"id":"old-id","oldName":"Before","name":"After"}],"customTriggers":triggers});
        let error = runtime
            .dispatch(
                request("SAVE", payload.clone(), Some(context.clone())),
                "main",
            )
            .unwrap_err();
        let error = WireError::from(error);
        assert_eq!(error.code, "stale_library");
        let recovery: Value =
            serde_json::from_slice(&fs::read(error.recovery_path.unwrap()).unwrap()).unwrap();
        assert_eq!(recovery["version"], 1);
        assert_eq!(recovery["request"]["context"], json!(context));
        assert_eq!(recovery["request"]["data"]["data"], payload);
        assert_eq!(files(temp.path()), before);
    }
    assert_eq!(
        fs::read_dir(runtime.stores.app_data.join("Recovery"))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn returning_to_the_same_folder_does_not_revalidate_an_old_save() {
    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("A");
    let new = temp.path().join("B");
    fs::create_dir(&new).unwrap();
    let mut runtime = Runtime::new(temp.path().join("app"), old.clone()).unwrap();
    let mut old_context = runtime.context();
    select(&mut runtime, &new);
    select(&mut runtime, &old);
    assert_eq!(
        fs::canonicalize(&runtime.context().data_path).unwrap(),
        fs::canonicalize(old).unwrap()
    );
    assert_eq!(runtime.context().library_generation, 2);
    // Even identical path spelling must not bypass the generation guard.
    old_context.data_path = runtime.context().data_path;
    let before = files(temp.path());
    let error = runtime
        .dispatch(
            request(
                "SAVE",
                json!({"SETTINGS":{"stale":true}}),
                Some(old_context),
            ),
            "main",
        )
        .unwrap_err();
    assert_eq!(WireError::from(error).code, "stale_library");
    assert_eq!(files(temp.path()), before);
}

#[test]
fn the_previous_process_context_cannot_write_into_a_reopened_profile() {
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("app");
    let data = temp.path().join("data");
    let old_context = Runtime::new(app.clone(), data.clone()).unwrap().context();
    let mut reopened = Runtime::new(app, data).unwrap();
    assert_ne!(old_context.runtime_id, reopened.context().runtime_id);
    let before = files(temp.path());
    let error = reopened
        .dispatch(
            request(
                "SAVE",
                json!({"SETTINGS":{"stale":true}}),
                Some(old_context),
            ),
            "main",
        )
        .unwrap_err();
    assert_eq!(WireError::from(error).code, "stale_library");
    assert_eq!(files(temp.path()), before);
}

#[test]
fn mutating_requests_require_context_and_reject_stale_context_before_io() {
    let temp = tempfile::tempdir().unwrap();
    let mut runtime = Runtime::new(temp.path().join("app"), temp.path().join("data")).unwrap();
    let before = files(temp.path());
    for command in [
        "SET_STORE_VALUE",
        "RESTORE",
        "DELETE_BACKUP",
        "UPDATE_DATA_PATH",
    ] {
        for context in [
            None,
            Some(RuntimeContext {
                library_generation: 99,
                ..runtime.context()
            }),
        ] {
            let error = runtime
                .dispatch(request(command, json!({}), context), "main")
                .unwrap_err();
            let error = WireError::from(error);
            assert_eq!(error.code, "stale_library", "{command}");
            assert!(error.recovery_path.is_none());
            assert_eq!(files(temp.path()), before);
        }
    }
    let error = runtime
        .dispatch(
            request("SAVE", json!({"SETTINGS":{"missing":"context"}}), None),
            "main",
        )
        .unwrap_err();
    assert!(WireError::from(error).recovery_path.unwrap().is_file());
    assert_eq!(files(temp.path()), before);
}

#[test]
fn recovery_write_failure_is_explicit_and_never_applies_the_stale_save() {
    let temp = tempfile::tempdir().unwrap();
    let mut runtime = Runtime::new(temp.path().join("app"), temp.path().join("data")).unwrap();
    fs::write(
        runtime.stores.app_data.join("Recovery"),
        "occupied by a file",
    )
    .unwrap();
    let before = files(temp.path());
    let error = runtime
        .dispatch(
            request("SAVE", json!({"SETTINGS":{"missing":"context"}}), None),
            "main",
        )
        .unwrap_err();
    let error = WireError::from(error);
    assert_eq!(error.code, "stale_library");
    assert!(error.recovery_path.is_none());
    assert!(error.recovery_error.is_some());
    assert_eq!(files(temp.path()), before);
}

#[test]
fn unsuccessful_or_unchanged_library_selection_preserves_the_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let bad = temp.path().join("bad");
    fs::create_dir_all(bad.join("Config")).unwrap();
    fs::write(bad.join("Config/events.json"), "{broken").unwrap();
    let mut runtime = Runtime::new(temp.path().join("app"), data.clone()).unwrap();
    let before = runtime.context();
    let events = runtime
        .dispatch(
            request(
                "UPDATE_DATA_PATH",
                json!({"oldPath":data,"newPath":bad}),
                Some(before.clone()),
            ),
            "main",
        )
        .unwrap();
    assert_eq!(
        events.last().unwrap().message.data.as_ref().unwrap()["success"],
        false
    );
    assert_eq!(runtime.context(), before);
    select(&mut runtime, &data);
    assert_eq!(runtime.context(), before);
}

#[test]
fn stale_library_reads_cannot_repair_or_publish_the_selected_library() {
    let temp = tempfile::tempdir().unwrap();
    let mut runtime = Runtime::new(temp.path().join("app"), temp.path().join("data")).unwrap();
    let before = files(temp.path());
    for command in [
        "SHOW",
        "SHOWS",
        "FULL_SHOWS_LIST",
        "GET_EMPTY_SHOWS",
        "REFRESH_SHOWS",
        "BACKUPS",
        "PROJECTS",
        "STAGE",
        "GET_STORE_VALUE",
    ] {
        let context = RuntimeContext {
            library_generation: 99,
            ..runtime.context()
        };
        let error = runtime
            .dispatch(
                request(
                    command,
                    json!({"file":"PROJECTS","key":"projects"}),
                    Some(context),
                ),
                "main",
            )
            .unwrap_err();
        let error = WireError::from(error);
        assert_eq!(error.code, "stale_library", "{command}");
        assert!(error.recovery_path.is_none());
        assert_eq!(files(temp.path()), before);
    }
}

#[test]
fn retagging_the_outer_request_cannot_revalidate_an_old_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let mut runtime = Runtime::new(temp.path().join("app"), temp.path().join("data")).unwrap();
    let current = runtime.context();
    let snapshot = RuntimeContext {
        library_generation: 99,
        ..current.clone()
    };
    let before = files(temp.path());
    let error = runtime
        .dispatch(
            request(
                "SAVE",
                json!({"runtimeContext":snapshot,"SETTINGS":{"stale":true}}),
                Some(current),
            ),
            "main",
        )
        .unwrap_err();
    let error = WireError::from(error);
    assert_eq!(error.code, "stale_library");
    assert!(error.recovery_path.unwrap().is_file());
    assert_eq!(files(temp.path()), before);
}
