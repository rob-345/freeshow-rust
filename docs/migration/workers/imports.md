# Native import preparation worker

Owned slice: `crates/freeshow-imports`, `tests/parity/imports.test.mjs`, and
`tests/desktop/imports_probe.js`. The original Electron runtime and Svelte
converters remain unchanged. Source oracle is FreeShow 1.6.7-beta.2, commit
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`; oracle Node is 22.21.1.

## Source boundary and effects

`responsesMain.ts::importFiles` calls `importShow(id, paths)` without awaiting or
returning it. `IMPORT_FILES` has no successful acknowledgement. The original
`sendToMain` publishes `MAIN` events whose message is `{channel,data}`, without a
listener ID. Supported IDs are `freeshow`, `freeshow_project`,
`freeshow_template`, `freeshow_overlay`, `freeshow_theme`, `freeshow_action`,
`freeshow_stage`, `txt`, `chordpro`, and `csv`.

Plain reads return raw UTF-8 replacement-decoded strings, original path, original
basename-minus-final-dot suffix, and lowercase extension. Missing/unreadable files
and directories still produce a record with empty content. Extensionless names
lose their last UTF-16 code unit in the original. No backend JSON conversion,
history update, library store mutation, or save is performed here.

Empty/null/missing selections emit nothing. Project/template/overlay imports send
`ALERT: "popup.importing"` first, classify ZIPs by the original four-byte signature,
read plain selections first, and process archives afterward. Projects suppress
`IMPORT2` for empty results; templates/overlays publish empty arrays. All three
originally create their final Imports subdirectory, including plain-file imports;
the native plan records this directory effect for the host instead.

For other IDs, selecting any `.zip` processes only ZIP selections and ignores
plain selections. A malformed/missing ZIP can emit the exact original
`Failed to open zip file: <path>` alert and contribute no records. Valid generic
ZIPs transport supported text/native entries in archive order, preserving nested
names and lowercase extensions. Hard resource/path/unsupported/encryption/CRC
failures return an error with prior alerts and no successful `IMPORT2`.

FreeShow bundles find **root** `data.json`, preserve its raw formatting and unknown
fields, and remap `files[]` through the original UTF-16 signed hash, upath 2.0.1
separator normalization, hashed project names and legacy template/overlay
basenames. Existing raw paths and case-sensitive protected/HTTP/data/blob URI
prefixes remain unchanged. Existing imported media receives an identity guard.
Multiple bundles reuse the first prepared file, matching original sequential
extraction. Basename collisions within a bundle retain the original last mapping;
when both hashed/legacy entries target one file, the last archive entry wins.
Missing archive media is still remapped and the destination is listed in
`plan.unresolved`; this retains original export/import behavior.

Raw global replacement includes the original Windows/Unix/escaped variants,
JavaScript replacement-string `$$`, `$&`, `$\``, `$'`behavior, JS integer-key
ordering, and the`**proto**` object-key quirk. Replacement output is bounded.

## Integration API and required host commit

`ImportService::new(ImportConfig::default())` creates two fixed parser threads and
one fixed cleanup thread. `COMMANDS` contains only `IMPORT_FILES`; unknown commands
and unsupported IDs return explicit errors.

```rust
let prepared = imports.prepare_with_cancel(
    "IMPORT_FILES",
    &serde_json::json!({"id":"freeshow_project", "paths":[selected]}),
    ImportRoots { imports_root }, // trusted absolute library Imports directory
    request_cancel,
).await?;
```

`PreparedImport.events` contains exact event transport, ready to contextualize and
publish **after** committing under the host's ordered library owner. On errors,
`ImportError {code,message,prior_events}` retains prior alerts for contextualized
publication; resource/unsupported failures must also surface their diagnostic.
No listener ID should be injected into successful original import events.

`PreparedImport.plan` is an in-memory, non-deserializable capability containing:

- `imports_root`, supplied by the trusted host, never a request field;
- `directories: Vec<ImportRelativePath>` for original directory effects;
- `installs: Vec<MediaInstall>` with private `staged_path`, generated root-relative
  `destination`, `expected_source` identity/length/mtime (Unix device/inode), SHA256,
  and explicit `expected_destination: DestinationGuard::Absent`;
- `existing` destination-relative identity guards and `unresolved` destinations;
- `ensure_current()` checking service/request cancellation, plus an optional
  `staging_directory()` and serializable diagnostic `manifest()`.

`ImportRelativePath` can only be constructed inside this crate. Its components are
normal and its first component is Projects/Templates/Overlays. There is no final
write or commit method. Before any install, the host must atomically validate its
RuntimeContext and library root under its ordered owner, call `ensure_current`,
validate root/subdirectory confinement and absence of symlinks, revalidate every
staged identity/length/hash and existing target identity, and enforce no-clobber
creation. Revalidate all guards before effects and roll back newly installed files
on failure; never remove preexisting files. Publish events only after successful
commit/snapshot refresh. The host decides how to surface unresolved media.

Dropping a prepared plan (including a buffered result dropped by a canceled future
or rejected commit admission) only queues private cleanup. A separate prepared
lease remains held through cleanup, independently bounding retained staging even
when no parser job is active. The bounded reaper queue has one slot per prepared
lease plus one reserved shutdown fence. No blocking send or filesystem fallback
runs on the caller. Cleanup failure fails future admission closed and is exposed
by `cleanup_failed()` / `cleanup_failure`; an unavailable reaper deliberately
retains the private directory instead of doing cleanup on the caller thread.

