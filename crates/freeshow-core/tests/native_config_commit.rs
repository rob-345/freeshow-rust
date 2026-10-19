#![cfg(unix)]
use freeshow_core::{protocol::Request, runtime::Runtime, service::LibraryService};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt};

fn request(service: &LibraryService) -> Request {
    serde_json::from_value(json!({"channel":"MAIN","data":{"channel":"TRUSTED_IDENTITY_FIXTURE"},"context":service.context().unwrap(),"listenerId":"preserved-media"})).unwrap()
}

#[tokio::test]
async fn trusted_identity_commit_does_not_reread_config_and_ordinary_commit_still_refreshes_media()
{
    let temp = tempfile::tempdir().unwrap();
    let mut runtime = Runtime::new(temp.path().join("app"), temp.path().join("library")).unwrap();
    runtime
        .stores
        .set_key("config", "mediaFolderPath", json!("original-media-path"))
        .unwrap();
    let path = runtime.stores.path("config").unwrap();
    let service = LibraryService::start(runtime).unwrap();
    let before = fs::read(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        fs::read(&path).is_err(),
        "actual unreadable-config control requires non-root"
    );
    let returned = service
        .commit_without_media_refresh(request(&service), "main", |_| Ok(vec![]))
        .await;
    let cached = service.media_settings(&request(&service));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    returned.unwrap();
    assert_eq!(
        cached.unwrap().configured_media_folder,
        json!("original-media-path"),
        "identity operations must not trigger the implicit generic CONFIG getter"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    service
        .commit(request(&service), "main", |runtime| {
            runtime.stores.set_key(
                "config",
                "mediaFolderPath",
                json!("changed-by-normal-commit"),
            )?;
            Ok(vec![])
        })
        .await
        .unwrap();
    assert_eq!(
        service
            .media_settings(&request(&service))
            .unwrap()
            .configured_media_folder,
        json!("changed-by-normal-commit")
    );
    service.finish_writes().await.unwrap();
}
