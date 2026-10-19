# Native READ_EXIF worker

This independent slice implements the original `readExifData({ id })` from `src/electron/utils/files.ts` and its installed `exif@0.6.0` dependency. The pinned application source is `2d94defcf98175e4f5c9504e7f4fd1d33c35c975` / FreeShow `1.6.7-beta.2`. It changes no Svelte flow, shared IPC contract, native entrypoint, or existing asset service. Native integration remains coordinator-owned.

## Host API and transport

```rust
use freeshow_exif::{Context, Control, ExifService, Limits};
let service = ExifService::start(Limits::default())?;
let outcome = service.read(id, Context {
    runtime_id,
    library_generation,
    data_path,
}, Control { deadline, cancel: library_context_cancel.child_token() }).await?;
// Revalidate the captured context before publishing anything.
// Send outcome.reply plus outcome.hydration as trusted transport metadata.
// The retained renderer callback receives the hydrated reply only.
service.shutdown().await?;
```

`Control.deadline` is an absolute `std::time::Instant`; its default is two seconds. The service clamps it to `Limits.max_job_duration`. The context is captured and echoed, with bounded strings and a JavaScript-safe generation. The service does not inspect the library store, switch context, mutate caches, or validate that an echoed context is still current. The host must cancel its context token on switching libraries, validate context again after completion, and keep its own admission/response bound through publication. No library-owner queue wait is needed for file parsing.

`ExifOutcome` contains `context`, `reply: serde_json::Value`, `hydration`, `diagnostics`, `sourceCompatible`, and resource `stats`. The original success callback receives `{ id, exif }`, where `exif` always has `image`, `thumbnail`, `exif`, `gps`, `interoperability`, and `makernote`. Ordinary read, missing-file, permission, unsupported-format, and parser failures retain `{ id, exif: undefined }`. JSON omits that own undefined property, so the native reply is `{ id }` with `hydration.undefinedPaths: ["/exif"]`. Diagnostics are host data, not extra fields in the retained callback.

Hydration is data only; no executable renderer code is supplied by this crate. The integrated bridge applies these trusted, generated JSON pointers to the reply after context acceptance and before the original callback:

| Metadata                         | Renderer restoration                                                                                                                                                           |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `undefinedPaths`                 | Assign `undefined` at the object property or array index; do not delete it.                                                                                                    |
| `specialNumbers: [{path, kind}]` | Restore `NaN`, `Infinity`, `-Infinity`, or `-0` for `NaN`, `PositiveInfinity`, `NegativeInfinity`, `NegativeZero`. JSON placeholders are null, except negative zero uses zero. |
| `byteBuffers`                    | Convert the number array to `Uint8Array`.                                                                                                                                      |

Objects with omitted own undefined fields additionally carry optional `objectKeys: [{path,keys}]` metadata so the bridge can restore their original key order. The empty path denotes the callback root; group paths remain below exif.

Only `READ_EXIF` needs this hydration. These pointers are relative to the callback reply, not the outer native event envelope. They come from final tag values after duplicate-key replacement. The host should not accept this metadata from renderer input or hydrate rejected/stale replies.

The actual untouched Electron 37.10.3 executable was probed with a disposable sandboxed BrowserWindow, IPC, and contextBridge preload. It delivered dependency Buffers as `Uint8Array`, retained own undefined properties, and preserved `NaN`, infinities, and negative zero. The original frontend's `getExifData` consumes these native types. JSON-stringifying Buffers into `{type:"Buffer",data:[...]}`, dropping the undefined property, or changing rationals into strings would change that behavior.

## Source behavior retained

The parser accepts JPEG only, with the first APP1 segment deciding success. An earlier XMP APP1 prevents finding later EXIF, as in the source. Raw TIFF is rejected even though EXIF contains a TIFF structure. Both II/MM TIFF byte orders and field formats 1–10 are covered. FLOAT/DOUBLE and unknown tags are skipped because the original does so. The complete static EXIF/GPS name tables are generated from the installed dependency by `tools/generate-tags.mjs`; `NOTICE` preserves its MIT notice and source SHA256.

