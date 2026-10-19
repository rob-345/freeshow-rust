Native menu worker
==================

The port follows the pinned Electron `menuTemplate.ts` and `setGlobalMenu`, commit
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975` (1.6.7-beta.2). No upstream source,
shared IPC/runtime code, manifests, existing tests, or migration gates changed.

Coordinator wiring
------------------

Declare `mod menu;` in `src-tauri/src/main.rs`. Register once on the builder:

```rust
.on_menu_event(menu::on_menu_event)
```

During setup, after managing runtime state:

```rust
menu::install(app.handle())
    .map_err(|error| std::io::Error::other(error.message))?;
```

Handle `MAIN.LANGUAGE` before forwarding requests to the core runtime:

```rust
"LANGUAGE" => {
    menu::set_language(app, &request.data["data"]["strings"])?;
    return Ok(vec![]);
}
```

This nesting matches the real caller in `src/frontend/utils/language.ts`:
`sendMain(Main.LANGUAGE, { lang: locale, strings: messages })`. Startup applies
fallback English; subsequent LANGUAGE calls replace the native app menu. Native
construction failures return `menu_error`; action failures use `MAIN.ALERT`.

Contracts
---------

- Production Windows removes the application menu; macOS adds the application
  submenu and omits the non-macOS fullscreen entry. Development adds reload and
  developer tools. Every source enabled state, separator, explicit accelerator,
  and checked state is represented. The source uses enabled items, no checkboxes,
  and no explicit custom-command accelerators.
- The macOS about/services/hide/hideOthers/unhide roles use Tauri predefined
  items. The adapter also uses predefined items for cut/copy/paste/selectAll roles
  if present. The pinned source defines those edit commands as renderer actions,
  so they retain `MAIN.MENU` and FreeShow's selection/clipboard behavior.
- All custom action IDs retain the original renderer contract. Both Quit entries
  request `MAIN.CLOSE false`; neither invokes native exit. The source renderer
  enters the same `initializeClosing(false)` path, including confirmation.
- Reload, developer tools and fullscreen operate on the focused WebView. Docs
  opens the original URL through the native opener. Platform role accelerators
  are retained where applicable (macOS Quit Cmd+Q, reload CmdOrCtrl+R,
  developer tools Cmd+Alt+I/Ctrl+Shift+I, non-macOS fullscreen F11).

Verification on Linux, 2026-10-18
-------------------------------

The native entrypoint was temporarily wired as above, checked/built in this
worker's `target`, then restored before committing.

```sh
node --test tests/parity/menu.test.mjs
cargo check --manifest-path src-tauri/Cargo.toml --offline
cargo test --manifest-path src-tauri/Cargo.toml --test menu --offline
npm run rust:frontend:build
cargo build --manifest-path src-tauri/Cargo.toml --features custom-protocol --offline
node scripts/migration/snapshot.mjs --check
npx prettier --config config/formatting/.prettierrc.yaml --check tests/parity/menu.test.mjs
rustfmt --check --edition 2021 src-tauri/src/menu.rs src-tauri/src/menu_spec.rs src-tauri/tests/menu.rs
```

Cargo used `CARGO_BUILD_JOBS=2`, `CARGO_TARGET_DIR=$PWD/target`, and the existing
local native-header PKG_CONFIG_PATH/PKG_CONFIG_SYSROOT_DIR environment.

The two Node differential tests execute the entire original menu module with
mocked module boundaries, plus the actual `setGlobalMenu` function. They compare
210 specifications (31 bundled locales plus four missing/empty/Unicode fixtures,
three platforms, development/production), invoke every original custom callback,
and verify both Quit adaptations against the real renderer closing function.
The probe compiles the real Rust specification in an isolated temporary Cargo
project using the checked-in serde_json version; it never changes app manifests.
Five native Rust integration tests pass.

The real GTK/WebKit/Svelte interaction test passed with:

```sh
GI_TYPELIB_PATH=/home/rob-admin/.local/share/freeshow-native/usr/lib/x86_64-linux-gnu/girepository-1.0 \
WEBKIT_WEBDRIVER=/home/rob-admin/.local/share/freeshow-native/usr/bin/WebKitWebDriver \
xvfb-run -a dbus-run-session -- /usr/bin/python3 tests/desktop/menu.py
```

The receipt is `test-output-rust/native-menu/receipt.json`. GTK/AT-SPI observed
top menus `Fichier 日本語`, `Modifier`, `Vue`, `Aide`. Actual native menu activation
delivered `MAIN.MENU shortcuts`, `MAIN.MENU import_more`, and `MAIN.CLOSE false`.
Quit opened the existing renderer's unsaved-changes popup and left the same live
window open, with no outbound final CLOSE before confirmation. The script first
waits for startup SETTINGS/layout and initial locale application, avoiding a
race between test LANGUAGE and the renderer's own initial LANGUAGE. Its
`native_menu()` and `find(node, label)` helpers remain importable.

Remaining GUI coverage
----------------------

macOS and Windows native GUI behavior was not run on this Linux host. Native
about/services/hide/show-all behavior and role accelerators therefore need their
platform GUI checks. Linux reload/developer-tools/fullscreen actions and keyboard
accelerator activation were compiled but were not exercised in the focused GTK
interaction test. No currently specified role is represented by a fake or no-op
handler; source conditions and callback specifications are compared on all
three platforms.