`shutdown().await` cancels all plans, closes worker admission, drains admitted
workers, checks `JoinHandle::is_finished` before joining, and fences cleanup queued
at that point. It never waits for host-held prepared leases. Retained plans keep
the cleanup channel alive and may clean up afterward. Shutdown uses a Tokio timer
while workers finish; without a runtime it returns `runtime_required` when a wait
is necessary, rather than panicking. Call shutdown before process exit. Default
Drop cancels/closes admission but does not provide awaited terminal drain.

## Bounds and cancellation

Default per-request limits: 64 selected files, 16 archives, 2,048 aggregate archive
entries/references, depth 16, 32KiB selected/reference path, 255-byte generated
media filename, 16MiB text, 64MiB
compressed archive/individual expanded entry, 128MiB aggregate actual file input,
128MiB expanded archive data, 32MiB encoded event output, ratio 200, 100,000
operations, and 15 seconds measured from admission (including worker queue time).
Configuration itself has finite maxima. Two independently retained prepared plans
are allowed; capacity rejection is explicit `busy`.

ZIP preflight scans the bounded end/central/local headers **before** invoking zip
9's parser or allocating from declared entry sizes. It checks counts, byte sums,
ratios, header field limits, local data ranges/names/flags, paths, types, duplicate
normalized names, and extra fields. ZIP parser/compressed reads/seeks share the
request cancellation, monotonic deadline and operation counter. All data reads
and staging writes are chunked at 32KiB; actual expanded counts and CRCs are
checked. Only private numeric staging filenames are opened, with create-new.

This is cooperative cancellation: individual filesystem syscalls (including
open/stat/write/remove, network mounts), allocator work, JSON parse/serialization,
UTF-8 decoding, hashing within a chunk and ZIP CPU work between reads cannot be
preempted. File byte/count limits and serde_json's default nesting limit bound
those units; this is not a hard wall-clock guarantee for stalled OS filesystems.
Unix reads use O_NONBLOCK before regular-file validation to avoid FIFO-open hangs.
Windows special-device/open behavior has not been validated. No blocking-Tokio IO,
unbounded spawn-blocking, worker per request, subprocess or Node sidecar is used.

## Supported scope and gaps

This slice deliberately rejects special/third-party/binary converters (PowerPoint,
Word, PDF, PowerKey, protobuf `.pro`, SQLite/MDB, `.probundle/.vpc/.qsp`) and generic
ZIP entries outside the listed native/text extensions. It does not claim complete
file-format import parity; the retained frontend converters define that behavior.
Malformed frontend JSON remains raw input and is left to those original handlers.
Bundle data must parse as serde_json: lone UTF-16 surrogate escapes and overflowing
non-finite JSON numbers accepted by JavaScript are outside established parity.

ZIP64, multi-disk ZIP, compression other than stored/deflate, encrypted archives,
alternate Unicode path extras, duplicate/traversing/absolute/drive/non-regular
entries and oversized headers are explicit native rejections. The original
sanitizes traversal and skips certain stream failures; native validation is
stricter and reports a diagnostic. CRC corruption also fails explicitly instead
of claiming a successful partial import. ZIP parser invalid-container fallback
and data-less archive empty-result behavior remain source-compatible.

Original asyncPool plain reads finish in nondeterministic completion order; native
plain records use selected input order, with all plain records before specialized
ZIP records. Multi-file oracle tests compare raw-record multisets and preserve
alert/archive ordering. Extensionless names whose original truncation creates a
lone UTF-16 surrogate are explicitly unsupported. Non-Unicode host roots are
rejected. The host must provide an absolute normalized Imports root; literal Unix
backslashes, repeated separators and dot-segment root variants are rejected.

Linux source comparisons and filesystem tests ran locally. Windows/macOS filesystem
case folding, drive/UNC destinations, native identity strength and packaging are
not established by Linux tests. No system tools/packages are installed. The
standalone crate/workspace/lock and harness-local target default are temporary;
the coordinator integrates root manifests, lifecycle/context/IPC, host commit,
strict CI and native smoke wiring.

## Verification

```sh
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-imports/target cargo test --manifest-path crates/freeshow-imports/Cargo.toml --all-targets
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-imports/target cargo clippy --manifest-path crates/freeshow-imports/Cargo.toml --all-targets -- -D warnings
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-imports/target cargo build --manifest-path crates/freeshow-imports/Cargo.toml
/home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node --test tests/parity/imports.test.mjs
cargo fmt --manifest-path crates/freeshow-imports/Cargo.toml -- --check
npx prettier --config config/formatting/.prettierrc.yaml --check tests/parity/imports.test.mjs tests/desktop/imports_probe.js docs/migration/workers/imports.md
```

Rust tests cover deterministic admission/shutdown orderings, no-runtime shutdown,
buffered-reply cancellation, a full retained-plan/reaper queue, stage identities,
SHA256/cleanup, no final writes, resource/deadline limits, malformed JSON/headers,
CRC, symlinks, traversal and encrypted/oversized declarations. Node tests execute
actual original import/ZIP/export functions against real filesystem/ZIP fixtures;
they also execute the actual frontend converters with controlled UI/store/history
boundaries. They do not copy a reference conversion algorithm.

The focused native probe takes `{requests:[{id,paths}], shows:[{name,text?,unknownKey?,unknownValue?}]}`
in the real main WebView. It sends through `window.api`, observes raw `IMPORT2`,
checks converted show names in the actual DOM, and sets
`window.__importsProbe.readyToSave`. The external driver then sends its real Ctrl+S
shortcut and sets `.saveRequested=true`. The probe reads saved shows through native
SHOW, checking text/Unicode/unknown fields, context stability and absent listener
IDs. `.done/.error/.saved` are reviewable evidence. This file is provided for
coordinator integration; it has not run standalone against an unwired native app.
