# Native timecode host

`src-tauri/src/timecode.rs` composes the pure timecode router, existing real MIDI
port owner/MTC parser/packet generator, and verified native LTC owner. This commit
contains the host and headless tests. Coordinator integration still owns native
manifest/module registration, MAIN routing, raw audio ingress, renderer hydration,
private completion and application shutdown. There is no hardware timecode,
capture/playback, ALSA availability, physical MIDI delivery or performance claim.

## Trusted API and source boundary

Construct one process `NativeTimecode::new(library, config, trusted_ltc_library,
backend_factory, publisher)` on the retained Tokio runtime. The backend factory
constructs a cheap native Backend value; all actual enumerate/open/send/close
calls run on `MidiService` owners. It must never be chosen from renderer data.
The LTC path is a native `TrustedLibrary`, not a renderer path or a system-library
lookup: the LTC owner verifies the exact supported ELF hash and immutable memfd
snapshot before loading it. Source ordinary MIDI handles remain separate from
MTC handles, as in `midi.ts` versus `MTC.ts`.

`dispatch(&Request, "main", cancellation)` accepts exactly the five original
`MAIN.TIMECODE_*` commands with full `RuntimeContext`. `TIMECODE_AUDIO_DATA` uses
`audio(context, mode, raw_bytes, announced_length, cancellation)` instead of JSON
byte expansion. `command(context, router::Command, cancellation)` is a native typed
boundary, and `value(context, Number, cancellation)` carries IEEE special numbers
without pretending JSON null is equivalent. Announced byte length must match.
The five wire handlers remain source calls without a successful IPC ACK.
`TimecodeReceipt` is a nonserializable native observation: Scheduled means a router
continuation entered bounded native supervision; it can be a verified source no-op
when no handle exists. It does not mean a port opened, bytes were delivered or an
OS close occurred.
An asynchronous later rejection appears in bounded native diagnostics/status.
The coordinator must consume observations internally, not publish them as replies.

Missing scalar/data properties remain `Undefined`; null is retained separately.
START type/mode must be bounded strings; complex/custom framerate coercion and
malformed outer shapes reject explicitly. Pure routing retains raw primitive
framerate truthiness/type before any IO. Actual native MTC IO supports finite
numeric fps; LTC supports verified numeric24/25/29.97/30 and finite nonnegative
encode time. Unverified variants reject without claiming original exception
partial-state equivalence. Unknown modes, including current source ableton
routing, keep their actual source no-op behavior. Incoming STATUS is only a
bounded native LogStatus observation; it does not update the shared status gate
or emit a renderer status.

Full context is checked at admission and immediately before the source router
call. Queued cancellation/stale context has no source effect. Once the source
call issues a real effect, its IO ticket, router token and whole-job lease remain
owned despite caller future Drop or later cancellation. Existing cached MTC
sends and synchronous source constructors/decode/encode/STOP continuations block
new source commands, callbacks and asynchronous router completions until they
settle. Actual native IO owners continue independently during that barrier.
Source STOP waits for an accepted cached eight-packet send before closing it.

## Async quirks and finite owner pool

The router preserves one synchronous continuation plus eight asynchronous slots.
Pending MTC receives are not deduplicated. An open can finish after STOP; later
completions overwrite the current source receiver map. Older accepted inputs
remain real owned orphans with their own parser state and can still produce source
callbacks. Source STOP closes only the current sender, then current receiver-map
entries in original order. A sender-close exception prevents receiver closure;
a receiver-close exception preserves the source map/tombstones and aborts later
closes. Current mode, send data, raw fps and last-status gate survive STOP.

The MIDI foundation intentionally has one MTC listener and one cached MTC output
per service. The host therefore uses at most8 input owners and8 sender owners,
lazily created in a finite16-slot pool. Simultaneous first sends can open distinct
outputs; their actual completion order assigns the current sender, and overwritten
outputs remain owned. Later sends use the actual current cached sender and ignore
new selector data, matching the source cache. STOP removes only that current
output; terminal retirement joins every accepted orphan. No selector, JZZ index
matching or MTC packet algorithm is duplicated here.

A slot is not reused before its real service joins. Failed pre-admission opens
still retire/join their allocated empty owner. If a source close succeeds but the
native thread join times out, a separate closed guard preserves that fact across
retry; the host does not call an obsolete handle again or invent a joined receipt.
The source's unbounded concurrency/leak behavior stops at the explicit finite
quota. When multiple OS operations become ready together, bounded polling can
choose a different completion order from JZZ's event loop; controlled ordering
claims use explicit gates. The16-owner architecture is a correctness tradeoff,
not a Rust performance improvement. A future shared multi-listener owner can
reduce threads without changing these source lifetimes.

## Retirement, global state and actual LTC

Call `context_changed()` after a library change. It cancels obsolete private
publications, marks the old router retired, requests cancellation of pending IO,
and independently starts draining all MIDI owners. Accepted native calls still
settle; source terminal cancellation may stop subsequent packet sends in a batch,
but the accepted call/partial effects and real close remain owned. Source STOP
uses the stronger ordinary source ordering described above and does not perform
this terminal cancellation.

