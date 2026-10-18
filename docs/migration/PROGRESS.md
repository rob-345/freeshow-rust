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

The Rust frontend waits for a correlated ordinary-save acknowledgment before switching when edits are unsaved. A failed save prevents the switch. The displayed path and portable-store state change after a successful backend commit. Show/scripture caches, active editing state and undo history are invalidated across libraries, including when both libraries reuse a show ID. The next slice below adds root/generation guards for queued stale payloads; edits made during an in-flight save remain uncertified.

Automatic-backup-only operations no longer clear queued renames/deletions or mark unsaved edits as saved. Their failure handler also leaves those queues intact. Tests execute the actual shared frontend save functions, plus acknowledgment/failure/timeout handling and the folder-change orchestration. These saved-state fixes also apply to the shared Electron frontend; the pinned Electron backend oracle remains unchanged.

The real Linux workflow now selects a folder during onboarding, saves there and reopens it after restart. It then creates an unsaved song, selects another library through Settings and the real native picker, verifies that pending edits reached the previous library, checks that destination settings were preserved, and opens a same-ID song with different lyrics. GTK path completion can consume the Open mnemonic, so the harness locates the actual enabled Open button through AT-SPI before activating it. It waits for the Settings button to be reachable after the editor's closing animation. Each run removes its previous known evidence files to prevent stale receipts/screenshots from being mistaken for the latest result.

Latest local suite: **15 Rust tests, 33 Node tests and 15 frontend regression tests passed** (63 total), including the existing generated show/retention corpora. The core now exposes **32 MAIN handlers**; this remains a count of handlers, not a parity percentage. The expanded native Linux workflow passed. The frontend/native binary build, core/native Clippy, formatting and snapshot checks passed locally. The complete parity gate still fails because major features remain unported.

Remote run `39127552216` passed core checks on all three operating systems. The desktop job reached the save-failure retry, then clicked New Show while a closing alert backdrop still covered it. The harness now observes the hit target before every actual click, without retrying click actions. The full local desktop workflow passed after this change.

