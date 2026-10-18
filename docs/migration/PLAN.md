# FreeShow Rust migration

The goal is to replace Electron with Rust while preserving FreeShow functionality, data compatibility, and user workflows, and to demonstrate performance gains with measurements. This requires a port; a syntax transpiler cannot replace Electron APIs, Chromium-specific behavior, or native Node addons.

## Baseline and existing forks

- Upstream: [ChurchApps/FreeShow](https://github.com/ChurchApps/FreeShow), commit `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, version `1.6.7-beta.2`.
- Fork: [rob-345/freeshow-rust](https://github.com/rob-345/freeshow-rust), branch `rust-migration`.
- On 2026-10-18, GitHub repository searches and the upstream fork list identified [Jesuite/FreeShow-Rust-Fork](https://github.com/Jesuite/FreeShow-Rust-Fork). Its recursive tree contained no `.rs`, `Cargo.toml`, or Tauri config; comparison showed zero commits ahead and 33 behind upstream. The Rust name did not represent an implemented port. `gladsonsam/freeshow-utils` is a Tauri companion, not a FreeShow replacement. This search cannot establish that no private or unindexed port exists.
- GPL-3.0 licensing and upstream attribution are retained.

## Architecture

Preserve the existing Svelte interface first. `src/frontend/rust/bridge.ts` supplies the existing `window.api` contract using Tauri commands/events. `src-tauri` supplies desktop integration; `crates/freeshow-core` supplies testable Rust services independent of a GUI. Retaining the UI does not guarantee identical browser behavior: WebKitGTK, WKWebView, and WebView2 need separate rendering, media, and input verification.

Keep the upstream Electron implementation runnable as an oracle until parity is demonstrated. No Node sidecar substitutes for unported Rust functionality. Unsupported operations return explicit errors. A handler's existence is never counted as proof of parity.

The Rust fork uses the separate application identifier `app.freeshow.rust`; its data defaults to an isolated `Data` directory inside its own application profile. `FREESHOW_RUST_APP_DATA` and `FREESHOW_RUST_DATA_DIR` override those locations for tests. Existing Electron user data is not imported or modified implicitly. Data import, relocation, and recovery need their own verified migration path.

## Development

Install Node >=22.12, the pinned Rust toolchain, and [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/). On Ubuntu, the core desktop packages are `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `librsvg2-dev`, build tools, and OpenSSL development headers. Optional hardware features will require additional libraries.

```sh
npm ci --ignore-scripts
npm run rust:snapshot
npm run rust:test
npm run rust:dev
npm run rust:build
npm run rust:parity:report
npm run rust:parity:gate
npm run rust:bench
```

`--ignore-scripts` supports Rust-only development without building Electron's native Node addons. Running the original Electron application still requires its normal native dependency installation. The full parity gate is expected to fail during migration.

For Linux desktop automation, install `tauri-driver` (`cargo install tauri-driver --locked`), `webkit2gtk-driver`, Xvfb and D-Bus. Then:

```sh
npm run rust:frontend:build
cargo build --manifest-path src-tauri/Cargo.toml --features custom-protocol
xvfb-run -a dbus-run-session -- python3 tests/desktop/smoke.py
```

Set `FREESHOW_DESKTOP_BIN` when the binary is outside `target/debug`, such as `src-tauri/target/debug/freeshow-desktop`. Driver paths can be supplied with `TAURI_DRIVER` and `WEBKIT_WEBDRIVER`. Test profiles are temporary; screenshots and receipts go to `test-output-rust`.

The desktop test uses the POSIX locale to exercise a WebKitGTK startup regression: `navigator.language` can be `C`, which `Intl.DateTimeFormat` rejects. The fork retains the 24-hour default for invalid tags, with regression tests for valid and invalid locales. WebKitWebDriver's text injection drops line breaks and non-BMP characters, so the test checks ordinary keyboard input and separately simulates a multiline paste input event for Unicode lyrics; this does not verify the native clipboard.

## Test strategy

1. **Pinned inventory and fixtures.** `upstream-inventory.json` records 153 MAIN commands, 13 IPC channels, renderer callbacks, and backend/server source files. The snapshot tool uses TypeScript's AST and executes only the constants module to capture defaults. `--check` rejects drift.
2. **Differential unit tests.** The Node harness extracts and transpiles the original upstream function declarations. It compares their outputs with the Rust binary on generated, Unicode, malformed, numeric-ID, and missing-value inputs. The initial corpus supplies 1,024 show-helper comparisons. JSON parsing and repair are compared separately.
3. **Persistence and failure tests.** Compare actual saved JSON and reply events, restart behavior, portable/local store paths, rename and delete rules, unknown-field preservation, corrupt data, failed writes, and duplicate IDs. Rust uses atomic file replacement. The initial SAVE oracle injects filesystem/store boundaries; it does not prove backups, cloud timestamp tracking, or native locking behavior.
4. **Desktop workflows.** Drive the actual application through WebDriver, beginning with onboarding, project/show creation, Quick Lyrics, Unicode, and keyboard save. Expand to editing, undo/redo, layouts, output cues, settings, imports, exports, and restart recovery. Renderer-only mocks supplement these tests but cannot replace them.
5. **Protocol tests.** Capture and replay remote/stage/controller, Socket.IO, REST/WebSocket, OSC/MIDI/timecode, and provider exchanges. Assert payloads, event order, authentication, reconnection, cancellation, and error behavior.
6. **Media and hardware tests.** Use deterministic clips and reference audio; measure frame timing, transitions, seek, sync, and duration. NDI/OMT/Blackmagic and MIDI/audio/display devices require real loopback checks on supported operating systems. Record unavailable hardware as unverified.
7. **Distribution and recovery.** Test Windows, macOS and Linux builds, installers, upgrades, rollback, data integrity, and existing-file compatibility. Replace inherited upstream publishing destinations before release.

`parity-status.json` records the remaining areas. Its `verified` show-helper scope is limited to the recorded functions and corpus. It does not certify the editor or all valid and malformed FreeShow inputs.

## Performance acceptance

Rust does not automatically improve the frontend's rendering speed. Compare release builds on the same machine, data, display configuration, media and workload. Record cold/warm startup, complete process-tree memory and CPU, 100/1,000/10,000-show load/search/save latency, p50/p95/p99 output frame time, dropped frames, audio sync, and sustained streaming. Run repeated samples after warmup and retain raw samples and environment details. Separate GPU acceleration from software-rendered CI.

The initial lyric microbenchmark excludes JSON/IPC, filesystem IO, startup and rendering. It only validates and times one in-memory helper and cannot prove application-level improvement. Establish application baselines before choosing numeric improvement thresholds. A release requires no unexplained functional regressions, no output-frame or audio-sync regression, verified data recovery, and measured gains in the workloads that motivated the port.

## Execution order

1. Establish the fork, immutable baseline, Rust core, bridge, parity harness and desktop smoke.
2. Complete storage/import safety and native shell: dialogs, paths, menus, shortcuts, lifecycle, profile migration, backups and restore.
3. Port output lifecycle/state routing, multi-display behavior, preview and rendering tests.
4. Port media/audio/capture and remaining import/export formats.
5. Port remote/control servers, hardware/broadcast protocols, sync/providers and AI.
6. Expand operating-system/hardware verification, measure/tune application performance, and prepare distribution.

Each slice needs implementation, differential evidence where possible, real integration tests, an updated gap list, and measurements before being called complete.