An accepted LTC synchronous receipt is collected before closing its codec owner.
That avoids changing a successful C constructor into a false source capability
failure merely because its context retired. Real constructor failures use the
source Failed receipt and sticky exact ALERT; repeated attempts do not retry that
constructor. Demonstrably nonaccepted/pre-C resource/admission rejections retire
the run and use retired-only `Completion::Abandoned`, preserving source failure
flags. This receipt cannot dispose an accepted codec or be used on a live router,
a foreign token, an encode/decode or repeated continuation.

Retired LTC stop effects await actual whole-codec shutdown/free/dlclose/join. This
cleanup bypasses an exhausted work quota; an old accepted encoder still has to be
freed when a replacement constructor could not be admitted. No fabricated audio
or decoded frame is emitted. Ordinary source stop uses actual individual codec
stop calls; failed replacement retains the previous encoder until disposal.

Only after `Router::drained()` **and** all port and codec service joins are proven
may `renew_after_drain()` establish a fresh private router identity. It preserves
process-global raw fps/mode/sendData/status and LTC constructor-failure flags while
resurrecting no handle, old token or callback. New callbacks have new full-context
and run provenance. `status().lastRetired` retains one bounded actual-join/counter
observation; it is not source IPC data. Native context cancellation and source
STOP are separate policies.

`shutdown()` is terminal: close admission, retire, drain and join all actual
owners before joining the supervisor. A timeout retains the same services/tickets
for retry, without replacement threads. Every independent MIDI owner is polled
for shutdown even while another OS close is blocked. The first terminal delivery
failure remains sticky on repeated shutdown. Failed/unproven native cleanup
retains logical ownership and returns an error rather than renewing or reporting
complete teardown. Port OS calls, native C and destructors are cooperative and
cannot be forcibly interrupted. The outer wait timeout does not prove real exit.
After native drain progress completes, shutdown awaits the retained supervisor
JoinHandle in place under its async mutex. Canceling or timing out that observer
releases the mutex while retaining the live handle; retry still awaits actual
task exit. Once the join settles, the handle is taken before propagating its
result so a finished handle is never polled twice. A collected supervisor panic
is retained as the first terminal failure on later shutdown calls. A test-only
gate holds actual supervisor return after progress completion to verify this
distinction independently of port-drain progress.

Keep the Tokio runtime and LibraryService alive until this terminal drain has
settled; last-handle Drop requests the same retained drain but supplies no receipt.

## Publication and bridge obligations

Publisher receives `NativeTimecodePublication` and must synchronously execute a
real, bounded, prompt native handoff. It must not block or grow an unbounded
WebView queue. The retained main handler must then call private
`complete("main", full_context, id, accepted)` after actual context acceptance,
hydration and handler delivery. One completion is pending at a time. Wrong window,
foreign context/ID, duplicate, lost, rejected or timed-out completion cannot count
as delivered. Current-context delivery failure fences admission and drains actual
owners; stale library publications are suppressed rather than retagged.

Only `publication.event` is original source data: MAIN/TIMECODE_VALUE,
TIMECODE_STATUS, TIMECODE_AUDIO_DATA or ALERT. Private delivery ID/context/run and
`hydration`/`bytes` remain outside that payload. Restore special numbers from
`Hydration.numbers` relative to event.message.data; an empty path means the whole
value. Restore source audio bytes from the private bounded byte envelope after
full context acceptance, not from a JSON null placeholder. The exact packaged
Electron Buffer-to-renderer class and retained Tauri byte hydration remain bridge
integration evidence, not a claim from this headless host. Supported decoder and
MTC fixtures emit finite values; the shared Number representation keeps any future
special-number transport explicit. Incoming top-level undefined STATUS remains
native data-only logging intent, not an invented renderer event.

Private `delivery.sequence` is a positive decimal-string u64 assigned by the
ordered publication owner from one process-wide checked counter. It does not reset
on router/context renewal or native host replacement. The renderer keeps one last
accepted sequence across context changes and rejects values less than or equal to
it before retained delivery. Gaps are valid. UUID completion remains the native
one-use authority; sequence exhaustion is an explicit resource failure. No source
event or source handler sees this private field.

## Aggregate bounds and execution limits

Host FIFO and retained whole jobs/results:16. A permit survives completed results
and asynchronous effects until both are collected. Router:8 listener reservations,
8 async effects,1 synchronous effect. Source audio:65536 bytes. START kind32,
mode/scalar text4096; JSON1024 nodes/depth8/65536 aggregate UTF-8 keys/values.
Context runtime ID256 and Unicode path32768. Native diagnostics retain a saturating
count and one last2048-byte message; each active receipt caps32 observations.
Private publication queue:16 records/1MiB aggregate, each serialized envelope at
most512KiB, one pending private completion. Defaults:5s operation/delivery bounds,
10s lifecycle wait; host timeouts cap30s.

