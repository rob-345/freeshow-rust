# Native now-playing cover and image backend

This independent crate makes a bounded subset of the now-playing planner's cover/image receipts real. It does not wire IPC, mutate stores, write `NowPlayingCover.png`, publish `AUDIO_METADATA`, or substitute a process for a native codec. The coordinator owns those effects and final full-context acceptance. The completed planner remains commit `fb5f6c21`.

## Source and actual capability

The immutable oracle is `2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/audio/nowPlaying.ts`, with installed `music-metadata@7.14.0`. It calls `parseFile(filePath)` with covers enabled, publishes the whole common metadata, uses only `picture[0].data` for artwork, writes `nativeImage.toPNG()` before checking dimensions, preserves the original cover bytes at or below 640×480, and otherwise calls `resize(round(width*scale), round(height*scale)).toJPEG(90)`. Its artwork data URL is always labeled `image/jpeg`, including a small original PNG. The planner preserves those decisions; this backend never silently changes the small-cover original bytes.

Real extraction currently supports leading ID3v2.3/2.4 APIC frames followed by MPEG audio, bare MPEG without APE/ID3v1 tail tags, FLAC PICTURE blocks with ordinary Vorbis comments, and ordinary RIFF/WAVE without embedded ID3. It preserves picture order and duplicates, descriptions, original data bytes, MIME normalization, all 21 source picture type strings, and FLAC width/height/colour_depth/indexed_color fields. Empty picture data is discarded like the source. A picture's dimensions in metadata never authorize pixel allocation: image dimensions are read again from the actual PNG header.

The source's encoding-2 APIC description is deliberately decoded as UTF16LE when there is no BOM: the actual pinned `TextEncodingToken` maps both 1 and 2 to `utf16le`. BE data without BOM therefore produces the same source garbling. Known single LE/BE BOMs are honored. Repeated BOM workarounds, invalid Unicode replacement quirks, and non-ASCII MIME locale case mapping are explicitly unsupported.

Unknown picture type produces an own `type: undefined` in the actual parser result. `Picture.fields` is its JSON projection with `type` absent; `Picture.undefined_fields == ["type"]` records the lost own property. The host must restore that own undefined property and Buffer/Uint8Array picture data only after accepting context, before the original callback. The tests check both raw parser ownership and JSON projection; this is not a claim of end-to-end Electron metadata structured-clone parity.

Coordinator integration also adds `Picture.field_order`, an authoritative native-only source own-key list including `data` and own undefined `type`. Actual APIC order is `format,type,description,data`; FLAC order is `type,format,description,width,height,colour_depth,indexed_color,data`. The existing fields-only JSON projection has a different insertion order and must not be mistaken for that raw receipt. The source suite now compares every actual picture's `Object.keys` to this receipt, including both unknown types. Complete common-object placement of `picture` among other tags remains unverified after separate skip-covers/common and cover reads; this per-picture evidence does not certify that separate property position.

The real image decoder is pinned `png@0.18.1`: noninterlaced 8-bit RGB/RGBA PNG, binary alpha (0/255), ordinary chunks plus perceptual sRGB and full-8-bit sBIT only. Transparent RGB is zeroed like Skia's premultiplied bitmap. Empty input produces the genuine empty image. Other malformed/unknown input is an explicit error, never a guessed empty-image success. JPEG cover extraction is supported. A subsequent narrow Linux x64 baseline JPEG decoder now uses verified native pixels on the same fixed owners; its exact runtime, preflight, canary, resource and lifecycle contract is in [cover-jpeg.md](cover-jpeg.md). Partial alpha, ICC/gamma/text/animation chunks, interlace, palette/gray/16-bit PNG, GIF/WebP/etc are unsupported.

PNG encoding is real and pixel preserving on the checked subset, but compressed bytes differ from Electron. `Reply::Bytes` carries `native_png_encoding_differs`. The legacy `ImagePolicy::VerifiedPngPixels` name now also admits the checked JPEG decode subset but rejects ordinary resize/JPEG encoding; `AllowNativeResizeDifference` explicitly opts into pinned `image@0.25.10` Lanczos3 plus its JPEG encoder at quality 90. It returns `native_lanczos3_jpeg90_differs`; that result is a native alternative, not Skia resize or compressed-byte parity. Rounded zero-height/width targets return the source's empty JPEG, including 2000×1→640×0. No upscaling/conversion is performed for small covers.

