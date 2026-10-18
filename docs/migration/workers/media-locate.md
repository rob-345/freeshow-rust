# Native media path resolution

Owned slice: `crates/freeshow-files/**` and `tests/parity/media-locate.test.mjs`. Source oracle: FreeShow `1.6.7-beta.2`, commit `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, `src/electron/utils/files.ts` and `src/electron/data/thumbnails.ts`. This crate implements filesystem path resolution; it does not establish media playback, decoding, loading or thumbnail generation.

## Host API

The crate is intentionally standalone with its own `[workspace]`, manifest and lockfile for isolated checks. Coordinator integration may remove its `[workspace]`/local profile and add it to the shared workspace. No shared manifests, native entrypoint, bridge, store implementation or IPC contracts are edited by this slice.

```rust
use freeshow_files::{FilesService, JobControl, Limits, LocateRequest, MediaRoot};

let files = FilesService::start(Limits::default())?;
// Both arguments are captured host values. This constructor does no IO.
let root = MediaRoot::from_host(configured_media_folder.as_deref(), &data_root);
let outcome = files.locate(
    LocateRequest { file_path, folders }, root, JobControl::default()
).await?;
let folder = files.media_folder_path(root_for_folder_request, JobControl::default()).await?;
files.shutdown().await?;
```

`MediaRoot::from_host(configured: Option<&str>, data_root: &str) -> MediaRoot` returns a nonempty configured path verbatim with `create_if_missing=false`. Otherwise it selects native `path.join(data_root, "Media")` with `create_if_missing=true`. Directory creation occurs on a filesystem worker, only after the original path misses for LOCATE, or immediately for GET_MEDIA_FOLDER_PATH. Failed fallback creation produces a diagnostic and continues to return/search the path, matching the original alert plus continued execution.

The coordinator captures the selected library's `RuntimeContext`, configured `config.mediaFolderPath` value and data root without queueing behind backup/archive work or doing filesystem work under the snapshot lock. For a non-string stored config value, use the source's truthiness contract: null/false/zero select fallback, while truthy non-strings fail explicitly rather than being silently converted into paths. Library settings/context are not dependencies of this crate.

`LocateOutcome` contains `reply: Option<Located { path, has_changed }>`, optional `replacement: Replacement { cache_key, path }`, `thumbnail_invalidations`, `diagnostics`, and bounded `work` counters. Return only `reply` through the existing `LOCATE_MEDIA_FILE` renderer contract. Return `FolderOutcome.reply` through `GET_MEDIA_FOLDER_PATH`.

For every outcome, including `reply=None`, submit effects through one trusted internal ordered library-owner job. Validate the captured context under the owner's filesystem barrier before touching stores/cache files or replying. Apply `replacement` by updating only `CACHE_SYNC.replacedPaths[cache_key]`, preserving unrelated entries/fields. Each invalidation supplies the original probe `file_path`, its JS UTF-16 signed-32-bit `hash`, and sizes `[900,500,250,100]`. Resolve `<hash>-<size>.png` only below the host-supplied thumbnail cache directory for that library; the original chooses appData/media-cache with a temp fallback, so this crate assumes neither location. Cache invalidation and replacement belong to the same context-validated application job. The service itself never writes stores or deletes thumbnail files.

`Error` carries `code`, `message`, already-observed `thumbnail_invalidations`, and `diagnostics`. Validate and apply those completed effects too when appropriate; a later resource/deadline failure must not erase the original missing-probe invalidation. Reject stale library contexts before effects or renderer replies. A context change should cancel its outstanding per-request tokens as well. Use a separate `CancelToken` per request; dropping its admitted future cancels that token. Successful/rejected admission does not cancel a caller's token.

## Source behavior retained

- Original path normalization tries `fileURLToPath`, falls back to stripping the exact lowercase `file://` prefix on failure, then tries all-or-nothing `decodeURI`. Reserved URI escapes retain their original hex spelling. Native joins and upath-style basename/dirname remain distinct.
- Existing original path wins before resolving the lazy media root. The hashed media-sync layout wins next, with `hasChanged=true` and no replacement-cache write.
- Search roots are sequential: media root, then supplied folders in order. At each level, the exact same-parent candidate wins, followed by current-directory filename matches, followed by children. Directory paths also satisfy the source access probes; matching directory-entry symlinks can be returned even when broken. Symlink directories are not recursively traversed, while explicitly supplied roots and exact candidates may follow symlinks.
- Depth starts at one and stops above eight. The same-parent shortcut at level eight may reach a file in that child folder, as in the original.
- Filename matching uses NFC and default JS lowercase, including contextual Greek final sigma. Path hashes use JS UTF-16 units, including surrogate pairs, with wrapping signed-32-bit arithmetic. Disabled cached-replacement lookup stays disabled.
- Missing original and hashed-layout probes yield thumbnail invalidation instructions even when search eventually returns null. Only a search result yields a replacement-cache instruction.

The original concurrently searches child folders through `asyncPool(8)` and can overwrite an earlier result when another in-flight child finishes. The new bounded worker scans children sequentially. Differential tests use unique matches and controlled precedence. A separate real-tree test delays only OS callbacks and demonstrates two different original duplicate winners, while asserting the native deterministic selection. Universal nested-duplicate winner equivalence is explicitly not claimed. Unix entry sorting matches libuv's bytewise `strcmp` scandir comparator ([libuv 1.51.0 source](https://github.com/libuv/libuv/blob/v1.51.0/src/unix/fs.c#L532)). Windows uses native enumeration and needs filesystem validation there.

