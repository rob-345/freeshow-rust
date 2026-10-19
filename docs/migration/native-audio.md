# Native AUDIO composition

`src-tauri/src/audio_pipeline.rs` composes the separately tested audio pipeline,
actual native Opus encoder and actual Icecast TCP owner. The desktop now connects
the retained AUDIO API and real transferred MessagePorts to a bounded raw binary
command. Controlled Linux WebKit PCM, Opus and actual TCP/Ogg bytes match the
original TypeScript and original native addon. Physical capture, server
authorization, playback and application performance remain unverified.

## Integration API and order

Construct one process `NativeAudioPipeline::start(library, trusted_opus_source,
limits, status_sink)`. The library is the same `LibraryService` whose full
`RuntimeContext` is captured by the trusted request boundary. The Opus source is
native configuration; never accept a renderer library path. `capability()` waits
for actual initialization and reports the verified library or a truthful
unavailable reason. Once closing/terminal, it returns an error instead of a stale
available flag. Missing Opus disables the source Icecast encoding path; it does
not turn other sinks into implemented outputs.

`pcm(full_context, raw_bytes, announced_length, AudioMetadata,
SinkAvailability, cancellation)` reserves the host FIFO and whole-result quota
before cloning bounded data. Announced length must equal the raw view length.
Preserve original metadata: absent sampleRate/id become `JsInput::Undefined`,
present JSON stays `JsInput::Json` (a native non-JSON number can use `Number`), and
missing icecast is `None`. Do not default/coerce target IDs or latch config in the
bridge. Native registry/activity snapshots, not renderer claims, supply sink
availability. Original conversion, resampling, config latching, 20ms accumulation
and failed-frame consumption belong to `freeshow-audio-pipeline`.

A single host owner validates the full context again, prepares and accepts one
packet, records the ordered planar sink observations, awaits real Opus encoding
and real Icecast methods for each frame, completes pipeline reconciliation, then
records ordered legacy sink observations **before** accepting the next packet.
The context snapshot check immediately before plan acceptance is the host's
retirement boundary. Effects accepted at that boundary survive later caller Drop,
cancellation, library retirement and shutdown. Queued stale/canceled packets have
no sink or encoder effects. This policy does not retract already accepted TCP
writes. `AudioReceipt` is a sealed native observation, never an original AUDIO
success ACK. Ordinary source-caught encode/send failures appear in ordered
`Effect::Failure` plus pipeline diagnostics; `failure()` is reserved for incomplete
host reconciliation. Prior effects remain in that receipt if reconciliation fails.

NDI, OMT, Blackmagic, OUTPUT_STREAM, WebRTC and RTMP have no native sink in this
slice. An actual source call with empty native registrations records `SourceNoOp`;
a requested unavailable sink records `Unsupported` in its original position. The
host continues the implemented Icecast path and ordered observations, and makes
no claim that the whole packet reached all requested outputs. It does not invoke
hardware stubs, fabricate delivery or reproduce hypothetical exceptions from an
unimplemented hardware library.

`metadata(context,title,cancel)` and `disconnect_icecast(context,cancel)` use the
same FIFO as PCM. Ordinary target/config/socket changes, CLOSE_PORT and library
changes must **not** destroy this host. All three foundations use a private stable
process audio epoch, preserving the original single encoder, resampler and global
accumulator. `context_changed()` only cancels obsolete status completions. Call it
after library retirement; current-context checks also gate every native delivery
and private completion. The coordinator still must reject stale renderer events
at actual handler delivery. Reconstructing the host is a terminal process-audio
boundary; that differs from the original source-global lifetime if done on every
library change.

## Status delivery and shutdown obligations

`status_sink(AudioStatusDelivery)` runs on one fixed native delivery thread. It
must synchronously perform the real trusted native renderer handoff and return a
result promptly. Queue acceptance alone is insufficient: a retained frontend
handler must return a private completion, routed to
`complete_status("main", full_context, private_id, accepted)`. This is a private
native control path, not a source success ACK. Window, complete context, live ID
and single consumption are checked. Foreign/duplicate completions fail. A current
handler rejection or deadline is terminal; obsolete-context delivery is counted
as stale. Both synchronous callback return **and** private completion are required.
Do not let the callback capture a strong host reference and create a shutdown
cycle; app/window handles can be captured independently.

