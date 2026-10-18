//! Platform-independent application menu contract, ported from the pinned
//! Electron menuTemplate.ts. Custom edit actions deliberately remain renderer
//! actions: the renderer chooses between text editing and FreeShow selections.
use serde_json::{json, Value};

pub const DOCS_URL: &str = "https://freeshow.app/docs";
pub const ID_PREFIX: &str = "freeshow.menu.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Renderer(&'static str),
    Close,
    Docs,
    Reload,
    ToggleDevTools,
    ToggleFullscreen,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub label: Option<String>,
    pub role: Option<&'static str>,
    pub action: Option<Action>,
    pub submenu: Option<Vec<Item>>,
    pub separator: bool,
    pub enabled: bool,
    pub checked: Option<bool>,
    pub accelerator: Option<&'static str>,
}

impl Item {
    fn normal(label: Option<String>, role: Option<&'static str>, action: Option<Action>) -> Self {
        Self {
            label,
            role,
            action,
            submenu: None,
            separator: false,
            enabled: true,
            checked: None,
            // Upstream explicit accelerators are all commented out. Native
            // roles get their platform shortcuts at the Tauri boundary only.
            accelerator: None,
        }
    }

    fn group(label: String, submenu: Vec<Item>) -> Self {
        let mut item = Self::normal(Some(label), None, None);
        item.submenu = Some(submenu);
        item
    }

    fn separator() -> Self {
        let mut item = Self::normal(None, None, None);
        item.separator = true;
        item
    }

    fn role(role: &'static str) -> Self {
        Self::normal(None, Some(role), None)
    }

    #[allow(dead_code)] // Source-driven parity probe; native construction uses typed fields.
    pub fn normalized(&self) -> Value {
        json!({
            "label": self.label,
            "role": self.role,
            "type": if self.separator { "separator" } else { "normal" },
            "enabled": self.enabled,
            "checked": self.checked,
            "accelerator": self.accelerator,
            "action": self.action.as_ref().map(Action::original_effect),
            "submenu": self.submenu.as_ref().map(|items| items.iter().map(Item::normalized).collect::<Vec<_>>()),
        })
    }
}

impl Action {
    pub fn id(&self) -> &'static str {
        match self {
            Self::Renderer(id) => id,
            Self::Close => "app_quit",
            Self::Docs => "docs_url",
            Self::Reload => "reload",
            Self::ToggleDevTools => "toggleDevTools",
            Self::ToggleFullscreen => "togglefullscreen",
        }
    }

    /// The old role's effect is retained in the source contract. Native Quit
    /// takes the safe renderer save handshake, including the macOS app item.
    #[allow(dead_code)] // The parity probe compares the original Electron callback effect.
    pub fn original_effect(&self) -> Value {
        match self {
            Self::Renderer(id) => json!({ "kind": "menu", "id": id }),
            Self::Close | Self::Reload | Self::ToggleDevTools | Self::ToggleFullscreen => {
                Value::Null
            }
            Self::Docs => json!({ "kind": "url", "url": DOCS_URL }),
        }
    }

    pub fn renderer_event(&self) -> Option<Value> {
        match self {
            Self::Renderer("quit") | Self::Close => {
                Some(json!({ "channel": "MAIN", "message": { "channel": "CLOSE", "data": false } }))
            }
            Self::Renderer(id) => {
                Some(json!({ "channel": "MAIN", "message": { "channel": "MENU", "data": id } }))
            }
            _ => None,
        }
    }
}

pub fn action_from_id(id: &str) -> Option<Action> {
    let id = id.strip_prefix(ID_PREFIX)?;
    match id {
        "app_quit" => Some(Action::Close),
        "docs_url" => Some(Action::Docs),
        "reload" => Some(Action::Reload),
        "toggleDevTools" => Some(Action::ToggleDevTools),
        "togglefullscreen" => Some(Action::ToggleFullscreen),
        id => [
            "save",
            "import_more",
            "export_more",
            "quit",
            "undo",
            "redo",
            "history",
            "cut",
            "copy",
            "paste",
            "delete",
            "selectAll",
            "focus_mode",
            "quick_search",
            "shortcuts",
            "quick_start_guide",
            "about",
        ]
        .into_iter()
        .find(|candidate| *candidate == id)
        .map(Action::Renderer),
    }
}

