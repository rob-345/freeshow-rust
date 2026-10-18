#[path = "../src/drop_spec.rs"]
mod spec;

#[test]
fn leave_after_accepted_drop_keeps_metadata_completion_but_leave_before_drop_cancels() {
    let mut state = spec::DragSession::new();
    state.enter("library-A");
    let accepted = state.accept_drop().unwrap();
    state.leave(); // Normal GTK leave/cleanup after the OS accepted the drop.
    assert!(state.accepts_completion(accepted.sequence));
    assert!(state.current().is_none());
    assert_eq!(accepted.context, "library-A");
    state.enter("library-A");
    assert!(!state.accepts_completion(accepted.sequence));
    state.leave(); // Cancellation while the drag was still only hovering.
    assert!(state.accept_drop().is_none());
}

#[test]
fn path_validation_preserves_unicode_and_multiple_file_order_without_io() {
    let paths = [
        std::env::temp_dir().join("日本語 space.png"),
        std::env::temp_dir().join("A&B.mp3"),
    ];
    let files = spec::path_descriptors(&paths).unwrap();
    assert_eq!(files[0]["path"], paths[0].to_str().unwrap());
    assert_eq!(files[1]["name"], "A&B.mp3");
    assert!(files[0].get("size").is_none());
}

#[test]
fn rejects_relative_empty_oversized_and_non_unicode_paths() {
    assert!(spec::path_descriptors(&[]).is_err());
    assert!(spec::path_descriptors(&["relative.png".into()]).is_err());
    let path = std::env::temp_dir().join("file.png");
    assert!(spec::path_descriptors(&vec![path; spec::MAX_FILES + 1]).is_err());
    assert!(spec::path_descriptors(&[
        std::env::temp_dir().join("x".repeat(spec::MAX_PATH_BYTES + 1))
    ])
    .is_err());
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        assert!(spec::path_descriptors(&[
            std::env::temp_dir().join(std::ffi::OsString::from_vec(vec![255]))
        ])
        .is_err());
    }
}

#[test]
fn worker_metadata_reports_real_file_size_and_directory_kind() {
    let dir = std::env::temp_dir().join(format!("freeshow-drop-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("日本語.txt");
    std::fs::write(&file, "actual contents").unwrap();
    let result = spec::file_descriptors(&[file.clone(), dir.clone()]).unwrap();
    assert_eq!(result[0]["size"], 15);
    assert_eq!(result[0]["directory"], false);
    assert_eq!(result[1]["directory"], true);
    std::fs::remove_dir_all(dir).unwrap();
    assert!(spec::file_descriptors(&[file]).is_err());
}
