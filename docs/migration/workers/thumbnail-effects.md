# Trusted thumbnail cache effects

Owned slice: new `crates/freeshow-thumbnail-effects` and this document, plus minimal
public native identity/plan-root/acknowledgement additions to the worker-owned
thumbnail preparation crate. Main/IPC, canvas scheduling, asset URLs, CLOSE,
library ownership, runtime contexts and canonical migration status remain with
the coordinator. This extends preparation commit
`1b04cfab6af19e2c0351b942590434d759283c9e`; it does not generate a thumbnail.

## API and two-phase ordering

`apply(trusted_cache_root: &Path, plan: &PreparedThumbnail) -> Result<Receipt>` is
synchronous. Call it only inside the existing trusted ordered LibraryService
owner with its filesystem barrier and validated frozen RuntimeContext. It creates
no runtime, task, worker, IPC command or renderer-accessible path operation.
`apply_with_limits` accepts bounded test/deployment resource limits.

The selected cache directory must already exist as trusted host setup. The owner
must pass exactly the captured root's OS-string value. The plan now preserves that
root privately and rejects public cache_root mutation. Final names come directly
from sealed CacheRelativePath values, preserving original signed-UTF16 hash/size
quirks; the effects engine never guesses a cache name from a source path.

A stale original cache entry must be removed before retained-canvas capture.
Prepare and complete are separate commits:

```rust
let mut pending = thumbnails.prepare(command, &data, &roots, cancel).await?;
// Run on ordered owner, after RuntimeContext validation:
let mut receipt = freeshow_thumbnail_effects::apply(&trusted_root, &pending)?;
if pending.outcome == ThumbnailOutcome::CapturePending && !pending.effects.is_empty() {
    receipt.acknowledge_preparation(&mut pending)?;
}
// Route the real retained-canvas request under a bounded frozen-context lease.
let ready = thumbnails.complete(pending, &id, CaptureCompletion::Png(&bytes)).await?;
// Re-enter ordered owner; validate RuntimeContext before effects and publication.
let receipt = freeshow_thumbnail_effects::apply(&trusted_root, &ready)?;
// Publish source reply only after successful receipt and host context validation.
```

Acknowledgement is single-use and accepts only its own live pending plan, opaque
plan identity, original captured root, private Existing expected guard, sealed
destination and exact original invalidation membership. Failed, foreign,
wrong-root, missing-effect, canceled and duplicate acknowledgement leave state
unchanged. Success clears the already-committed invalidation and advances the
private completion destination guard from Existing to Absent. Without this
transition a later PNG install correctly refuses the stale Existing guard.

Receipt fields are private and have no Deserialize implementation or public
constructor. Read-only getters expose applied effects and bounded cleanup
diagnostics/omitted counts. GeneratedPng without a completed install effect is
rejected; an unavailable/pending canvas request is not installed as success.
A no-effect receipt does not provide pre-capture invalidation authority.

The native owner still owns canvas availability, ids/listeners/timeouts,
source-to-local-URL grants, RuntimeContext checks and accepted-effect lifecycle.
The engine checks plan cancellation but cannot independently validate a window's
context. No receipt authorizes publishing a reply after a stale context.

## Guards, anchors and installation

The Linux implementation walks directories with anchored O_PATH/O_DIRECTORY
handles and O_NOFOLLOW, revalidating directory inode/device/mode chains. Cache
root parent traversal is rejected. Static source/cache/stage/parent symlinks and
special files are rejected; source paths may be resolved from the process cwd,
but all opened file handles must match the captured full identity. Supporting an
original source symlink through final effects remains an explicit gap, even
though thumbnail preparation follows stat symlinks.

Preflight checks plan.ensure_current(), source metadata, every staged PNG's
private-directory membership, expected identity and SHA256, and every existing
or absent cache-target guard before any cache mutation. Source videos are opened
with O_PATH and statx only; they are never read or hashed. PNG data is bounded and
hashed from the actual staged handle. Staged handles/path identities are checked
again around copying.

A private transaction directory with mode `0700` is created under the anchored cache root.
A new PNG file with mode `0600` is created atomically with O_EXCL and copied in 32 KiB chunks,
hashed, length-checked and synced. Existing targets are first moved with
renameat2(RENAME_NOREPLACE) to a private guarded backup. The prepared temp file is
then promoted with no-clobber rename, verified against its pre-existing open
handle, rehashed and checked against settled metadata. Replacements temporarily
leave the final name absent; this is not an atomic replace claim.

File identity includes actual raw Linux ctime and mtime seconds/nanoseconds.
Rename/hardlink operations legitimately change ctime. Ownership therefore starts
from a file opened before promotion/quarantine, requires that same inode plus
stable content metadata across the operation, and only then adopts its settled
ctime. The engine never adopts an arbitrary destination found after promotion
as its own output. The small public trusted_file_identity(&File) helper uses
statx AT_EMPTY_PATH with the same projection as preparation, without fallback,
content reads, runtime or worker creation.

Immediately before commit the engine revalidates source/root/cancellation. It
then removes backups and private temporary entries. Failures during this
post-commit cleanup produce bounded receipt diagnostics and retained paths;
they do not falsely report that committed cache changes were rolled back.
Cancellation arriving after the commit point is a host publication/lifecycle
decision. Host must preserve its ordered admission/context semantics.

