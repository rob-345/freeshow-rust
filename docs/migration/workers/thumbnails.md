# Native thumbnail preparation worker

Owned slice: `crates/freeshow-thumbnails`, `tests/parity/thumbnails.test.mjs`,
`tests/desktop/thumbnail_probe.js`, and this document. Original Electron and the
retained frontend are unchanged. Oracle: FreeShow 1.6.7-beta.2, commit
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`.

The host lifecycle is now integrated as documented in [native thumbnails](../native-thumbnails.md). This document describes the independent preparation contract and its source evidence.

## Source architecture and effects

The pinned `src/electron/data/thumbnails.ts` uses the frontend's
`captureCanvas` (`components/helpers/media.ts`), followed by Electron nativeImage
PNG encoding. Thumbnail generation does not use the FFmpeg resolver. This crate
preserves that canvas boundary. It does not launch a decoder/browser, install
binaries, grant a local media URL, delete cache files or write final cache files.

DOES_MEDIA_EXIST calls fs.access first. A missing/inaccessible path returns only
`{path,exists:false}` and invalidates existing cache entries at sizes900/500/250/100.
`noCache:true` short-circuits to `{path,exists:true}`. Otherwise it uses
`Math.floor(stats?.ctimeMs || 0)`; truthy nonmatching creationTime returns false and
the same invalidations. This is actual change time, independent of mtime/birthtime.
Unknown request fields are not copied into this command's reply.

GET_THUMBNAIL spreads unknown payload fields, overwriting output. Missing source
stat returns output empty. Its hash is signed32-bit wrapping arithmetic over
UTF-16 units, including the minimum i32 case, with `a`/`i` prefixes. Effective
size is requested size or500. Cache hit is a stat/mtime comparison only; even
corrupt cached bytes qualify, so this crate labels it `CacheHitUnverified`.
Newer source mtime requires stale-cache invalidation and a new capture. Native
preparation records guarded effects instead of performing those mutations.

The original createThumbnail defaults250 and immediately returns a predicted
path. Its extension-gated queue launches captures without an effective worker
limit; the renderer draws images or seeks videos to50% duration, sets width to
requested size and height to floor(width/aspect), then submits canvas PNG bytes.
Although the backend sends `config.seek:0.5`, the renderer reads `data.seek` and
therefore uses its own default0.5. This crate emits the actual typed request
shape and preserves that behavior without claiming a completed capture.

The source GET_THUMBNAIL wait is10s; capture timeout is15s. Waiting can return a
predicted nonexistent path. Raw requested size forms the wait/failure key, while
effective size forms the generation key. With0/null/omitted size, failed capture
can therefore return a predicted path instead of empty. Explicit native
`CaptureFailed`/`TimedOutPrediction` outcomes retain those supported wire shapes
and diagnostics identifying the absent/unverified artifact. The host must schedule
its actual source wait/capture deadlines; this crate does not pretend a wait ran.

Same-key coalescing and the original concurrent-global-Set clearing races are
host scheduling responsibilities. Independent prepared plans have independent
leases and expected cache guards; two plans must not blindly install over each
other. The source's duplicate in-flight omitted-size branch predicts a250 path,
instead of the normal500 path. The source test records this quirk; this crate
does not claim complete duplicate/timer interleaving equivalence. A host must
coalesce/retry under its bounded frozen-context capture registry before exposing
that concurrency as supported.

## API and trusted host integration

```rust
let service = ThumbnailService::new(ThumbnailConfig::default())?;
let plan = service.prepare("GET_THUMBNAIL", &payload, &roots, cancel).await?;
// Missing/cache-hit/existence results can be finalized under the ordered owner.
// CapturePending must first be routed through a supported bounded canvas host.
let capture = plan.capture.as_ref().unwrap();
let id = capture.id.clone();
// Host binds the owned plan, listener, frozen RuntimeContext and private id;
// it receives bounded PNG bytes through SAVE_IMAGE without trusting a path.
let ready = service.complete(plan, &id, CaptureCompletion::Png(&bytes)).await?;
// Ordered host: validate context, ready.ensure_current(), source/staged identity,
// SHA256, selected root confinement and destination guard; install, then reply.
service.shutdown().await?;
```

`COMMANDS` contains GET_THUMBNAIL and DOES_MEDIA_EXIST. `prepare_capture(input,
size,roots,token)` accepts browse capture intents without a dependency on browse.
The integrated browse host uses typed `PreparedListing` data and intents, preserving
original capture/depth/noMedia data before resolving them. Legacy dispatch still
returns `thumbnail_unavailable` when no capture host was supplied; re-running
capture=false would change source semantics and is not an adapter. `prepare_capture` uses GET_THUMBNAIL
preparation semantics; it does not emulate createThumbnail's immediate queue-only
return for files that disappear after the browse stat.

`ThumbnailRoots.cache_root` is chosen by the trusted host, never by IPC payload.
The request cannot choose a final destination. `CacheRelativePath` has no public
constructor and is always one hash-size.png filename. The root workspace and native manifests now include this crate; coordinator owns
command routing, context checks, shutdown and UI capability status.

`PreparedThumbnail` owns reply/outcome/diagnostics, an optional typed CaptureCanvas,
cache-relative invalidate/install effects and private staging. Public manifest
fields are trusted native integration data, not authority returned by a renderer.
`CapturePending.reply` is a template/prediction; it must not be published as a
verified generated PNG. The native host routes this through the bounded retained-canvas registry, raw
binary completion and ordered effects described in the integration document.

Completion consumes the actual live owned private plan and checks service identity,
capture id, cancellation, source identity, width and the15s capture age. Bare ids
or caller-supplied output paths cannot produce an install. Read-only
`source_guard()` and `staging_directory()` expose the native identity/private-root
boundaries needed for trusted asset grants and commit validation. Failed/timeout completion
is explicit. Host must validate frozen context before routing, after completion,
and atomically with effects. Stale context must drop the plan. It must ignore a
late SAVE_IMAGE event once its plan/lease/context is gone, and reject completion
paths/ids that do not belong to its registry.

A generated install includes private staged path, expected file identity/length,
SHA256, decoded RGBA hash/dimensions, source identity and absent/existing destination
guard. A fresh valid PNG replaces the stale-invalidation effect with one guarded
install. Failure/timeout retains stale invalidation. Host must not apply pending
invalidation twice or delete it early and then reuse the old destination guard;
these plans assume effects are finalized once. Final cache directory creation,
root/symlink confinement, replacement/rollback and effect ordering belong to the
coordinator's trusted ordered owner.

## PNG completion evidence and bounds

A reproducible probe launches the untouched official package with private app,
data and XDG paths. It makes real canvas PNGs and re-encodes them through actual
packaged nativeImage. Alpha sweep (all256 alpha values), opaque sRGB and display-P3
samples establish identical decoded RGBA pixels but different compressed PNG
bytes. Real GET_THUMBNAIL requests additionally exercise original image scaling,
video50%-duration seek, actual cache PNG output and unknown-field transport.
Real missing/corrupt sources, raw-size-zero failure-key behavior, unsupported
extensions and DOES_MEDIA_EXIST change time are checked over packaged IPC.
The probe removes its private profile. The small committed receipt is
`crates/freeshow-thumbnails/evidence/packaged-png.json`; its source declaration
hashes normalize CRLF to LF. Larger local output stays under ignored target.
Worker evidence records runtime versions,
PNG/raw-pixel hashes, dimensions and chunk types; it is generation evidence,
not local movie playback/delivery evidence.

Supported completion is deliberately narrow: PNG signature,8-bit RGB/RGBA,
noninterlaced, sRGB perceptual-intent0, IHDR/sRGB/IDAT/IEND and an optional single pre-IDAT sBIT declaring full eight-bit
RGB/RGBA precision. Actual WebKit fixture bytes/pixels and rejection of lower
precision, duplicate/late sBIT and invalid CRC are checked.
Dimensions, aggregate encoded bytes and every declared chunk length/count are
validated before png0.18.1 decoder allocation. Strict critical/ancillary CRCs,
decoder allocation limits, actual row lengths/count and aggregate pixels are
checked. A separate bounded zlib pass requires exactly the declared scanlines
and one complete stream; this rejects surplus inflation that png intentionally
tolerates after its last required row. The decoder consumes one row at a time, hashing canonical RGBA bytes.
Validated original canvas PNG bytes are staged unchanged. The trusted host
submits only bytes from its matched retained-canvas operation; arbitrary
SAVE_IMAGE sources and their nativeImage transformations remain unsupported. There is no guessed
color conversion or encoder substitute, and no claim of raw PNG byte equality
with Electron nativeImage. ICC/P3, APNG,16-bit, palette/grayscale, interlacing,
unknown chunks or other rendering intents return explicit unsupported diagnostics.

Defaults:2 fixed FS/PNG workers,4 independently bounded prepared leases,32KiB
paths,64KiB request JSON with depth64/node4096 limits,128MiB source file size,
2048 maximum requested/completed dimension,4,194,304 output pixels,8MiB encoded
PNG/decoder allocation budget,256 chunks and10s cooperative per-work deadline.
Configuration has finite hard maxima. Source files are only statted; this crate
does not decode source image/video headers. The host must additionally preflight/bound source decoding, source dimensions
and WebView resources. Current native capture grants exact source paths and
bounds completed PNGs, but source browser decoder resource preflight remains a gap.

Completion bytes are length-checked before cloning. No unbounded tasks,
spawn_blocking, thread per request or subprocess is used. Admission rechecks
shutdown after a worker permit and while holding sender ownership. Permits stay
held until worker completion. Dropped futures cancel their child token. A fixed
cleanup worker receives private staging through nonblocking Drop; queue capacity
covers all retained leases plus a shutdown fence, and the lease remains held
through cleanup. Shutdown cancels/closes admission, drains active jobs and queued
cleanup, checks is_finished and joins workers; it does not wait for host-retained
plans to be dropped. Those plans become canceled and clean on later Drop. A
cleanup failure blocks further admission and is surfaced rather than accumulating
unbounded abandoned staging. No-runtime shutdown is checked without panic.

Cancellation is cooperative between stat/access/PNG rows/chunks/write operations.
Kernel/network filesystem stalls, decoder CPU between rows, bounded JSON traversal,
and individual write/sync calls are not preemptible. The PNG crate's allocation
limit is best-effort; preflight rejects metadata decompression chunks and caps
all dimensions/input independently. These are resource budgets, not a hard
wall-clock/whole-process-memory guarantee.

## Explicit gaps and verification

Filesystem semantics are verified on Linux only, using direct SYS_statx like the
browse slice, with source f64 `seconds*1000 + nanos/1e6` operation order. Unavailable
statx returns unsupported_stat_backend; Windows/macOS and fallback stat semantics
remain gated. noCache uses access only, as source. Request sizes are bounded nonnegative integers; unverified string/fractional
coercions and nonboolean noCache options are rejected explicitly. Regular source
symlinks are followed, but cache symlinks/directories/special files are an explicit unsupported
cache variant because trusted final mutations require verified identity guards.

Original unsupported extensions can return a predicted path without any output;
native cache misses for these extensions return unsupported_capture_format instead
of successful generation. Existing regular cache hits still preserve the source
mtime-only contract. Protected protocol URLs hit the source stat-missing reply
with an explicit protected-capture-unavailable diagnostic; encrypted files are
not decrypted. Remote/protected delivery, arbitrary format/browser decoder
compatibility, full concurrent timeout ordering and thumbnail cache cleanup policy
are not implemented. No FFmpeg decoder replaces the retained renderer pipeline.

```sh
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-thumbnails/target cargo test --manifest-path crates/freeshow-thumbnails/Cargo.toml --all-targets
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-thumbnails/target cargo clippy --manifest-path crates/freeshow-thumbnails/Cargo.toml --all-targets -- -D warnings
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-thumbnails/target cargo build --manifest-path crates/freeshow-thumbnails/Cargo.toml
FREESHOW_THUMBNAILS_BIN=crates/freeshow-thumbnails/target/debug/freeshow-thumbnails /home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node --test tests/parity/thumbnails.test.mjs
FREESHOW_TEST_PACKAGED_THUMBNAILS=1 FREESHOW_ELECTRON_BIN=/absolute/path/to/extracted/opt/FreeShow/freeshow FREESHOW_THUMBNAILS_BIN=crates/freeshow-thumbnails/target/debug/freeshow-thumbnails FREESHOW_THUMBNAIL_RECEIPT=crates/freeshow-thumbnails/target/evidence/packaged-png.json /home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node --test tests/parity/thumbnails.test.mjs
```

Ordinary tests require no packaged executable or FFmpeg and register no missing
package skip. The separate packaged probe explicitly requires the untouched
package, Xvfb/dbus and locally available FFmpeg only to generate deterministic
video test fixtures. It fails clearly when a required tool is absent. Harness
default is root target/debug (+.exe Windows); worker sets its private target via
FREESHOW_THUMBNAILS_BIN. No packages are downloaded or installed by these tests.
