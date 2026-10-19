# Retained frontend timecode adapter

`src/frontend/rust/timecodeBridge.ts` is a pure callback adapter for the owned
native timecode host. This slice adds no shared bridge/main/manifest/CI changes.
`97e21295` is the composition host; `96edd50a` separately adds its private
process-wide publication sequence. Router `304e30a3`/`dce59bab` and LTC
`e621e311` are host prerequisites. The coordinator owns actual Tauri routing,
native authority, packaging, lifecycle wiring and renderer integration evidence.

## Exact callback contract

Construct one `createTimecodeBridge(options)` for the main window. It retains a
frozen full `{runtimeId, libraryGeneration, dataPath}` from `getContext()`.

| Callback                                                              | Obligation                                                                                                                                                                        |
| --------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `command(message, fullContext): Promise<unknown>`                     | Invoke the trusted native command/value boundary with the original four nonraw MAIN timecode calls. This is a JS snapshot callback, not a JSON serialization API.                 |
| `audio(bytes, "LTC", fullContext, announcedLength): Promise<unknown>` | Invoke `NativeTimecode.audio`; bytes are a private `Uint8Array` snapshot, length must match.                                                                                      |
| `complete(fullContext, id, accepted): Promise<unknown>`               | Invoke private `NativeTimecode.complete("main", ...)` once, after bounded synchronous retained dispatch. No source ACK.                                                           |
| `retire(oldFullContext, terminal): Promise<unknown>`                  | For false, await actual `NativeTimecode.context_changed()`; for true, await actual `shutdown()`. Return only on actual owner drain/join; failures retain native owners for retry. |
| `deliver({channel, data}): void`                                      | Synchronously dispatch the original MAIN message through retained handlers. Do not enqueue an unbounded WebView drain or return an asynchronous delivery promise.                 |
| `report(error)`                                                       | Bounded native diagnostics; exceptions are contained.                                                                                                                             |

The adapter returns `handle(message): boolean`, `publish(publication): void`,
`contextChanged(): Promise<void>`, `close(): Promise<void>`, and bounded
`status()`. Call `handle` at the MAIN send interception boundary; false means an
unrelated command. True only means intercepted, including source no-ops and
explicit errors. No method result is an original command-success ACK.

The four `TimecodeMessage` channels are TIMECODE_START, TIMECODE_STOP,
TIMECODE_VALUE and TIMECODE_STATUS. Send-time copies retain key insertion order,
missing versus own undefined, null, raw IEEE NaN/infinities/negative zero and
unknown bounded plain data. START requires string type/mode and VALUE requires a
Number. The native trusted callback must map these snapshots to the host's
`command`/`value` APIs without blindly calling JSON.stringify. In particular,
top-level undefined STATUS maps to `Data::Undefined`, a Number maps through
`Number::new`, and framerate retains raw primitive truthiness. Native JSON cannot
represent nested undefined or special numbers without a separate restoration
contract: the coordinator must explicitly encode supported variants or reject
them, never silently turn them into null/omitted fields. This adapter's lossless
callback test is not a claim that Tauri serialization is already wired.

## Actual LTC audio hook and source no-op

There is no new AUDIO port or PCM engine. The retained hook is
`components/timeline/timecode.ts::startListeningLTC`, whose worklet onmessage
calls `sendMain(Main.TIMECODE_AUDIO_DATA, e.data)`. The original
`public/assets/ltc-processor.js` posts a raw Uint8Array of4096 samples. Electron's
actual `processAudioData` reads `data.mode`, so this unwrapped source worklet
route performs no effect. The adapter consumes it with the same no-op. It does
not invent `{mode:"LTC",buffer:...}` or claim microphone decode works. MTC input
arrives from the native MIDI owner and is independent of the PCM audio pipeline.

Explicit `{mode:"LTC",buffer}` callers use the raw callback. Source-compatible
Buffer.from copies numeric typed-array elements, including subarray bounds;
it does not reinterpret a Uint16Array as backing bytes. ArrayBuffer copies all
bytes, DataView becomes empty, and bounded serialized Buffer/array inputs use
source primitive byte coercion. Null/missing payload or missing explicit LTC
buffer has an explicit error. Other modes preserve the original no-op without
walking an unused buffer. Shared storage, BigInt arrays and custom object byte
coercion reject explicitly. No conversion to Float32/PCM16 or resampling occurs
here. The existing raw AUDIO framing's32768-byte bound belongs to its separate
engine; this explicit timecode callback uses the host's65536-byte bound and still
needs coordinator-owned binary framing, not JSON numeric expansion.

## Publication and one-use private completion

`publish` consumes the exact native struct shape, outside the original event:

```ts
{
    event: {channel: "MAIN", message: {channel, data}, context, listenerId?: never},
    hydration: {undefinedPaths: [], numbers: [{path: "", number: {kind}}]},
    bytes?: Uint8Array | number[],
    delivery: {id: "timecode-<UUID>", context, run: number, sequence: string}
}
```

