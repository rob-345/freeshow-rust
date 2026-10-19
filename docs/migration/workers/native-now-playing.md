# Native now-playing composition

`src-tauri/src/now_playing.rs` adds one fixed Tokio supervisor and no OS threads,
per-request tasks, filesystem implementation, encoder, socket, or polling timer.
It composes the actual integrated assets, opened audio parser, cover/image owner,
planner, phase scheduler, ordered file intents, metadata wire producer, and
process-global NativeAudioPipeline. Shared IPC, main-only authorization, raw
command parsing, bridge hydration, dependency construction/packaging, reveal,
and renderer event delivery remain coordinator-owned.

## Native API and authority

`NativeNowPlaying::start(Dependencies, Config, Callbacks)` requires an entered
Tokio runtime. Dependencies holds LibraryService, NativeAssets, Arc<AudioService>,
Arc<CoverImageService>, and the existing NativeAudioPipeline. Config defaults to
30-second cooperative operation and shutdown-observer deadlines (maximum300s).
The host captures the complete current RuntimeContext once. `submit(&context,
Command, CancellationToken) -> Result<Ticket>` is immediate bounded admission.
Commands are `Set { payload: Value, duration_number: Option<f64> }`, `Unset`, and
`Open`. Set accepts at most64KiB serialized JSON, depth8/1024 value nodes,
planner's64KiB raw accounting, and a32768-byte UTF8 source path without NUL.
The typed duration preserves NaN, infinities and negative zero. Ordinary finite
JSON remains available. Empty filePath follows source null metadata without a
media grant; nonempty files require an actual registered asset authority.

`Ticket::finish` returns a private Receipt with context, operation, mutated_input,
diagnostics and failure getters. It must never become a source success ACK.
Dropping a ticket cannot abandon accepted preparation, phase, file, image,
publication or cleanup work. Explicit CancellationToken cancellation cooperates
with actual parser/image cancellation and awaits their real receipts.

`Callbacks::publish` accepts owned `Publication::{Metadata,Alert}` and returns
CallbackFuture. MetadataDelivery has `event`, `hydration`, `id`, `sequence`, and
`context` getters; AlertDelivery omits hydration. Both have original MAIN source
events and frozen outer context. Metadata is AUDIO_METADATA `{filePath,metadata}`;
ALERT is exactly the original string. IDs are
`native-now-playing-{Uuid.simple()}`; sequence is a decimal process-wide u64 that
never resets across host/context replacement. Native-only completion is
`complete(id, &full_context, accepted: bool)` and is one-use. No private fields
belong inside source data. The coordinator adds `{id,sequence,hydration?}` to its
private event envelope, applies main/full-context/monotonic replay checks, and
then sends completion. A false completion records rejection without rollback.

Publisher queue acceptance precedes post-metadata effects. Renderer completion
**does not block title/file continuation**. It retains the whole operation until
ACK, explicit rejection, or authoritative host retirement. A missing ACK causes
bounded terminal timeout/retirement, never verified delivery. Callback rejection
or panic closes that private publication and settles actual session cleanup.
Reveal is a trusted callback receiving only the fixed existing text path plus
context; the actual ordered Exists observation precedes it.

Callbacks are exclusively inside the retained supervisor future and temporary
active-phase future. Shared/Handle do not retain them. Normal exit explicitly
drops them before completion; unwind/drop also drops them. Thus AppHandle
captures end at actual supervisor exit. A blocked callback retains the same
owner and cannot be replaced after timeout; trusted callbacks must settle.

## Ordering and actual effects

`OperationBudget` is a sealed cloneable fixed-four semaphore. The controller
must retain one budget across context replacements and use
`NativeNowPlaying::start_with_budget(deps,config,callbacks,budget.clone())`.
Standalone `start` delegates to a fresh default budget. Old joined hosts' native
uncollected results keep their actual permits charged against the new host.

Four whole leases include queued/active/finished/uncollected operations/results.
There are at most four preparation futures, one active phase future, four total
cleanup futures/operation records, four private publications, and four immediate
lifecycle observers; these categories share the same four operation records.
Dependencies retain their own independently bounded worker/byte quotas, so a
burst can explicitly fail smaller dependency admission. There is no waiting
admission queue beyond the fixed four-work channel. Diagnostics are capped at
16 entries of at most2048 UTF8 bytes per operation. Input, raw metadata,
pictures, planner text, images, wire events and file comparisons retain their
foundation limits; no zero-copy or memory-performance claim is made.

Observed source ingress is handled before observed preparation outcomes; all
ready outcomes are processed before choosing the next scheduler phase. The
scheduler preserves actual source await regions: PublishMetadata, PostMetadata,
Text, Unset, Open. Each synchronous filesystem region uses a real LibraryPhase
on the existing IO owner, held through actual title/image/format effects and
closed/drained before the next region. Text begins at the actual source
`await convertDynamicValues` continuation; whole requests are not FIFO and
UNSET does not cancel pending SET. Node/Tokio microtask arrival timing is not
universally equivalent; controlled source traces verify the supported ordering.

