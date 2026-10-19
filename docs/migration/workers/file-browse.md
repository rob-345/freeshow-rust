# Native filesystem browse worker

Owned slice: `crates/freeshow-browse`, `src/frontend/rust/fileStats.ts`, the new
file-browse parity/probe/evidence files. Original Electron, existing bridge/core
and shared manifests remain untouched. Oracle: FreeShow 1.6.7-beta.2, commit
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`.

## Source behavior

The actual `responsesMain.ts` handlers return `readFolderContent`, `getFileInfo`
and `getSimularPaths` results. They reply on the requested MAIN command/listener.
This crate returns reply data; the host owns request/context validation, event
construction and stale-result suppression.

READ_FOLDER follows stat symlinks, skips missing/non-directory roots, and returns
raw root keys plus joined child paths. At depth zero it emits root files and child
folder records containing their immediate file paths, without expanding child
files. Fractional/negative depth comparisons retain original numeric behavior.
Every ordinary folder record has `files` and own `noMedia`; `noMedia` is true only
when capture mode finds no image/video extension and no directory. Audio does not
count as media for this predicate. Failed stats still leave paths in folder.files.
Read-directory permission failures yield empty folder records, like the source.

Capture uses the source ternary precedence exactly. Below depth two the normal
depth-stop conditional is bypassed; if already beyond requested depth, traversal
uses only the first four image/video-extension entries at depth one. Such entries
may themselves be directories, reducing actual thumbnail requests. At depth two
or beyond, exceeded-depth capture records are empty stubs without `noMedia`.
Generate-thumbnails requests apply to root-level image/video files. The original
queues generation and returns a predicted cache path; it does not await success.

GET_SIMILAR reads each original path's parent, deduplicates exact candidate paths
in first-seen order, includes directories and non-media names, excludes selected
paths and uses case-sensitive UTF-16 Levenshtein similarity >= 0.5. A candidate is
also skipped when an already accepted name **includes** that candidate name. This
asymmetric suppression precedes scoring. Scores are stable-sorted descending and
only the first ten are returned. Native rolling-row scoring preserves the source
numeric expression while using O(min(m,n)) memory instead of a complete matrix.
Pure `similarity(left,right)` is available independently of filesystem support.

FILE_INFO returns `{path,stat,extension,folder}` or null. Linux direct statx syscalls follow links,
use actual ctime/birthtime independently and request BASIC_STATS plus BTIME.
All fourteen fields are converted to JavaScript Number semantics. Timestamp
arithmetic preserves `seconds * 1000 + nanoseconds / 1_000_000` operation order;
no millisecond rounding or birth/mtime substitution is used. Device IDs use native
makedev. As libuv does for a successful statx, an unavailable BTIME field remains
zero rather than being replaced with ctime.

## Actual IPC, undefined values and Date semantics

The untouched official Linux package was launched with a private app/data/XDG
profile through Playwright, verified as Electron 37.10.3 / Node 22.21.1 / V8
13.8.258.32-electron.0 / ICU 74.2 / Unicode 15.1. Real `window.api` request/reply
roundtrips covered all three commands and controlled trees. The profile was
removed. The compact committed receipt is
`tests/parity/fixtures/file-browse-ipc.json`; larger local evidence is in the
ignored worker target directory.

Over actual packaged IPC, untouched fs.Stats arrives as a plain object with only
these own numeric properties, in this order: dev, mode, nlink, uid, gid, rdev,
blksize, ino, size, blocks, atimeMs, mtimeMs, ctimeMs, birthtimeMs. Dates and methods
are absent. Node22's Date accessors are inherited/lazy; these source handlers do
not materialize them. No Date hydration or method emulation is appropriate.

Normal folder records arrive with an own `noMedia: undefined` property. JSON
cannot represent undefined. Native emits `noMedia: null` only at these positions;
the host must apply `hydrateFileBrowse("READ_FOLDER", data)` at that command's
bridge delivery boundary. It restores own undefined only for folder records with
an explicit null placeholder. Capture stubs retain a missing key; true values,
unknown fields, Stats and all other commands remain unchanged. This helper must
not be applied as a global JSON-null rewrite.

Implementation evidence: [pinned Electron serializer](https://raw.githubusercontent.com/electron/electron/v37.10.3/shell/common/v8_util.cc),
[pinned Node Stats/time construction](https://raw.githubusercontent.com/nodejs/node/v22.21.1/lib/internal/fs/utils.js),
and [libuv statx projection](https://raw.githubusercontent.com/libuv/libuv/v1.51.0/src/unix/linux.c).
The real IPC evidence, rather than standalone V8 serialization alone, determines
the compatibility contract. Source declaration SHA256 values normalize CRLF to LF
before hashing, avoiding platform checkout line-ending drift.

## API and integration

```rust
let browse = BrowseService::new(BrowseConfig::default())?;
let reply = browse.dispatch_with_cancel(
    "READ_FOLDER", &serde_json::json!({"path": selected, "depth": 0}), cancel,
).await?;
// Host revalidates RuntimeContext, replies under the original command/listener,
// then restores READ_FOLDER noMedia placeholders before frontend delivery.
browse.shutdown().await?;
```

`COMMANDS` exports READ_FOLDER, FILE_INFO, GET_SIMILAR. `dispatch` creates a default
request token. Results are `BrowseReply {data,thumbnail_intents,diagnostics}`;
ordinary defaults/null/empty listings are source-compatible. Unknown commands,
invalid payloads, busy/shutdown/cancellation/resource/worker errors are explicit.

Actual thumbnail requests return `BrowseError {code:"thumbnail_unavailable",
message,thumbnail_intents}` and no successful listing. Each typed intent has
`input`, `size:250`, and `reason: RootMedia | FolderCapture` (camel-case serialized).
No thumbnails are generated, cache paths fabricated or media/cache files written.
Options that produce no generation requests can succeed, including audio-only
capture trees. Coordinator must surface unavailable capability; these intents do
not claim an available generator or authorize stale-context writes.

The standalone manifest/workspace/lock are temporary integration aids. Coordinator
adds the crate to the root workspace/native app, removes its private workspace
and lock, wires context/replies/lifecycle/hydration and updates shared CI. The
parity harness already defaults to root `target/debug/freeshow-browse` (+`.exe`),
with `FREESHOW_BROWSE_BIN` for this worker's local target.

## Resource and cancellation boundaries

Defaults: two independent fixed filesystem threads, bounded admission/channel,
64 roots/selected paths, 10,000 aggregate scanned entries, requested depth <=16,
32KiB paths, 2,048 UTF-16 units per scored name, 16MiB retained path/serialized
output limits, 100,000 operations, 10 million Levenshtein cells, 10 million
substring-search byte upper-bound units and a 10-second monotonic deadline from
admission. Configuration has finite maxima. Similarity charges matrix work before
allocating its bounded rolling row and checks cancellation each row. Original
selection exclusions are hashed once; candidate and accepted ordering remains
stable. Sorting/decoding directory names follows libuv's Linux raw-byte scandir
ordering before Node-style lossy UTF-8 conversion.

No Node/FFmpeg runtime, subprocess, unbounded spawn_blocking or thread per request
is used. All stat/scandir work runs on the fixed workers. A dropped request future
cancels its child token; workers keep permits through completion. Admission
rechecks shutdown after permit acquisition and while holding sender ownership.
Shutdown cancels, closes admission, drains active permits, checks is_finished and
joins workers. Without a Tokio runtime, a needed timer wait returns
`runtime_required` rather than panicking. Default Drop cancels/closes but is not an
awaited terminal drain.

Cancellation is cooperative. OS stat/scandir syscalls and kernel/network mount
stalls cannot be preempted. Sorting, bounded JSON serialization and individual
substring operations may finish between checks. Directory depth is bounded even
when symlinks form cycles; this preserves source path-based traversal rather than
silently dropping all repeated inodes. It is not a hard wall-clock guarantee for
unresponsive filesystems.

## Gaps and verification

Native filesystem commands are explicitly gated to Linux. Windows/macOS fields,
ctime/birthtime behavior, drive/UNC path parsing and directory sort contracts are
unverified. Pure similarity still runs everywhere. If Linux statx is unavailable
(EINVAL/EPERM/ENOSYS/EOPNOTSUPP), the service returns `unsupported_stat_backend`;
libuv's fallback-stat behavior has not been established end to end and is not
silently approximated. A kernel reporting successful statx without BTIME is a
different case and retains its actual zero birthtime.

Original asyncPool completion/Map insertion order can vary between roots and file
stats. Native listing maps use deterministic sequential traversal; controlled
comparisons ignore only outer map key order. Folder.files arrays, path/name/data
fields, source candidate/duplicate ordering, final similarity arrays, and IPC
property presence are asserted. No live-filesystem snapshot consistency is
claimed; controlled fixture directory atimes are stabilized so one implementation's
scan does not alter later comparison results.

```sh
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-browse/target cargo test --manifest-path crates/freeshow-browse/Cargo.toml --all-targets
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-browse/target cargo clippy --manifest-path crates/freeshow-browse/Cargo.toml --all-targets -- -D warnings
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-browse/target cargo build --manifest-path crates/freeshow-browse/Cargo.toml
FREESHOW_BROWSE_BIN=crates/freeshow-browse/target/debug/freeshow-browse /home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node --test tests/parity/file-browse.test.mjs
```

The ordinary Node test glob needs no packaged Electron binary and registers no
missing-package skip. Pure source comparisons cover 256 Unicode/case/empty-string
scores on every platform. Linux real trees cover recursion, noMedia/stubs,
permissions, missing paths, files/devices/FIFOs, negative and fractional timestamps,
symlinks/cycles/invalid-byte filenames, candidate order/top-ten/duplicate behavior,
and all original image/video extensions' generation intents. Rust tests cover
resource/admission/shutdown/no-runtime behavior and source path fixtures.

Actual packaged IPC verification is a separate opt-in command, which fails if the
required executable or display tools are absent:

```sh
FREESHOW_TEST_PACKAGED_BROWSE=1 FREESHOW_ELECTRON_BIN=/absolute/path/to/extracted/opt/FreeShow/freeshow FREESHOW_BROWSE_BIN=crates/freeshow-browse/target/debug/freeshow-browse FREESHOW_BROWSE_RECEIPT=crates/freeshow-browse/target/evidence/packaged-ipc.json /home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node --test tests/parity/file-browse.test.mjs
```

`file-browse-electron.mjs` runs under a fresh Xvfb/dbus session from the opt-in test.
No executable is downloaded and no package installed by these tests. For native
Windows harness invocation, set the worker override to the local `.exe`; native
stat tests assert unsupported status there while pure source checks remain active.
