# Migration progress

Status: **active, incomplete**. Full feature parity and application performance improvement have not been demonstrated. The complete parity gate intentionally fails.

## Completed first slice (2026-10-18)

- Created `rob-345/freeshow-rust` from ChurchApps/FreeShow and downloaded it to `/home/rob-admin/GitHub/freeshow-rust`; development branch: `rust-migration`.
- Checked the Rust-named public fork; it contained no Rust code and no commits ahead of upstream. Details and limitations are in [PLAN.md](PLAN.md).
- Pinned upstream `2d94defcf98175e4f5c9504e7f4fd1d33c35c975` (`1.6.7-beta.2`) and generated an inventory of 153 MAIN commands, 13 IPC channels and 263 runtime source files, including normalized source hashes.
- Added the Rust workspace, core service crate and NDJSON headless runner. Initial handlers cover show helpers, core storage and ordinary save/load operations. The first slice added 27 core MAIN command handlers; several handlers have unported variants and side effects. This count is not a parity percentage.
- Added a Tauri desktop shell, a tested compatibility bridge, and an independent Rust frontend build retaining the Svelte UI. Basic window actions and OS/media-directory discovery are wired separately in the desktop binary.
- Added atomic file replacement, path validation, duplicate-show-ID repair, deletion ID checks and unknown-field preservation. Corrupt store files are reported and preserved. These intentional failure-handling improvements do not certify all legacy error behavior.
- Added differential tests using extracted original TypeScript functions, filesystem/restart/failure checks, bridge correlation/lifecycle checks, and the parity report/acceptance gate.
- Drove the real Linux Tauri binary through WebKitWebDriver: onboarding, new project, new song, ordinary keyboard input, simulated multiline paste input, Japanese text/emoji, Ctrl+S, and saved-file verification passed. Native clipboard integration remains unverified.
- Fixed an observed WebKitGTK startup problem: the POSIX `C` browser locale caused `Intl.DateTimeFormat` to abort onboarding mount. Invalid locale tags now retain the 24-hour default; valid locale behavior has regression tests.
- Added a CI workflow for core checks on Linux/Windows/macOS and real Linux desktop automation. The first remote core runs passed on Linux and macOS; Windows and the Linux desktop job were still running at the latest inspection. The latest commit must receive its own checks.

## Verification

- Rust core integration tests: **4 passed**, covering corrupt data, failed writes, atomic replacement and duplicate IDs.
- Node differential/bridge tests: **18 passed**, including **1,024** generated show-helper comparisons, 15 parser inputs, storage/restart and save/rename/delete scenarios.
- Locale regression tests: **2 passed**.
- Core Clippy with warnings denied, Rust formatting, added TypeScript/JS formatting, snapshot drift checks and `git diff --check`: passed locally.
- Rust frontend production bundle and Linux desktop binary with embedded assets: built successfully.
- Real Linux desktop workflow: passed. Screenshots, saved show and receipt are written to `test-output-rust/desktop`.
- Full parity gate: **fails as expected** with unported commands and incomplete feature areas.

## Backup, recovery and native-dialog slice (2026-10-18)

The Rust runtime now creates manual and automatic ZIP backups, lists/deletes backups, restores ZIP and legacy JSON/`SHOWS_CONTENT` backups, and emits the existing backup/restore IPC events. Backup retention matches the original pruning algorithm across 72 generated count/age cases. Both backup modes include shows, matching the actual upstream implementation despite its old comment suggesting otherwise. Archive writes stream files and replace the destination atomically.

Restore validates the selected entries before committing, rolls back failed writes, and retains a journal plus original files when rollback itself fails. It rejects unsafe archive paths, corrupt data and oversized entries before changing destination files. Automatic recovery from a process crash during restore is still pending. ZIP restore currently limits a string entry to 50 MiB and an archive to 1 GiB uncompressed; large/atypical compatibility has not been certified.

Native Tauri file/folder pickers, file reads, restore selection, external URLs and system open/reveal operations are wired. Actual Linux automation verifies file selection/read, restore selection and folder cancellation. Positive data-folder selection still needs `UPDATE_DATA_PATH`/relocation support. Operating-system behavior outside the tested Linux workflow remains unverified.

Storage definitions/defaults are now extracted from the original constants and store table. The Rust stores apply defaults once at construction and read external edits/deletions directly, matching the real `Conf` dependency. The SAVE oracle now uses that dependency instead of simplified store mocks.

The tested desktop workflow now reopens its saved profile and renders the saved song after restart, creates a ZIP backup, deliberately changes a show on disk, restores it through the native file picker, and checks the recovered file. It also forces a real save failure with file permissions, verifies the visible error and unsaved state, restores permissions and successfully retries. Rejected saves restore queued rename/delete operations, including later edits/undo and possible intermediate on-disk names.

