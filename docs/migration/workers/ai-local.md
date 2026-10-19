# Native AI_GET_BIN local inventory foundation

The immutable `MAIN.AI_GET_BIN` handler returns `LocalModelManager.getDownloadedBinFiles()` and ignores its payload. The module captures `path.join(app.getPath("userData"), "bin")` once. Its fixed engine list is exactly `["nemotron"]`; it returns `[{path, name, size}]` only when that engine directory's apparent folder size is positive. Any nonzero file counts, including a hidden file or README. This is not binary validation, setup readiness, model integrity, inference or provider availability.

`freeshow-ai-local` implements that read-only query with one fixed filesystem thread, no idle polling, no child process, network, download, delete, setup or native executable authority. Native setup alone creates the opaque `TrustedUserData`; the crate accepts no renderer command, payload, engine name or custom model path. No authority, result, control or configuration type implements Deserialize.

```rust
use freeshow_ai_local::{Control, InventoryService, Limits, TrustedUserData};

// Only trusted native setup chooses the actual process app-data root.
let inventory = InventoryService::start(
    TrustedUserData::from_host(native_user_data_path)?,
    Limits::default(),
)?;
let read = inventory.try_submit(Control::default())?.finish().await?;
let rows = read.result().map_err(Clone::clone)?; // borrowed, source field order path/name/size
// Host validates its full frozen RuntimeContext, then serializes/publishes rows
// while retaining read and its own whole IPC/publication lease.
// publish_original_main_reply(rows, original_listener_id)?;
drop(read); // actual result/path allocations and result capacity are released
inventory.shutdown().await?; // actual native join plus all retained results
```

The illustration's native path, context and publication functions are coordinator obligations. The crate does not depend on Tauri/AppState, select a window role, construct MAIN events, or add a success ACK. The host must bound ignored input, preserve the original listener and outer MAIN response, and reject obsolete contexts before publication. `result()` is borrow-only in production; no public consuming unguarded-row escape or Clone exists. The private cfg(test) consuming convenience is absent from production. Errors are typed static `code`/`message` strings, with no raw root, entry names or OS error text.

## Filesystem source semantics

The actual original `getFolderSize` first constructs a complete `readdirSync({withFileTypes:true})` result. Directory entries recurse. Every other entry uses `statSync(fullPath).size`, which follows symlinks. Child errors contribute zero; a failed whole enumeration returns zero for that folder. The native query likewise completes bounded enumeration before summing, discarding that entire folder's contribution on ordinary enumeration/type errors. Missing/denied folders and failed leaf metadata are source-compatible zero, not a fabricated backend-availability result. Native cancellation, limits and unsupported representations are explicit errors instead of zero.

Ordinary files, nested/empty folders, zero sizes, Unicode, hidden entries, read-denied folders versus stat-able read-denied files, sparse apparent size and duplicate hardlink counting have real private-filesystem source comparisons. The query uses apparent lengths, not allocated disk blocks or deduplicated inode totals. The total and each metadata size must be at most `2^53-1`; larger/overflowing totals return `ai_local_unsupported`. All accepted additions are exact JavaScript Numbers regardless of enumeration order. No approximate or fractional size is invented.

On Linux, actual source tests certify that an engine-root symlink is followed; a child symlink to a directory contributes that target directory's metadata size and does **not** recurse. File links, dangling links, a self-loop and a user-data root alias are exercised. These links can observe metadata outside the logical user-data tree. That is a bounded read-only source compatibility boundary under native root authority, not a file-content grant, anchored containment guarantee or renderer path capability. Concurrent namespace changes can affect observations; this is not an immutable filesystem snapshot.

On Windows, ordinary file/directory comparisons are committed for actual native CI, while encountered reparse points are deliberately unsupported. A dedicated genuine directory-junction source control asserts the explicit capability gap. Checks observe root/bin/engine/entry leaves, not an OS race-proof ancestry proof. No Windows junction-following parity is claimed. Native execution on Windows/macOS remains pending; successful cross compilation alone is not behavior evidence. Only the actual platform-specific cases are skipped by OS. macOS generic/POSIX suites must execute on macOS CI.

Trusted roots require absolute UTF-8, normalized native spelling (no dot/parent/redundant separators); Windows root prefixes are limited to drive/verbatim-drive forms. The returned path keeps that logical spelling and the source `bin/nemotron` joins. Relative/UNC/device roots, undecodable entry names, and unsafe numeric representations are explicit unsupported boundaries. A genuine POSIX invalid-byte-name fixture demonstrates Node's replacement-spelling/stat-error behavior versus this explicit native rejection. No Unicode or Windows path simulation is used as native evidence.

## Bounds and actual ownership

