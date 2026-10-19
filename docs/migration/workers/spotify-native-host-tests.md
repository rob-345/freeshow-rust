# Native Spotify host integration evidence

This tests the actual `src-tauri/src/spotify.rs` binding from `c0456366`, the real
`LibraryService`/`Runtime` and the imported bounded Spotify process owner. The
only binding changes are `cfg(test)` one-shot gates immediately after admission
and immediately before the final library-context check, plus a registered-caller
count. There is no production backend/configuration, main, IPC, bridge, manifest
or owner-architecture change.

Three Rust entry tests pass. Two portable cases prove rejection before lazy
startup: wrong main-window role/command, missing context or a changed runtime,
generation or data path, listener UTF-8 size beyond 8192, sealed admission, and
absence of an async runtime. Compile-time assertions require the real Event,
NativeLifetime and Receipt to be Send+Sync. These are native module tests, not
evidence of frontend callback delivery or foreign-platform Spotify support.

The Linux entry runs seven serial scenarios in fresh instances of the actual
test executable. The existing source helper owns each private session daemon
and genuine Python/GIO MPRIS fixture. Only each child receives that bus address
through its process environment; no test mutates global Rust environment. The
production `NativeSpotify::new` captures its real fixed `/usr/bin/dbus-send`
backend. Its native Owner is diagnostic equality/lifetime provenance, never a
library-selected bus, executable or account.

| Scenario            | Actual boundary exercised                                                                                                                                                                                                                                                                                                                                                                         |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `lifetime`          | Four real query Receipts remain charged through Event clones and serde encoding. Deserialization cannot recreate the skipped native guard. Actual old-owner join leaves those four publications charged; a replacement rejects admission until the last real guard clone drops, then consumes exactly the released slot. Joined shutdown and repeated shutdown do not dispose still-held results. |
| `callerDrop`        | A real GIO Position method is entered and held. Aborting the accepted host future cancels its registered parent, removes registration and settles the native child with actual wait/PID disappearance. A later query uses the same process owner and no new help probe.                                                                                                                           |
| `contextChanged`    | Real `UPDATE_DATA_PATH` commits a changed full RuntimeContext. Current-context notification preserves the old caller; the changed context cancels it while its accepted Ticket is still held. Releasing the test frame returns cancellation, waits the real client, and permits a new-context reply without recreating the process owner.                                                         |
| `finalContextFence` | A real completed reply and Receipt are held before final tagging. Actual library replacement occurs without notification. The final library snapshot independently rejects the already-built old event and drops its native result. A new-context query succeeds on the same owner.                                                                                                               |
| `heldCommit`        | A real ordered `LibraryService::commit` blocks on its actual `freeshow-library` thread. Spotify completes and retains its real result while that exact ordered job is still active; it does not queue behind or release that owner.                                                                                                                                                               |
| `sealActive`        | Terminal sealing cancels a genuinely held Position call and rejects new admission. Actual cleanup, join and retry complete before a fresh host can start another process owner.                                                                                                                                                                                                                   |
| `missingService`    | The bus has no Spotify name or activation service directories. Actual ServiceUnknown produces the original null reply with absent listener, not an availability placeholder or an invented ACK.                                                                                                                                                                                                   |

For each scenario the supervisor subsequently executes the actual immutable
Spotify helper and original `[Main.SPOTIFY_GET_STATE]` property against the same
private bus. It compares the real native event's restored fields with the
original callback using exact property order and `Object.is`, including own
undefined albumArt. The `lifetime` fixture omits art deliberately. Original
request payloads and Unicode listener are retained; public message keys are
exactly channel/data, with no source success ACK. This proves the native event
and private restoration envelope; actual WebView hydration/public delivery is
separate bridge evidence.