Values preserve numeric arrays versus single elements, signed values, IEEE-754 rational division including special values, byte buffers, Latin-1-style `String.fromCharCode` text decoding, and removal of exactly one terminal NUL. Timestamps remain unparsed strings. Thumbnail offsets remain TIFF-relative. Padding is decoded subject to bounds then discarded, as with the source default.

The source caps IFD0, IFD1, GPS and interoperability at 128 entries. Its EXIF IFD has no default count cap; native hard bounds deliberately prevent unsafe allocations. IFD1's pointer is read after the source-capped IFD0 count. Unknown interoperability entries stop that group; unknown tags in the other groups are skipped. APP1's declared length is ignored by the source; native parsing likewise uses the bounded complete JPEG buffer. Out-of-range numerical values reproduce the dependency Buffer helper's partial integers, undefined bytes and NaNs rather than silently sanitizing them.

The original vendor MakerNote dispatch passes an unset `this.makernoteOffset` instead of `this.offsets.makernoteOffset`. Its six recognized vendor plugins consequently return empty objects; this quirk is verified for all recognized headers. Unrecognized headers expose the original `makernote.error` string. A non-byte MakerNote makes the original entire EXIF read fail and preserves own undefined. This implementation does not claim vendor metadata extraction beyond that pinned behavior.

`id` is the literal original filesystem path and reply spelling, with no URI decoding, NFC normalization or asset URL conversion. Relative names resolve against the native process's current directory. Literal `file://` strings fail as they do in the source. Embedded NUL paths are explicitly rejected; the original dependency schedules file loading with `setImmediate`, so this invalid-path exception escapes the helper’s Promise catch instead of producing a normal reply. Protected URI strings are not decrypted; they are literal failed filesystem paths, matching this handler. No media playback, thumbnail generation, image-pixel decoding, or protected media decryption is established by metadata parsing.

## Bounds, cancellation and shutdown

Exactly two named OS workers own file open/metadata/read/close and parsing. The async caller does not perform file IO, use Tokio's blocking pool, spawn OS processes, or create per-request threads/tasks. A bounded channel and admission semaphore allow two active workers plus eight queued/completed-but-uncollected requests by default. A queue can reject earlier if both workers have not dequeued pending jobs yet. Admission rejects explicitly with `exif_busy`; it never silently drops a callback.

| Limit                                             |     Default |                                  Hard maximum |
| ------------------------------------------------- | ----------: | --------------------------------------------: |
| queued requests                                   |           8 |                                            64 |
| input file bytes                                  |      16 MiB |                                        64 MiB |
| path/context data-path UTF-8 bytes                | 32 KiB each |                                   32 KiB each |
| tags across directories                           |       1,024 |                                         8,192 |
| components per recognized field                   |      65,536 |                                     1,048,576 |
| directory depth                                   |           3 | 8 (parser visits only five fixed directories) |
| conservative cumulative allocation budget per job |      64 MiB |                                       128 MiB |
| complete serialized outcome bytes                 |       2 MiB |                                         8 MiB |
| charged parsing/serialization operations          |   1,000,000 |                                     4,000,000 |
| request duration ceiling                          |  10 seconds |                                    60 seconds |
| shutdown wait                                     |   2 seconds |                                    10 seconds |

Numeric limits must be nonzero. The allocation budget conservatively charges input buffering, context/path copies, tag/container capacity, field components, final JSON values and hydration paths before growing them; it is a cumulative bound, not a reported allocator/RSS measurement. The fixed worker stacks/channel have separate bounded overhead. Input metadata and growing files are checked before allocation. Output serialization counts the whole envelope without allocating a second JSON string and checks cancellation, operations and output size on writes. All component/tag loops check deadlines/cancellation. Unknown huge-count tags remain skipped without allocating their data; recognized huge counts fail with `exif_limit` before allocation.

There is no recursive IFD following: only IFD0/IFD1/EXIF/GPS/interoperability are visited, so cyclic pointers cannot create an unbounded chain. Symlink resolution is left to one normal OS open; ELOOP and missing targets produce failed-read outcomes. Unix opens use O_NONBLOCK before rejecting non-regular files, preventing FIFOs from occupying the pool. Directories match ordinary source failure; other non-regular files are explicit uncertified `exif_unsupported` diagnostics.

