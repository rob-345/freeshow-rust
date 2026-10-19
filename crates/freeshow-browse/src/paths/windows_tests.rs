use super::*;
#[test]
fn raw_parts_borrow_original_path_and_parent_is_parse_dir() {
    let path = String::from("C:/音楽\\folder//.hidden.FLAC\\\\");
    let parts = parse(&path).unwrap();
    assert_eq!(
        parts,
        Parts {
            root: "C:/",
            dir: "C:/音楽\\folder/",
            base: ".hidden.FLAC",
            ext: ".FLAC",
            name: ".hidden"
        }
    );
    assert!(std::ptr::eq(
        parts.base.as_ptr(),
        path[parts.dir.len() + 1..].as_ptr()
    ));
    assert_eq!(parent("foo").unwrap(), "");
    assert_eq!(parent("foo\\bar").unwrap(), "foo");
    assert_eq!(basename("C:\\\\").unwrap(), "");
    assert_eq!(stem("...").unwrap(), "..");
    assert_eq!(extension("..").unwrap(), "");
}
#[test]
fn join_dot_roots_and_trailing_separator_are_lexical() {
    for (left, right, expected) in [
        ("", "", "."),
        ("a", "../", ".\\"),
        ("C:/a//", "..", "C:\\"),
        ("C:/a", "", "C:\\a"),
        ("a/../", "", ".\\"),
        ("foo", "bar/", "foo\\bar\\"),
        ("", "..\\", "..\\"),
        ("C:/", "./", "C:\\"),
        ("a", "../../b", "..\\b"),
    ] {
        assert_eq!(join(left, right).unwrap(), expected);
    }
}
#[test]
fn explicit_capability_errors_and_work_boundaries() {
    for path in [
        "C:",
        "C:foo",
        "/rooted",
        "\\rooted",
        r"\\server\share",
        r"\\?\C:\file",
        r"\\.\pipe\name",
        "name:stream",
    ] {
        assert_eq!(
            parse(path).unwrap_err().code,
            "unsupported_windows_path",
            "{path}"
        );
    }
    assert_eq!(parse("nul\0name").unwrap_err().code, "invalid_request");
    assert_eq!(
        parse(&"x".repeat(MAX_PATH_BYTES + 1)).unwrap_err().code,
        "browse_resource_limit"
    );
    assert_eq!(
        parse(&"x/".repeat(MAX_COMPONENTS)).unwrap_err().code,
        "browse_resource_limit"
    );
    let accepted = format!("C:\\{}", "x".repeat(MAX_PATH_BYTES - 3));
    assert_eq!(basename(&accepted).unwrap().len(), MAX_PATH_BYTES - 3);
    assert_eq!(
        join(&accepted, "x").unwrap_err().code,
        "browse_resource_limit"
    );
    assert_eq!(
        join(&"x".repeat(MAX_PATH_BYTES - 1), "x").unwrap_err().code,
        "browse_resource_limit"
    );
    assert_eq!(
        join("C:\\base", "D:\\child").unwrap_err().code,
        "unsupported_windows_path"
    );
}
