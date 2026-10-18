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

The untouched upstream unit baseline was run before changes: **179 passed, 2 failed** across 12 files. Both failures were in `src/electron/cloud/syncManager.test.ts`:

1. `multiple machines sync scenarios > Scenario 1: if not found locally, and marked as 'deleted' in cloud: skip` (line 845).
2. `full-file store real edit timestamp (MEDIA/EVENTS/THEMES) > should fall back to file mtime when neither device has ever tracked a real edit time (backward compatibility)` (line 1330).

These baseline failures have not been fixed or represented as migration regressions. The complete original Electron UI test was not run; normal native Node addon installation is still required for that baseline.

An additional differential test against the actual `Conf` dependency exposed array values being replaced by objects and doubled backslashes being collapsed in dotted store keys. Both were fixed, and the new regression test passes with the full local suite. Unusual array-property/length behavior remains outside this test.

## Initial measurement

The first release-mode lyric-extraction microbenchmark measured a median **95.93 ms** for the original TypeScript helper versus **42.01 ms** for Rust (**2.28×**) across 20,000 extractions per sample, with three warmup rounds and nine measured samples. Outputs/checksums matched.

[Raw samples and environment](baselines/lyric-extraction-linux.json) are committed. This is one in-memory helper on one Linux machine. It excludes JSON/IPC, disk IO, startup, memory, rendering, audio/video and networking; it does not demonstrate application performance parity or overall speedup.

## Next work

1. Inspect the new remote CI run and fix any remaining platform-specific failures.
2. Implement `UPDATE_DATA_PATH`/portable data relocation and persistence of the selected root across restart; test positive native folder selection. Continue native menus, shortcuts, close/save/capture lifecycle and profile import.
3. Suppress unchanged disk writes, preserve cloud edit timestamps and finish array-property/locking semantics. Verify automatic-backup saved-state behavior, restore-journal recovery after a crash and backup metadata/time zones. The backup/restore core and tested native workflows above are implemented; the remaining cases are not certified.
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
WEBKIT_WEBDRIVER=/home/rob-admin/.local/share/freeshow-native/usr/bin/WebKitWebDriver XDOTOOL=/home/rob-admin/.local/share/freeshow-native/usr/bin/xdotool XDOTOOL_LIBRARY_PATH=/home/rob-admin/.local/share/freeshow-xdotool-libs xvfb-run -a dbus-run-session -- python3 tests/desktop/smoke.py
```

These machine-specific variables are not required on a normal machine with Tauri's system development packages installed.
