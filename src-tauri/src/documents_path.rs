//! Match packaged Electron on Linux, including absent/disabled XDG Documents.
//! Tauri's dirs resolver returns an error for these valid configurations.
pub fn resolve() -> std::path::PathBuf {
    glib::user_special_dir(glib::UserDirectory::Documents).unwrap_or_else(glib::home_dir)
}
