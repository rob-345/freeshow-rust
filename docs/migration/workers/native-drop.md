# Native file-drop worker

This slice adapts native OS file drags to the retained Svelte handlers. It does
not change `DropArea.svelte`, `helpers/drop.ts`, `dropActions.ts`, editor drags,
the pinned Electron preload, shared IPC/runtime code, capabilities or manifests.

## Integration API

In the native entrypoint, declare `mod drop;`, then install once after managing
the library service and creating the main WebviewWindow:

```rust
drop::install(
    &app.get_webview_window("main").ok_or("Main window missing")?,
    app.state::<AppState>().library.clone(),
).map_err(|error| std::io::Error::other(error.message))?;
```

`install` attaches a WindowEvent listener to that main WebviewWindow. Native
Enter captures the current RuntimeContext. `freeshow://drop` events are addressed
only to `main`, with phase, sequence, context, ordered paths and position units.
Enter/Over/Leave do no filesystem IO. Drop metadata is admitted to at most two
blocking jobs; admission failure reports an error. Limits are 256 paths,
32 KiB per UTF-8 path and 2 MiB total path bytes. Metadata completion rechecks
the library and gesture before emission. A platform Leave after an accepted Drop
is suppressed because `accept_drop` removes the active hover; it cannot cancel
that Drop's pending metadata. Leave before Drop cancels the active gesture.

In `bridge.ts`, import `installNativeDrop` from `./nativeDrop` and install only
for the main window before exposing `window.api`:

```ts
if (nativeWindow.label === "main") {
    const nativeDrop = await installNativeDrop(target, {
        getRuntimeContext: bridge.api.getRuntimeContext,
        reportError: (error) => {
            console.error("Native file drop failed", error)
            target.dispatchEvent(
                new CustomEvent("freeshow:runtime-error", {
                    detail: { error, request: { channel: "native-drop" } }
                })
            )
        }
    })
    bridge.api.showFilePath = nativeDrop.showFilePath
}
```

The installer awaits its targeted event subscription. Existing core:event
permissions suffice. It returns `dispose` for subscription/listener cleanup.
Ordinary output/browser Files should retain their path property or return an
empty native path, without the previous pending-port error.

## Compatibility and UI marker

The adapter hit-tests the exact client coordinates and dispatches to the actual
element inside the nearest `.droparea` or `.timeline-track-wrapper`. Original
Svelte handlers decide the action, target index and import format. Local
descriptors contain actual names, paths, directory kind, size and mtime. The
`path` getter and `showFilePath` resolver both reject a changed RuntimeContext.
No name-only path matching is used.

Descriptors are explicitly path-only objects, not empty Files or Blobs. They
use a generic nonempty MIME type for regular files and an empty MIME type for
directories, preserving the source's local-file and directory branches. Byte
methods fail explicitly. Real browser Files remain untouched and keep their
bytes; their empty native path lets the original web-media base64 branch run.

`data-native-drop-active="true"` marks the currently hit drop area. It is removed
on Drop, Leave, Escape, blur, invalid target or stale context. No stylesheet or
Svelte changes are required or included. Hover drag events use no file items:
the pinned DropArea file-over flag never resets on dragleave, so the adapter
avoids latching it. Leave events balance native Enter events. Genuine internal
dragstart/dragend bypass native adaptation and keep their original DOM events,
selection state and editor semantics.

## Coordinate evidence

Installed sources were inspected directly (Wry 0.57.0, Tauri runtime 2.12.2):

- `wry/src/webkitgtk/drag_drop.rs::connect_drag_event` receives GTK WebView
  widget-local x/y from drag-motion/drop and forwards them unchanged. Those are
  logical GTK coordinates, excluding the native menu/title area.
- `wry/src/wkwebview/drag_drop.rs` uses AppKit draggingLocation points and flips
  y using the WebView frame height. Its units are logical points.
- `wry/src/webview2/drag_drop.rs` calls ScreenToClient on the WebView2 child HWND;
  those coordinates are physical.
- `tauri-runtime-wry/src/lib.rs` wraps all these values in PhysicalPosition
  without converting their platform units. The adapter therefore carries an
  explicit units field rather than relying on that type's name.

