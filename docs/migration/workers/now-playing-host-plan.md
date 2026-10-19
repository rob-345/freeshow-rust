# Native now-playing host integration plan

This is a read-only integration investigation, not a runtime/API implementation or a claim of production now-playing parity. The immutable source is FreeShow `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, version `1.6.7-beta.2`. Coordinator code references were inspected on 2026-10-19 with root HEAD `99a376808ff1c9f3fdeb976c8cedef089c8a4a0c`; line numbers describe that observation and may move during integration.

The pure planner is preserved separately in `fb5f6c2179cf1f23930941843d85b764f43bf6a0` ([foundation notes](now-playing.md)). The actual bounded cover/image backend is preserved separately in `703b09fa975bb5d83623fe5a2038e0faa34f6c6c` ([capability and evidence](cover-images.md)). This document does not change either implementation. The proposed names below identify minimal native seams for coordinator assignment; they are not existing public methods unless explicitly stated.

## Source contracts and asynchronous phases

The original commands are `MAIN.NOW_PLAYING`, `MAIN.NOW_PLAYING_UNSET` and `MAIN.OPEN_NOW_PLAYING`. They return JavaScript `undefined`; [IPC/main.ts:23–37](../../../src/electron/IPC/main.ts#L23) sends a response only when the result is not undefined. Do not add a source success ACK. `getNowPlayingMediaPath()` is an internal helper used by the server cover route, not a new renderer command.

[nowPlaying.ts:30–69](../../../src/electron/audio/nowPlaying.ts#L30) has three distinct SET phases:

1. **Asynchronous metadata preparation.** `getAudioMetadata` awaits `music-metadata.parseFile(filePath)` with covers enabled, takes `common`, sends `AUDIO_METADATA { filePath, metadata: common }`, and resolves the helper promise. Missing/empty input or a caught parser failure yields null without a metadata event. Publication occurs before the outer SET continuation resumes. See [nowPlaying.ts:166–183](../../../src/electron/audio/nowPlaying.ts#L166).
2. **The post-metadata synchronous continuation.** It derives artist/title and calls `IcecastSender.updateMetadata`, resolves the Audio folder and cover path, then either checks/deletes the old cover or creates an image, calls `toPNG`, writes PNG, obtains size and optionally resizes/JPEG90. It calls `convertDynamicValues`, whose body runs synchronously. All eight replacement callbacks execute in fixed order even when their token is absent; both artwork callbacks resolve/create the Audio folder. This phase does not yield to another source IPC handler between those operations.
3. **The text continuation after an await.** Although `convertDynamicValues` has no internal await, it is async. `await convertDynamicValues(...)` yields before the final Audio-folder resolution and text write. The source mutates its own received `data.format`; Electron already cloned the renderer's original object at send time.

UNSET is synchronous: empty Icecast title, resolve/write empty text, resolve/check/delete cover. OPEN synchronously resolves the text path and calls the default reveal behavior of `openInSystem`. Neither command waits for or cancels outstanding SET metadata preparation. A pending earlier SET may restore title/text after UNSET. There is no latest-request cache, cancellation generation or deduplication for these commands in the source.

### Controlled overlap experiment

The read-only experiment executed the actual pinned `nowPlaying.ts`, transpiled with installed TypeScript to CommonJS. It used `new Function("require", "exports", "console", js)` in the **same Node realm** as the controlled promises. The only replacements were module boundaries: deferred `parseFile`, fixed `/fixture/Audio` paths, no existing cover, traced filesystem helpers, traced `sendToMain`, traced Icecast update, and an image boundary that was never invoked. Inputs A/B had their respective file path, name/title A/B, `format: "{title}"`, `unknownLang: []`, duration 0, and no pictures. Each deferred parser resolved `{ common: { title: label } }`. Trace rows below omit repeated folder/exists calls, not asynchronous steps.

| Controlled sequence                                                                            | Observed ordered trace                                                                         |
| ---------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| Start SET A and SET B; resolve B then A in the same turn; await both                           | metadata B; metadata A; title `Unknown Artist - B`; title `Unknown Artist - A`; text B; text A |
| Start both; resolve B; await one `setImmediate`; resolve A; await both                         | metadata B; title B; text B; metadata A; title A; text A                                       |
| Start both; resolve B then A in the same turn; synchronously UNSET before yielding; await both | title empty; text empty; metadata B; metadata A; title B; title A; text B; text A              |

The important reproduction operations are:

```js
const a = source.setPlayingState(inputA)
const b = source.setPlayingState(inputB)
pending.get("B")({ common: { title: "B" } })
// Separate-turn variant: await new Promise(setImmediate)
pending.get("A")({ common: { title: "A" } })
// UNSET variant: source.unsetPlayingAudio()
await Promise.all([a, b])
```

Do not derive overlap ordering from a source module in `vm.runInNewContext` receiving host-realm promises: cross-realm promise assimilation introduced extra microtasks in an initial exploratory harness. Its ordering was discarded. The existing single-operation source suites use controlled boundaries but do not certify this new overlap scheduler. Add same-realm differential overlap tests during host implementation. Controlled promise ordering proves source phase behavior, not a universal winner for real filesystem races or exact Node/Tokio event-loop equivalence.

## Host scheduling and accepted ownership

Use the planner's four whole-operation slots through preparation, accepted effects, private results/publications and image disposal. Existing metadata and image workers perform real work; the host needs one bounded continuation owner, not a thread/task per request. Metadata preparation can overlap within those slots. Once common metadata and **complete cover extraction** are available, enqueue the actual metadata event before its post-metadata continuation. Image decoding is later and must not gate a valid metadata event.

A whole-request FIFO would produce metadata/title/text B before publishing metadata A in the same-turn case, changing the demonstrated source order. A latest-only queue would incorrectly cancel pending SET after UNSET. Instead, preserve parser-completion ordering and the separate continuation/text-yield phases, with a deterministic bounded scheduler tested against the actual source. Other now-playing commands must not interleave arbitrarily inside the source's synchronous post-metadata phase merely because native image/audio/filesystem effects require awaits. Queue their source phases while retaining real accepted effect futures.

There is a separate ordering decision against other library mutations. [LibraryService::commit, service.rs:460–474](../../../crates/freeshow-core/src/service.rs#L460) accepts a trusted closure, validates full context under the filesystem barrier, and completes accepted effects even when its response caller drops. It is reusable for each typed filesystem effect. However, separate commits allow SAVE/UPDATE_DATA_PATH or another mutation between effects that the original synchronous source phase cannot interleave.

If strict ordering against those mutations is required, the missing seam is a small native phase lease: fence earlier ordered work, capture/validate context, retain admission for the bounded source phase, and admit its own typed sub-jobs without reacquiring that admission mutex. It must not hold the filesystem barrier on GTK or while awaiting a codec/audio worker. It must remain owned through caller drop and genuine drain. Current [enqueue_operation, service.rs:354–412](../../../crates/freeshow-core/src/service.rs#L354) has no such API. Taking its admission mutex and then calling ordinary `commit` would deadlock. A host-only phase gate preserves ordering among now-playing commands but cannot be advertised as strict ordering against all library mutations. This is an explicit coordinator API decision, not a reason to silently serialize entire requests.

Accept each planner effect exactly once and keep its token plus actual native future until the real receipt arrives. Caller drop must not drop an accepted metadata/parser/image/library continuation or count cancellation as drain. A source false filesystem helper result is distinct from an unsupported operation, context failure or resource error. Preserve actual image ownership and `ReleaseImage` through later failures. Metadata publication means actual event queue acceptance; source does not wait for renderer processing. If private publication receipts are used to bound queued byte ownership, they may retire that private ownership without blocking the source continuation or creating a source ACK.

## Audio paths and filesystem policies

The trusted Audio folder is `runtime.stores.data.join("Audio")`, derived on the actual ordered library owner after validating the captured full `RuntimeContext`. It is selected/default library data, **not** application userData. The only output leaves are `NowPlaying.txt` and `NowPlayingCover.png`; source data cannot choose a root, filename, executable or sink. A native-only sealed root receipt can bind later effects to that exact context. Resolve the exported media-path helper through the same authority, without introducing a renderer grant command.

The source [data-folder helpers, files.ts:349,375–406](../../../src/electron/utils/files.ts#L349) attempt mkdir and return the path even when creation logs a failure. The host must preserve folder intent timing, including both unconditional artwork substitutions. Existence/delete/write failures generally return false and the now-playing caller continues. Keep bounded diagnostics separately; do not turn a native unsupported/resource case into fake helper success or a fabricated parser-null result.

The two writes have different observable policies:

| Source effect     | Required policy                                                                                                                                                                                                                                                                                                                |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| PNG Buffer write  | `BufferAlways`: a Buffer cannot strictly equal `readFile`'s string, so an identical PNG is still written. Preserve the write before get-size/resize.                                                                                                                                                                           |
| Text string write | Skip only if the path exists and the source UTF-8-decoded read equals the exact string. Node UTF-8 replacement decoding, empty files, missing files, directories, errors and the source string-size guard matter. A failed/directory read returns `""`, so an existing such path and desired empty text may skip before write. |
| Ordinary write    | Source `fs.writeFileSync` truncates the existing target, follows its normal filesystem semantics and reports a caught failure as false. Atomic replacement changes inode/watcher/permission behavior.                                                                                                                          |
| Delete            | `unlinkSync` is attempted only after the source existence check; failure returns false and does not by itself stop formatting.                                                                                                                                                                                                 |

These policies come from [files.ts:37–56](../../../src/electron/utils/files.ts#L37), [UTF-8 conversion:107–121](../../../src/electron/utils/files.ts#L107), [write/delete:166–186](../../../src/electron/utils/files.ts#L166) and [strict comparison:424–425](../../../src/electron/utils/files.ts#L424). Do not substitute byte equality, strict Rust `read_to_string`, or [atomic_write_bytes, storage.rs:83](../../../crates/freeshow-core/src/storage.rs#L83) without recording the changed behavior. The host must bound reads/work before allocating; source's much larger uncapped practical resource domain is not certified.

The native opener should be a trusted `RevealFile` callback, similar to [SYSTEM_OPEN, main.rs:1244–1279](../../../src-tauri/src/main.rs#L1244) and the native callback pattern in [media_copy.rs:29–31](../../../src-tauri/src/media_copy.rs#L29). OPEN resolves the text folder first, then checks existence, emits source ALERT `"This does not exist!"` when absent, otherwise reveals the item in its containing directory. See [files.ts:331–335](../../../src/electron/utils/files.ts#L331). Do not substitute opening an editor or arbitrary `open_path`. An actual opener invocation receipt does not prove an OS window became visible.

## Authorization, descriptors and coherence

[NativeAssets, assets.rs:36–39](../../../src-tauri/src/assets.rs#L36) currently exposes grant publication, not an authorized native read descriptor. Its four publication leases and protected-root checks are useful existing authority. [AssetServer's private Entry, lib.rs:272–276](../../../crates/freeshow-assets/src/lib.rs#L272) stores the public URL grant, canonical path and file identity. Public `AssetGrant` is a renderer URL capability, not authority to reopen an arbitrary native path. The worker's [private Identity/Open handling, io.rs:15,168–195](../../../crates/freeshow-assets/src/io.rs#L168) uses Unix dev/inode or Windows volume/file index; it does not record a content version.

Proposed minimal native-only seams:

1. `NativeAssets::open_registered_media(library, request, source_key, control) -> AuthorizedMediaRead`. Resolve only an already committed/trusted source key using the same key rules as the registry. Keep the original source-path spelling for callbacks. Open on the fixed bounded filesystem owner, verify regular-file identity and current canonical protected-root exclusion, and validate the grant/context before and after opening. The result has no Deserialize/renderer constructor and owns a charged descriptor lease plus captured context/grant identity. Rejected, remote, protected or absent keys remain explicit; receiving a renderer `filePath` is not authorization.
2. `AudioService::read_common_opened(authorized_input, cancel) -> NativeMetadataRead`. Reuse the existing bounded reader/parser and fixed parser workers. Current [parser.rs:79–114](../../../crates/freeshow-audio/src/parser.rs#L79) performs path metadata/open again; [parse_reader:115–129](../../../crates/freeshow-audio/src/parser.rs#L115) already supports `Read + Seek` and deliberately disables cover reads. Public READ_AUDIO_METADATA behavior can remain unchanged while a native-only reply retains common metadata and hydration information before JSON projection.
3. Sequential cover extraction from that same opened identity using `TrustedMediaFile::from_opened_host`. Keep an anchor descriptor for worker-side fstat revalidation before/after both reads and immediately before publication. Compare strong filesystem identity, length and supported native modification/change timestamps. An opened descriptor prevents rename/symlink replacement from mixing two different files; do not reopen by source spelling between parses. `File::try_clone` can share the seek cursor, so parser reads must be sequential, with explicit rewind; the anchor is for metadata checks, not concurrent reading.

The source performs one metadata parse with pictures. Native common parsing followed by cover parsing is two reads. Identity alone misses same-inode edits; version revalidation is necessary. It remains an observed identity/version check, not an immutable snapshot against a writer changing bytes in place while restoring timestamps. A stronger immutable snapshot/replay policy would require separately bounded work and must be named explicitly. Never claim two independent path reads are source-coherent.

The current [AudioService dispatcher, lib.rs:218–263](../../../crates/freeshow-audio/src/lib.rs#L218) cancels its parser token if its waiting future drops, and [workers:190–193](../../../crates/freeshow-audio/src/lib.rs#L190) release their permit before waking the caller. The new host must retain that actual future and its own whole-operation lease through completed-but-uncollected data; it cannot use the parser permit alone as publication/drain accounting.

## Metadata bytes, undefined and special numbers

Source NOW_PLAYING publishes common metadata, not READ_AUDIO_METADATA's `{common,replayGainMultiplier}` wrapper. Preserve all real pictures in the event even though only `picture[0]` controls output cover work. Wait for complete cover extraction before the event; a certified empty list is different from an unknown/unsupported cover result. Image decoding is later, so an image capability failure must not retract or silently strip an already valid metadata event.

Use a separate private metadata envelope, validated as a whole after exact context acceptance and before the original `AUDIO_METADATA` callback. Decode compact byte payloads into actual `Uint8Array` picture data. Restore own undefined properties only at the known picture fields; the actual parser's unknown picture type is own `type: undefined`, absent in JSON. Preserve raw structured-clone evidence separately from the source/native JSON projection. Object-key order, if claimed, also needs a bounded source order receipt: APIC and FLAC common-picture field insertion orders differ.

The existing [EXIF hydrator, exif.ts:17,55–82](../../../src/frontend/rust/exif.ts#L17) is fixed to `/exif`, has a 64KiB individual byte-array bound and a 2MiB aggregate bound, and should not be reused unchanged for metadata picture data up to the planner's 4MiB aggregate. Reuse its validation approach, not a generic untrusted JSON-pointer walker. Limit the new envelope to the expected channel/shape and the known fields, with picture count, byte, JSON-node, depth, diagnostic and aggregate copy budgets reserved before allocation. The source payload after hydration remains `{filePath, metadata}`; private transport fields do not enter it.

[AudioService's current projection, lib.rs:169–172](../../../crates/freeshow-audio/src/lib.rs#L169), calls [Js::json, js.rs:97–111](../../../crates/freeshow-audio/src/js.rs#L97): undefined object fields disappear and nonfinite numbers become null. An invalid YEAR may be actual NaN, which source formats as `NaN`, not `Unknown Year`. Keep the planner's explicit ambiguous-null rejection until a native common/scalar receipt and corresponding planner entry point preserve the real value. Do not fake this with a year string or silently hydrate every null as NaN.

Future bridge integration also needs send-time cloning for NOW_PLAYING. [bridge.ts:262–286](../../../src/frontend/rust/bridge.ts#L262) defers transport but does not currently include that command in its `structuredClone` cases. Capture the request and full context at send time, preserve the source received-object format mutation privately, and do not mutate the renderer's original object. If nonfinite duration is supported, transfer it through an explicit private number representation into `SourceData::set_duration_number`; JSON alone cannot retain it.

## Actual Icecast metadata receipt seam

Use the existing real [NativeAudioPipeline::metadata, audio_pipeline.rs:611–627](../../../src-tauri/src/audio_pipeline.rs#L611), not a replacement title cache or a fake sink. It already validates full context, bounds title bytes and submits owned work. Retain its actual future through completion. Its [worker operation:1225–1233](../../../src-tauri/src/audio_pipeline.rs#L1225) returns no source events, which is appropriate for the source void contract.

There is a concrete receipt mismatch to resolve before host wiring:

- Source [IcecastSender.ts:261–275](../../../src/electron/audio/IcecastSender.ts#L261) assigns the current title, attempts connected header update inside try/catch, logs an error and continues now-playing filesystem work.
- Native [freeshow-icecast/service.rs:569–578](../../../crates/freeshow-icecast/src/service.rs#L569) likewise assigns `self.song`, then `self.headers().await?` can fail. The pipeline currently propagates that error, without a typed receipt distinguishing an applied title plus caught header failure from a rejected/unapplied operation.

The minimal seam is an actual metadata disposition/diagnostic receipt that preserves source continuation after the caught header failure while retaining real socket cleanup/status. Do not map every error to success, nor abort all cover/text work merely because an already-applied title's header update failed. Context/cancellation/input rejection remains distinct and must not be hydrated as applied.

There is also a capability boundary: [audio_pipeline.rs:1551](../../../src-tauri/src/audio_pipeline.rs#L1551) gates all operations on pipeline+Opus codec+Icecast availability. Source title state can update without Opus capability. Either allow the actual metadata owner independently or report this platform/capability gap explicitly. Existing 8192-byte and no NUL/CR/LF title limits are narrower than source strings; unsupported inputs stay explicit.

## Retirement and shutdown order

Same-context UNSET preserves pending SET behavior. Library retirement is a separate native authority event: synchronously close old-context admission, cancel unaccepted preparation, reject stale publication, and await every already accepted native operation and image cleanup. An accepted effect's cancellation is not proof that it did not happen. Keep original owners/tokens/futures on timeout so the same retirement can retry. Do not reset process DSP or Opus state as a side effect of now-playing cleanup.

CLOSE must drain the new host while its dependencies and ordered write admission still exist: stop admission; settle accepted metadata/parser/cover/image/library effects; release images and private publications/results; then join/shut down dependencies in their existing process owners. Attempt all independent owners and retain errors. Only after the host no longer needs ordered jobs may the coordinator call terminal [LibraryService::finish_writes, service.rs:435–447](../../../crates/freeshow-core/src/service.rs#L435). Calling that barrier first permanently rejects required accepted cleanup/continuation jobs. Likewise, shutting down AudioService, assets or the pipeline concurrently with an undrained now-playing host can convert accepted work into a false source parser/constructor failure.

The host must not keep a strong publisher cycle. Status and private completion paths stay independently callable during drains. Source events are only the original metadata/ALERT effects; lifecycle observations and cleanup receipts remain private, bounded and context checked.

## Real image scope and required host checks

Backend `703b09fa` provides real bounded extraction and a measured PNG subset, not complete Electron nativeImage capability. JPEG cover extraction is real but JPEG image decoding is unavailable. General PNG profiles/partial alpha/other codecs remain unsupported. Verified PNG decoding has actual Linux bitmap evidence; native PNG compressed bytes differ. Alternate Lanczos3/JPEG90 requires explicit opt-in and has measured pixel differences for textured resizes. Small covers keep their original bytes and the source's `image/jpeg` data-URL label. Never claim compressed-byte parity or make an unsupported decoder return a successful empty image.

Meaningful integration checks should include:

- Same-realm actual-source overlap cases above, including same-turn versus separate-turn preparation, UNSET during both pending parses, and a later image failure after a metadata event.
- Actual held-descriptor rename/symlink replacement, same-inode edits and stale grants/context; common/covers must never silently combine two path identities.
- UTF-8 text skip behavior, Buffer-always PNG writes, PNG-before-size ordering, false mkdir/write/delete receipts and source reveal/ALERT behavior.
- Picture byte/own-undefined hydration, known field order if claimed, invalid YEAR NaN as distinct from null, and send-time input snapshots.
- Real pipeline metadata title application followed by controlled header failure, including continued text/cover work and explicit unsupported title/capability cases.
- Four whole-operation leases including ready-uncollected results, blocked ordered owner, accepted caller drop, real image cleanup, stale publication, timeout/retry and CLOSE before the terminal library barrier.

These are future host acceptance requirements. This investigation added no runtime code, API, manifest, bridge, task, process, app invocation or GUI claim.