Finite TIMECODE_VALUE, TIMECODE_STATUS and ALERT use empty number restoration.
Only a null whole-value TIMECODE_VALUE placeholder may restore NaN,
PositiveInfinity, NegativeInfinity or NegativeZero. TIMECODE_AUDIO_DATA requires
a null placeholder and bounded exact bytes; the retained handler receives a
fresh Uint8Array. The host currently emits no undefined source publication;
nonempty undefinedPaths reject rather than applying a generic null rewrite.
Only original `{channel,data}` reaches `deliver`. Context/run/sequence/bytes
metadata stays private. A retained source handler can use the Uint8Array's
indices/length to produce the same unsigned8-bit Float32 samples as Buffer;
this does not prove packaged Electron's exact Buffer class or WebAudio output.

Both event and delivery contexts must match each other and the current full
context before delivery. The decimal-string u64 sequence must be positive and
canonical. The adapter retains one BigInt last sequence across context changes,
rejecting <=last before delivery, including numbers beyond JS safe integer
precision. Sequence gaps are valid. `run` currently remains the host's safe
positive JS integer; values beyond2^53-1 reject explicitly until the coordinator
also serializes that private field as a string. UUID completion is the native
one-use authority. No unbounded UUID history is retained.

One completion promise is retained at a time. Concurrent or reentrant publication
rejects without consuming a different live nonce. Already consumed/reordered
publications do not send another completion. A well-identified malformed or
stale-context publication completes false. Once retirement starts, late
publications are rejected without starting more completion work; native
retirement revokes their old nonces. Current-context completion transport failure
fences new work and remains sticky across terminal cleanup/repeated close. An
old-context rejection caused by native retirement cannot poison a new context.

Completion bounds synchronous handler dispatch, not a later AudioContext resume,
device playback, asynchronous Svelte handler effect or hardware delivery. The
publisher and completion transport must only accept real trusted native events;
this adapter is not a renderer-provided capability issuer.

## Bounds, cleanup and accepted work

There are seven whole ordinary FIFO leases covering queued plus entered callback
work; at most one ordinary native promise is entered. Each accepted snapshot
remains retained until its real callback settles even when the caller ignores
the return. A separate cleanup lease runs independently of an entered callback.
There is no per-frame task/thread, idle poll or idle timer. A drain wait uses one
temporary timeout, default5000ms and bounded(0,5000], with one waiter at a time.
Timeout retains the same pump/cleanup/completion promises for retry.

Admission checks the full context before copying; queued context is checked again
before native entry. Native host validation remains authoritative at actual
effects. Explicit context retirement cancels queued old work, retains accepted
promises, awaits native drain and only then adopts the current context. Close is
terminal. If close arrives during an entered nonterminal drain, the adapter also
awaits a terminal native shutdown before declaring cleanup finished. A failed
cleanup remains fenced and retries the same old context; no replacement owner is
created. Page/runtime destruction can still prevent JS promises from settling;
native accepted owners must remain process-owned and independently drained by
application shutdown.

Per source snapshot:1024 nodes, depth8,65536 aggregate UTF-8 key/string bytes
(primitive values charge8); audio65536 bytes/elements. Runtime ID256 UTF-8 bytes,
path32768, generation safe nonnegative JS integer. Strings and scalar JSON are
bounded before allocation-heavy encoding. Native audio publication arrays are
validated per byte before copying; private completion retains no large source
payload. Worst seven queued audio copies total458752 bytes, or seven metadata
budgets plus bounded object/array/context overhead. One immediate publication
can contain65536 bytes. Custom prototypes/accessors, functions, cycles and
unsupported host payload variants reject. Hostile JavaScript proxies, VM/OS memory
pressure and synchronous native callback stalls are not preemptible by these
cooperative JS bounds. The report callback must not retain an unbounded log.

## Reproducible evidence

```sh
"$ORACLE_NODE" --test tests/parity/timecode-bridge.test.mjs
npx tsc --noEmit --target ES2022 --module ES2022 --moduleResolution node --strict --skipLibCheck --lib ES2022,DOM src/frontend/rust/timecodeBridge.ts
npx prettier --config config/formatting/.prettierrc.yaml --check src/frontend/rust/timecodeBridge.ts tests/parity/timecode-bridge.test.mjs docs/migration/workers/timecode-bridge.md
```

Fifteen portable Node tests run without hardware, installed Electron, codec addon,
GUI/native app build, FFmpeg or network output. Actual immutable declarations
come from `git show 2d94defcf98175e4f5c9504e7f4fd1d33c35c975:<path>`, with
CRLF normalized before pinned SHA256 assertions. Tests execute the actual
sendMain wrapper, four Electron handler properties, timecode processAudioData,
frontend listener/playback helpers and LTC worklet, with controlled OS/WebAudio
boundaries. A genuine MessageChannel carries actual generated worklet bytes;
explicit buffer variants compare to the original Buffer.from call. Tests also
cover IEEE/property presence, Float32 sample equality, u64 replay, stale queue and
runtime/path separation, saturation, caller abandonment, independent cleanup,
waiter/drain timeout, terminal upgrade, reentrance, malformed resources and sticky
completion failure. No algorithm is copied as the source oracle. Pinned Node
22.21.1 was used locally. Root bridge wiring, production raw serialization,
Tauri/WebView scheduling, packaged private nonce transport, actual browser audio
capture/playback and physical MTC/LTC remain separate integration evidence.