Only `delivery.event()` is the original context-tagged `MAIN.ICECAST_STATUS`
wire event. ID, run, diagnostics and `own_undefined_error()` are separate private
envelope metadata. For the latter, restore the source's own `error: undefined`
property when dispatching an original no-error status; do not add private fields
to source data. The source oracle asserts actual own-property presence. A native
local header write can report connected before any server response: connected and
`Written` receipts never mean server authorization or decoded audio delivery.

Run provenance is registered under an async publication barrier before status
mapping. Old-run statuses retain the context that started that run, including
later error/close notifications after library changes; they are never retagged to
the new library. Batched events already produced during registration are frozen
before old provenance is pruned. The bounded pump waits for real Icecast events
and completion notifications, not an idle polling timer.

Only terminal process shutdown calls `shutdown()`. Close admission, drain accepted
host work and independently await **all** pipeline, Opus and Icecast owners, even
if one reports failure. Then drain/stop the event pump and actually join the one
delivery thread. The first terminal error is sticky across retries. A shutdown
timeout retains the real owners and callback thread; retry the same service. Do
not replace it or drop the runtime/library owner while its drain remains pending.
A callback blocked indefinitely cannot be forcibly canceled; no replacement
callback thread is created. Final host join after its done signal can include TLS
or destructor work; the timeout cannot preempt a synchronous join. There is no
zero-blocking TLS guarantee. Dropped caller futures do not own accepted shutdown
or packet effects. Last-handle Drop closes admission and the retained host drains,
but explicit terminal shutdown is needed to observe the join/error receipt.

`status()` exposes bounded counters and first terminal error for trusted native
inspection. Drain bits are pipeline=1, Opus=2, Icecast=4, status delivery join=8;
`joined` additionally confirms the host JoinHandle was consumed. These are private
observations, not source IPC payloads. The host has exclusive access to all
prepared/accepted foundation plans and calls foundation shutdown after its FIFO
has reconciled or dropped each plan.

## Bounds and limitations

Defaults: host FIFO4, retained whole jobs/results4 (hard8 each), raw input32768
bytes, metadata8192 bytes/depth8/nodes256, at most8 generated frames, total source
accumulator256KiB, native prepared/accepted lease10s, delivery deadline5s,
shutdown wait15s. Sink snapshots cap64 registrations per type,1024 bytes per key,
8192 aggregate bytes. Metadata titles cap8192 bytes and reject NUL/CR/LF. Full
contexts cap runtime ID256 and Unicode path32768 bytes. No file IO is performed.
Explicit resource/unsupported errors are not source empty or successful output.
Pathological metadata coercion remains the foundations' explicitly delimited
subset; this does not claim arbitrary JS objects/prototypes or source exception
partial-state parity.

The host owns one current-thread Tokio runtime thread, one blocking status thread,
one pipeline CPU thread, one Opus CPU thread and one Icecast runtime thread. A
process-global Icecast DNS thread is created lazily for hostnames (one active and
one queued lookup); literal loopback IP tests bypass it. These are five fixed
slice/foundation threads plus up to one DNS thread, independent of library workers,
with no per-packet threads or periodic disconnected polling. OS resolver calls
remain non-preemptible and can delay real shutdown; singleton ownership and queued
lookup caps do not give an OS getaddrinfo deadline. See the Icecast worker doc.

The status thread queue has one slot. The pump has at most one private completion
pending and two retained envelope leases, capped serialized source event+context
256KiB. Icecast queues cap32 events; a pump snapshot is at most64 records including
events collected under the publication barrier. Provenance caps33 runs. Whole host
leases survive completed/uncollected replies and accepted caller Drop; shutdown
can finish while the host still holds sealed completed observations.

