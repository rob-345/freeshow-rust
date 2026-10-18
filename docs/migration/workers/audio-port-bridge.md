# Retained AUDIO port adapter

This preparation changes only the new `src/frontend/rust/audioPortBridge.ts`,
its focused source/MessagePort tests and this note. It does not install the
adapter, change the shared bridge/native framing, configure an encoder, open a
device, play sound or claim Icecast/network delivery.

## Integration API

```ts
const audio = createAudioPortBridge({
    getContext: () => api.getRuntimeContext(),
    postPort: (message, origin, ports) => window.postMessage(message, origin, ports),
    transport: (bytes, metadata, frozenContext, origin) => nativeRawAudio(bytes, metadata, frozenContext, origin),
    disconnect: (frozenContext) => nativeIcecastDisconnect(frozenContext),
    closeContext: (frozenContext) => nativeAudioContextClose(frozenContext),
    report: (error) => reportError(error),
    drainTimeoutMs: 5000
})
audio.handle(message) // AUDIO's existing inner {channel,data}; returns void
await audio.contextChanged()
await audio.close()
```

All operations and window ownership are injected by the coordinator. There is
no main-window restriction in this pure helper: the retained prelude also runs
in output windows, and the real native owner decides each window's authority.
Callbacks must be bounded, nonblocking and nonreentrant. `postPort` must perform
the actual transfer, not JSON serialization or a fabricated response event.

The transport receives a copied `Uint8Array`,
`{sampleRate?: unknown, id?: unknown, icecast?: unknown}`, frozen full
`{runtimeId,libraryGeneration,dataPath}` and diagnostic origin `port`, `PCM` or
`CAPTURE`. The metadata preserves own missing/undefined/null/false and supported
JSON values, including arrays, objects, unknown config fields and negative zero.
Native `JsInput` retains missing/undefined as Undefined and JSON null as Null;
Icecast absent is None while explicit null is Some(Null). These distinctions
were coordinated with the native audio-pipeline host worker. Do not apply
`Number(sampleRate)`, defaults, target-ID coercion or remembered latest Icecast
config here. Those are native pipeline responsibilities. Origin is framing
provenance, not a source data field or new renderer acknowledgement.

Intercept AUDIO before ordinary JSON IPC. Do not stringify a PCM typed array or
turn its elements into a JSON object. Every accepted packet retains its exact
context, even if the caller ignores the operation. Hook accepted library-context
retirement and terminal window/application teardown into the two awaited lifecycle
methods. A timeout means actual transport/cleanup is still owned; retain the
adapter and retry drain. The injected `closeContext` must actually revoke/drain
native audio for that frozen context and interrupt its stalled native calls;
its receipt cannot be a synthetic no-op. No receiver is switched to a new context
until both that operation and the entered old transport have settled.

## Source boundary and timing