Cancelling/dropping a read future cancels only its child token. A library token can cancel all its children. Shutdown closes admission, cancels all admitted requests, drains the fixed workers and reports `exif_shutdown_timeout` if a kernel operation cannot return within the bound. A stuck network mount/Windows path open cannot be force-killed; no replacement workers are spawned. Subsequent requests fail `exif_shutdown`. Successful service results still require the host's final context validation.

Resource/admission/cancellation/deadline/shutdown failures are explicit errors (`exif_limit`, `exif_busy`, `exif_cancelled`, `exif_deadline`, `exif_shutdown`, `exif_shutdown_timeout`), not synthetic source success. Empty `id` is `exif_invalid`: the source constructor never calls its callback for an empty string, so its Promise hangs. Non-numeric malformed directory pointers invoke JavaScript coercions in the dependency; native parsing marks those variants `sourceCompatible: false` with a bounded `exif_unsupported` diagnostic and undefined EXIF instead of inventing equivalent metadata. Unpaired UTF-16 path strings are outside this UTF-8 native API. These deliberate limits and uncertified variants prevent a claim of universal EXIF parser equivalence.

## Focused evidence

`fixtures/full-II.jpg`, `full-MM.jpg` and `raw-II.tiff` are original generated binary fixtures, reproducible with `tools/fixtures.mjs`. The JPEG generator writes an 8×8 grayscale image; the actual Electron image decoder successfully decoded it as 8×8. No third-party photographs are copied into the repository. Nine original dependency photographs are read from the pinned installed package during source comparisons.

Commands, from this isolated worktree (the coordinator uses the normal workspace target instead):

```sh
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo test --manifest-path crates/freeshow-exif/Cargo.toml
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo clippy --manifest-path crates/freeshow-exif/Cargo.toml --all-targets -- -D warnings
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo build --manifest-path crates/freeshow-exif/Cargo.toml --bin freeshow-exif-parity
FREESHOW_EXIF_PARITY="$PWD/target/assets/debug/freeshow-exif-parity" /home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node --test tests/parity/exif.test.mjs
FREESHOW_EXIF_PARITY="$PWD/target/assets/debug/freeshow-exif-parity" xvfb-run -a /home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node crates/freeshow-exif/tools/probe-electron.mjs
```

The parity binary defaults to `target/debug/freeshow-exif-parity` (`.exe` on Windows), with `FREESHOW_EXIF_PARITY` overriding its location. Tests execute the actual pinned TypeScript helper and installed `exif@0.6.0`, with only logging/Electron boundaries injected. They compare generated formats, group defaults, unknown tags, duplicate replacement, cap/interop quirks, cycles, offsets/truncations, timestamp/byte/numeric representations, Unicode/literal paths, permission/missing cases, nine photographs, and 160 deterministic generated field/count/offset variants. Unsafe billion-component source allocations are inspected rather than executed against the unsafe original; native tests assert rejection. Rust tests add all resource ceilings, every fixture truncation, 1,000 deterministic corrupt mutations, real permission/FIFO/symlink-loop IO, the real two-worker admission boundary, queued cancellation/recovery, cancellation during field work, dropped futures, captured contexts, deadlines and shutdown (including a controlled non-returning OS boundary that proves timeout without replacement workers).

The actual packaged oracle compares 212 source fixture outcomes and separately tests the renderer clone path. Its receipt is `test-output-rust/exif/electron-exif.json` and records executable/source/dependency hashes and runtime versions. Electron 37.10.3 embeds Node 22.21.1 / ICU 74.2 / Unicode 15.1; standalone Node 22.21.1 has different ICU tables. This parser's byte-wise text decoding has no ICU dependence; the packaged probe independently verifies its source fixture output and transported types. The Linux oracle helper uses private app/profile/data/user directories and removes them on exit.

Linux runtime behavior is tested. Windows/macOS compile checks cover this portable parser and service, but filesystem/permission/native shutdown behavior and Electron clone behavior remain unverified on those OSes. The [native file-read integration](../native-file-reads.md) now owns root/native wiring and retained callback restoration; its desktop checks and remaining gaps are documented separately.
