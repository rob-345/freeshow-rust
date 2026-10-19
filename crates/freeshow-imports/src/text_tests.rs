use super::*;

const TEXT_FORMATS: &[&str] = &[
    "openlp",
    "opensong",
    "mediashout",
    "quelea",
    "songbeamer",
    "easyslides",
    "verseview",
    "videopsalm",
];
fn input(id: &str, paths: &[&Path]) -> Value {
    json!({"id":id,"paths":paths})
}
fn roots(dir: &TempDir) -> ImportRoots {
    ImportRoots {
        imports_root: dir.path().join("final-library-not-created"),
    }
}
async fn reject(service: &ImportService, id: &str, files: &[&Path], dir: &TempDir) -> ImportError {
    match service
        .prepare("IMPORT_FILES", &input(id, files), roots(dir))
        .await
    {
        Ok(_) => panic!("expected an explicit import error"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn all_text_ids_keep_raw_utf8_payload_and_never_create_final_directories() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("敬拜.XML");
    std::fs::write(&file, b"\xef\xbb\xbf<song>\xff</song>").unwrap();
    let service = ImportService::new(ImportConfig::default()).unwrap();
    for id in TEXT_FORMATS {
        let prepared = service
            .prepare("IMPORT_FILES", &input(id, &[&file]), roots(&dir))
            .await
            .unwrap();
        assert_eq!(prepared.events.len(), 1);
        assert_eq!(prepared.events[0].message.channel, "IMPORT2");
        assert_eq!(prepared.events[0].message.data["channel"], *id);
        let value = &prepared.events[0].message.data["data"][0];
        assert_eq!(value["content"], "\u{feff}<song>\u{fffd}</song>");
        assert_eq!(value["extension"], "xml");
        assert_eq!(value["name"], "敬拜");
        assert_eq!(value["path"], json!(file));
        assert!(prepared.plan.directories.is_empty());
        assert!(prepared.plan.installs.is_empty());
        assert!(prepared.plan.staging_directory().is_none());
        assert!(!roots(&dir).imports_root.exists());
    }
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn special_selection_rejects_before_archive_open_or_oversized_binary_read() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("broken.zip");
    std::fs::write(&archive, b"not an archive").unwrap();
    let service = ImportService::new(ImportConfig {
        text_bytes: 4,
        ..ImportConfig::default()
    })
    .unwrap();
    for (id, extension) in [("openlp", "sqlite"), ("mediashout", "mdb")] {
        let binary = dir.path().join(format!("oversized.{extension}"));
        std::fs::write(&binary, [b'X'; 16]).unwrap();
        let error = reject(&service, id, &[&archive, &binary], &dir).await;
        assert_eq!(error.code, "unsupported_format");
        assert!(
            error.prior_events.is_empty(),
            "No fake archive ALERT before source binary selection"
        );
    }
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn chordpro_pro_is_text_but_other_pro_inputs_remain_explicitly_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Chords.PRO");
    std::fs::write(&file, b"{title: Chords}\n[C]words").unwrap();
    let service = ImportService::new(ImportConfig::default()).unwrap();
    let prepared = service
        .prepare("IMPORT_FILES", &input("chordpro", &[&file]), roots(&dir))
        .await
        .unwrap();
    assert_eq!(
        prepared.events[0].message.data["data"][0]["content"],
        "{title: Chords}\n[C]words"
    );
    assert_eq!(
        prepared.events[0].message.data["data"][0]["extension"],
        "pro"
    );
    drop(prepared);
    for id in TEXT_FORMATS {
        assert_eq!(
            reject(&service, id, &[&file], &dir).await.code,
            "unsupported_format"
        );
    }
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn new_text_inputs_retain_existing_limits_cancellation_and_shutdown_contract() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("song.xml");
    std::fs::write(&file, b"12345").unwrap();
    let service = ImportService::new(ImportConfig {
        text_bytes: 4,
        max_prepared: 1,
        ..ImportConfig::default()
    })
    .unwrap();
    for id in TEXT_FORMATS {
        assert_eq!(
            reject(&service, id, &[&file], &dir).await.code,
            "import_resource_limit"
        );
    }
    let held = service
        .prepare("IMPORT_FILES", &input("opensong", &[]), roots(&dir))
        .await
        .unwrap();
    assert_eq!(reject(&service, "openlp", &[], &dir).await.code, "busy");
    service.shutdown().await.unwrap();
    assert_eq!(held.plan.ensure_current().unwrap_err().code, "cancelled");
    assert_eq!(reject(&service, "openlp", &[], &dir).await.code, "shutdown");
}
