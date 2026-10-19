# AudioAnalyser: private full-editor integration

This bounded functional slice starts from root `ef7fadfb` in
`freeshow-worktrees/audio-analyser-fullfork`, branch
`agent/audio-analyser-fullfork`. It imports the sealed investigation/candidate/
isolated WebKit commits as `c2321d77`, `d2ae2264`, and `7ae2c8e2`; their original
`ec30ca83`/`dbf91d8b`/`6120d9c2` branches remain intact. No root source, cache,
shared native contract, manifest, pinned baseline or production binary changed.
The separately cherry-pickable CORS correction is `abf5faed`.

## Actual application boundary

`tests/desktop/audio_context_fullfork/build.mjs` builds the complete retained
Svelte frontend and standalone resources, with a declared test-only early hook.
It does not substitute stores, routing, AudioPlayer, Web Audio, media decoding,
worklets, grants, raw IPC, DSP, codec, TCP or callbacks. The hook observes the
same real modules/singletons as the application. It delegates browser calls and
Tauri invocation, records exact framed PCM, then observes the completed private
native receipt before allowing the original frontend packet pump to advance.
That adds diagnostic IPC latency and is unsuitable for performance measurement.
Only this explicit test build imports the hook; ordinary production builds do
not expose its test controls.

The real custom-protocol Tauri binary uses `tauri://localhost`, a private
selected library/profile, Xvfb/DBus and an actual private virtual Pulse sink.
A deterministic 20-second stereo WAV (48 kHz, 440/660 Hz channels) enters the
ordinary native READ_FOLDER/grant path. Genuine WebDriver clicks select the
retained folder and AudioFile button. The app's actual AudioPlayer, graph,
SoundTouch and PCM AudioWorklet execute. Its real context chooses **44,100 Hz**;
the source's worklet produces 882 planar samples/channel, 7,056 raw bytes per
20 ms packet. Nothing coerces that to a synthetic 48 kHz input.

Routing preferences are configured through the actual production Svelte stores;
the graph/sender subscribe normally. This does not certify every routing-editor
gesture. The configured Icecast endpoint is only the owned inert loopback peer
from `audio_peer.py`. Native outcomes describe local completed writes, not
server acceptance, audible playback or hardware availability.

## Results and the separately fixed boundary

Both immutable eager-analyser and lazy-analyser controls exposed a pre-existing
native asset CORS omission. HTML media/context clocks advanced with null CORS,
but actual source/main/speaker meters were all −60 dB. Reloading the same granted
URL/element with anonymous CORS restored nonzero samples without replacing the
context, graph or gain. `audio-player-cors.md` records the exact observations and
the native-only correction for playback and duration creators.

Two final **ordinary** runs use that correction without any diagnostic reload:

| Observation                                               | Eager control          | Lazy candidate         |
| --------------------------------------------------------- | ---------------------- | ---------------------- |
| Full editor before audio use                              | One actual context     | Zero actual contexts   |
| Real AudioFile playback/main meter                        | −7.49508 dB            | −7.48255 dB            |
| Actual SoundTouch node, pitch 3/tempo 0.9, nonzero output | −24.63642 dB           | −26.60647 dB           |
| Same real media-source reattachment                       | −7.51101 dB            | −7.37513 dB            |
| Real worklet/native accepted/completed packets            | 12/12/12               | 12/12/12               |
| Actual input packet size/rate                             | 7,056 bytes /44,100 Hz | 7,056 bytes /44,100 Hz |
| First real Icecast disposition                            | ConnectingDropped      | ConnectingDropped      |
| Subsequent real local writes                              | Eleven                 | Eleven                 |
| Original raw Ogg CRCs/EOS checked                         | Fourteen/yes           | Fourteen/yes           |
| Normal native CLOSE before fixture termination            | Yes                    | Yes                    |

Signal levels are independent nonzero observations, not numerical waveform,
wet-path/pitch-quality equivalence, timing or CPU comparisons. Each actual PCM
snapshot is finite/nonzero and bounded; frozen full context matches native
receipt provenance. Accepted/completed counters each increase by twelve, with
no unexpected AUDIO source ACK. Original status own key order and own undefined
error/listener properties are recorded explicitly before JSON transport loses
them. Final source/playing/sender/pending-port collections and actual native
active/queue/retained/group counts are empty. The context remains alive for page
lifetime; harness cleanup does not impose a production context-close policy.

The exact source recorder MIME `audio/webm; codecs=opus` is unsupported on this
WebKit. The actual retained constructor throws NotSupportedError. Source has
already registered a destination before that throw; the fixture explicitly
unregisters it/stops its track and records ended state. This is test-resource
cleanup, not a recording fix or recording-success claim.

## Source and archival evidence