Frontend conversion divides physical coordinates by devicePixelRatio and logical
coordinates by devicePixelRatio/native scale. Unit tests cover native scales
1/1.25/1.5/2/3 and DPR ratios representing zoom. Actual GTK target hit testing was
verified at native scale 1 and 2, with a 25 logical-pixel GTK menu above content.
The observed native position (143,257 logical) reached the original project area
at CSS target (143,257.5) in both runs. Actual webview zoom, macOS and Windows GUI
behavior still need platform execution.

## Checks and real desktop probe

```sh
node --test tests/parity/native-drop.test.mjs
npx tsc --noEmit --skipLibCheck --lib ES2022,DOM --moduleResolution node --target ES2022 src/frontend/rust/nativeDrop.ts
cargo check --manifest-path src-tauri/Cargo.toml --offline
cargo test --manifest-path src-tauri/Cargo.toml --test drop --offline
npm run rust:frontend:build
cargo build --manifest-path src-tauri/Cargo.toml --features custom-protocol --offline
```

Cargo used this worktree's target directory, two jobs and the existing local
native-header environment. Temporary bridge/main wiring was restored before
commit. The five Node tests execute the original Electron preload, DropArea
functions, ondrop and project dropActions with OS/IPC/store boundaries mocked.
They compare ordered Unicode paths, original media history and PowerPoint/
FreeShow-project import dispatch, cancellation, internal drags, delayed metadata,
coordinate conversion, stale path getters and real browser File bytes. Four
native Rust tests cover bounded validation, real metadata and Leave ordering.

```sh
GDK_SCALE=1 \
GI_TYPELIB_PATH=/home/rob-admin/.local/share/freeshow-native/usr/lib/x86_64-linux-gnu/girepository-1.0 \
WEBKIT_WEBDRIVER=/home/rob-admin/.local/share/freeshow-native/usr/bin/WebKitWebDriver \
XDOTOOL=/home/rob-admin/.local/share/freeshow-native/usr/bin/xdotool \
XDOTOOL_LIBRARY_PATH=/home/rob-admin/.local/share/freeshow-xdotool-libs \
xvfb-run -a -s '-screen 0 3200x2200x24' dbus-run-session -- /usr/bin/python3 tests/desktop/drop.py
```

Repeat with `GDK_SCALE=2`. The coordinator integration reuses the bounded shared script-receipt helper and
addresses the same native driver/session directly after Tauri maps POST /session.
Receipts are
`test-output-rust/native-drop/receipt-1x.json` and `receipt-2x.json`. A separate GTK
EventBox exports real PNG/WAV URI-list paths and xdotool performs the native
mouse drag. Both ordered Unicode files arrive in exactly one DOM drop at the
real project target, and original dropActions creates image/audio references.
Escape creates no second drop. Holding another real GTK drag while the bridge
commits UPDATE_DATA_PATH advances the library generation; releasing it produces
no stale DOM drop into the new library.

## Explicit remaining limits

- Native hover feedback beyond the target marker, file input controls and arbitrary File/Blob byte readers are outside
  this path-only adapter; its descriptors never claim successful empty bytes.
- The real project flow still reports missing `MAIN.LOCATE_MEDIA_FILE`; image
  loading/playback and other media backends are not established by this probe.
  Original PowerPoint/.project IMPORT_FILES dispatch is compared, but those
  import backends remain unrelated work. Directory/project FILE_INFO-dependent
  operations likewise require their existing backend handlers.
- Enter/drop/metadata/path-resolution contexts are guarded. Original Svelte
  handlers that start independent asynchronous operations after a successful
  delivery have not been rewritten; future asynchronous import/folder handlers
  must retain their own runtime contexts across awaits.
- Cross-application web URL/image drags that Wry claims as non-file URI-list
  events, OS modifiers held before the target sees key events, virtual promised
  files, non-UTF-8 Unix paths, and macOS/Windows GUI behavior are unverified or
  explicitly unsupported. Absolute native files/directories are the supported
  OS payload. Normal browser DOM Files and internal editor drags remain intact.
