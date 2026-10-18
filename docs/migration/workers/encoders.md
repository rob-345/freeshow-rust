# Encoder detection and profiles preparation

This crate ports the unchanged `src/electron/streaming/encoderDetection.ts` and
`encoderProfiles.ts` from Electron parity oracle
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975` (1.6.7-beta.2). It prepares detection,
encoder commands and native host intents. It does not resolve/download/install
FFmpeg, write the application's cache/configuration, start RTMP relays, or own
capture lifecycle. No GPU is marked working from an encoder-list entry alone.

## Host API and authority

`EncoderService::new(EncoderConfig)` starts one dedicated current-thread Tokio
runtime on one owned OS thread. The constructor does not require an entered Tokio
runtime. The host supplies `FrozenDetectionInput` containing:

- Its trusted generation/epoch and resolved absolute executable, or `None` when
  FFmpeg is unavailable. Native execution authority is separate from
  `FrozenExecutable.source_path` and `mtime_ms`, which preserve the source's exact
  cache-key spelling and numeric mtime. A source PATH lookup can use `"ffmpeg"` and
  mtime zero in the key while executing a separately resolved absolute path.
  For a stat-able source path, preserve fractional Node fs.Stats `mtimeMs`
  (`seconds * 1000 + nanoseconds / 1e6`), rather than the rounded Date value used
  by HTTP ETags.
- Frozen Node platform/arch spellings (`linux`, `darwin`, `win32`; `x64`, `arm64`,
  etc.), VAAPI device string, and optional cache **bytes** read by the trusted host.
  The VAAPI default is `/dev/dri/renderD128`; empty device preserves the source's
  environment fallback. No process environment is mutated by this crate.

`detect(input, force).await` returns `PreparedDetection`, with raw `detection:
serde_json::Value`, source (`missing/cache/probe`), generation/probe ID, bounded
diagnostics and an optional native-only `CacheWriteIntent`.
`force_requested(payload)` implements the original `data?.force` JS truthiness;
using only `force == true` would change truthy string/array/object inputs.

For `MAIN.ENCODER_DETECT`, return only the original detection object to the
renderer **after** detection completes and the host has handled any cache intent.
The envelope, diagnostics and intents are host metadata, not a new UI protocol.
Under its trusted owner, revalidate the frozen generation and freshly computed
executable cache key with `intent.ensure_current(generation, key)` before atomically
writing `intent.bytes()` to the expected trusted user-data
`encoder-detection.json`. The intent is not deserializable. The host owns path,
parent/identity checks, serialization of competing writes, and final commit.
Coalesced callers receive the same probe ID/intent; one trusted owner should
deduplicate persistence. Source cache-write errors only warn and still return
detection, so a final cache write failure must not be fabricated into a successful
write or unnecessarily suppress the valid detection reply.

`resolve(input, setting: Option<&str>, disable_hardware: bool).await` returns
`PreparedResolution { encoder, detection: Option<PreparedDetection> }`. Preserve
and handle that detection's cache intent too. For auto/empty settings and exactly
boolean `disableHardwareAcceleration === true`, it returns x264 without detection
or child work. The host must pass the strict boolean comparison, not truthiness.
An explicit setting still probes/uses cache and can select available hardware.
`resolve_detection` is the pure cached-result field-access helper.

`set_rtmp_encoder(output_id, encoder)` stores the bounded ephemeral setting and
returns `SettingIntent { output_id, encoder, reconfigure_rtmp: true }`. Empty
encoder becomes `auto`, unknown strings remain unchanged, and repeated identical
sets still ask for reconfiguration. `rtmp_encoder` reads the ephemeral setting or
`auto`. The actual `MAIN.SET_RTMP_ENCODER` source sets this map and invokes
`CaptureHelper.Lifecycle.updateRtmpState()` with **no success ACK**. The host must
apply the reconfiguration intent through its capture owner and must not add a
renderer ACK or app-config persistence. Retained output settings already have
their separate renderer persistence path.

Keep accepted host operations owned across dropped IPC reply futures, through
cache commit/reconfiguration and completion. The service owns admitted probes
even if a detection caller drops its future, but a dropped caller cannot itself
consume or commit the resulting preparation. The host must reject stale final
effects/replies. Call `shutdown().await` at terminal shutdown: it closes admission,
cancels all probes, waits for children and worker completion, destroys the private
runtime, then joins the OS thread. It is idempotent and has capacity independent
of ordinary callers. Dropping the last service requests cancellation; only an
awaited successful shutdown proves completed teardown.

## Source behavior retained

Profiles keep insertion order x264, videotoolbox, nvenc, qsv, amf, vaapi; exact
labels/codecs/platforms, filters, pre-input VAAPI setup, profile-specific flags,
CBR/VBR choices, JS number formatting/Math.round, GOP, audio pipe/silence input,
FLV output and relay copy arguments are source-compared. Command builders prepare
arguments only; they never start network streams. Unknown ordinary IDs use the
source x264 profile fallback. Source object-prototype keys are not valid profiles;
command builders reject them explicitly.

Detection lists with `-hide_banner -encoders`, then sequentially executes the
source's exact three-frame 256x144 black lavfi test for each listed profile
supported on the frozen platform. Unsupported profiles are omitted. Missing
executable instead returns all six unavailable profiles with `FFmpeg not installed`,
even for other platforms. Failed listing still returns the source's supported
profiles with `Not included in this FFmpeg build` and prepares a cache write.
Listed encoders can fail actual initialization. Recommended is the first working
hardware profile in source order, otherwise x264, including when x264 itself is
unavailable. VideoToolbox retains `-allow_sw 1`; source hardware classification is
not a claim that the GPU performed the encode.

`first_error_line` uses the first nonempty JS-trimmed stderr line, up to 200 UTF16
units; otherwise `Timed out` for a killed timeout, then the error message or
`Test encode failed`. Partial stderr survives timeout/output-limit cancellation.
Node/FFmpeg pointer addresses remain in the actual results. Live comparisons
normalize only volatile hexadecimal addresses when comparing independently run
FFmpeg errors; returned results are not normalized.

Cache version/key is exactly `1|sourcePath|mtimeMs|platform|arch`. The original
omits device, driver state, disable-HW setting and profile-table version from this
key; those omissions remain. Matching truthy raw `detection` JSON, including
unknown fields, malformed shapes, arrays and primitives, is returned unchanged.
Missing/null/false/zero/empty-string detection, bad syntax, version mismatch or
key mismatch proceeds to a probe. There is no implicit in-memory result cache:
the host must provide the newly committed bytes on later calls. A force ignores
the supplied cache.

The cache parser preserves finite JS binary64 rounding from raw numeric tokens,
negative zero, subnormals, a one-ULP regression, escapes and duplicate-key
replacement with original insertion position. It uses only nonsemantic
`serde_json/raw_value` alongside the existing preserve-order convention; it does
not enable global `float_roundtrip`/`arbitrary_precision` features.

Explicit resolution stops at the first matching cached ID, uses JS truthiness of
its available field, and otherwise falls back to x264. Invalid cached `.encoders`
or null entry field access returns `invalid_cached_detection`; it is not replaced
with successful default detection. Unknown string recommendations remain raw
strings as in source. Undefined/non-string recommendations cannot satisfy the
typed native resolution result and are explicit errors.

## Bounds, lifecycle and intentional differences

Defaults: two owned probes, sixteen admitted callers/replies, a sixteen-entry
request channel, sequential children per probe, 5 s per child, 35 s cooperative
whole-probe deadline, 4 MiB **per pipe** for listing, 1 MiB per pipe for test
encodes, 4 MiB supplied cache, 256 settings and 4,096 UTF8 bytes each for output ID,
encoder setting and device. Generated child arguments are capped at 256 arguments,
16 KiB each, 64 KiB aggregate; source/authority paths are capped at 16 KiB.
Configuration validates upper bounds (four probes, sixty-four callers and at most
60 s operation timeout). Process buffers retain only their configured prefix;
diagnostics are bounded to the finite profile count and 4 KiB per message.

Nonforce calls join the newest active probe with matching generation, executable
authority, exact key and device. Force starts a new probe subject to capacity;
it never silently returns the old probe/cache. Saturation is an explicit
`caller_limit` or `probe_limit`, rather than an unbounded queue or fake cached
success. The source has unbounded overlapping forced calls and a global inFlight
finally race where an older completion can clear a newer promise. Native jobs
are independently owned and completion only removes that job. Cross-generation
or changed executable/device calls cannot join an unrelated frozen context. These
are deliberate concurrency differences.

Both child pipes drain concurrently. Windows uses CREATE_NO_WINDOW; stdin is
closed. Normal exit, error, timeout, output limit and cancellation explicitly
kill when needed and await `wait()` before returning the result or releasing
child accounting. A whole-operation timeout/cancellation is an explicit failure
with no cache intent. Successful shutdown verifies no retained direct children.
There is no `spawn_blocking`, filesystem worker or hidden blocking-pool job; one
fixed runtime/thread owns all child tasks. Runtime teardown precedes completion
notification; final thread join includes an unmeasured TLS/destructor interval.

These are cooperative application deadlines, not OS process containment. Spawn,
kernel kill/reap and final runtime/OS destruction are not preemptible; supervision
covers the direct trusted FFmpeg child, not arbitrary descendant process trees.
The source's FFmpeg arguments retain its internal thread policy, with no invented
CPU/thread/cgroup guarantee. A child reaping failure is explicit and prevents a
successful shutdown claim. OS spawn/exit diagnostic wording can differ from Node;
FFmpeg stderr-based reasons and source fallback decisions are preserved.

Additional explicit representation limits: overflowing JSON numbers and lone
UTF16 surrogate cache strings cannot cross the native finite UTF8 JSON contract
and return `json_number_overflow`/`unsupported_utf16`. A first-error-line cutoff
splitting a surrogate pair emits U+FFFD instead of a lone surrogate. Prototype
output IDs can expose inherited objects/functions in the source's plain map;
the native string getter reports `unsupported_prototype_setting` instead. A
`__proto__` primitive setting assignment is ignored while still producing the
source's reconfiguration intent. Hardware-positive results on unavailable GPU
machines are not simulated, and no Windows/macOS hardware/runtime execution is
claimed from Linux tests.

## Focused evidence and integration

`tests/parity/encoders.test.mjs` evaluates both entire actual pinned TS modules,
asserting normalized source hashes. Electron user-data, configuration, resolver
and cache read/write boundaries are injected; cache writes are captured in memory.
The real Node child_process execFile executes local FFmpeg or native deterministic
failing/gated child fixtures. No final application cache/config write, download,
install, external stream or production output file occurs. Standalone source
runtime is Node 22.21.1 / TypeScript 4.9.5. Packaged Electron embeds different
V8/ICU tables; this is not packaged-app UI validation.

The real Linux acceptance binary is `/usr/bin/ffmpeg`, version
`6.1.1-3ubuntu5`, SHA256
`ed16af623947494a72e284b6eb8ff225f2da22b38b5d5069c2fd4b4ba3384e41`.
Its genuine x264 test works. NVENC fails to load `libcuda.so.1`, QSV fails MFX
session creation (-9), and VAAPI finds no display at `/dev/dri/renderD128`.
Recommended is therefore x264. Listed hardware profiles are tested and reported
according to these actual initialization results, not listing text.
Source/native comparison includes profile table and argument corpus, all platform
filters, regex boundaries/Unicode whitespace, first-error handling, missing/failed
listing, raw caches/force/invalidation, disable-HW/explicit selection, settings,
in-flight sharing/force saturation/older completion, different generations,
dropped caller, cancellation/kill/reap and failure recovery. Gated child PID
witnesses prove direct children are gone from Linux `/proc` after shutdown.
Timeout/output-limit fixtures always fail; none impersonates working hardware.

Focused local result: **6 Rust tests and 12 source/live Node tests pass**;
Clippy `--all-targets -- -D warnings`, Cargo formatting, Prettier and diff checks
pass. Windows/macOS runtime and positive hardware initialization remain untested
by this Linux slice.

Local focused commands reuse the agent's existing worktree-only target to avoid
duplicate disk use (no root/native/release build):

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" cargo test --manifest-path crates/freeshow-encoders/Cargo.toml
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" cargo clippy --manifest-path crates/freeshow-encoders/Cargo.toml --all-targets -- -D warnings
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" cargo build --manifest-path crates/freeshow-encoders/Cargo.toml --example encoders-parity
FREESHOW_ENCODERS_BIN="$PWD/crates/freeshow-remotes/target/debug/examples/encoders-parity" FREESHOW_FFMPEG_PATH=/usr/bin/ffmpeg node --test tests/parity/encoders.test.mjs
cargo fmt --manifest-path crates/freeshow-encoders/Cargo.toml --check
npx prettier --config config/formatting/.prettierrc.yaml --check tests/parity/encoders.test.mjs docs/migration/workers/encoders.md
```

Coordinator integration removes this crate's standalone `[workspace]`/lockfile,
adds shared/native dependencies, supplies trusted executable/platform/cache inputs
and handles typed intents. No shared runtime, manifest, media/FFmpeg manager,
vendor or frontend changes are included in this slice.
