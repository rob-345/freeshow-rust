# Migration progress

Status: **active, incomplete**. Full feature parity and application performance improvement have not been demonstrated. The complete parity gate intentionally fails.

## Completed first slice (2026-10-18)

- Created `rob-345/freeshow-rust` from ChurchApps/FreeShow and downloaded it to `/home/rob-admin/GitHub/freeshow-rust`; development branch: `rust-migration`.
- Checked the Rust-named public fork; it contained no Rust code and no commits ahead of upstream. Details and limitations are in [PLAN.md](PLAN.md).
- Pinned upstream `2d94defcf98175e4f5c9504e7f4fd1d33c35c975` (`1.6.7-beta.2`) and generated an inventory of 153 MAIN commands, 13 IPC channels and 263 runtime source files, including normalized source hashes.
- Added the Rust workspace, core service crate and NDJSON headless runner. Initial handlers cover show helpers, core storage and ordinary save/load operations. There are 27 core MAIN command handlers; several handlers have unported variants and side effects. This count is not a parity percentage.
- Added a Tauri desktop shell, a tested compatibility bridge, and an independent Rust frontend build retaining the Svelte UI. Basic window actions and OS/media-directory discovery are wired separately in the desktop binary.
- Added atomic file replacement, path validation, duplicate-show-ID repair, deletion ID checks and unknown-field preservation. Corrupt store files are reported and preserved. These intentional failure-handling improvements do not certify all legacy error behavior.
- Added differential tests using extracted original TypeScript functions, filesystem/restart/failure checks, bridge correlation/lifecycle checks, and the parity report/acceptance gate.
- Drove the real Linux Tauri binary through WebKitWebDriver: onboarding, new project, new song, ordinary keyboard input, simulated multiline paste input, Japanese text/emoji, Ctrl+S, and saved-file verification passed. Native clipboard integration remains unverified.
- Fixed an observed WebKitGTK startup problem: the POSIX `C` browser locale caused `Intl.DateTimeFormat` to abort onboarding mount. Invalid locale tags now retain the 24-hour default; valid locale behavior has regression tests.
- Added a CI workflow for core checks on Linux/Windows/macOS and real Linux desktop automation. Remote CI results still need inspection after pushing.

## Verification

- Rust core integration tests: **4 passed**, covering corrupt data, failed writes, atomic replacement and duplicate IDs.
- Node differential/bridge tests: **17 passed**, including **1,024** generated show-helper comparisons, 15 parser inputs, storage/restart and save/rename/delete scenarios.
- Locale regression tests: **2 passed**.
- Core Clippy with warnings denied, Rust formatting, added TypeScript/JS formatting, snapshot drift checks and `git diff --check`: passed locally.
- Rust frontend production bundle and Linux desktop binary with embedded assets: built successfully.
- Real Linux desktop workflow: passed. Screenshots, saved show and receipt are written to `test-output-rust/desktop`.
- Full parity gate: **fails as expected** with unported commands and incomplete feature areas.

The untouched upstream unit baseline was run before changes: **179 passed, 2 failed** across 12 files. Both failures were in `src/electron/cloud/syncManager.test.ts`:

1. `multiple machines sync scenarios > Scenario 1: if not found locally, and marked as 'deleted' in cloud: skip` (line 845).
2. `full-file store real edit timestamp (MEDIA/EVENTS/THEMES) > should fall back to file mtime when neither device has ever tracked a real edit time (backward compatibility)` (line 1330).

These baseline failures have not been fixed or represented as migration regressions. The complete original Electron UI test was not run; normal native Node addon installation is still required for that baseline.

## Initial measurement

The first release-mode lyric-extraction microbenchmark measured a median **95.93 ms** for the original TypeScript helper versus **42.01 ms** for Rust (**2.28×**) across 20,000 extractions per sample, with three warmup rounds and nine measured samples. Outputs/checksums matched.

[Raw samples and environment](baselines/lyric-extraction-linux.json) are committed. This is one in-memory helper on one Linux machine. It excludes JSON/IPC, disk IO, startup, memory, rendering, audio/video and networking; it does not demonstrate application performance parity or overall speedup.

## Next work

1. Inspect the first remote CI runs and fix platform-specific build/test failures.
2. Extend the real desktop test to reopen saved projects/shows; port native dialogs, menus/shortcuts, close/save/recovery and portable data relocation.
3. Complete store semantics (external changes, escaped paths/arrays, legacy profile import), cloud edit timestamps, backups and restore with differential/failure tests.
4. Port output window lifecycle and MAIN/output state routing, then add display/transition/preview tests.
5. Continue through media/audio/capture, protocols/remote servers, hardware/broadcast, cloud/providers, AI and distribution as described in [PLAN.md](PLAN.md).
6. Establish comparable release-build Electron startup/memory/rendering baselines and measure the Rust application before setting numerical acceptance thresholds.

The smoke receipt currently reports unported `MAIN.LANGUAGE`, `MAIN.PROVIDER_STARTUP_LOAD`, `MAIN.REFRESH_SHOWS`, `MAIN.SPOTIFY_GET_STATE`, `MAIN.START`, `OUTPUT`, `REMOTE`, and `STAGE` requests. They are explicit errors; the passing editor smoke does not certify those features.

## Local build environment

Rust 1.99.1 and `tauri-driver` were installed under `/home/rob-admin/.cargo`. This machine has no passwordless sudo. Linux development packages were downloaded and extracted into `/home/rob-admin/.local/share/freeshow-native` without changing system packages. For desktop builds in this workspace:

```sh
export PATH="/home/rob-admin/.cargo/bin:$PATH"
export PKG_CONFIG_PATH="/home/rob-admin/.local/share/freeshow-native/usr/lib/x86_64-linux-gnu/pkgconfig:/home/rob-admin/.local/share/freeshow-native/usr/share/pkgconfig"
export PKG_CONFIG_SYSROOT_DIR="/home/rob-admin/.local/share/freeshow-native"
export CARGO_TARGET_DIR="/home/rob-admin/GitHub/freeshow-rust/target"
cargo build --manifest-path src-tauri/Cargo.toml --features custom-protocol -j 3
WEBKIT_WEBDRIVER=/home/rob-admin/.local/share/freeshow-native/usr/bin/WebKitWebDriver xvfb-run -a dbus-run-session -- python3 tests/desktop/smoke.py
```

These machine-specific variables are not required on a normal machine with Tauri's system development packages installed.