fn label(strings: &Value, section: &str, key: &str, fallback: &str) -> String {
    strings[section][key]
        .as_str()
        .filter(|label| !label.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn command(strings: &Value, section: &str, key: &str, fallback: &str, id: &'static str) -> Item {
    Item::normal(
        Some(label(strings, section, key, fallback)),
        None,
        Some(Action::Renderer(id)),
    )
}

pub fn template(strings: &Value, platform: &str, production: bool, app_name: &str) -> Vec<Item> {
    let mac = platform == "darwin";
    let mut menus = Vec::new();
    if mac {
        menus.push(Item::group(
            app_name.into(),
            vec![
                Item::normal(
                    Some(label(strings, "main", "about", "About")),
                    Some("about"),
                    None,
                ),
                Item::separator(),
                Item::role("services"),
                Item::separator(),
                Item::role("hide"),
                Item::role("hideOthers"),
                Item::role("unhide"),
                Item::separator(),
                Item::normal(
                    Some(label(strings, "main", "quit", "Quit")),
                    Some("quit"),
                    Some(Action::Close),
                ),
            ],
        ));
    }
    menus.push(Item::group(
        label(strings, "titlebar", "file", "File"),
        vec![
            command(strings, "actions", "save", "Save", "save"),
            command(strings, "actions", "import", "Import", "import_more"),
            command(strings, "actions", "export", "Export", "export_more"),
            Item::separator(),
            command(strings, "main", "quit", "Quit", "quit"),
        ],
    ));
    let mut edit = vec![
        command(strings, "actions", "undo", "Undo", "undo"),
        command(strings, "actions", "redo", "Redo", "redo"),
        command(strings, "popup", "history", "History", "history"),
        Item::separator(),
        command(strings, "actions", "cut", "Cut", "cut"),
        command(strings, "actions", "copy", "Copy", "copy"),
        command(strings, "actions", "paste", "Paste", "paste"),
        command(strings, "actions", "delete", "Delete", "delete"),
    ];
    if !mac {
        edit.push(Item::separator());
    }
    edit.push(command(
        strings,
        "actions",
        "selectAll",
        "Select All",
        "selectAll",
    ));
    menus.push(Item::group(
        label(strings, "titlebar", "edit", "Edit"),
        edit,
    ));
    let mut view = Vec::new();
    if !production {
        view.push(Item::normal(None, Some("reload"), Some(Action::Reload)));
        view.push(Item::normal(
            None,
            Some("toggleDevTools"),
            Some(Action::ToggleDevTools),
        ));
        view.push(Item::separator());
    }
    view.push(command(
        strings,
        "actions",
        "focus_mode",
        "Toggle Focus mode",
        "focus_mode",
    ));
    if !mac {
        view.push(Item::normal(
            Some(label(strings, "actions", "fullscreen", "Toggle Fullscreen")),
            Some("togglefullscreen"),
            Some(Action::ToggleFullscreen),
        ));
    }
    menus.push(Item::group(
        label(strings, "titlebar", "view", "View"),
        view,
    ));
    menus.push(Item::group(
        label(strings, "titlebar", "help", "Help"),
        vec![
            command(
                strings,
                "main",
                "quick_search",
                "Quick search",
                "quick_search",
            ),
            command(strings, "popup", "shortcuts", "Shortcuts", "shortcuts"),
            Item::normal(
                Some(label(strings, "main", "docs", "Docs")),
                None,
                Some(Action::Docs),
            ),
            command(
                strings,
                "guide",
                "start",
                "Quick start guide",
                "quick_start_guide",
            ),
            command(strings, "main", "about", "About", "about"),
        ],
    ));
    menus
}

pub fn application_menu(
    strings: &Value,
    platform: &str,
    production: bool,
    app_name: &str,
) -> Option<Vec<Item>> {
    if platform == "win32" && production {
        None
    } else {
        Some(template(strings, platform, production, app_name))
    }
}

#[allow(dead_code)] // Entry point for the independent source-driven parity probe.
pub fn normalized(items: Option<&[Item]>) -> Value {
    items
        .map(|items| Value::Array(items.iter().map(Item::normalized).collect()))
        .unwrap_or(Value::Null)
}