Foundation hard queue/input limits remain active. Host Opus call budget and
Icecast write timeout are each at most250ms; defaults use those maxima. Eight
ordinary encode+send pairs fit within the native plan's10s lease, but native C,
allocator/syscall/destructor stalls are cooperative and not preemptible. Opus
budget is checked after actual C returns; an overrun fences the service, reconciles
the source failure continuation where possible, then drains every owner. The
source's unbounded JS accumulator/work and asynchronous non-blocking socket write
callbacks are not a native resource or timing guarantee. Icecast uses monotonic
native ticks and bounded writes; source Date/timer/backpressure differences and
bounded OS/platform support remain documented in its worker doc. No Opus reset is
introduced for ordinary config, reconnect, metadata or library transitions.

## Evidence and reproduction

The focused test is `src-tauri/tests/audio_pipeline.rs`. It includes the owned host
module directly and needs no GTK, Tauri application build or GUI. Coordinator
integration must add `freeshow-audio-pipeline`, `freeshow-opus`,
`freeshow-icecast` to the native manifest/module tree and preserve existing
core/serde/serde_json/tokio/tokio-util/uuid dependencies. Test-only socket2 is used
for a real peer RST. The test uses available tempfile. The temporary independent
headless verification manifest used the host file as `[lib]`, this file as the
`audio_pipeline` integration test, and those path dependencies; it did not edit
shared manifests. Tokio features: sync,rt,net,time,macros,io-util; tokio-util:rt.

