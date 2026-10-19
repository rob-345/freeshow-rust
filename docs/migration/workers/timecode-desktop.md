# Desktop timecode comparison probe

This slice owns only `tests/desktop/timecode_probe.js`,
`tests/desktop/timecode_desktop_oracle.mjs` and these notes. It does not change or
run the shared app, bridge, GUI controller, smoke tests, manifests or CI. The
coordinator supplies actual native wiring and verified native-only LTC library
authority. There is no packaged binary distribution, hardware MIDI, microphone,
speaker playback or performance claim.
The root controller sets `FREESHOW_RUST_LTC_LIBRARY` to the verified existing ELF
before launch; renderer fixture fields never choose a native library path.

## WebDriver contract

Run `timecode_probe.js` through the existing desktop script runner with one
fixture argument: `{phase:"normal",fps:25}`. fps supports 25 or 29.97. Optional
values are 4..16 finite times in [0,86400000); defaults are
`[0,40,80,120,1000,1040,1080,1120,400,440,480,520]`. The view must be the main
window with the real retained `window.api` and typed timecode command/raw hooks.
Private `timecode_status` must return `NativeTimecode.status()` directly. The
probe does not invoke a fake codec/port command or inject an event itself.

Use one process for normal, switched-library and restored-ZIP phases. Select the
new root through the existing controller first, then run a new frozen-context
probe. Passing 25/29.97/25 exercises source framerate changes as well as normal
encoder state/backward time. Keep application timecode/timeline activity idle;
unrelated source callbacks are a hard contamination error. In particular, this
probe does not modify Svelte timecode device stores or suppress retained handlers.
The ordinary source handler may attempt WebAudio if the profile already selects
an output; that is not counted as successful playback.
The probe waits for native status to carry the same full context, completed
retirement and zero retained/publication work before sending its first command.

Each phase executes actual retained MAIN sends:

1. Explicit STOP, then START receive LTC and await real decoder construction.
2. START send LTC and the actual raw-worklet-shaped Uint8Array no-op.
3. For each time, VALUE and await one actual Uint8Array audio callback. Feed that
   captured frame through `{mode:"LTC",buffer:subarray}` with offset 7, poisoned
   outer bytes and immediate backing mutation after send. Await actual native
   decoder sample accounting plus source callback/private completion drain.
4. STATUS with own undefined, then STOP and await actual encoder/decoder frees.
5. VALUE without another START, proving source STOP preserves current mode and
   permits encoder reopen; final STOP and actual resource/publication drain.

All source calls remain void. A unique listener ID detects any fabricated source
ACK. Source audio callbacks must have a fresh Uint8Array with exact range and
backing length; message keys remain `["channel","data"]`, data is own and
listenerId is undefined. Exact base64 bytes are retained, with no decoder/pixel
tolerance or normalization. At least one real decoded VALUE is required; an
empty result cannot establish codec parity. Constructor ALERT or a fabricated
STATUS fails the required Linux capability run explicitly.

The return receipt contains phase/full context/current context, before/STOP/final
native statuses, bounded script/actions, ordered original events with typed-byte
and own-property evidence, offset/reuse feed evidence, source ACKs, errors and
browser errors. Runtime errors fail the probe; browser error/unhandledrejection
records are retained separately without a physical-output success claim. Caught
console-only WebAudio errors are not claimed to be fully captured.

The final STOP proves C encoder/decoder frees and accepted callback drain.
It intentionally leaves the fixed LTC service owner live; source STOP is
reopenable. `terminalShutdownVerified:false` prevents that receipt from pretending
the OS/codec thread has joined. The coordinator's context retirement/CLOSE checks
must separately prove actual service/supervisor joins. MTC is explicitly
unexercised rather than opening a physical port or inventing hardware availability.

## Actual immutable source oracle

Feed the complete ordered native receipt series to the oracle once (or replay
the whole growing series after each phase). Never invoke a fresh source VM for
only a later phase:

```sh
FREESHOW_LTC_ORACLE_ADDON=/absolute/private/ltc-original/ltc-native.node \
"$ORACLE_NODE" tests/desktop/timecode_desktop_oracle.mjs < complete-timecode-series.json
```

Input is `{phases:[normalReceipt,switchedReceipt,zipReceipt]}`. All receipts must
come from one actual native runtime ID, with one frozen context in each phase.
The library generation/path may change. Each script starts and ends with an
explicit source STOP. The oracle does not add implicit STOPs, reset caches or
create a new VM at phase boundaries; raw fps/current mode/failed flags retain
actual process-global source lifetime.

The oracle loads actual immutable timecode.ts/LTC.ts/MTC.ts from
`git show 2d94defcf98175e4f5c9504e7f4fd1d33c35c975:<path>`, asserting pinned
SHA256 after CRLF normalization. It extracts the exact five timecode handler
properties from immutable responsesMain.ts, including STATUS console.log and
void returns. Actual original wrapper JS calls the untouched privately built
N-API addon. The original source calls actual encoder/decoder methods; no packet,
waveform, decoder or routing algorithm is reimplemented by the oracle.

The private addon builder is
`crates/freeshow-ltc/tools/build-original.mjs`. It uses pinned Node22.21.1 and
existing C tools/headers without modifying node_modules. Oracle checks the exact
original C/header/wrapper/library input hashes, private addon receipt/hash,
neighboring library hash and actual `/proc/self/maps` library image. The supported
original Linux x64 ELF SHA256 is
`f02c75f982fe01d16f345875d2b09404b8b326bb847f2b5456616f6d0227d09f`.
Missing/unsupported actual addon/platform is an explicit failure, not skipped
parity. The MTC import seam throws if called: this LTC corpus never simulates a
port or claims a hardware result.

The comparison asserts ordered exact source audio bytes and decoded values,
original own-property/envelope evidence, offset/reuse feed accounting, void/noACK,
unchanged process identity, actual native constructor/reset deltas and STOP free
counts. Source native wrappers rely on GC for their resource lifetime; only
native host receipts prove immediate guarded frees. Source constructor counts
are compared independently of JS GC timing. The full raw observed receipt and
oracle sourceEvents retain evidence before any controller summaries.

`sourceOnly:true` is an explicit diagnostic mode for generating actual-source
reference events when no GUI receipt exists. Its output always says
`nativeCompared:false` and `rawAudioBytesMatched:false`; it is not desktop/native
parity. It still uses one real source VM/addon across all supplied phases.

## Bounds and local checks

At most 8 phases, 64 actions/64 retained events per phase; input JSON 2 MiB; each
base64 frame 65536 raw bytes, native explicit feeds at most 16. Probe error and
browser-error records cap 16 each with 2048-character messages and omitted counts.
Each individual async observation uses a 6s deadline and 10ms test-only poll; the
controller must retain its existing whole WebDriver/oracle deadlines. The source
oracle uses bounded git reads and actual finite C operations. These are test
helpers, not a production sidecar or idle polling service.

Local syntax/format and actual source-only three-phase replay were run without
GUI or root/native build. The source-only 25/29.97/25 corpus yielded 13 audio and 11
decoded callbacks per phase, fed 23040/19219/23040 bytes, logged own undefined
STATUS once per phase and constructed one decoder/three encoders per phase.
It explicitly did not count as native comparison. Actual WebDriver comparison
and retained private-completion/context/teardown integration remain for the
coordinator to execute against the wired application.