Preparation retains actual failed Sessions via prepare_retained. Only proven
`audio_input_rejected` maps source parser-null; authorization, context, native
resource, unsupported and other errors remain native failures. Nonempty parsed
common is transferred into the native planner receipt. Picture fields follow
field_order; undefined_fields alone supplies own undefined values. No null
inference is used. Full context and actual source authority are revalidated at
publication and later effect boundaries. Source input parser failure continues
with fallback title/files, but invalid authority cannot silently become null.

Actual pipeline metadata Applied observations continue despite caught header
write diagnostics; pre-operation failures stop explicitly. File helper false
results continue according to source. Source writes preserve ordinary truncate,
equal-text skipping, fixed filenames, and source partial effects; no rollback or
batch atomicity is claimed. Images use VerifiedPngPixels, real create/png/size/
jpeg/release receipts, then explicit Session retirement. JPEG input, unsupported
PNG/color/alpha/chunks and uncertified native resize remain explicit capability
gaps; no alternate resize opt-in occurs. PNG pixels/dimensions have foundation
source evidence; compressed PNG byte identity with Electron is not claimed.
Original common.picture insertion position relative to other tags is still
unverified; native field order and own values remain exact observations.

## Retirement and host obligations

`shutdown` and `context_changed` synchronously stop admission when first polled,
retire private publication authority, cancel cooperative preparation/image work,
and retain all actual accepted futures/phase/image cleanup. Both use at most
four immediate whole lifecycle observers, independent of operation/result
quota. A timed-out or dropped observer never releases accepted work or the
retained JoinHandle. The handle is awaited through Option::as_mut under an async
mutex and removed only after actual join. Repeated calls retain the first
terminal error. Status exposes per-host decimal-string admitted/completed counters: admitted
only after successful native work-queue transfer; completed only after actual
operation cleanup/publication settlement and native result send. A cancellation
signal alone never increments completed; queued retirement before actual native
acceptance is not a completed source operation. The counters reset with the
scoped context and are observations, not authority. Status also exposes
currentPhase/preparations/cleanups, publication
and operation counts, complete, supervisorJoined, and drained. **drained requires
both real drain proof and actual supervisor join**, even when shutdown returns
a sticky error. A timeout with drained=false keeps dependencies alive for retry.

The coordinator must retire this scoped host before changing pipeline, ingress,
assets or library context; then construct a new host with the new full context.
The shared process-global codec/resampler/accumulator is not reset by this host.
Terminal ordering is host drain, pipeline/audio/assets/covers retirement and
actual dependency joins, then LibraryService::finish_writes. The library's
existing finish_writes is a write fence, not an IO-thread join. Keep a native
lifecycle authority alive until explicit shutdown: dropping the last host handle
requests retirement, but async Drop cannot itself provide a join receipt.

An exceptional supervisor panic is not drain proof; status remains undrained
and dependencies must not be declared cleaned. Publisher/reveal panics are
caught; their actual accepted dependency work remains owned. OS syscalls and
native image/parser calls remain cooperatively bounded, not preemptible. No
physical capture, playback, reveal success, server acceptance, codec packet
version parity, GUI, or performance gain is asserted by this slice.

## Evidence and checks

16 focused Linux native tests pass with strict actual Opus required, including:
real opened WAV and tagged MP3/PNG; source NaN/undefined picture restoration;
actual PNG output/truncation/release; empty/parser-null/authority distinction;
real TCP title pages with raw on-wire CRC validation; private ACK independence;
controlled overlap and UNSET; bounded retained results/caller drop and shared-budget scoped replacement; explicit
cancellation/input caps; publisher rejection/panic; missing ACK sticky timeout;
stale context; actual blocked ordered IO and phase admission; four lifecycle
observers; aborted waiter after drain proof but before genuine supervisor exit.
The actual immutable nowPlaying.ts functions + original music-metadata execute
on a real WAV, matching native mutated input, text, metadata publication path,
source void and title. Other foundation suites establish native/common/PNG and
scheduler source comparisons separately; this does not claim a full GUI oracle.
Windows/macOS host execution is not locally verified; foundation capability
rejections remain explicit and root CI owns cross-platform integration.

Run the focused test with the existing private headless manifest including the
new host, scheduler, files, prepare, assets and audio modules and cached direct
futures-util/native dependencies. Set FREESHOW_NOW_PLAYING_ORACLE_ROOT to this
checkout and prepend pinned Node22.21.1 to PATH. FREESHOW_REQUIRE_OPUS=1 makes the
loopback codec check mandatory; otherwise an unavailable capability reports an
explicit omission rather than native packet parity. Use one Cargo job,
CARGO_INCREMENTAL=0 and dev/test debug=0. No root/GUI build or downloads required.

Final local checks: all16 host tests, strict Linux headless lib+integration-test
Clippy with warnings denied, rustfmt, Prettier and git diff --check. Final tests
ran after all API/counter/budget changes, with Node22 and strict real Opus enabled.
No cross-target host build, GUI, root workspace/native build, or package install
was run. The separate prerequisite commit a0d16ad0 supplies retained preparation
failure Sessions and the sealed source-phase accessor; coordinator imported it
as c0af37f5. Scheduler426659ac and metadata-wire c512e6d0 are independent worker
prerequisites, not duplicated host changes.