Deliberate fixes to upstream behavior in this slice:

- `SYNCED_SETTINGS.json` restores to synced settings. The upstream substring branch sends it to local `SETTINGS`.
- Backups made within the same minute receive distinct names, preserving earlier copies.
- Failure events precede the escaped error alert, so the frontend cannot immediately close that alert.
- Failed saves remain unsaved and retain pending operations rather than remaining stuck in a saving state or losing queued renames/deletions.

Latest local validation: **8 Rust tests, 30 Node differential/integration tests and 6 frontend regression tests passed** (44 total), plus 72 retention cases and a CRLF-checkout snapshot regression. The core exposes 31 MAIN command handlers; several variants and native-only commands are counted separately and this is not a parity percentage. The expanded real Linux desktop workflow passed, including native dialogs and save-failure retry. Core Clippy, Rust/added-code formatting, source snapshot checks and the expected failing full-parity gate were checked. The native production frontend and debug desktop binary built successfully.

Remote CI on the previous commit passed the Linux/macOS core checks; one Linux desktop run passed and another encountered a transient WebDriver proxy reset. Windows failed on CRLF differences in generated snapshots. The snapshot reader now normalizes line endings, and the desktop harness re-observes read-only queries after a reset while preserving the same live session. Clicks, key input and mutating scripts are never automatically retried. CI also now has cancellation for superseded runs and Rust build caches. These changes still need remote verification on the new commit.

The next remote run passed Linux/macOS core checks and the Windows snapshot check. Windows then caught a Unix-only test import under warnings-as-errors; the import now has the same platform guard as its test. Linux desktop automation reached the native picker but sent input before GTK realized its entry widgets. The harness now allows the native event loop to initialize before typing and CI installs full-desktop screenshot capture for diagnosis. The complete local desktop workflow passed with these changes. The harness also isolates GTK preferences from the user's profile. Remote verification is pending.

