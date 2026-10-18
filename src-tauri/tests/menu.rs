#[path = "../src/menu_spec.rs"]
mod spec;

use serde_json::json;
use spec::{Action, Item};

fn leaves(items: &[Item]) -> impl Iterator<Item = &Item> {
    items
        .iter()
        .flat_map(|item| item.submenu.as_deref().unwrap_or_default())
}

#[test]
fn production_windows_has_no_application_menu() {
    assert!(spec::application_menu(&json!({}), "win32", true, "FreeShow").is_none());
    assert!(spec::application_menu(&json!({}), "win32", false, "FreeShow").is_some());
}

#[test]
fn renderer_actions_have_unique_stable_ids_and_round_trip() {
    let items = spec::template(&json!({}), "darwin", false, "FreeShow");
    let mut ids = std::collections::HashSet::new();
    for action in leaves(&items).filter_map(|item| item.action.as_ref()) {
        assert!(ids.insert(action.id()));
        assert_eq!(
            spec::action_from_id(&format!("{}{}", spec::ID_PREFIX, action.id())),
            Some(action.clone())
        );
    }
    assert_eq!(spec::action_from_id("quit"), None);
    assert_eq!(spec::action_from_id("freeshow.menu.future_command"), None);
}

#[test]
fn both_quit_paths_request_unsaved_close_without_exiting() {
    let expected = json!({"channel":"MAIN","message":{"channel":"CLOSE","data":false}});
    assert_eq!(Action::Close.renderer_event(), Some(expected.clone()));
    assert_eq!(Action::Renderer("quit").renderer_event(), Some(expected));
}

#[test]
fn source_menu_is_enabled_and_uses_no_checkboxes_or_custom_shortcuts() {
    for platform in ["linux", "darwin", "win32"] {
        let menu = spec::template(&json!({}), platform, false, "FreeShow");
        for item in menu.iter().chain(leaves(&menu)) {
            assert!(item.enabled);
            assert_eq!(item.checked, None);
            assert_eq!(item.accelerator, None);
        }
        assert!(spec::normalized(Some(&menu)).is_array());
    }
}

#[test]
fn localized_labels_keep_whitespace_unicode_and_empty_fallbacks() {
    let menu = spec::template(
        &json!({"titlebar":{"file":"日本語 & ملف"},"actions":{"save":"","import":" "}}),
        "linux",
        true,
        "FreeShow",
    );
    assert_eq!(menu[0].label.as_deref(), Some("日本語 & ملف"));
    let file = menu[0].submenu.as_ref().unwrap();
    assert_eq!(file[0].label.as_deref(), Some("Save"));
    assert_eq!(file[1].label.as_deref(), Some(" "));
}
