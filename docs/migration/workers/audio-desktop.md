# Desktop AUDIO receipt helpers

This slice adds only `tests/desktop/audio_probe.js`, `audio_peer.py`,
`audio_stream_oracle.mjs` and this note. Existing `audio_oracle.mjs` still serves
the metadata smoke test unchanged. The coordinator owns smoke.py, production
frontend/native routing, status counters, build and actual GUI runs. These helpers
do not select native executable libraries, sinks, filesystem paths or library
authority from renderer input.

## Controller API

```python
from audio_peer import AudioPeer

audio_phases = []  # Retain this across normal/library-switch/ZIP phases.
with AudioPeer(timeout=20) as peer:
    observed = script((ROOT / "tests/desktop/audio_probe.js").read_text(), {
        "phase": "normal", "seed": 12345, "config": peer.config,
    })
    wire = peer.receipt(timeout=10)
audio_phases.append({
    "script": observed["script"], "wire": wire,
    "capability": observed["capability"], "statuses": observed["statuses"],
})
# After later probes append their exact scripts/frames to the SAME series:
source = subprocess.run([
    pinned_node, "tests/desktop/audio_stream_oracle.mjs"
], input=json.dumps({"phases": audio_phases}), text=True,
    capture_output=True, check=True, cwd=ROOT, timeout=15)
```

Use one series per genuine native application process lifetime. A library switch
or ZIP backup does not justify resetting the source encoder. The oracle can
replay an accumulated prefix after each phase, but final comparison must include
all packets previously submitted to that native encoder, including each initial
connection-triggering dropped packet. No unrelated Opus input may be omitted.
A real application restart starts a separate series explicitly.

`AudioPeer` binds only `127.0.0.1` on an actual retained ephemeral TCP port and
creates unpredictable per-peer mount/password values in `.config`. Pass this
exact native-controller fixture to the probe. There is one non-daemon worker,
one listener and at most one accepted connection; the listener closes after
accept. Read/send timeouts and a maximum thirty-second lifetime bound socket
operations. Raw wire is capped at one MiB, headers at 16 KiB and Ogg pages at 4096. The context manager always shuts down/closes sockets and joins its worker;
a failed join is explicit, never represented as complete cleanup.

`.receipt()` requires actual EOF and joined worker, validates PUT mount and UTF-8
Basic authorization, and returns original `wireBase64`, SHA256, HTTP header,
every raw Ogg page/payload/lacing/serial/sequence/granule/CRC, CRC coverage, EOS,
accepted connection count and join evidence. It sends a real HTTP 100 Continue
response. This bounded peer is not a full Icecast server or deployment test.

## Real page probe

The probe expects these coordinator routes:

- `window.api.send('AUDIO', {channel,data}, optionalIgnoredListenerId)` for
  INIT_PORT, PCM, CAPTURE and CLOSE_PORT, with the retained transferred-port path.
- `window.__TAURI__.core.invoke('audio_capability')` returning the actual native
  Opus capability enum; exact comparison requires Available.version `libopus 1.4`.
- `audio_status` returning `{host,ingress}` with host.retained and ingress
  retained/queued/active/groupOutstanding/acceptedPackets/completedPackets.
  Packet counters describe real native PCM admission/future completion, not
  a source success ACK or browser-post acceptance.

It creates an actual INIT_PORT handshake for raw target `icecast`, checks exact
`AUDIO_PORT_RESPONSE` key order and one real transferred MessagePort, then sends
four deterministic planar Float32 stereo 960-sample packets. Each allocation has
guard bytes around a nonzero offset; port Uint8Array/DataView and fallback
Float32Array/Uint8Array ranges select exactly 7680 bytes. The backing allocation
is mutated immediately after send, so the accepted snapshots must match the
reported exact input bytes. Source scripts carry those bytes as bounded base64
artifacts; PCM is sent over the real binary path, not JSON numeric arrays.

The first packet is encoded before opening the TCP stream and is therefore
connecting-dropped. The probe awaits an actual retained MAIN.ICECAST_STATUS
connected callback before its three subsequent port/PCM/CAPTURE packets. Every
packet waits for a monotonic completedPackets increment plus real ingress/host
drain. A zero queue status by itself is not a MessagePort delivery barrier.
The exact four-packet admission/completion delta catches unexpected interference.