Aggregate MIDI limits for this dedicated timecode pool:16 IO owner threads,
8 physical input handles and8 physical output handles (each service is used only
for its single MTC role), at most9 source IO tickets across the router's async/sync
slots, plus at most16 join futures. Each input owner has the foundation's256 raw
and256 decoded-event slots; therefore at most2048 raw and2048 MTC event records
are retained across eight inputs. Raw packets cap12 bytes. Decoded events include
bounded full Owner provenance: worst-case path/runtime bytes can contribute about
68MiB across those2048 event records, in addition to bounded channel/record/JSON
and stack overhead. Native backend/platform allocations and OS buffers have their
own limits. At most256 selected device candidates and8 MTC packets are processed
by each foundation command; deadlines do not preempt a blocked OS call.

The host adds one ordered Tokio task, not one task/thread per frame. LTC adds one
lazy fixed codec thread with8 work/ready leases and its documented sample/work
limits. Native MIDI drivers may add callback threads: cached midir0.11.1 ALSA
source creates one input handler thread per input connection, up to8 more here.
The ordinary MIDI service and its own driver callbacks are additional, separate
application owners. With its8-input/16-output limits, ordinary MIDI plus timecode
can account for16 physical inputs/24 outputs,17 MIDI IO owner threads,1 LTC
owner and up to16 ALSA input-handler threads (34 foundation/driver threads,
before the shared application runtime and other features). Independent native
opens may also differ from JZZ's shared physical-port engine when a device or
Windows driver requires exclusive access; this pool has controlled logical
ownership evidence, not physical-driver equivalence. Backend idle/health polling and LTC receive timeouts remain
those foundations' existing behavior; this commit makes no idle/performance claim.
No broad native/UI build or hardware initialization was used for these tests.

Physical full-frame SysEx is currently ignored/rejected by the MIDI backend even
though the pure MTC parser supports it. Real port naming/permissions, unplugging,
Windows/macOS hardware drivers, callback scheduling and OS closure failures need
platform evidence. Native callback wallclock is captured at bounded callback
arrival, not at a delayed JavaScript event-loop callback; clock timing can diverge
under queue pressure. These are explicit gaps, not successful empty parity cases.
LTC is available only for the worker's verified Linux x64 binary/ABI. Resource
packaging, capture/output device selection and production bridge remain pending.

## Evidence and reproduction

Focused headless tests live in `src-tauri/tests/timecode.rs`; the module is included
directly and needs no GTK/Tauri application build. A temporary standalone manifest
used path dependencies on core, midi, midi-ports (default-features=false for the
controlled backend), timecode and ltc, plus serde/json, tokio(sync/rt/time/macros),
tokio-util(rt), uuid(v4), and test-only tempfile. Coordinator adds dependencies to
the native shared manifest; no shared manifest was edited here. Production native
MIDI feature prerequisites, including ALSA development files on Linux, remain
separate from controlled-handle evidence.

Build the private original addon with the existing pinned-source helper, without
installing packages or modifying node_modules:

```sh
FREESHOW_LTC_ORACLE_DIR=/absolute/private/ltc-original "$ORACLE_NODE" crates/freeshow-ltc/tools/build-original.mjs
FREESHOW_REQUIRE_LTC=1 FREESHOW_REQUIRE_TIMECODE_ORACLE=1 \
FREESHOW_LTC_ORACLE_ADDON=/absolute/private/ltc-original/ltc-native.node \
FREESHOW_TIMECODE_NODE="$ORACLE_NODE" \
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 \
cargo test --manifest-path src-tauri/Cargo.toml --test timecode -- --nocapture
```

The addon builder requires pinned Node22.21.1 plus existing C tools/headers and
verifies installed original wrapper/C/header/ELF hashes. The source oracle executes
actual immutable timecode.ts, MTC.ts and LTC.ts modules, with original actual N-API
LTC methods and a controlled JZZ port seam. It does not rewrite packet/routing or
waveform algorithms. Source revision is
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`; SHA256 after CRLF→LF:

- timecode.ts: `18506a959deaf9053f5bae84235963e2dbf3779688d5a3d646a4995a65820a79`
- MTC.ts: `d0127f1074686d169e7deeae708a2f4ef281bac2f028f2d75b0bc400a7ebab2c`
- LTC.ts: `f4cbf2b19e2f6a8e22967c2409c2acca72b03e1f027dcead85dec318f12aaacf`

Strict Linux evidence includes actual waveform bytes/reset/backward-time handling,
one-frame decode/notification order, exact MTC packets (including NaN coercion),
status/STOP behavior, unavailable sticky alerts, pending/overwritten/orphan handles,
16-owner saturation, accepted caller Drop, source close short-circuit, native quota
failure without false constructor alert, real frees/joins before renewal,
context-stale private completions, result/byte/JSON bounds and delivery timeout.
Rust JSON integer/float storage variants are compared as the same JavaScript
Number; raw audio hex remains byte-exact. If the actual original addon or supported
codec is absent, tests print that evidence gap explicitly; strict flags fail rather
than claim a real comparison. Mandatory controlled ownership/source-MTC cases do
not depend on a system codec or hardware. Windows/macOS cross checks establish
buildability, not runtime capability, packaged resources or hardware parity.
