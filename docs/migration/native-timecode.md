# Native timecode

The retained Svelte interface now routes its five MAIN timecode commands through
the Rust host. This is a partial migration. Physical MIDI/LTC devices, browser
capture/playback, complete SysEx support and distributed codec resources remain
unverified or incomplete. No application performance improvement is claimed.

## Source and transport contracts

`TIMECODE_START`, `TIMECODE_STOP`, `TIMECODE_VALUE` and `TIMECODE_STATUS` use a
bounded typed binary envelope. Undefined, null, raw primitive framerate values,
NaN, infinities and negative zero are preserved where supported. Finite nested
JSON retains property order and negative zero; unsupported nested undefined,
special numbers, custom objects and lone UTF-16 surrogates fail explicitly.
Commands remain void. STATUS records a native diagnostic; it does not gain a
renderer success callback.

Explicit `{mode:"LTC",buffer}` input uses a separate 64 KiB raw frame. The
adapter snapshots the actual Buffer.from-compatible elements/view range before
the caller can reuse it. The original worklet posts a raw Uint8Array, while the
original backend requires a wrapper with `mode`. That source route is a no-op;
the migration preserves it rather than silently adding decoding to that route.

Native events carry private hydration, a full runtime/library context, a safe
run ID, a process-wide monotonic decimal sequence and a one-use UUID. The shared
bridge restores real Uint8Array audio and whole-value IEEE numbers before
retained synchronous MAIN dispatch. A private completion bounds that dispatch;
it is not an original timecode ACK or proof of later playback.

The native owner can publish its next notification after consuming a completion
nonce but before the invoke response reaches JavaScript. One validated, hydrated
next publication is retained across that response ordering. Its bytes/context
are copied, excess and replayed entries reject, and retirement clears staged
old data before context adoption. See [handoff regression](workers/timecode-publication-handoff.md).

## Ownership and concurrency

One fixed Tokio supervisor owns sixteen whole command/result leases, bounded
publication storage and the source router. Native MIDI port owners are lazy and
bounded to sixteen; LTC has one lazy fixed codec owner. These are ownership
bounds, not a measured speedup. Browser IPC queues outside admitted work are not
claimed to have a strict byte bound.

The renderer has seven ordinary jobs, one independent cleanup continuation, one
drain observer, one completion transport and one publication handoff slot. The
main window exposes four immediate retirement observers; the host also owns
four shared immediate lifecycle permits across context change and shutdown,
including generic CLOSE callers. Raw invokes, completion
and lifecycle routes enforce native window/context authority; renderer fields
cannot select hardware backends, native libraries or native run identities.

Source STOP frees current encoder/decoder resources but remains reopenable.
VALUE after STOP uses the retained current mode/framerate. Native context
retirement instead drains all accepted source effects, overwritten/late MIDI
handles, codecs and actual worker joins before resealing the router. Raw source
mode/framerate, sticky constructor failures and the shared status gate survive
that reseal. Publication sequence numbers never reset between native runs.

Closing attempts timecode shutdown alongside other independent native owners
before the library write fence. A frontend cleanup failure still reaches native
CLOSE. Accepted effects and the genuine supervisor JoinHandle remain owned after
caller cancellation and timeout. Terminal context observers return the original
failure after actual drain/join; a clean terminal host cannot falsely renew.
See [lifecycle regressions](workers/timecode-lifecycle.md).

## Real LTC capability and limits

The supported backend identifies the existing Linux x64 ELF by SHA-256
`f02c75f982fe01d16f345875d2b09404b8b326bb847f2b5456616f6d0227d09f`.
It snapshots that verified resource into immutable native-owned storage and
uses a C shim compiled against the unchanged public libltc header. No private
opaque layout is guessed. Header version 1.3.1 is not a queried runtime-version
claim. Actual original N-API code is built privately for source comparisons.

The native launch environment may supply `FREESHOW_RUST_LTC_LIBRARY`; otherwise
the host resolves its application codec resource path. No binary is committed
or packaged yet. Missing/other-platform capability remains explicit. Notices
and the resource distribution obligations are recorded in
[the vendor notice](../../crates/freeshow-ltc/vendor/NOTICE.md).

Real comparisons preserve original wrapper defaults and the source plural
`frames` versus addon singular `frame` behavior. Constructor failure, failed
replacement, backward time, STOP/reopen and one-frame-at-a-time decoding retain
the source rules. Native allocation/input/session bounds can reject extreme
source inputs; they do not certify every malformed or unbounded source variant.

## Verification

The current local workspace passes 390 Rust, 419 pinned-Node and 20 frontend
tests, with four existing OS/capability skips. The full Linux native run passes
246 native test executions: 236 host integration executions and ten desktop
binary unit executions, including shared cases repeated by importing targets.
That run includes 22 timecode ownership/source tests and eight raw framing tests.
Strict workspace/native all-target Clippy and focused shared-bridge TypeScript
pass. Native observer bounds and the real desktop replay pass. Published commit
`e81f315f` passes [all six CI jobs](https://github.com/rob-345/freeshow-rust/actions/runs/39213713551).
Downloaded green-run artifacts independently verify 156 editor steps, three
exact source/native LTC phases, real STOP frees, four menus and both drop scales.
The raw editor receipt preserves unrelated expected capability/stale diagnostics.

The [desktop probe and source oracle](workers/timecode-desktop.md) require real
native receipts, exact original addon audio bytes, decoded values, original
typed envelopes, offset/reused-input snapshots, void commands and actual STOP
resource frees. One source VM replays all phases using only explicit START/STOP
boundaries. Source-only diagnostics cannot count as native or GUI parity.

The final 156-step Linux editor, four native menu checks and both real file-drop
scales pass against the rebuilt binary. Three phases use one native runtime and
one original source VM/addon: normal at 25 fps, after selection at 29.97 fps and 25 fps during
the same actual 64 MiB ZIP compression. Each phase matches 13 real encoded audio
callbacks and 11 decoded callbacks; the raw feeds total 23040/19219/23040 bytes.
Offset-seven views have poisoned surrounding bytes and are mutated immediately
after send. No phase has a source ACK, timecode runtime error or browser error.
The selected-library checkpoint proves actual old codec/port owner joins. Final
STOP frees codec resources while the service remains live/reopenable; actual
terminal shutdown is separately exercised by native ownership tests and the real
Quit/save/exit workflow. Exact controlled codec results do not certify physical
capture or playback. Raw receipts are in `test-output-rust/desktop/timecode.json`.