`oracle.mjs` reuses the hash-pinned existing desktop stream oracle, changing only
its controlled input-shape allowances to actual 44,100/48,000 Hz 20 ms frames and
a 64-packet cap. Original receiveAudio/processAudio/resampling/accumulator,
Icecast state/headers/Ogg code and original @discordjs/opus 0.10.0 native addon
run unchanged. Each real application lifetime is a separate fresh source VM
with **one** continuous encoder. Measured system libopus 1.4/defaults/library and
addon hashes are checked. All source/native HTTP, tags, lacing and real Opus
payloads compare exactly. Every original raw CRC, sequence and granule is checked
before filtering only the exact idle packet and normalizing random serials.
There is no cross-version Opus byte-parity claim.

Committed `tests/desktop/audio_context_fullfork/receipt.json` is a bounded
archival projection of the two ordinary runs and CORS controls. It retains exact
real PCM, raw source/native wire bytes, hashes, typed private dispositions,
status evidence, process cleanup and source/native comparison receipts. Repeated
diagnostic status snapshots are omitted; bytes/numbers are not regenerated.
The raw worker receipts remain in ignored `test-output-rust` directories:

- `audio-context-fullfork-candidate-fixed-ordinary`: raw receipt SHA256
  `85c4e6eb98a5115751873e9ffa1099a5d7ed47dc44b8140d36dcd905f2532503`.
- `audio-context-fullfork-eager-fixed-ordinary`: raw receipt SHA256
  `fa8ae3e35a016cca5ba085886e9cd492787e40e88ddbd92f6b2e3809a7567c9b`.
- Separate failed-selector, startup-browse, silent-route and explicit CORS
  diagnostic directories retain unsuccessful/insufficient attempts. Startup
  `browse_busy` events preserve frozen context/request data and observed error
  order in the committed receipt. They do not establish all native admission
  ordering. The controller uses explicit ordinary navigation after an actual
  folder-count **or file-list** result, rather than fabricating a failed read.
  The coordinator owns that separate startup/admission gap.

Both archival ordinary inputs were replayed again through the actual source/
original codec and match. The archive is historical evidence, not a current GUI
test pass. Ordinary use does not require an official Electron executable.

```sh
# Fresh private target, real native dependency/tool environment and Node22.21.1.
node tests/desktop/audio_context_fullfork/build.mjs
cargo build --manifest-path src-tauri/Cargo.toml --features custom-protocol --locked --offline
xvfb-run -a dbus-run-session -- /usr/bin/python3 tests/desktop/audio_context_fullfork/run.py
node tests/desktop/audio_context_fullfork/oracle.mjs

# Immutable eager control uses the SAME corrected player (no diagnostic reload).
FREESHOW_AUDIO_FULLFORK_EAGER=1 node tests/desktop/audio_context_fullfork/build.mjs
# Rebuild the private native binary, then set the same EAGER flag for run.py.

# Explicit historical-cause reproduction: use ORIGINAL_PLAYER=1 at build time,
# EAGER=1 optionally, then DIAGNOSE=1 at run time. This intentionally reloads CORS.
# These flags are test-only and never renderer/native production configuration.

FREESHOW_OPUS_ORACLE_DIR=/trusted/original-addon-oracles \
node tests/desktop/audio_context_fullfork/oracle.mjs tests/desktop/audio_context_fullfork/receipt.json lazy
FREESHOW_OPUS_ORACLE_DIR=/trusted/original-addon-oracles \
node tests/desktop/audio_context_fullfork/oracle.mjs tests/desktop/audio_context_fullfork/receipt.json eager
```

## Ownership and limits

The worker read-copied the root's approximately 15 GiB cached target into its own
external `/mnt/data/cache/freeshow-rust-build/audio-analyser-fullfork/target`
without hardlinks or root writes, before root resumed builds. Its final private
cache is approximately 16 GiB; jobs1/debug0/incremental0 builds never use the
shared root target. Sources, baseline binaries and earlier branches remain intact.

The controller owns private process groups, a Linux subreaper, profile, Pulse
process, driver/app/WebKit descendants and the real bounded peer thread/socket.
It observes normal app exit after source CLOSE before fixture termination;
driver/Pulse joins and actual descendant reaping are separately recorded, not
inferred from a dropped observer. Failure paths terminate/reap owned resources
and retain the raw receipt. The peer validates actual EOF/EOS and joins its thread.

Test diagnostics cap eight contexts, 64 errors/packets/events, 32 KiB PCM per
packet, 512 WebDriver calls, 4 MiB response bodies and a 120-second controller
deadline checked before each bounded request. The native service's production
quotas are unchanged. Audio diagnostics cannot certify success after overflow.
Original source replay is bounded by two MiB input, four MiB output and a
20-second child deadline, with actual child termination on timeout.

Full-editor unsupported-command errors remain visible; there is no invented
global-zero requirement. This slice certifies neither physical microphone/
speaker, capture UI, successful recording, every routing control, video soft
loops, all file/codec variants, other platforms, full application parity nor
performance. No CPU/performance sample was collected.