Remote run [39126074008](https://github.com/rob-345/freeshow-rust/actions/runs/39126074008) on `fbff3eae` subsequently passed **all four jobs**: core on Linux, Windows and macOS, plus the real Linux desktop workflow. The library-selection work below is newer and needs its own remote checks.

## Data-library selection and confirmed saves (2026-10-18)

`UPDATE_DATA_PATH` now selects an existing data folder, persists it in the fork's own configuration and reopens it after restart. Explicit environment/CLI overrides still take precedence. An unavailable saved folder stops startup with an error instead of creating an empty replacement library. A recovery UI for disconnected volumes is pending.

The selector follows the upstream distinction between selecting a library and moving its contents: **shows and media stay in their existing folders**. Existing destination portable-store values win; missing portable stores are copied from the current library, with the original top-level defaults applied. The previous library is retained. Source/destination JSON and managed paths are validated before file changes. Portable-store changes, cache/undo invalidation and the configuration update use the same staged commit/rollback mechanism as restore; the configuration update is last. A failed rollback retains its journal and original files. Automatic crash recovery and multiple-instance/concurrent-save isolation remain pending. Failed preparation can leave newly created empty managed subfolders.

The Rust frontend waits for a correlated ordinary-save acknowledgment before switching when edits are unsaved. A failed save prevents the switch. The displayed path and portable-store state change after a successful backend commit. Show/scripture caches, active editing state and undo history are invalidated across libraries, including when both libraries reuse a show ID. The tested normal workflow does not certify saves or edits racing a library change; queued stale payloads still need root/generation guards.

Automatic-backup-only operations no longer clear queued renames/deletions or mark unsaved edits as saved. Their failure handler also leaves those queues intact. Tests execute the actual shared frontend save functions, plus acknowledgment/failure/timeout handling and the folder-change orchestration. These saved-state fixes also apply to the shared Electron frontend; the pinned Electron backend oracle remains unchanged.

The real Linux workflow now selects a folder during onboarding, saves there and reopens it after restart. It then creates an unsaved song, selects another library through Settings and the real native picker, verifies that pending edits reached the previous library, checks that destination settings were preserved, and opens a same-ID song with different lyrics. GTK path completion can consume the Open mnemonic, so the harness locates the actual enabled Open button through AT-SPI before activating it. It waits for the Settings button to be reachable after the editor's closing animation. Each run removes its previous known evidence files to prevent stale receipts/screenshots from being mistaken for the latest result.

Latest local suite: **15 Rust tests, 33 Node tests and 15 frontend regression tests passed** (63 total), including the existing generated show/retention corpora. The core now exposes **32 MAIN handlers**; this remains a count of handlers, not a parity percentage. The expanded native Linux workflow passed. The frontend/native binary build, core/native Clippy, formatting and snapshot checks passed locally. The complete parity gate still fails because major features remain unported.

The untouched upstream unit baseline was run before changes: **179 passed, 2 failed** across 12 files. Both failures were in `src/electron/cloud/syncManager.test.ts`:

1. `multiple machines sync scenarios > Scenario 1: if not found locally, and marked as 'deleted' in cloud: skip` (line 845).
2. `full-file store real edit timestamp (MEDIA/EVENTS/THEMES) > should fall back to file mtime when neither device has ever tracked a real edit time (backward compatibility)` (line 1330).

These baseline failures have not been fixed or represented as migration regressions. The complete original Electron UI test was not run; normal native Node addon installation is still required for that baseline.

An additional differential test against the actual `Conf` dependency exposed array values being replaced by objects and doubled backslashes being collapsed in dotted store keys. Both were fixed, and the new regression test passes with the full local suite. Unusual array-property/length behavior remains outside this test.

## Initial measurement

The first release-mode lyric-extraction microbenchmark measured a median **95.93 ms** for the original TypeScript helper versus **42.01 ms** for Rust (**2.28×**) across 20,000 extractions per sample, with three warmup rounds and nine measured samples. Outputs/checksums matched.

[Raw samples and environment](baselines/lyric-extraction-linux.json) are committed. This is one in-memory helper on one Linux machine. It excludes JSON/IPC, disk IO, startup, memory, rendering, audio/video and networking; it does not demonstrate application performance parity or overall speedup.

## Next work

1. Inspect remote CI for the data-library slice and fix any platform-specific failures.
2. Guard queued saves with their originating library/generation, and finish save/edit concurrency and profile locking. Continue native menus, shortcuts, close/save/capture lifecycle, unavailable-volume recovery and profile import. Data-root selection, portable-store copying and positive Linux folder selection are implemented above; bulk moving shows/media is a separate operation.
3. Suppress unchanged disk writes, preserve cloud edit timestamps and finish array-property semantics. Implement transaction-journal recovery after a crash and verify backup metadata/time zones. Automatic-backup saved-state/queue regressions are now covered; overlapping operations remain uncertified.
4. Port output window lifecycle and MAIN/output state routing, then add display/transition/preview tests.
5. Continue through media/audio/capture, protocols/remote servers, hardware/broadcast, cloud/providers, AI and distribution as described in [PLAN.md](PLAN.md).
6. Establish comparable release-build Electron startup/memory/rendering baselines and measure the Rust application before setting numerical acceptance thresholds.

The latest smoke receipt observed unported `MAIN.LANGUAGE`, `MAIN.SPOTIFY_GET_STATE`, `MAIN.START`, `OUTPUT`, `REMOTE`, and `STAGE` requests, plus the deliberately induced permission error. The source inventory/status manifest tracks additional unported features; the receipt is not an exhaustive list. `REFRESH_SHOWS` is now implemented. The passing workflow does not certify other features.

## Local build environment

Rust 1.99.1 and `tauri-driver` were installed under `/home/rob-admin/.cargo`. This machine has no passwordless sudo. Linux development packages were downloaded and extracted into `/home/rob-admin/.local/share/freeshow-native` without changing system packages. For desktop builds in this workspace:

```sh
export PATH="/home/rob-admin/.cargo/bin:$PATH"
export PKG_CONFIG_PATH="/home/rob-admin/.local/share/freeshow-native/usr/lib/x86_64-linux-gnu/pkgconfig:/home/rob-admin/.local/share/freeshow-native/usr/share/pkgconfig"
export PKG_CONFIG_SYSROOT_DIR="/home/rob-admin/.local/share/freeshow-native"
export CARGO_TARGET_DIR="/home/rob-admin/GitHub/freeshow-rust/target"
cargo build --manifest-path src-tauri/Cargo.toml --features custom-protocol -j 3
GI_TYPELIB_PATH=/home/rob-admin/.local/share/freeshow-native/usr/lib/x86_64-linux-gnu/girepository-1.0 WEBKIT_WEBDRIVER=/home/rob-admin/.local/share/freeshow-native/usr/bin/WebKitWebDriver XDOTOOL=/home/rob-admin/.local/share/freeshow-native/usr/bin/xdotool XDOTOOL_LIBRARY_PATH=/home/rob-admin/.local/share/freeshow-xdotool-libs xvfb-run -a dbus-run-session -- /usr/bin/python3 tests/desktop/smoke.py
```

These machine-specific variables are not required on a normal machine with Tauri's system development packages installed.
