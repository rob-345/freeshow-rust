# Bounded real LTC codec capability

This slice provides real Linux x64 LTC encoding and decoding on one owned thread. It does not open an audio device, schedule playback, measure real-time performance, or wire renderer IPC. The existing `freeshow-timecode` router supplies the source lifecycle/frame decisions; its host can submit these codec operations and retain their real completion receipts.

## Oracle and ABI evidence

The immutable application source is FreeShow `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, `src/electron/timecode/LTC.ts`. Its dependency is `libltc-wrapper` 1.1.2 at Git `9637e39773317e82e4112ff42b2c63afd144549f`. The installed wrapper's normal `../build/Release/ltc-native` addon is absent in the baseline. That remains an honest source capability limitation; the separate comparison build compiles the unchanged original `src/ltcnative.c` with the pinned Node 22.21.1 headers into a private ignored target directory. It does not repair the baseline or mutate `node_modules`.

`tools/build-original.mjs` verifies all original C/header/JavaScript/binary hashes before compiling. Its receipt records actual compiler arguments, input hashes, Node version and built-addon hash. Tests execute the actual wrapper and that original N-API addon, and independently execute immutable `LTC.ts` with its libltc dependency bound to those real classes. The dependency boundary is observed, not replaced with simulated PCM/packets.

The bundled Linux ELF is `libltc/bin/libltc.so.11`, SHA-256 `f02c75f982fe01d16f345875d2b09404b8b326bb847f2b5456616f6d0227d09f`, SONAME `libltc.so.11`, Build ID `3a8a7058c0af0ea897b687b850a14329f28ef900`. Its public header declares 1.3.1. `Capability.headerVersion` identifies that header, not an independently queried runtime version. The library exports no runtime version function. Its direct ELF dependencies are `libm.so.6` and `libc.so.6`.

The checked-in C shim includes the exact public `ltc.h` and resolves only public function symbols. Rust never mirrors libltc bitfields, opaque codec internals, or `LTCFrameExt`. Compiled Linux x64 observations are `sizeof(LTCFrame)=12`, `LTCFrameExt=368`, `SMPTETimecode=13`, `ltcsnd_sample_t=1`, `ltc_off_t=8`; the shim asserts the critical ABI sizes. The 64-byte plain bridge result is our own structure, checked on both sides. The public header and LGPL/GPL notices are retained in `vendor`; no codec/addon binary is committed. Packaging must supply the native resource and fulfill its distribution obligations before enabling this capability.

## Host API and ownership

```rust,ignore
use freeshow_ltc::{Config, LtcService, Operation, Owner, TrustedLibrary};

// Native trusted resource selection and full captured host context, never IPC input.
let owner = Owner::new(runtime, generation, data_path)?;
let library = TrustedLibrary::from_host(native_resource_path)?;
let codec = LtcService::start(owner.clone(), library, Config::default())?;
let ready = codec.begin(&owner, Operation::CreateDecoder { fps: 25.0 }, deadline)?
    .finish().await?;