Remote run [39128049382](https://github.com/rob-345/freeshow-rust/actions/runs/39128049382) on `752fc7d7` then passed all four jobs, including the complete library-selection workflow.

## Queued-save library provenance (2026-10-18)

The Rust runtime now assigns each process a session ID and each successful library change a generation. Library-dependent reads and writes require the current context. Returning to the same folder does not make earlier requests valid again, and a new process rejects contexts from the previous process. Failed/unchanged selections retain their generation.

The desktop bridge obtains the initial context before exposing `window.api`. SAVE captures that context with its data before the deferred send. The backend checks both the outer request and any captured SAVE context, so retagging only the envelope cannot apply an old snapshot. The bridge sends mutation requests in order and handles each result/failure before starting the next mutation. Window/control channels are not placed in that frontend queue; at this stage most filesystem work still shared the backend mutex. The next slice separates control/context access from library IO; output responsiveness is still unverified.

Desktop core replies carry context metadata. Replies from an earlier generation or process cannot replace current state. Only the DATA_PATH marker within a completed UPDATE_DATA_PATH response advances a client's library context. An unrelated reply or standalone path query cannot prematurely tag old UI data with a new generation. Multi-window propagation and cancellation of pending request listeners remain to be implemented. The headless parity adapter supplies current context for legacy comparisons; direct Rust boundary tests and native automation separately verify rejection behavior.

A rejected stale SAVE is atomically retained under the fork profile's `Recovery/save-<uuid>.json`, including the original request data/context and the active context. Neither library is modified by that save. The UI shows the escaped recovery-file path and does not merge old rename/delete operations into the current library. If writing the recovery file fails, the error reports that failure and the frontend retains the complete snapshot in memory while the window remains open. An assisted recovery/reapplication UI is still pending; old snapshots are not automatically applied over newer work.

Latest local validation: **23 Rust tests, 37 Node tests and 17 frontend tests passed** (77 total). New tests cover stale/missing contexts, A→B→A selection, process restart, invalid retagging, stale reads, explicit recovery-write failure, snapshot capture before deferred IPC, ordered mutations, late replies and destination-state isolation. The real Linux workflow passed **39 recorded steps**, including rejection of an old-library save, inspecting its recovery file, an unchanged destination and the visible recovery location. Core/native Clippy, formatting, snapshot checks and frontend/native builds passed locally. Remote run [39129775612](https://github.com/rob-345/freeshow-rust/actions/runs/39129775612) on `64785128` passed core checks on Linux, Windows and macOS plus the real Linux desktop workflow. The complete parity gate continues to fail.

This protects queued requests across library changes; it does not certify concurrent edits during a save, acknowledgment of newer edits, multiple writers in the same library, profile locks or process-crash recovery. Those cases remain in the status manifest.

## Library worker and independent controls (2026-10-18)

A dedicated library IO thread now owns `Runtime`. The desktop awaits a bounded async queue instead of spawning one blocking task per request and then waiting for a shared runtime mutex. Up to eight jobs wait behind the active job. Dependent file operations remain ordered. A caller dropping its reply after admission does not cancel an accepted write; dropping admission before a queue slot is available does not apply it. Normal IO/validation failures propagate without stopping subsequent work. A closed worker rejects pending and future requests explicitly.

Library context and job status are short snapshots, updated outside filesystem work. Native window controls, OS/path queries, dialog startup and context reads do not join the library queue. Native dialogs and file-read callbacks still run in the desktop blocking pool, and library-dependent callbacks retain the same context validation. Replies retain the context of the completed operation. Worker state contains no sender back to itself, allowing admitted work to drain after all handles are dropped while the process remains alive. Application exit still needs explicit write completion; a detached thread alone cannot guarantee that.

Five core tests cover blocked IO with independent snapshot access, bounded admission and cancelled waiting writes, accepted writes whose listeners disappear, failure propagation, ordered root changes rejecting old saves, new-context publication, stopped workers and draining on handle drop. The real desktop workflow also compresses four generated 16 MiB JSON fixtures and probes context/window/OS services while that exact backup job is active, checking both before and after each probe. It verifies ZIP entry sizes, CRCs and decompression. The test has a 1,500 ms control-response bound; raw timings go to `test-output-rust/desktop/responsiveness.json` and CI artifacts. This is a debug-build responsiveness regression, not an Electron comparison or a large-library/rendering benchmark. The final local run recorded 193 overlapping probes at 1–13 ms (median 1 ms) during a 19.98 s backup. [Raw measurements and environment](baselines/control-during-backup-linux.json) are committed.

Latest local suite: **28 Rust tests, 37 Node tests and 17 frontend tests passed** (82 total). The expanded native workflow has 41 recorded steps. Local formatting, core/native Clippy and source snapshot checks passed. Remote run `39130589564` passed Linux core checks, then macOS caught a test comparing its `/var` temporary-directory alias with the canonical `/private/var` library path. The test now compares canonical paths, matching the runtime contract and Windows extended paths. The runtime behavior is unchanged; the correction needs a fresh CI run. The full parity gate still rejects the build.

The [concurrency plan](PLAN.md#concurrency) now records independent output ownership/queues, async network tasks, bounded CPU workers and supervised media/native subprocesses. Only the library worker and independent native controls/context are implemented here. Library reads still queue behind writes/backups; queue limits count admitted jobs, not payload bytes or callers waiting outside the queue. Output routing, parallel indexing/media work, network cancellation/backpressure, profile locks and supervised processes remain pending.

The untouched upstream unit baseline was run before changes: **179 passed, 2 failed** across 12 files. Both failures were in `src/electron/cloud/syncManager.test.ts`:

1. `multiple machines sync scenarios > Scenario 1: if not found locally, and marked as 'deleted' in cloud: skip` (line 845).
2. `full-file store real edit timestamp (MEDIA/EVENTS/THEMES) > should fall back to file mtime when neither device has ever tracked a real edit time (backward compatibility)` (line 1330).

These baseline failures have not been fixed or represented as migration regressions. The complete original Electron UI test was not run; normal native Node addon installation is still required for that baseline.

An additional differential test against the actual `Conf` dependency exposed array values being replaced by objects and doubled backslashes being collapsed in dotted store keys. Both were fixed, and the new regression test passes with the full local suite. Unusual array-property/length behavior remains outside this test.

## Initial measurement

The first release-mode lyric-extraction microbenchmark measured a median **95.93 ms** for the original TypeScript helper versus **42.01 ms** for Rust (**2.28×**) across 20,000 extractions per sample, with three warmup rounds and nine measured samples. Outputs/checksums matched.

[Raw samples and environment](baselines/lyric-extraction-linux.json) are committed. This is one in-memory helper on one Linux machine. It excludes JSON/IPC, disk IO, startup, memory, rendering, audio/video and networking; it does not demonstrate application performance parity or overall speedup.

## Next work

1. Inspect remote CI for the bounded library-worker slice and fix any platform-specific failures.
2. Port output window lifecycle and MAIN/output state routing, keeping control independent of long filesystem jobs, then add display/transition/preview tests.
3. Finish save/edit acknowledgments and concurrency, profile locking, context propagation across windows and pending-request cancellation. Add assisted recovery of retained snapshots. Continue native menus, shortcuts, close/save/capture lifecycle, unavailable-volume recovery and profile import. Bulk moving shows/media is a separate operation from selecting an existing library.
4. Suppress unchanged disk writes, preserve cloud edit timestamps and finish array-property semantics. Implement transaction-journal recovery after a crash and verify backup metadata/time zones. Automatic-backup saved-state/queue regressions and queued stale-library rejection are now covered; other overlapping operations remain uncertified.
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