The untouched packaged Linux Electron 37.10.3 / Node 22.21.1 / ICU 74.2 / Unicode 15.1 comparison is reproducible via `tools/electron-reference.mjs`. `fixtures/electron-linux.json` contains only source runtime/version/input and output hashes, dimensions and diagnostics, no codec binary. Nine final input fixtures have exactly equal decoded BGRA bitmap hashes and dimensions; eight nonempty PNG encodings differ. Five resize target dimensions agree: two zero-height results are empty on both sides, uniform red has identical decoded JPEG pixels but different JPEG bytes, and both textured examples have different pixels as explicitly diagnosed. That historical receipt records JPEG and partial-alpha input as unsupported. The later bounded JPEG capability and its actual source comparisons are documented separately; partial alpha remains unsupported. This does not certify other operating systems, codecs, or arbitrary nativeImage behavior.

## Native API and ownership

Construct one process host service, outside request dispatch:

```rust,ignore
let service = CoverImageService::new()?; // exactly two fixed std workers
let context = Context::from_host(runtime, generation, data_path)?;
let session = service.begin_session(&context)?;
let file = TrustedMediaFile::from_opened_host(&context, source_path, authorized_file)?;
let observation = service.covers(&session, file, Cancel::default())?
    .finish().await?;
observation.inspect(|reply| { /* bounded native receipt conversion */ })?;
drop(observation); // releases the one outstanding command/result permit
// create(session, owned picture bytes, policy, cancel), png, size,
// resize_jpeg90 and release use the SAME whole-session lease.
service.retire(&session, timeout).await?;
service.shutdown(timeout).await?; // actual join of both workers
```

`Context` has a native-only private run seal in addition to runtime/generation/dataPath. Carry its clone across the accepted operation; recreating equal strings does not recreate authority. None of Context, TrustedMediaFile, Session, Cancel, Ticket or Observation is deserializable. The test CLI is an isolated test driver, never an application command. `from_host(context, absolutePath)` is available only when the host independently authorizes opening that path; it is not an arbitrary renderer-path grant. Prefer `from_opened_host` from an existing canonical/protected-root authorizer: it moves an owned regular-file descriptor through queued/active work, resets its exclusive cursor on the worker, and preserves verified identity if the path is replaced. Do not concurrently read/seek another clone of that descriptor. Source-path spelling remains unchanged.

A separate earlier `freeshow-audio` skip-covers metadata read plus this later cover read is not an atomic source-file snapshot. The host must retain/revalidate metadata/file identity or diagnose changed input before combining them. This crate does not turn an unsupported cover variant into `Covers::Complete([])`, a parser-error null, or permission success. Native metadata diagnostics and the planner's ambiguous invalid-year/null distinction remain authoritative.

Each service has four entire session leases, each reserving 128MiB of native buffer/scratch budget before any extraction/decoder work. The service ceiling is 512MiB, not an eager allocation. A session includes its image, queued/active command and completed-uncollected observation; there is only one outstanding command/observation per session. A native inspection closure must be bounded, nonblocking and must not call back into service lifecycle methods. If the host copies data for an IPC envelope/planner receipt, that copy is a separately bounded host allocation and must retain its own publication ownership. No quota is released merely because `finish()` returned.

Dropping a ticket/waiting future retains accepted work on its fixed worker until actual return. Dropping a session synchronously closes admission and cancels queued work; active work stays owned until real return. `retire` and `shutdown` also close/cancel at call time, before their returned futures are polled. Retirement genuinely removes queued commands and their payloads even when both workers are blocked, waits actual pending work, and disposes real images/private results. Later observation collection fails explicitly. Per-job cancellation also rejects late ready publication. Retirement never claims to undo an already completed filesystem read/codec call.

Shutdown attempts both real joins, preserves sticky worker failures, and clears any outer-panic ownership only after actual joins prove no worker survives. A timeout keeps the same owners/handles for retry; it is not a cleanup receipt. Service Drop closes/wakes actual owners but does not prove joins; the host must explicitly await shutdown. One service should be held by the process host, not constructed per request/context. Context validation before source callbacks, real filesystem write ordering, Icecast metadata completion, grants for written cover files, and release of the planner's accepted effect remain host obligations.

## Hard bounds and limits