The oracle is actual immutable FreeShow 1.6.7-beta.2 source at
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`: `src/electron/preload.ts`,
`src/electron/audio/receiveAudio.ts` and `public/assets/pcm-worklet.js`. Tests pin
each complete file's SHA256 before evaluating its actual code, replacing only
Electron/window/AudioWorklet host boundaries with real Node Web MessagePorts and
a manually driven worklet host.

`INIT_PORT` creates a real Web `MessageChannel`, starts its receiving port, and
posts exactly `{type:"AUDIO_PORT_RESPONSE",targetId:data?.id}` with `"*"` and one
transferred port. The raw target ID remains raw in this response. Source port
keys use truthy `String(id)` or `"default"`; JSON IDs retain those aliases,
including missing/null/false/zero/empty string. A replacement closes the previous
port with the same key. Received worklet messages require `channel === "AUDIO"`
and a truthy payload. Fallback `PCM` and `CAPTURE` use `data`. `RESET_DECODER` is a
source no-op. Unsupported channels report a controlled diagnostic. None of these
void commands creates a success ACK; INIT's transferred-port response is the
actual source handshake.

Byte conversion matches source Uint8Array/Buffer, ArrayBuffer and arbitrary view
storage, byteOffset and byteLength. Float/word views are copied as raw storage,
not numerically reinterpreted. Serialized `{type:"Buffer",data:[...]}` uses
bounded Uint8Array element coercion matching actual Buffer.from array cases.
The reachable source quirk where `data`/`payload` itself is a view selects that
view's complete `.buffer` backing allocation; `{buffer:view}` selects its range.
Missing, unsupported and zero-length buffer shapes are ignored as in source;
detached storage is an explicit `audio_packet_invalid`. SharedArrayBuffer views
are supported; their copy is not an atomic cross-thread snapshot. A bare
SharedArrayBuffer is unsupported/ignored, as with source's ArrayBuffer check.

Source replacement of an Icecast port disconnects on that old port's close
event. Source CLOSE explicitly disconnects even when no port exists, and an
existing port's independent close event disconnects again. The helper accounts
for known own-port closure once: INIT replacement queues one disconnect, CLOSE
queues two when a port existed or one when absent. Each independent disconnect
attempt still runs if the previous one fails. No latest config is cleared in the
adapter. A real peer-close event is honored on runtimes that expose it.

These timing differences are deliberate and finite: the adapter's FIFO waits
for earlier accepted native packets before ordinary disconnect effects; source
`processAudio` calls and Electron close events have separate scheduling. Own-close
effects are queued when closing the known port rather than depending on a Web
peer-close notification. There is no sender-time epoch that discards earlier
accepted packets on INIT/CLOSE. Library/terminal retirement explicitly cancels
queued old work and closes old ports, retaining the already-entered transport.
This is a context-cancellation change from Electron's process-global port map.

Browsers do not promise a MessagePort peer-close event. Consequently a remote
peer that only closes its port cannot be guaranteed to trigger immediate native
Icecast disconnect or remove the adapter's entry there. Retained AudioSender
also issues explicit `CLOSE_PORT`; that path is supported. Node's actual close
events in tests are not evidence that WebKit supplies that browser capability.

## Resource and failure ownership

There are at most sixteen adapter-owned receiving ports, seven ordinary accepted
packet/control jobs and one reserved lifecycle operation: eight whole leases
including actual entered transport/cleanup. A single FIFO async pump executes
ordinary jobs, with no per-frame Promise/task. Incoming bytes/config/context are
copied before returning; later mutation cannot change an accepted packet.
Overload reports `audio_busy` and does not claim successful delivery. Overloaded
INIT replacement/CLOSE is rejected before closing the affected Icecast port, so
its required disconnect is not silently lost. If a peer-close cleanup cannot
obtain an ordinary lease, terminal reserved cleanup faults/closes that adapter.

Raw packets are at most 32768 bytes. Metadata is at most 8192 encoded JSON bytes,
256 nodes, depth eight including its final metadata wrapper. Source metadata
nonfinite numbers, functions, symbols, BigInt, cycles, accessors and non-JSON
prototypes are explicit capability errors. Serialized Buffer arrays are at most
32768 elements, 128 KiB/cooperative traversal budget, depth eight and 65537 nodes;
their source numeric byte coercions include nonfinite-to-zero. Context strings
are bounded (runtime ID 256 characters, data path 32768), with a safe nonnegative
integer generation. Diagnostics carry a bounded 2048-character message, not raw
PCM or an unbounded metadata dump.

The adapter bounds its own ports, queue, tasks and copies. The browser's internal
MessagePort sender queue is outside that bound: the original worklet protocol
has no ACK/backpressure. It cannot be given a strict pre-dispatch byte bound
without changing that protocol. This helper does not claim otherwise.

One reserved `closeContext` operation runs independently of a stalled ordinary
packet. Queued old jobs receive explicit cancellation diagnostics; the active
transport and real cleanup receipts remain held until settlement. Only one drain
waiter/timer is permitted. A default/max five-second waiter deadline rejects with
`audio_drain_timeout` without marking work complete or creating another cleanup.
A retry while cleanup is still pending waits that same operation. An actual
cleanup failure blocks replacement; a later retry performs a real cleanup again.
Successful terminal retry is idempotent. Transport/disconnect errors are reported
and their whole leases reclaimed, while other queued jobs still run. Diagnostic
callback exceptions cannot strand cleanup. Synchronous JS, native IPC/driver calls
and browser task scheduling cannot be preempted by the adapter's timer.

## Focused evidence and remaining checks

Run from the checkout with existing node_modules; no install/build is required:

```sh
/home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node --test tests/parity/audio-port-bridge.test.mjs
npx tsc --noEmit --strict --lib es2022,dom --target es2022 --moduleResolution node --module esnext --skipLibCheck src/frontend/rust/audioPortBridge.ts
npx prettier --config config/formatting/.prettierrc.yaml --check src/frontend/rust/audioPortBridge.ts tests/parity/audio-port-bridge.test.mjs docs/migration/workers/audio-port-bridge.md
```

The ten cases compare actual preload/receiver code, genuine port transfers and
the retained worklet's exact planar stereo/mono bytes; metadata presence/order,
Unicode/raw IDs, offset views, serialized Buffer coercions and shared views;
ignored/malformed/oversize inputs; source Icecast duplicate close effects; FIFO
retention across replacement; overload; held transport/cleanup, timeout/retry,
context snapshots, cleanup failures and throwing diagnostic/transfer callbacks.
Tests use standalone Node 22.21.1, not packaged Chromium/WebKit audio devices.
No fake device/playback, Rust framing, Tauri bridge installation, WebKit
MessagePort transfer, capture hardware, actual encode or Icecast delivery is
certified. Those integration/OS checks remain coordinator work.
