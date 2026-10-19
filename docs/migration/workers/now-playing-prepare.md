# Native NOW_PLAYING preparation

This is a native preparation seam, without IPC, metadata publication, planner
effects, image decoding, cover-file writes, Icecast titles or dynamic-value work.
It composes existing asset, common-metadata and cover workers; it creates no
service, task, thread, timer, queue, renderer authority or successful AUDIO ACK.

## API and ownership

```rust
let services = PreparationServices::new(library, assets, audio, covers);
let control = PreparationControl::default(); // cooperative 30s overall budget
let prepared = services
    .prepare(&captured_full_context, &frozen_cover_context, existing_source_key, &control)
    .await?;
prepared.revalidate(library, &control).await?; // later real effect/publication boundary
let (common, all_pictures, authority, session, captured) = prepared.into_parts();
// Retain authority + session + the host whole-operation lease through subsequent
// sequential image/planner effects. Release all of them before dependency CLOSE.
```

The host supplies one already validated opaque `CoverContext` per native run.
Preparation compares its runtime ID, library generation and Unicode data-path
spelling with the complete `RuntimeContext`; it preserves the supplied seal
rather than manufacturing a replacement run. No renderer can construct a
`Prepared`, `MediaAuthority` or transferred native file.

`NativeAssets::open_registered_media` resolves only an existing current exact
source key. It cannot create a grant from NOW_PLAYING text. The guarded File goes
to `AudioService::reserve_opened/submit`, then the **same returned File** goes to
`TrustedMediaFile::from_opened_host` and actual `CoverImageService::covers`.
Readers rewind sequentially; no File clone or shared-offset concurrent read is
introduced. The cover reader receives the authority's canonical native path as
its diagnostic path and never reopens it. The original `source_path` remains
available verbatim through the retained authority.

Every stage retains that authority/worker anchor. Real worker revalidation occurs
before and after common parsing, before and after cover extraction, and immediately
before returning `Prepared`. It checks current full context, exact live grant,
protected/canonical path, held/path identity, length and native modification/change
times. The audio worker also returns its own before/after descriptor observations.
These are observations, **not an immutable byte snapshot**, protection against every
external ancestor rename race, or a performance improvement claim. Coarse native
timestamp resolution can hide an in-place edit restored within one tick.

`Prepared` retains typed `NativeCommon`, all ordered `Picture` values (including
each `undefined_fields` list and any authoritative field-order metadata supplied
by the cover backend), `MediaAuthority`, the actual cover `Session`, full context
and audio observations. It does not flatten common data into lossy JSON. The
cover result observation is dropped before return so this same Session can accept
later real image operations. Pictures are cloned only after certified extraction:
at most eight pictures, aggregate encoded data at most 4MiB, bounded fields, under
the existing four whole cover sessions. No unknown/unsupported extraction becomes
an empty list. Ordinary WAV absence is certified by an actual complete cover read.

The coordinator's fixed **four whole-operation owner must retain the actual
preparation future** through caller Drop and actual dependency completion. Directly
dropping this helper future is **not ownership proof**: a transferred File can still
belong to an accepted parser/cover operation after its local authority is dropped.
This helper deliberately adds no per-request task to disguise that obligation.
The test models an already retained owner and a separately dropped observer.
Completed/uncollected `Prepared` values still consume asset-read and cover-session
quotas; consume or drop them before dependency shutdown. No dependency shutdown is
called from preparation or while a returned result is deliberately retained.

`PreparationControl::cancel()` synchronously cancels the asset/audio token and the
cover flag. The helper awaits accepted dependency receipts instead of racing a
timeout/cancel observer and abandoning actual work. It checks the overall deadline
between stages; existing per-operation worker deadlines remain cooperative around
OS calls. The default overall budget is 30s, not an OS syscall-preemption promise.
Session Drop uses the existing serialized close/remove-queued/dispose semantics
on failure; exceptional worker failure still requires the service's actual join.

`PrepareError` retains a bounded message, native code and a category: input/parser
rejection, authorization/observed changes, context, cancellation, unsupported,
resource/deadline or dependency failure. `audio_input_rejected` remains an actual
input/parser failure and is never fabricated as successful null; the final source
parse-null policy belongs to the host. Revocation/replacement remains distinct
from a missing source and from a deadline. No image/session is published on error.

## Evidence and remaining integration

The focused headless native test target uses real files and the existing fixed
workers. Coverage includes WAV certified-empty, MP3/FLAC complete pictures,
Unicode/source spelling, own undefined common/cover fields, picture ordering,
later actual PNG creation on the retained Session, all full-context fields,
library retirement, changed paths versus regranted revocation, in-place changes
after common/covers/before-ready, a symlink swap into protected cache, cancellation
before admission and after actual audio/cover acceptance, four retained results,
and retained-owner/dropped-observer cleanup. Tests check actual worker joins.

One test executes the immutable original `getAudioMetadata` declaration from
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, transpiled with the installed TypeScript
and actual music-metadata 7.14.0, against all three genuine fixtures. It checks the
original AUDIO_METADATA publication occurred with its returned common object,
compares typed common shape (including undefined) excluding separately prepared
`picture`, compares every picture field/byte/undefined marker, and checks original
source spelling. Finite parser numbers use the existing source tolerance. This
does not certify `common.picture` insertion among other common keys; it also does
not use JSON object equality as proof of source picture own-key order. The cover
backend's explicit field-order metadata is the authority for later reconstruction.

Local verification: **14 native tests passed**, including the three-file actual
source comparison, on Linux with pinned standalone Node 22.21.1. Strict Clippy
(`--all-targets -- -D warnings`) passed for both production and test module
configurations; rustfmt, Prettier and diff whitespace checks passed. Native
Windows/macOS filesystem execution remains for root CI. Only Windows symlink
creation failing with actual privilege error 1314 skips that specific swap
capability.

No root manifest, existing module, main, bridge, lockfile or CI changes are included.
At base `8f234480`, the missing native dependency is exactly
`freeshow-cover-images = { path = "../crates/freeshow-cover-images" }`; native
assets/audio/core and tokio-util are already declared. Integration also needs
`mod now_playing_prepare`, this dedicated test target, the fixed four-operation
owner, frozen cover-run creation, source-aware publication/planner continuation,
and CLOSE ordering that drains it before assets/audio/images/library dependencies.
The test resolves the checkout from `CARGO_MANIFEST_DIR` (native manifest parent);
only a private standalone check needs `FREESHOW_NOW_PLAYING_ORACLE_ROOT`.

Focused local commands (same cached private target, jobs=1, debug=0, incremental=0):

```sh
cargo test --offline --manifest-path /tmp/freeshow-osc-now-playing-prepare-check/Cargo.toml --test now_playing_prepare
cargo clippy --offline --manifest-path /tmp/freeshow-osc-now-playing-prepare-check/Cargo.toml --all-targets -- -D warnings
rustfmt --edition 2021 --check src-tauri/src/now_playing_prepare.rs src-tauri/tests/now_playing_prepare.rs
```