Oracle pin is `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, Node 22.21.1. The existing
helper verifies the whole Spotify module SHA256
`75d1ceab68e46c2a21dfc5a61f4e667f1d7d130e7b34b377938b7a97082918fd`.
The new supervisor independently extracts/hashes the original MAIN handler:
`8d655e635c64c162bbf128b08089ec4f48a0f9a13af8979c4874e5f5f6069944`.
Only CRLF-to-LF source transport normalization is applied. The existing six
parser/private-bus suites and 14 foundation Rust tests remain separate evidence;
this does not replace their malformed printed output or process-panic corpus.

The first host-fixture run caught a test race: a spawned Position PID did not
prove the GIO method had entered. Canceling then could leave the fixture's next
query as its first held call. The final test waits an independent exact
`GET Position` provider-log witness, written by the supervisor only after genuine
method entry, before cancellation. Native counters and PID checks remain
independent of that witness. No source/fixture metadata was changed to fit a
native result.

Three temporary negative controls failed and were restored: removing the actual
Event guard fails four-slot retention; omitting Caller parent cancellation fails
the real caller-Drop assertion; replacing final `contextualize` with a copied
old request context fails the already-built stale-event case. Restored full
checks pass. External logs are under
`/mnt/data/cache/freeshow-rust-build/provider-shows/spotify-host-evidence`.

## Ownership and limits

The production budgets remain fixed: one native owner, four whole jobs/results,
four lifecycle observers, default 2s native/host deadlines, bounded pipe output,
and native-only fixed executable/environment capture. The new gates retain
existing real operations; releasing a test gate is not proof of OS cancellation.
IO drain is verified separately through actual native join, zero active PID,
spawned==reaped, zero final retained results, and actual observed client PID
disappearance. Joined/drained and retained publications are tested separately.

Every scenario retains/aborts/awaits its actual Tokio caller, releases any held
ordered commit and attempts all native shutdowns plus the library write fence
before surfacing an assertion failure. `LibraryService::finish_writes` proves
its existing ordered write fence, not an OS-thread join. The Node helper awaits
each actual fixture/daemon/test-process exit and all original source shells.
The host supervisor reports zero fixture processes only after those waits.
Failure cleanup is not counted as a successful native drain.

Scenarios run serially; no account or real desktop Spotify is touched. Existing
source helper bounds include 512 lines, 2MiB incomplete line and 16KiB stderr per
controlled process. New native receipts are at most 64KiB; supervisor capture is
at most 128KiB per stream, native case observation 15s, ordinary Rust observers 10s,
and a final supervisor watchdog 120s. The last-resort watchdog kills/waits the
supervisor and fails; it is not a claim that an externally broken/uninterruptible
grandchild was joined. Normal tested paths await the genuine owned processes.
Child memory/OS calls are not an enforced RSS or hard syscall-time bound.

Linux prerequisites fail explicitly if absent: pinned Node+TypeScript, dbus-send,
dbus-daemon, Python3 GIO/GLib. Windows/macOS compile checks cover the portable
admission/runtime tests and hooks; no native foreign Spotify backend, account,
physical playback, commands or actual renderer delivery is certified here.

## Focused checks

The normal integrated target is:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --test spotify --locked
```

Local checks use an external headless manifest that points directly at these
unchanged production-module/core/source-crate files, with only serde_json,
Tokio(rt/macros/sync/time), tokio-util(rt), futures-util and tempfile dependencies.
It adds no Tauri/build script/GUI dependencies and does not modify either tracked
workspace manifest or lock. Current paths:

```sh
export CARGO_HOME=/mnt/data/cache/freeshow-rust-build/linux-deb-package/cargo-home
export CARGO_TARGET_DIR=/mnt/data/cache/freeshow-rust-build/provider-shows/target
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export FREESHOW_SPOTIFY_HOST_NODE=/path/to/node-v22.21.1/bin/node
cargo test --offline --locked --manifest-path /mnt/data/cache/freeshow-rust-build/provider-shows/spotify-host-manifest/Cargo.toml --all-targets
cargo clippy --offline --locked --manifest-path /mnt/data/cache/freeshow-rust-build/provider-shows/spotify-host-manifest/Cargo.toml --all-targets -- -D warnings
# Repeat Clippy for x86_64-pc-windows-gnu and aarch64-apple-darwin.
rustfmt --edition 2021 --check src-tauri/src/spotify.rs src-tauri/tests/spotify.rs
```

Final checks: 3 Rust entries, 7 genuine native child scenarios, 7 actual-source
comparisons; warnings-denied all-target Linux/Windows-GNU/macOS-ARM Clippy,
rustfmt, worker-note Prettier and git diff checks. No GUI or performance sample
was run, and no production runtime change is included.
