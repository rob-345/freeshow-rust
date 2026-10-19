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
`context` getters; AlertDelivery omits hydration and adds `kind()` and
`folder_path()`. Native `AlertKind::{MissingText, FolderCreationFailed { folder:
PathBuf }}` has no Deserialize; kind.as_str()/Serialize emits `missingText` or
`folderCreationFailed`, and folder_path returns Some only for the latter. That
path comes solely from the actual typed EnsureFolder file observation and must
match the captured data_path/Audio leaf. Both have original MAIN source
events and frozen outer context. Metadata is AUDIO_METADATA `{filePath,metadata}`;
ALERT is exactly the original string. IDs are
`native-now-playing-{Uuid.simple()}`; sequence is a decimal process-wide u64 that
never resets across host/context replacement. Native-only completion is
`complete(id, &full_context, accepted: bool)` and is one-use. No private fields
belong inside source data. The coordinator adds `{id,sequence,hydration?}` for metadata or
`{id,sequence,alertKind,alertFolder?}` for alerts to its private event envelope, applies main/full-context/monotonic replay checks, and
then sends completion. Metadata forbids alert fields; all alerts forbid
hydration; MissingText forbids alertFolder. A false completion records rejection
for that one private ID without rollback or settling a still-live sibling.

Publisher queue acceptance precedes post-metadata effects. Renderer completion
**does not block title/file continuation**. It retains the whole operation until
ACK, explicit rejection, or authoritative host retirement. A missing ACK causes
bounded terminal timeout/retirement, never verified delivery. Callback rejection
or panic rejects only that private publication and settles actual session
cleanup; any earlier pending metadata or folder notification remains retained.
Up to five independent private notifications belong to each source operation.
Every ID must settle or be authoritatively retired before the whole-operation
result/lease can be released; acknowledged metadata cannot hide a later folder
notification's timeout.
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
cleanup futures/operation records, twenty private publications (at most five per
operation), and four immediate lifecycle observers; these categories share the same four operation records.
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

Actual EnsureFolder failure observations enqueue the original folder ALERT before
that source effect advances. The exact string is `Error: Could not create folder
at: ${observed.paths.folder}!`; the trusted native Unicode folder path is capped
at32774 bytes (captured context path32768 plus fixed Audio leaf), without NUL.
No alert is inferred from generic false, diagnostics, missing files, writes or
renderer message data. For a regular-file obstruction named Audio, valid SET
emits metadata then four folder ALERTs (three PostMetadata, one Text), UNSET emits
two, and OPEN emits one plus the original missing-text ALERT. Folder failures
continue the source path/false-helper behavior; they are not fatal errors or
successful file writes. A valid format without artwork placeholders still
executes both eager artwork-path lookups. Private completion does not block
these continuations. Main/bridge must attach kind/folder privately, validate the
exact source string, and keep a process-wide bound of twenty entered completion
promises across context replacements/reentrancy until actual settlement; source
command admission remains four.

Actual pipeline metadata Applied observations continue despite caught header
write diagnostics; pre-operation failures stop explicitly. File helper false
results continue according to source. Source writes preserve ordinary truncate,
equal-text skipping, fixed filenames, and source partial effects; no rollback or
batch atomicity is claimed. Images use VerifiedPngPixels, real create/png/size/
jpeg/release receipts, then explicit Session retirement. The narrow verified
[Linux JPEG subset](cover-jpeg.md) now uses the same image owners. Broader JPEG,
unsupported PNG/color/alpha/chunks and uncertified native resize remain explicit
capability gaps; no alternate resize opt-in occurs. PNG pixels/dimensions have foundation
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

The coordinator must retire this scoped host before retiring pipeline, ingress
or asset dependencies. A committed library switch invalidates old preparation
authority; existing LibraryPhase admission prevents it from splitting an
already accepted synchronous source region. After the actual old-host drain and
dependency retirement, a new host captures the new full context.
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

22 focused Linux native tests pass with strict actual Opus required, including:
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

Final local checks: all22 host tests, strict Linux headless lib+integration-test
Clippy with warnings denied, rustfmt, Prettier and git diff --check. Final tests
ran after all API/counter/budget changes, with Node22 and strict real Opus enabled.
No cross-target host build, GUI, root workspace/native build, or package install
was run. The separate prerequisite commit a0d16ad0 supplies retained preparation
failure Sessions and the sealed source-phase accessor; coordinator imported it
as c0af37f5. Scheduler426659ac and metadata-wire c512e6d0 are independent worker
prerequisites, not duplicated host changes.

Folder-alert refinement verification retained all16 prior tests and adds six:
real regular-file Audio source SET/UNSET/OPEN comparison in one pinned Node22
realm over the actual real WAV, including false ACK of one folder event with
metadata still pending; second-callback rejection retaining metadata authority; four whole
operations retaining twenty distinct sequence-ordered publications; dropped
caller/context retirement held behind the real phase and Session cleanup; and
metadata acknowledged while a later folder publication times out. The shared
OperationBudget replacement test remains green. Source helper prerequisite is
8910f62e; its typed Observation.folder_creation_failed is true only for an actual
EnsureFolder mkdir failure. No helper/main/bridge/canonical-doc edit belongs to
this refinement. Strict Linux focused Clippy, rustfmt, Prettier and diff checks
run after final owned changes. Cargo uses the coordinator-authorized canonical
cache after its verified move, jobs1/debug0/incremental0; no GUI build or network
app was started. Windows/macOS execution remains integration-CI evidence.

Registration and retirement share the publication-map lock. After constructing
bounded event data, registration rechecks closing under that lock before
inserting a live Pending. Thus a concurrent close cannot finish its retirement
scan and then miss a late insertion that no completion could acknowledge. A
held actual metadata-registration boundary regression proves no event/Pending
escapes after retirement, while real Session cleanup and supervisor join remain
required. Earlier filesystem diagnostics are collected before awaiting a later
source-alert callback, so callback rejection cannot erase the actual mkdir
failure evidence.

`status().revealCalls` is a monotonic decimal-u64 per scoped host. It increments
only at actual trusted reveal callback invocation, after authority revalidation,
and checks exhaustion without wrapping. It counts attempts, including a later
callback error/panic, and makes no successful OS reveal claim. Missing-text OPEN
and regular-file Audio failure OPEN leave it at zero; a real existing text path
invokes the supplied callback once and records one. It resets with the scoped
host/context, independently of the process-wide publication sequence.
