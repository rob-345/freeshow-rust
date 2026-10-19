# Opened-descriptor common metadata

This slice adds a native-only descriptor API to `freeshow-audio`. It reuses the existing fixed parser workers and shared job quota; it does not add a renderer command, authorize a path, open a source-name string, read covers, publish metadata, or wire the now-playing host. Public `READ_AUDIO_METADATA`, its compatibility JSON/replay-gain projection and ordinary caller-drop behavior remain unchanged. The coordinator owns assets authorization, full RuntimeContext validation, metadata hydration and subsequent cover/image effects.

The source oracle is FreeShow `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, version `1.6.7-beta.2`, with actual installed `music-metadata@7.14.0`. Original `nowPlaying.ts::getAudioMetadata` parses common metadata with covers and publishes that exact object. Native common parsing deliberately uses the existing skip-covers parser, so its picture knowledge is always `CoverKnowledge::Unknown`, including when the returned common value has no picture key. Only a later real complete cover read may certify absence or add source pictures.

## API and authority

```rust,ignore
// The native authorizer has already opened/verified this File and retains its
// MediaAuthority/context/whole-operation lease. These APIs do not create one.
let admission = audio.reserve_opened()?;
let ticket = admission.submit(
    authorized_file,
    exact_trusted_source_name,
    FormatHint::FromSourceName,
    cancel,
)?;
let read = ticket.finish().await?;
let common = read.common()?; // borrowed NativeCommon, or the actual parse error
assert_eq!(common.covers(), CoverKnowledge::Unknown);
let source_values = common.value(); // CommonValue: no lossy JSON conversion
let compatibility_json = common.json_projection();
let observed = read.observations();

