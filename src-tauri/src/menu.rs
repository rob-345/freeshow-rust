//! Native menu construction and original renderer event delivery.
#[path = "menu_spec.rs"]
pub mod spec;

use freeshow_core::{protocol::Event, WireError};
use serde_json::Value;
use spec::{Action, Item, ID_PREFIX};
use tauri::{
    menu::{
        AboutMetadata, CheckMenuItem, Menu, MenuEvent, MenuItem, MenuItemKind, PredefinedMenuItem,
        Submenu,
    },
    AppHandle, Emitter, Manager,
};
use tauri_plugin_opener::OpenerExt;

fn failed(error: impl std::fmt::Display) -> WireError {
    WireError::new("menu_error", error.to_string())
}

fn role_window(app: &AppHandle) -> Result<tauri::WebviewWindow, WireError> {
    // Electron's native roles operate on the focused BrowserWindow, whereas
    // FreeShow custom commands always address its main renderer.
    app.webview_windows()
        .into_values()
        .find(|window| window.is_focused().unwrap_or(false))
        .or_else(|| app.get_webview_window("main"))
        .ok_or_else(|| failed("No window is available for the native menu role"))
}

fn native_item(app: &AppHandle, item: &Item) -> tauri::Result<MenuItemKind<tauri::Wry>> {
    if item.separator {
        return Ok(MenuItemKind::Predefined(PredefinedMenuItem::separator(
            app,
        )?));
    }
    let escaped_label = item.label.as_ref().map(|text| text.replace('&', "&&"));
    let label = escaped_label.as_deref();
    // Tauri/muda predefined items implement the native selectors themselves.
    // Never use predefined Quit: it exits before FreeShow can save its data.
    let predefined = match item.role {
        Some("about") => Some(PredefinedMenuItem::about(
            app,
            label,
            Some(AboutMetadata {
                name: Some("FreeShow".into()),
                version: Some("1.6.7-beta.2".into()),
                ..Default::default()
            }),
        )?),
        Some("services") => Some(PredefinedMenuItem::services(app, label)?),
        Some("hide") => Some(PredefinedMenuItem::hide(app, label)?),
        Some("hideOthers") => Some(PredefinedMenuItem::hide_others(app, label)?),
        Some("unhide") => Some(PredefinedMenuItem::show_all(app, label)?),
        // These roles are supported if a future upstream specification uses
        // them. Current upstream edit items are renderer commands, not roles.
        Some("cut") => Some(PredefinedMenuItem::cut(app, label)?),
        Some("copy") => Some(PredefinedMenuItem::copy(app, label)?),
        Some("paste") => Some(PredefinedMenuItem::paste(app, label)?),
        Some("selectAll") => Some(PredefinedMenuItem::select_all(app, label)?),
        _ => None,
    };
    if let Some(item) = predefined {
        return Ok(MenuItemKind::Predefined(item));
    }
    let action = item.action.as_ref().ok_or_else(|| {
        tauri::Error::Io(std::io::Error::other(
            "Menu item has no action or supported native role",
        ))
    })?;
    let id = format!("{ID_PREFIX}{}", action.id());
    let (fallback, role_accelerator) = match action {
        Action::Close => ("Quit", Some("CmdOrCtrl+Q")),
        Action::Reload => ("Reload", Some("CmdOrCtrl+R")),
        Action::ToggleDevTools => (
            "Toggle Developer Tools",
            Some(if cfg!(target_os = "macos") {
                "Alt+Cmd+I"
            } else {
                "Ctrl+Shift+I"
            }),
        ),
        Action::ToggleFullscreen => ("Toggle Fullscreen", Some("F11")),
        _ => ("", None),
    };
    // Ampersands in localized labels are literal in Electron unless marked as
    // mnemonics. Muda uses '&' for mnemonics, so preserve the visible label.
    let text = label.unwrap_or(fallback);
    let accelerator = item.accelerator.or(role_accelerator);
    if let Some(checked) = item.checked {
        Ok(MenuItemKind::Check(CheckMenuItem::with_id(
            app,
            id,
            text,
            item.enabled,
            checked,
            accelerator,
        )?))
    } else {
        Ok(MenuItemKind::MenuItem(MenuItem::with_id(
            app,
            id,
            text,
            item.enabled,
            accelerator,
        )?))
    }
}

/// Installs the default English menu, matching Electron's initial setGlobalMenu.
pub fn install(app: &AppHandle) -> Result<(), WireError> {
    set_language(app, &serde_json::json!({}))
}

/// Handle MAIN.LANGUAGE's `data.strings`; completion means the native menu has
/// been replaced. It is independent of library IO and does not change stores.
pub fn set_language(app: &AppHandle, strings: &Value) -> Result<(), WireError> {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let Some(spec) = spec::application_menu(strings, platform, !cfg!(debug_assertions), "FreeShow")
    else {
        app.remove_menu().map_err(failed)?;
        // Also remove any explicit menu left by a previous native setup.
        if let Some(window) = app.get_webview_window("main") {
            window.remove_menu().map_err(failed)?;
        }
        return Ok(());
    };
    let menu = Menu::new(app).map_err(failed)?;
    for group in spec {
        let submenu = Submenu::new(
            app,
            group.label.unwrap_or_default().replace('&', "&&"),
            group.enabled,
        )
        .map_err(failed)?;
        for item in group.submenu.unwrap_or_default() {
            let native = native_item(app, &item).map_err(failed)?;
            submenu.append(&native).map_err(failed)?;
        }
        menu.append(&submenu).map_err(failed)?;
    }
    // Electron uses a global app menu. Tauri also applies it to existing and
    // subsequently-created windows without an explicit per-window menu.
    app.set_menu(menu).map_err(failed)?;
    Ok(())
}

pub fn dispatch(app: &AppHandle, id: &str) -> Result<(), WireError> {
    let Some(action) = spec::action_from_id(id) else {
        return Ok(());
    };
    if let Some(event) = action.renderer_event() {
        let event: Event = serde_json::from_value(event).map_err(failed)?;
        app.emit_to("main", "freeshow://ipc", event)
            .map_err(failed)?;
        return Ok(());
    }
    match action {
        Action::Docs => app
            .opener()
            .open_url(spec::DOCS_URL, None::<&str>)
            .map_err(failed),
        Action::Reload => role_window(app)?.reload().map_err(failed),
        Action::ToggleFullscreen => {
            let window = role_window(app)?;
            window
                .set_fullscreen(!window.is_fullscreen().map_err(failed)?)
                .map_err(failed)
        }
        Action::ToggleDevTools => {
            #[cfg(debug_assertions)]
            {
                let window = role_window(app)?;
                if window.is_devtools_open() {
                    window.close_devtools();
                } else {
                    window.open_devtools();
                }
                Ok(())
            }
            #[cfg(not(debug_assertions))]
            Err(failed("Developer tools are unavailable in production"))
        }
        Action::Renderer(_) | Action::Close => unreachable!("Renderer actions return above"),
    }
}

/// Register once on tauri::Builder. Failures reach the main renderer's existing
/// alert contract rather than silently swallowing a native operation failure.
pub fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    if let Err(error) = dispatch(app, event.id().as_ref()) {
        let _ = app.emit_to(
            "main",
            "freeshow://ipc",
            Event::reply("MAIN", "ALERT", Value::String(error.message), None),
        );
    }
}