| Resource                        | Bound before allocation/work                                                                                 |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| Worker owners                   | 2 fixed threads per process host service; no per-frame/request spawn                                         |
| Sessions / outstanding commands | 4 whole sessions; 1 command/result per session; 128 total commands per session                               |
| Native reservation              | 128MiB/session, 512MiB/service aggregate                                                                     |
| Metadata scan                   | 8MiB aggregate; 256 ID3 frames / FLAC blocks / WAV chunks; reads in ≤64KiB slices                            |
| Pictures / compact data         | 8 pictures, 4MiB aggregate; 64KiB serialized fields per picture                                              |
| Encoded input/output            | 4MiB; bounded output writer; no whole-audio/video allocation                                                 |
| Pixel dimensions                | side ≤4096, ≤4,194,304 pixels; checked from IHDR before decoder/raster allocation                            |
| PNG work                        | ≤256 chunks; exact bounded inflate length preflight; decoder private limit 16MiB; no ancillary decompression |
| Resize work                     | precomputed ≤64,000,000 separable Lanczos taps; target ≤640×480; JPEG quality fixed90                        |
| Diagnostics                     | at most two sticky worker errors, ≤1024 UTF8 bytes each; fixed byte-result diagnostics                       |
| Deadline                        | default 10s, trusted configuration >0 and ≤10s; cooperative checks around reads/rows/codec calls             |

The 128MiB reservation covers the old/new 16MiB raster during replacement, up to 8MiB tag input plus 4MiB compact pictures/field projections, bounded encoded buffers, PNG decoder's 16MiB private budget, audited PNG encoder compressed/fallback vectors and row scratch, or the resize source clone/f32 intermediate/target/RGB/JPEG buffers. Bounds follow the exact pinned encoder/resampler source rather than relying on `image::Limits::max_alloc`, which upstream [documents as best effort](https://docs.rs/image/0.25.10/image/struct.Limits.html). The later JPEG path separately audits native RGB/pixel-buffer copies, old/new RGBA and encoded input within the same reservation; [its contract](cover-jpeg.md) records the bound and trusted native-call limits. Native allocator/global process overhead and host-owned IPC copies are outside this service reservation.

Regular-file `open`/metadata/read on a stalled filesystem and a synchronous bounded codec call cannot be preempted safely by a Rust deadline. Only the fixed owners can remain blocked; drain times out explicitly and retains them. POSIX opens use O_NONBLOCK and fstat rejects actual FIFO/device/directory inputs before seek/read. Other OS regular-file behavior is cross-checked for compilation only. No external image process or image codec library path is selected by renderer data.

## Validation

Focused checks use the private existing `target/assets`, jobs1 and incremental off. Default parity binary is workspace `target/debug/freeshow-cover-images-parity(.exe)`; `FREESHOW_COVER_IMAGES_PARITY` selects an isolated build.

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo test --manifest-path crates/freeshow-cover-images/Cargo.toml --locked --all-targets
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo clippy --manifest-path crates/freeshow-cover-images/Cargo.toml --locked --all-targets -- -D warnings
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo build --manifest-path crates/freeshow-cover-images/Cargo.toml --locked --bin freeshow-cover-images-parity
FREESHOW_COVER_IMAGES_PARITY=target/assets/debug/freeshow-cover-images-parity "$ORACLE_NODE" --test tests/parity/cover-images.test.mjs
cargo fmt --manifest-path crates/freeshow-cover-images/Cargo.toml -- --check
```

Fifteen Rust tests cover actual PNG lifecycle, ready/uncollected quota, caller drop with blocked real owner, synchronous cancellation and queued disposal independent of blocked workers, timeout/retry, late publication rejection, foreign service/run authority, preserved held-file identity after replacement, genuine POSIX FIFO refusal, actual clock deadline, serialized submission/retirement, and inner/outer worker panic cleanup with both joins. Ten source suites compare 19 real generated containers and 37 actual parser pictures, all 21 source type strings, encodings/Unicode/order/duplicates, two raw own-undefined types, twelve malformed/resource cases, nine explicit unsupported variants, and real unprivileged EACCES. Source variants outside the limits are not certified merely because the original parser tolerates them. Tests never download artwork/audio or execute native image subprocesses per application request.

Windows GNU and macOS ARM library cross-checks pass; execution/image reference is Linux only. Unsupported extraction includes ID3v2.2, unsync/compressed/encrypted/extended/footer tag forms, mixed/consecutive tag priorities, APE pictures, base64 Vorbis picture comments, empty MIME async file-type sniffing/order, embedded WAV ID3, unknown containers and malformed Unicode variants. General lofty metadata parsing remains in the existing audio service; no broad parser rewrite or source mutation is included.

The maximum-pixel fixture tool `tools/measure-fixture.mjs` completed real 2048×2048 decode→PNG→native alternate JPEG90→release, with original PNG 1,920,795 bytes, output PNG 214,317 bytes and JPEG 310,616 bytes. Its whole private debug driver peaked at 60,216 KiB RSS, including JSON/base64, two workers and allocator overhead; final reserved bytes were 0 and both owners joined. `fixtures/max-pixels-linux.json` records that one binary hash/input/output receipt. This measured fixture is not a universal peak-memory or performance equivalence claim.
