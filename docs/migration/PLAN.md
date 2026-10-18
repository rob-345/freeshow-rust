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

Save snapshots retain their library context and a Rust-only renderer/module-epoch token. Acknowledgments can mark only the latest snapshot saved, and only if its tracked edit revision is still current. Newer edits stay unsaved; a closing save retries its newer snapshot, and library selection requires its pre-switch save to leave no tracked edits pending. This does not replace final-write draining, multiple-writer isolation or verification of asynchronous/excluded state.

The Rust fork uses the separate application identifier `app.freeshow.rust`; its data defaults to an isolated `Data` directory inside its own application profile. A selected data library is persisted across restart. `FREESHOW_RUST_APP_DATA` and `FREESHOW_RUST_DATA_DIR` override those locations for tests. Existing Electron user data is not imported or modified implicitly. Library selection preserves shows/media in their existing folders and copies missing portable stores. Bulk relocation, legacy profile import, concurrent-save isolation and crash recovery need further verification and implementation.

### Concurrency

Use Rust's ownership and concurrency facilities to keep presentation control responsive during background work. The following boundaries guide the port:

| Work | Ownership and execution | Verification |
| --- | --- | --- |
| Library persistence, selection and restore | One owner on a dedicated IO thread, with a bounded async request queue. Dependent writes remain ordered. | Queue backpressure, write order, failure propagation, stale-context rejection and dropped callers. |
| Show reads during backups | Two dedicated readers with a separate bounded queue. Admission fences wait for earlier mutations/index repairs; a filesystem barrier excludes their ordered phase. Backup compression reads private copies. | Real uncached UI loading and paired disk reads during compression; blocked readers, queue saturation, old-context rejection and shutdown races. |
| Library context and job status | Small immutable snapshots behind a short lock, never held during filesystem IO or compression. | Queries complete while the IO worker is blocked and while a real ZIP backup runs. |
| Output windows and cue routing | Independent window lifecycle worker and ordered cue queue; forwarding does not join the library queue or wait for window creation. | Two live Linux projections, targeted/broadcast cues, transition visibility, event isolation and library cleanup are tested. Cached-lyric delivery/visible-state round trips are measured during a real ZIP backup; physical frame timing, uncached projected content and other workloads remain pending. |
| Remote/control and provider connections | Async sockets and cancellable per-connection tasks, with explicit bounded queues. | Protocol replay, reconnection, slow clients, cancellation and backpressure. Pending implementation. |
| CPU work such as indexing and media analysis | Bounded worker pools. Snapshot input before offloading; validate its library generation before applying results. | Saturation tests, control latency, stale-result rejection and resource limits. Pending implementation. |
| Media tools, capture and native SDKs that can crash | Supervised subprocesses where isolation is useful, with bounded transport and explicit restart/cleanup. | Worker crashes, hangs, cancellation, process-tree memory and hardware loopback. Pending implementation. |

The library worker admits up to eight waiting jobs and one active job. The SHOW pool has eight waiting slots and two active readers. Awaiting admission/dependency fences does not occupy an async executor's blocking thread. These bounds count admitted jobs, not IPC payload bytes or callers waiting outside the queues. SHOW is the only library read routed through this pool; index scans can repair IDs and remain ordered. Backup preparation copies bytes to a private temporary directory under the exclusive filesystem barrier; readers can proceed during compression, while subsequent writes stay queued. Copies require extra temporary disk space and are cleaned on normal completion/failure; external edits during copying and crash-orphan cleanup are not solved. Native dialogs/read callbacks still use the desktop blocking pool. The output lifecycle has its own eight-slot worker queue; cue forwarding uses a short registry snapshot. Per-request size limits, other parallel reads, CPU pools and subprocess supervision need separate implementation and measurement. Closing the application must explicitly await accepted writes; detached worker shutdown alone is not a process-exit guarantee.

Add responsiveness and concurrency tests alongside functionality tests. Measure latency under sustained load before increasing parallelism: dependency ordering, disk contention and output timing matter more than the number of workers.

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

For Linux desktop automation, install `tauri-driver` (`cargo install tauri-driver --locked`), `webkit2gtk-driver`, `xdotool`, Xvfb and D-Bus. Then:

```sh
npm run rust:frontend:build
cargo build --manifest-path src-tauri/Cargo.toml --features custom-protocol
xvfb-run -a dbus-run-session -- /usr/bin/python3 tests/desktop/smoke.py
```

The desktop workflow also selects a library during onboarding, reopens the same profile/library, loads/renders the saved show, reads it through a native file picker, creates an automatic backup, restores it through the native restore picker, cancels a folder picker, and verifies a failed save can be retried after restoring file permissions. A second selection with unsaved work checks that those edits reach the old library, destination settings survive, and a same-ID destination song loads new content without old preview/cue data. It creates a second projection through the real output wizard, checks broadcast and targeted cues against both WebViews and a fresh main-state reply, captures Japanese/emoji output, and verifies both old windows close and both new ones render after library selection. During a real 64 MiB ZIP backup, both warm projections report delivery and visible-text round trips while native controls remain responsive. These probes inspect actual DOM/CSS/RAF progress, not physical scanout. Python GI, the AT-SPI 2 typelib/service and xdotool are required for native controls; the CI workflow lists the Ubuntu packages. Local extracted-package overrides are documented in [PROGRESS.md](PROGRESS.md).

Set `FREESHOW_DESKTOP_BIN` when the binary is outside `target/debug`, such as `src-tauri/target/debug/freeshow-desktop`. Driver paths can be supplied with `TAURI_DRIVER` and `WEBKIT_WEBDRIVER`. `XDOTOOL` and `XDOTOOL_LIBRARY_PATH` support a user-local xdotool installation. Test profiles are temporary; screenshots and receipts go to `test-output-rust`.

Linux visual tests require an emoji font (`fonts-noto-color-emoji` in CI). `FREESHOW_TEST_FONT_DIR` can supply locally extracted fonts; the harness copies them into its isolated XDG profile without changing the user font catalog.

The desktop test uses the POSIX locale to exercise a WebKitGTK startup regression: `navigator.language` can be `C`, which `Intl.DateTimeFormat` rejects. The fork retains the 24-hour default for invalid tags, with regression tests for valid and invalid locales. WebKitWebDriver's text injection drops line breaks and non-BMP characters, so the test checks ordinary keyboard input and separately simulates a multiline paste input event for Unicode lyrics; this does not verify the native clipboard.

## Test strategy

1. **Pinned inventory and fixtures.** `upstream-inventory.json` records 153 MAIN commands, 13 IPC channels, renderer callbacks, and backend/server source files. The snapshot tool uses TypeScript's AST and executes only the constants module to capture defaults. `--check` rejects drift.
2. **Differential unit tests.** The Node harness extracts and transpiles the original upstream function declarations. It compares their outputs with the Rust binary on generated, Unicode, malformed, numeric-ID, and missing-value inputs. The initial corpus supplies 1,024 show-helper comparisons. JSON parsing and repair are compared separately.
3. **Persistence and failure tests.** Compare actual saved JSON and reply events, restart behavior, portable/local store paths, rename and delete rules, unknown-field preservation, corrupt data, failed writes, and duplicate IDs. Rust uses atomic file replacement. The SAVE oracle executes the original save function against its actual `Conf` dependency. Backup tests run the original ZIP writer/reader against Rust archives and compare saved contents. Restore tests cover legacy JSON, mixed stores, malformed data, traversal names, oversized headers, commit failure and failed rollback. Cloud edit timestamps, large/atypical legacy archives and native locking behavior remain unverified.
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