// Consuming handoff; the audio result/quota ends at this actual transfer.
let (common_result, file, observations) = read.into_parts();
// Host/assets authority and whole-operation lease now bound common_result/File.
// Reuse sequentially for actual covers; then revalidate original authority and
// full context before combining or publishing. Never concurrently seek/read a
// duplicate sharing this descriptor's OS file offset.
```

`OpenedAdmission`, `OpenedTicket`, `OpenedRead`, `NativeCommon`, `FormatHint` and the input File have no Deserialize/IPC constructor. File provenance is a trusted caller obligation, not something proved by a method named `submit`. The assets worker's independently prepared `(File, MediaAuthority)` fits this seam directly; retain the authority through the common read, handoff, cover read and final revalidation. Keep the original source-path spelling for the eventual source callback. No executable or parser library is selected by input.

`reserve_opened` synchronously acquires the existing shared semaphore before the API copies a source-name buffer. `submit` takes an owned File and a borrowed nonempty source name of at most32,768 UTF8 bytes without NUL. It performs no file metadata/read/open on the caller thread. Queue send and shutdown are serialized through the existing sender mutex. Rejected submissions actually drop their input File and reservation; accepted work moves both into a fixed worker. A reservation that has not yet been submitted must itself be dropped before shutdown can finish.

`FormatHint::Detect` uses supported content detection without interpreting the source name. `Require(AudioFormat)` checks that content is the specified supported family. `FromSourceName` constrains supported source extensions (including uppercase ASCII aliases), detects content for an unknown extension, and rejects unsupported known source parser families or content/hint mismatch explicitly. This is a bounded compatibility subset, not a recreation of source extension-first parsing on misleading inputs. The source name is never opened/canonicalized and is not a permission grant. Tests pass nonexistent source names while reading an independently opened File.

## Typed common values and projection

`NativeCommon::value()` borrows `CommonValue`: ordered object entries, arrays, string, boolean, null, own Undefined and f64 Number. `into_value()` consumes the native wrapper and moves that value to the host without cloning. It keeps NaN, positive/negative Infinity and signed zero before serialization. Inspect object entries to distinguish a present own Undefined from an absent key; the convenience `get` method returns Undefined for both. There is no prototype/function/JavaScript object identity claim.

The real invalid RIFF `YEAR` fixture yields a typed NaN. Actual unknown ID3 genre produces an array with an own undefined index, retained by the native typed array. Raw comparison checks these separately from JSON. Source unknown picture type is also own `type: undefined`, but that is separate source/cover evidence: this common parser skips pictures and does not claim to restore that property itself.

`json_projection()` remains a separate compatibility projection using the existing Js conversion: undefined object keys disappear, undefined array entries and nonfinite numbers become null. It cannot represent raw JS values. A future metadata transport must derive bounded hydration from the typed receipt before projection; it must not infer NaN from arbitrary nulls or claim an omitted picture means no cover. The legacy public metadata/replay-gain calculation still uses its original projection path. This slice does not change the now-playing planner's native-common constructor or bridge hydration.

## Descriptor observations and sequential coherence

Workers fstat the held regular File before parsing, rewind it through the same budgeted reader, parse common tags, then observe the same File again. Nonregular inputs are rejected before seeking. Both successful and failed parsing return an owned `OpenedRead`, including the File and whichever observations were actually obtained. Native file-observation failure is explicit; no birth time or guessed timestamp substitutes for missing change time.

`FileVersion` records:

| Platform                                         | Native evidence                                                                                                                                                                                                          |
| ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Unix/Linux/macOS                                 | dev+inode, length, mtime seconds/nanoseconds, ctime seconds/nanoseconds from the held descriptor's metadata                                                                                                              |
| Windows                                          | volume serial+64-bit file index from `GetFileInformationByHandle`; LastWriteTime and ChangeTime from `GetFileInformationByHandleEx(FileBasicInfo)` with exact windows-sys public layouts; length from held File metadata |
| Other platforms / unavailable native information | Explicit unsupported receipt                                                                                                                                                                                             |

Windows timestamps retain signed100ns ticks since1601; Unix timestamps retain native seconds/nanoseconds. Microsoft documents [FILE_BASIC_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_basic_info) LastWriteTime as data-stream write evidence and ChangeTime as metadata-change evidence; neither is substituted with CreationTime. These fields are data-only Serialize receipts without Deserialize authority. The Windows calls use the held File and checked API results. Calls obtaining different fields are not claimed atomic. Linux execution is verified; Windows GNU and macOS ARM checks establish compilation only, with actual platform filesystem behavior still unverified.

When before/after versions differ, `common()` returns `audio_input_changed` while preserving both observations and the owned File. `FileObservations::unchanged()` is `None` if either observation is absent and otherwise compares the recorded evidence. Equality does not certify an immutable snapshot, prevent in-place writes that evade these observations, or establish agreement with a separate cover read. The host must retain/revalidate the assets anchor and full context after actual sequential reads, and reject changed input before combining/publication.

The service does not clone a File. Tests seek the original to EOF, replace its pathname, verify the original held identity/title is read, consume/reuse the same File after another sequential cursor movement, and verify the replacement path contains different bytes. No concurrent clone seeks are used. A controlled mutation after real parsing but before the second observation proves same-inode changed-length rejection. No timing tolerance is used as proof of content equality.

## Capacity, cancellation and real shutdown

There are no new threads, tasks or per-request processes. The default existing AudioService has two fixed workers and two shared permits; existing trusted configuration permits1–8 workers. Public jobs and opened jobs share those permits. Native permits follow queued/active work, completed-uncollected oneshot results and the owned `OpenedRead`. A returned `finish()` result alone does not release quota. Dropping an observation actually closes its service-owned File/data and releases quota. `into_parts()` releases quota by genuine consuming ownership transfer; the host must separately bound/drain the transferred resources, and the service makes no claim that they closed.

Dropping a ticket or its waiting future does not cancel accepted opened parsing. The worker retains File/permit until actual return, then disposes a result whose receiver disappeared. Caller cancellation and global service shutdown are checked cooperatively by the same budgeted read/seek path and again before ready publication. A late cancellation produces a cancelled native common receipt while retaining the actual returned File for disposal/handoff. It does not undo earlier reads. This native behavior is intentionally distinct from the existing public dispatch's CancelOnDrop contract.

`AudioService::shutdown` closes admission, cancels real work, waits all shared permits and actual worker completion, then joins the fixed threads. Concurrent shutdown calls are serialized through actual joins. The completion notification is registered before checking the worker count, avoiding a lost wake. A completed-uncollected native result still owns its permit and can hold shutdown pending: the operation-wide host must consume/drop its ticket/read before shutting down the dependency. A timeout around shutdown retains the same owners/service for retry; it is not a drain receipt. Service Drop is not proof of joins.

The existing default bounds remain read16MiB,50,000 operations, parser allocation4MiB,4096 tags, output4MiB and10s cooperative parse deadline. Native output additionally checks raw values before allocating a projection: depth16, at most `tag_limit*16` nodes, and a conservative output charge of24 bytes per value plus6 per UTF8 string/key byte, including undefined fields. It may explicitly reject values that would fit their smaller JSON projection. Existing trusted configuration maxima are unchanged. Host copies and consumed result ownership need the host's own whole-operation budget.

Filesystem metadata/read/seek can block in the OS and cannot be safely preempted by a cooperative deadline. Only the existing fixed workers remain owned in that case; no task cancellation proves their return. Input File authorization/opening is outside this API. Parser format/variant limitations remain those of the existing lofty0.24/schema adapter; no broad parser rewrite, real cover extraction, native image parity or playback claim is included.

## Focused validation

Twenty-four Rust tests pass: the existing13 plus11 opened tests. New checks include actual held-file replacement/sequential reuse, real invalid YEAR NaN, typed undefined/special values, unsupported hints, shared saturation, result ownership through collection, completed-uncollected ticket drain, blocked accepted caller drop, actual descriptor disposal, late cancellation, changed-input observations and bounded/error receipts. Worker gates are test-only and retain actual Files/permits through real return; they are not fake filesystem completion.

Five new independent source suites execute the pinned original helper with actual `music-metadata` on14 generated real WAV/MP3/FLAC files:11 raw special-number occurrences, own undefined genre index, separately checked own undefined source picture type,3 sequential held reads,2 source parse failures and2 explicit unsupported hints. Picture bytes are real parser values in the source probe; the native common result remains Unknown. No source algorithms are copied as the oracle. These tests use Node22.21.1; they do not claim packaged Electron structured-clone/end-to-end GUI verification.

The original four source suites also pass unchanged, including19 real tagged MP3/FLAC/OGG/Opus/M4A/WAV replies and actual ReplayGain projection. No new product app, Electron/Xvfb run or profiler is used for this slice. The isolated binary defaults to workspace `target/debug/freeshow-audio-opened-parity(.exe)`; the override below selects the private existing cache. The fixture CLI is a bounded test driver, not a production path-grant command.

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo test -p freeshow-audio --all-targets
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo build -p freeshow-audio --bins
FREESHOW_AUDIO_OPENED_BIN=target/assets/debug/freeshow-audio-opened-parity "$ORACLE_NODE" --test tests/parity/audio-opened.test.mjs
FREESHOW_AUDIO_BIN=target/assets/debug/freeshow-audio "$ORACLE_NODE" --test tests/parity/audio-metadata.test.mjs
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo clippy -p freeshow-audio --all-targets -- -D warnings
cargo fmt --manifest-path crates/freeshow-audio/Cargo.toml -- --check
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo check -p freeshow-audio --lib --target x86_64-pc-windows-gnu
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo check -p freeshow-audio --lib --target aarch64-apple-darwin
```

Shared workspace/native manifests and locks, main/IPC/assets/core/bridge/frontend, CI and the prior cover/planning commits are not changed. The coordinator updates its workspace lock for the crate-local Windows dependency during integration.