Hard/default bounds: four complete requests including ready/uncollected results; one fixed thread; 1,024 attempted directories; 16,384 entries; depth 32 (engine root zero); 65,536 traversal-work charges; 32,768 UTF-8 bytes per native path and 8MiB cumulative path text; 65,536 serialized result bytes. Enumeration paths are charged before storing them. The result has at most one row in fixed engine order. Native limits may be reduced for controlled tests, never raised above those bounds.

`try_submit` reserves a whole slot before retaining queued work. There is no async waiting admission, task-per-query, retry loop or per-call worker. Cancellation and the default two-second total job deadline cover queue wait, traversal and observation. Dropping a Ticket cancels only its private child token; entered/queued work owns the same lease until its actual native frame and failed-send result are disposed. Ready replies and borrowed InventoryRead results retain it until actual Drop. Returned rows are not an ownership proof for the coordinator's later IPC transport; that host must retain the Read through publication.

Shutdown closes admission/cancels cooperatively, with at most four try-admitted observer futures and a two-second default observation budget. Timeout/observer Drop preserves the actual handle and outstanding results for retry. Successful shutdown requires both actual join and zero retained result leases. Caught job panics become sticky before reply; actual native teardown panic is detected by join and stays sticky. A named mutex guard spans taking/joining the handle, preventing concurrent callers from observing a temporarily empty handle as completed teardown.

Construction requires no runtime. Already-finished teardown can join without Tokio; pending teardown returns explicit `ai_local_runtime` outside Tokio, retaining ownership. Reply waiting/pending teardown require Tokio with time enabled. OS directory/stat calls cannot be forcibly interrupted; work checks cancellation/deadline around bounded operations and retains its thread until physical return. Join occurs only after `is_finished`; Rust documents a possible brief final native teardown interval. The native host must retain/retry the service through shutdown; last-handle Drop closes admission but is not proof of join. No done notification substitutes for native ownership.

## Source and verification evidence

Oracle pin: `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, Node22.21.1. The oracle executes the actual immutable LocalModelManager module, original getFolderSize function, actual MAIN handler property and actual receiveMain dispatcher in one realm, with genuine Node fs/path and only Electron app.getPath replaced by the private native fixture root. Any lazy model/backend import, createFolder or non-userData path request throws. No algorithm rewrite, inference model or real user profile is opened.

The loader verifies normalized hashes: manager `8f015d524d7f14ac536033530a15407989033556be3ac1c21ca3f0c265b1f80b`; folder function `5d68a709d96083c42805dd8649af0ea30e03ddb377227224db852ab19db2109a`; MAIN property `6ae4d5a589fde80716918f6e64a4a29f304730f5a9ed149715a3514e0d773127`; receiveMain `943411675b781c85f088eab5ddfb4bb9294cfc5a5171e07372115ac956bda91b`. CRLF normalizes only to LF before source hashing.

Thirteen Rust cases exercise real files, limits, exact numeric bounds, four ready results, queued and active caller Drop, cancellation/deadline without late query entry, ready replies respecting cancellation/elapsed deadline, timeout/retry, bounded shutdown observers, a held actual exit after logical done, no-runtime behavior, caught worker panic and genuine native teardown panic. A negative control returning success from logical done deterministically fails the held-exit test; restored implementation passes. Linux Node results: seven passing source suites, one Windows-only junction suite skipped. The sparse fixture's actual POSIX block count is below its apparent eight-MiB size. Strict all-target Linux/Windows-GNU/macOS-ARM Clippy and formatting are focused integration checks, not foreign runtime evidence.

```sh
export PATH=/path/to/node-v22.21.1/bin:$HOME/.cargo/bin:/usr/bin:/bin
export CARGO_TARGET_DIR=/mnt/data/cache/freeshow-cover-native-portability
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo +1.99.1 test --offline --locked --manifest-path crates/freeshow-ai-local/Cargo.toml --all-targets
cargo +1.99.1 build --offline --locked --manifest-path crates/freeshow-ai-local/Cargo.toml --example inventory-parity
FREESHOW_AI_LOCAL_BIN="$CARGO_TARGET_DIR/debug/examples/inventory-parity" node --test tests/parity/ai-local.test.mjs
cargo +1.99.1 clippy --offline --locked --manifest-path crates/freeshow-ai-local/Cargo.toml --all-targets -- -D warnings
# Repeat Clippy with --target x86_64-pc-windows-gnu / aarch64-apple-darwin.
```

The example is a bounded test executable, not a product sidecar. Its root argument is fixture-native authority only; it prints borrowed data before releasing its result and joining. The nested workspace/lock are temporary standalone integration aids; all 35 locked dependency versions already occur in the root lock. Coordinator owns workspace/dependency integration, source command routing, current-context/window/callback handling, process app-data selection and terminal drain. No setup/inference/download/delete/provider or whole AI parity is claimed.