CLOSE waits for a newly received disconnected callback after its close request
and actual ingress/host drain. All status receipts capture source own key order
`connected,connecting,error`, own error existence, undefined error and undefined
listener ID before JSON loses them. AUDIO and correlated MAIN observers must
receive no fabricated void-command ACK. Full context remains frozen/checked
through every wait. Timers, ports and named/error listeners are removed in
finally, including on failure.

This is controlled PCM, not a microphone/device capture or playback claim.
AudioWorkletNode availability is reported separately; the helper does not claim
it ran an AudioWorklet or physical device. The actual retained worklet's code
was exercised separately by audio-port source tests. WebKit transfer and native
effects become evidence only after the coordinator runs this probe in the real
application, which this worker did not do.

## Actual continuous source oracle

The CLI accepts at most eight MiB of JSON and eight ordered phases. Each phase
has the probe's `script` (2..16 exact frame descriptors), optional actual peer
`wire`, capability and retained statuses. Exact frame descriptors contain
`origin`, `planarBase64`, sampleRate 48000, id `icecast` and first-packet
`connectDropped`. Empty NOW_PLAYING title is the current supported native route;
this helper does not invent cover/metadata state. `oraclesDir` or
FREESHOW_OPUS_ORACLE_DIR can select a trusted controller-owned oracle directory;
the default is `target/opus-oracles`.

It hash-pins the complete actual immutable receiveAudio, processAudio and
IcecastSender TypeScript files from FreeShow 1.6.7-beta.2 commit
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`. Only host boundaries are replaced:
ordered mock sockets/port delivery, deterministic clock/serial and controlled
idle timers; actual source conversion, receive routing, accumulator, request
headers, statuses, Ogg page/lacing/granule/EOS/CRC logic execute unchanged.

Actual original @discordjs/opus 0.10.0 NAPI `system/opus.node` encodes every PCM
packet. Build receipt binary and core source hashes are checked. The Linux
process must map exactly one libopus whose SHA256 matches the committed measured
system 1.4 receipt. Native comparisons additionally require the actual capability
version 1.4 and matching default bitrate. This helper is explicitly Linux/system
1.4; it never claims bundled 1.3.1 cross-version byte equivalence or fabricates
availability. There is one actual encoder constructor across the whole ordered
series. Codec, accumulator and source-global config state are never recreated
between phases. The output includes constructor count, total encoded packets,
codec/source hashes, every source wire and same-version comparison evidence.

All original raw CRCs, sequences and granules are validated **before** any
normalization/filtering, independently for source and native captures. Only the
source's exact hardcoded idle packet `f8fffe` is removed. Controlled real encoded
packets are asserted not to alias it. Original granules must count every raw
real/idle audio frame correctly. After that validation, serial, sequence and
granules are normalized to the retained real-frame stream and CRCs recomputed.
HTTP bytes, tags, lacing and real Opus payloads then compare exactly. Raw pages
and ignored-idle counts remain evidence; normalization cannot hide a bad idle
CRC, incorrect original granule or missing real frame.

## Standalone checks completed

No GUI, native build, release build, device or external endpoint was used:

- Node 22.21.1 source-only replay of three phases encoded twelve packets with
  exactly one real original NAPI constructor; each source stream had six checked
  raw CRCs and EOS. All phases reported nativeCompared false.
- An actual Python loopback capture of source-generated wire received 1229 bytes,
  six valid original CRCs and EOS through one connection; its worker joined.
- Synthetic source-wire comparison exercised exact comparison and a valid added
  idle page. Corrupting that idle page's original checksum failed both Python
  validation and the oracle before filtering. This is comparison-tool validation,
  not native-app equality evidence.
- A Node-only facade using the real audio port adapter and genuine transferred
  MessagePorts executed the page probe, preserving all four offset snapshots,
  three own-undefined status receipts and zero source ACKs. Its native callbacks
  were controlled; it is not WebKit/native execution evidence.
- Python py_compile, Node syntax checks and repository Prettier checks passed.

Root integration must save observed probe/peer/oracle receipts for normal,
post-library-switch and during-ZIP phases, and report actual GUI results separately.