let reply = ready.into_reply()?;
// Feed ordered operations, stop/dispose, then await actual terminal ownership.
codec.shutdown().await?;
```

`Owner` and `TrustedLibrary` have no `Deserialize`/renderer surface. Owner identity includes runtime, generation, absolute data path, and a private run identity; matching strings from a different run cannot reuse it. Construct one service per active host run, retain it through draining, and serialize router effects into it. There is exactly one native worker per service and no per-frame task/thread. Context conversion, native resource resolution, pending host effect bounds and fresh-context publication remain host responsibilities.

The worker opens a bounded regular file with final-component `O_NOFOLLOW`, copies at most 2 MiB into a private memfd while computing SHA-256, checks the pinned hash, applies write/grow/shrink/seal seals, and loads that held immutable snapshot through `/proc/self/fd`. Its descriptor outlives `dlclose`. Replacing or modifying the original file cannot replace the loaded bytes. Resource loading/hash/codec construction never runs on the caller/GTK thread. A missing resource, wrong hash, unsupported platform, unavailable memfd/loader or missing symbol yields `Capability { available:false, error }`; codec operations return `ltc_backend_unavailable` without fake allocations.

Operations are `Capability`, `CreateEncoder { fps }`, `CreateDecoder { fps }`, `Encode { hours, minutes, seconds, frames }`, `Decode { bytes }`, `StopEncoder`, `StopDecoder`. Replies are `Capability`, `Done`, `Audio(Vec<u8>)` and `Frame(Option<Decoded>)`. `Decoded` has the original packet fields, including volume, signed sample offset, timezone and date. `Frame(None)` is a native no-frame receipt; the source returns JavaScript `undefined`. The future host must preserve its no-frame/no-callback semantics, not expose the parity driver's JSON `null` as a source IPC payload.

Eight leases cover queued, accepted, and completed-but-uncollected results together. `Ticket::finish().await` returns a consuming `Ready`; `Ready::into_reply()` releases the lease and validates live publication context. Dropping a queued ticket cancels before the C operation. Once the worker accepts an operation, dropping/canceling its caller does not abandon the C call or codec handle; the worker settles it, retaining the codec until replacement/stop/drain. A lost result drops its data and lease only after the real operation returns. Deadlines are checked before acceptance, not used to fabricate cancellation of an already accepted native operation.

Retiring the owner or calling shutdown closes admission and revokes ready publication. The owner cancels queued work, finishes accepted work, frees real encoders/decoders, calls `dlclose`, drops the descriptor and exits; shutdown joins the actual thread. Its timeout retains the same owner/JoinHandle for retry. It never detaches work or treats timeout as drained. The final `Drop` fallback joins synchronously, so the host must await shutdown before dropping on a GUI executor; OS resource opens/loader calls cannot be forcibly interrupted. The host must distinguish context/cancel/admission errors from a real constructor failure when updating source sticky capability flags. Await any accepted source constructor/stop continuation before explicit codec retirement if its source state receipt is still needed, while independently refusing stale renderer publication.

## Source behavior and limits

The supported constructor corpus is numeric 24, 25, 29.97 and 30 fps, fixed 48 kHz, unsigned 8-bit input, default decoder queue 32. The decoder's samples-per-video-frame argument is truncated to `int32` exactly as the original N-API call. Encoder TV standard is 625/50 only for fps 25, otherwise 525/60. Default `LTC_USE_DATE=1`, initial date zeros, timezone `+0200`, default filter/volume, buffer flush-before-encode, and one read after every decoder write are preserved. Frames left in the native queue remain for later reads; the original queue-overflow behavior is measured rather than silently drained/reordered.

FreeShow passes plural `frames` to `LTCEncoder.setTimecode`, while the wrapper adds singular `frame:0`; the C addon reads singular `frame`. Encoding deliberately ignores the plural value, including NaN, and does not increment timecode. Reset/backward-time/framerate decisions stay in the router; creating a replacement codec here frees the old codec after the new allocation succeeds. Incoming decoder packets can still contain nonzero frames and dates from real external audio.

Bounds are checked before native allocation/write: eight leases; 65,536 input bytes per operation; four certified fps values; integer hours 0–23 and minutes/seconds 0–59; at most 24 hours of 48 kHz samples per decoder session; at most 8,000,000 operations per service. Host config may lower these caps. One encoder and one decoder remain live; replacement may temporarily hold two of that codec. The pinned constructor parameters bound native queue/backlog to 32 and samples-per-frame to 1600–2000. The public encoder buffer is checked at construction and capped at 4096 bytes before copying. No exact opaque-allocator byte/performance claim is made. Ready PCM buffers add at most eight times 4096 bytes, queued audio at most eight times 65,536 bytes, plus fixed codec/library state and caller-owned data. Errors are capped at 1024 UTF-8 bytes; each decoded result has fixed fields and a five-character timezone.

Other formats/sample rates/queues, arbitrary flags/date/volume/filter controls, uncertified fps, negative/fractional/nonfinite clock fields and indefinite sample clocks return explicit unsupported/limit diagnostics or have no public operation. These wrapper-wide controls are not used by `LTC.ts`. Decoder tests nevertheless compare actual incoming audio generated using date/frame values and BGF flag modes 0/1/2/4/8. Runtime binary variants, Linux ARM, Windows and macOS return unavailable until their actual ABI/loading and dependency behavior are separately tested. Cross-compilation only establishes that unavailable branches compile. No sound-card capture, output waveform delivery, hardware latency or full end-to-end timecode claim is made.

## Verification

Use the pinned Node and its existing headers; no system installation is required:

```sh
FREESHOW_LTC_ORACLE_DIR=target/assets/ltc-original "$ORACLE_NODE" crates/freeshow-ltc/tools/build-original.mjs
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo test --manifest-path crates/freeshow-ltc/Cargo.toml --locked --all-targets
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo build --manifest-path crates/freeshow-ltc/Cargo.toml --locked --bin freeshow-ltc-parity
FREESHOW_LTC_PARITY=target/assets/debug/freeshow-ltc-parity FREESHOW_LTC_ORACLE_ADDON=target/assets/ltc-original/ltc-native.node "$ORACLE_NODE" --test tests/parity/ltc.test.mjs
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo clippy --manifest-path crates/freeshow-ltc/Cargo.toml --locked --all-targets -- -D warnings
cargo fmt --manifest-path crates/freeshow-ltc/Cargo.toml -- --check
```

The parity CLI defaults to `target/debug/freeshow-ltc-parity` (`.exe` on Windows); `FREESHOW_LTC_PARITY` selects the isolated build. Its JSON is a bounded fixture format, not a renderer command. Library selection comes only from a trusted environment/native fixture path. All source-generated PCM is created in memory by the original addon; no external binary download or audio device is needed.

Nine Rust tests cover explicit unavailable/hash rejection, full run identity, deadlines, eight uncollected leases, real replacement/free/reset/sample/input/operation bounds, queued cancellation, accepted caller-drop with retained timeout/retry, actual joins, immutable loaded bytes and symlink rejection. The accepted-operation barrier is a private test-only pause on the actual codec owner immediately before its real constructor, not a fake codec implementation. Seven differential suites cover four fps, 45 wrapper comparison cases, 30 exact encoder buffers (53,692 compared bytes), 265 decoded packets and 2,009 no-frame reads, plus nine real `LTC.ts` send buffers and nine receive callbacks. Additional fixture waveforms exercise arbitrary chunking, queue overflow, reverse, silence/noise, nonzero incoming frames/date/timezones and BGF/drop flags. Linux strict Clippy/fmt and Windows GNU/macOS ARM library checks pass. The source-only provenance fixture records exact inputs and measured corpus; binaries and receipts with machine-specific paths stay in the ignored target directory.