## Rollback and external changes

Before the commit point, failures trigger reverse-order best-effort rollback.
A newly promoted output is removed only if its handle and current path still
match the captured settled identity. It is first quarantined with no-clobber
rename and rechecked against its original anchor. A changed or colliding external
file is preserved. Original backups are restored only after identity validation
and no-clobber rename. An external target collision or modified backup remains
untouched and is reported as incomplete rollback with its retained private path.

Cleanup attempts continue across all bounded journal objects even when detail
limits are full. Errors carry rollback_incomplete, at most 16 UTF-8-safe diagnostic
strings totaling 16 KiB, an omitted count and a separately bounded 2 KiB main message.
Receipt diagnostics have the same bounds. Diagnostics do not claim all retained
files were removed. The caller must surface incomplete rollback and leave
retained recoverable data intact.

This is best-effort multi-effect compensation, not batch/crash atomicity.
Unresponsive filesystem operations cannot be preempted. There is no durable
journal or directory-fsync crash-recovery guarantee. A process/OS crash can leave
private backups, a removed stale name or a newly installed PNG. Identity guards inherit filesystem timestamp granularity; metadata-only changes
within the same exposed timestamp can be indistinguishable. Concurrent
external ancestor rename/replacement is detected at checkpoints; anchored handles
avoid writing a substituted symlink tree, but cannot freeze the whole external
filesystem or prevent every same-UID rename/unlink race between syscalls. A moved
original directory can retain private recovery data under its new location.
The engine preserves observed foreign objects rather than deleting them to force
a rollback. Concurrent external deletion may make full restoration impossible.

## Resource and platform boundaries

Defaults: at most 4 effects, 32 MiB aggregate existing-target lengths plus PNG
reads/copies/hashes, 4096 chunks,
16,384 cooperative operations and 10 s work deadline. Paths are at most 32 KiB/64
components; sealed final names are single components at most 128 bytes. Private
transaction-name creation has 8 attempts. Configured hard maxima remain finite.
Preflight hashing, copying and final verification count against byte/chunk
budgets. Existing cache lengths count against bytes even though no backup
contents are copied/read; oversized corrupt cache files are resource-rejected
before mutation. Source video length never contributes content-read work.

Rollback has a finite journal derived from at most 4 effects and ignores the
exhausted/canceled work budget so cancellation cannot prevent cleanup. Its
metadata/rename/unlink operations are bounded by those journal entries. Diagnostic
saturation never stops later cleanup. Kernel stat/open/read/write/sync/rename and
individual bounded hash operations are cooperative boundaries, not hard timeout
or whole-process memory guarantees. The engine never scans or recursively deletes
arbitrary cache trees.

Only Linux is supported. Direct statx/AT_EMPTY_PATH and renameat2 NOREPLACE must
work; unavailable kernel/filesystem primitives return typed errors without
fallback. Windows/macOS select an explicit unsupported branch; no ctime,
birthtime, rename or inode semantics are invented. No canvas, FFmpeg, Electron,
package installation or bundled binary distribution is added by this slice.
Warnings-denied cross-Clippy checks of all targets pass for Windows GNU x86_64
and macOS arm64 using already-installed Rust targets. Their native filesystem
runtime contracts remain unverified and explicitly unsupported.

## Verification and integration

Filesystem tests use the actual ThumbnailService prepare/complete APIs and valid
PNG completions. They check exact installed bytes; original stale-cache
prepare→apply→ack→complete→apply; foreign/duplicate/failed acknowledgements;
ctime-only hardlink and mtime guard changes; all four original invalidations;
preflight of all targets before the first mutation; late collision; source,
staged-file, cache-root and symlink tampering; cancellation during copy, after
backup and after promotion; parent rename isolation; changed external output
with retained original backup/incomplete rollback; same-PNG bytes from an
arbitrary post-persist replacement never being adopted; full four-effect
cancellation compensation; quotas; bounded diagnostics;
oversized sparse cache rejection without content reads, and a 64 MiB source without read permission sparse source proving video metadata-only handling.

```sh
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-thumbnail-effects/target cargo test --manifest-path crates/freeshow-thumbnail-effects/Cargo.toml --all-targets
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-thumbnail-effects/target cargo clippy --manifest-path crates/freeshow-thumbnail-effects/Cargo.toml --all-targets -- -D warnings
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-thumbnail-effects/target cargo clippy --manifest-path crates/freeshow-thumbnail-effects/Cargo.toml --all-targets --target x86_64-pc-windows-gnu -- -D warnings
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-thumbnail-effects/target cargo clippy --manifest-path crates/freeshow-thumbnail-effects/Cargo.toml --all-targets --target aarch64-apple-darwin -- -D warnings
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-thumbnails/target cargo test --manifest-path crates/freeshow-thumbnails/Cargo.toml --all-targets
```

The standalone workspace/lock are temporary. Coordinator removes them, adds the
new crate to root/native manifests and calls the synchronous API from the existing
ordered owner. No native manifest, root lock, bridge, renderer, canonical doc or
IPC/CLOSE integration is changed by this worker. New dependencies are the local
thumbnail crate, serde derive, sha2 and getrandom, plus Linux libc. Tests use
existing tempfile/Tokio/png tooling; no GUI or FFmpeg probe is required here.