Strict Linux run, after native manifest integration:

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 \
FREESHOW_REQUIRE_OPUS=1 FREESHOW_REQUIRE_NATIVE_AUDIO_ORACLE=1 \
FREESHOW_OPUS_ORACLE_DIR=/absolute/private/opus-oracles \
FREESHOW_NATIVE_AUDIO_NODE=/absolute/node-v22.21.1/bin/node \
cargo test --manifest-path src-tauri/Cargo.toml --test audio_pipeline -- --nocapture
```

Build the private immutable original addon/1.3.1 library using the existing
`tests/parity/fixtures/opus/build-oracles.py` and prerequisites documented in
`docs/migration/workers/opus.md`. There are no new downloads, system installs or
node_modules changes. The optional source E2E oracle is separate from mandatory
controlled admission/cancellation/shutdown ownership tests, which deliberately
use an absent library and still run. Absent real codec/original-addon evidence is
explicit stderr; the two strict flags fail the run rather than claim coverage.
Node22.21.1 and the existing TypeScript module are used for the actual TS oracle.
The source helper has a10s watchdog, bounded input/output and only its own inert
127.0.0.1 listener; no production endpoint is contacted.

`support/audio_pipeline_oracle.mjs` executes immutable original processAudio and
IcecastSender plus the original actual N-API methods, not rewritten algorithms.
Revision: `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`.
SHA256 after CRLF→LF:

- `src/electron/audio/processAudio.ts`:
  `51fa05f893c126856ce60f23cda79c1784bfdfceb818ca184ed82da6426d9198`
- `src/electron/audio/IcecastSender.ts`:
  `0492c69db5c5ace4637e1a469e3e14a1cc3b48189f7ab78dc8abc64b3e274bc9`

The same1.3.1 scenario proves actual transmitted encoded packets, first-connect
trigger drop despite encoding, PUT/title/auth bytes, chained OpusHead/tags,
statuses and EOS. Raw TCP bytes are retained throughout the test and every raw
Ogg page CRC is checked before any port/serial normalization or filtering. Real
20ms timers may insert idle silence on a loaded machine; the equality claim is
real packet bytes, all Opus header/tag payloads, flags and statuses. No normalized
CRC can conceal invalid raw wire bytes, and no exact wall-clock scheduling or
cross-version packet-byte parity is claimed. Source pages/CRC/generation are
already covered separately by the Icecast corpus; decoder/quality and1.3.1/1.4
version differences by the Opus corpus.

Tests also cover consumed failed frame/remaining accumulator continuation with an
actual original encode followed by a controlled source exception, native real
encode followed by budget expiry, native unsupported config send failure and
ordered legacy observations, two half-resampled buffers across target changes,
disconnect and library retirement, real401 rejection/cooldown while encoding,
raw length/limits, unknown/foreign/duplicate status completion, stale queued
requests versus accepted caller Drop, result retention saturation, both shutdown
admission orderings, actual peer RST causing sticky EOS teardown error, and a
blocked synchronous status callback whose early private ACK cannot fake drain.
Cross compilation checks build native code/tests without asserting that codec
binaries are packaged or runnable on Windows/macOS. Real codec/host evidence here
is Linux only; deployment library capability and raw AUDIO bridge remain host
integration obligations.

## Shared native verification integration

The coordinator added the foundation path dependencies and this direct host target to native CI. Linux builds private original add-ons and requires real Opus and immutable-source/addon end-to-end evidence; other platforms retain truthful unavailable-capability checks without a packaged codec claim. The initial host was prepared independently; the subsequent raw AUDIO/MessagePort/status integration is described below. Fourteen direct integrated Linux host tests pass with both strict evidence flags; no source-success ACK or rendered playback claim is added.


## Desktop raw input and retained bridge (2026-10-19)

The main window installs `createAudioPortBridge` in the shared Rust compatibility
bridge. INIT_PORT transfers a real MessagePort using the original response shape;
port packets and PCM/CAPTURE fallback copy their exact typed-view range before
reuse. Seven ordinary whole jobs plus one cleanup job, sixteen ports and one
fixed FIFO pump bound adapter-owned input. CLOSE_PORT preserves the source's
ordinary disconnect effects; it does not reset DSP/codec. Source void commands
receive no fabricated success callback. Browser-owned MessagePort queues before
dispatch cannot be strictly bounded without changing the original worklet's
protocol; this is not a whole-browser memory bound.

The actual Tauri `audio_pcm` command accepts only a main-window raw body. A
four-byte little-endian length prefixes at most49,152bytes of JSON followed by
at most32,768PCM bytes. The exact schema carries version1, full native context,
origin and at most8192bytes/depth8/256nodes of finite metadata. Missing and null
remain distinct; bounded escaped JSON pointers restore negative zero, including
nested own prototype-named data. The native parser borrows the PCM suffix and
performs exact binary64 conversion without changing global serde behavior. The
frontend encoder and native parser run together in an actual cross-language test.
Renderer input cannot select an executable codec library or a hardware registry.

One `AudioIngress` adds four whole packet/result leases, one fixed Tokio pump and
one join supervisor. An entered native PCM future remains owned after its IPC
caller drops. Explicit context closure cancels synchronously, bypasses a blocked
ordinary FIFO and retains its group until actual native return/disposal. Old
ready receipts are disposed before their group guard or quota is released.
Unknown older contexts with no owned work cannot close the current group; foreign
runtime/path identities are rejected. At most four lifecycle callers may retain
drain futures, including unpolled futures. Busy rejection still applies
synchronous closing/cancellation. Private accepted/completed packet counters
prove actual native admission/return for tests; they are not source ACKs and do
not imply successful encoding or delivery by themselves.

The native status handoff carries a private UUID/run and own-undefined marker.
After exact context and envelope validation, the frontend restores original
`connected,connecting,error` order and `error: undefined`, invokes unchanged
MAIN listeners, then completes the private delivery even if a listener throws.
Obsolete events cannot retag the current library. MAIN.CLOSE attempts native
shutdown even after frontend audio retirement fails, so independent native
owners still receive real drain/join attempts before final library shutdown.

The local153-step Linux editor, four native menu checks and both actual file-drop
scales passed. Three controlled audio phases ran in one application process:
normal use, after library selection and while the same real64MiB ZIP compression
remained active. Twelve actual encoded inputs used one continuous original addon
encoder. Native/source HTTP, tags, lacing and real Opus payloads matched after the
explicit serial/idle-page normalization documented in
[the desktop helper contract](workers/audio-desktop.md). All original CRCs,
sequences and granules were checked before normalization, every stream ended
with EOS and every TCP peer thread joined. Observed status ownership/order and
zero source ACKs are retained in `test-output-rust/desktop/audio-stream.json`.
The probe reports controlled PCM and browser worklet availability separately;
it does not claim an actual AudioWorklet/device recording or decoded playback.

Connected-idle context retirement is being strengthened separately: packet/group
drain alone does not prove an already connected socket closed. The full parity
gate remains incomplete while that actual native lifecycle proof and the wider
capture/output/backend cases remain open.