## Actual application Unicode oracle

The untouched official Linux executable reports Electron `37.10.3`, Node `22.21.1`, ICU `74.2`, Unicode `15.1`. Standalone Node `22.21.1` reports Unicode `16.0`; it is not an interchangeable filename oracle. The Rust NFC dependency is pinned to `unicode-normalization=0.1.23` (Unicode 15.1). Lowercase mappings and Cased/Case_Ignorable ranges are generated by evaluating the actual source normalization function in the packaged application's main process, never by copying its algorithm or trusting the host Node tables.

`tools/generate-case-table.mjs` launches the unchanged application under an isolated Xvfb/profile, verifies its version and private data paths, extracts the upstream source function, and records runtime versions, source-function SHA256 and executable SHA256 in `tests/unicode-oracle.json`. Official executable SHA256: `eb3263aad96ce7c35df3db4516bb04c67bdc6c2f30ae8ebc0cb5649783c82a15`. The corpus retains every changed scalar fold, raw lowercase/property data, range-boundary sigma contexts and seeded combining/Hangul contexts. The parity suite compares every Unicode scalar plus the contextual corpus to Rust and explicitly asserts the standalone Node Garay mismatch, preventing accidental regression to Unicode 16.

Standalone pinned Node remains the runner for extracted source filesystem/URI/upath tests with real controlled trees. These tests use filenames whose behavior is shared with the packaged runtime; the exhaustive folding assertions use the packaged-app corpus. Virtual POSIX/Windows lexical tests load the actual upath module with the corresponding native Node path implementation. Linux real filesystem behavior is verified; Windows/macOS actual filesystem and GUI integration remain unverified.

## Resource and shutdown boundaries

Two dedicated native filesystem threads and eight queued jobs are the defaults. Admission uses `try_send`: overload returns `files_busy`, and payload/folder limits are checked before retaining a queue slot. Filesystem calls never run on GTK, the Tokio IO reactor, its general blocking pool, or a library-owner lock. Defaults are 128 supplied roots, 32 KiB per path, 1 MiB input paths, 50,000 entries, 4,096 searched directories, 250,000 checkpoints/operations, 16 MiB directory-name/path work, and a ten-second maximum job duration. Hard caps prevent callers configuring an unbounded pool or workload. Counts cover all roots in one request; no file content is read.

Deadlines include queue wait; cancellation/deadline/shutdown checks surround filesystem operations and occur per entry/recursive step. Directory enumeration streams into a bounded collection before sorting/scanning. Symlink recursion is avoided and depth remains eight. `shutdown()` closes admission, cancels tracked jobs, drains queued replies and awaits all workers; repeated shutdown is valid. OS filesystem calls themselves cannot be interrupted: a stuck network mount can delay a worker and shutdown beyond the cooperative deadline. The fixed pool does not create replacement threads; the host can bound its wait while retaining this explicit limitation.

Invalid typed payloads/NUL paths fail explicitly. Non-Unicode filesystem names return `files_encoding`; JS unpaired-surrogate paths and byte-name replacement behavior are unsupported. Filesystem races can invalidate a returned path immediately, as with the source. Public `DOES_MEDIA_EXIST` is deferred: its truthy `creationTime` check uses floored Node `ctimeMs`, including Windows change time. Substituting modified/birth time would weaken cache invalidation. LOCATE needs existence probes only and retains their invalidations without claiming that timestamp contract.

## Focused verification

From this worktree, without changing shared manifests or node_modules:

```sh
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$PWD/target/files" cargo test --manifest-path crates/freeshow-files/Cargo.toml --locked
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$PWD/target/files" cargo clippy --manifest-path crates/freeshow-files/Cargo.toml --all-targets --locked -- -D warnings
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$PWD/target/files" cargo build --manifest-path crates/freeshow-files/Cargo.toml --locked
node --test tests/parity/media-locate.test.mjs
xvfb-run -a node crates/freeshow-files/tools/generate-case-table.mjs --check
```

`FREESHOW_FILES_PARITY` overrides the binary location. `FREESHOW_ORACLE_NODE` selects standalone Node 22.21.1 if the runner differs. `FREESHOW_ELECTRON_BIN` selects the verified packaged executable. The generator's default location is the locally extracted pinned official release; its profile is temporary and removed after exit.

Native tests cover overload, event-loop responsiveness, queue deadlines/cancellation, dropped-future cancellation, terminal/repeated shutdown, concurrent worker completion, admission payload bounds, real entry budgets and symlink loops, and lazy/default-folder behavior. Source-driven tests cover normalization/invalid escapes, Unicode/UTF-16 hashes, POSIX/Windows lexical path corpora, ordered real searches/depth limits, missing/unreadable paths, disabled cached lookup, symlinks/directories, failed mkdir, source cache deletions versus native instructions, and explicit malformed/resource/deadline/cancellation errors. Host context/effect application and renderer media dispatch are coordinator integration work; this standalone check does not claim those paths are wired.
