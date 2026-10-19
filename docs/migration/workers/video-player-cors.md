# VideoPlayer native asset CORS

Scope: `videoPlayer.ts` only, based on `ed56f311`. This changes the media element
fetch mode for mapped Rust assets; it adds no grants, server policy, file fallback,
bridge commands, or audio-routing policy.

## Source and failure

The immutable Electron source is
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, FreeShow `1.6.7-beta.2`.
The normalized LF SHA-256 of its VideoPlayer is
`915987dd58d721b8a890a0ed7dee282daeac4ca9d18c3ceac27041da3afcc0b5`.
Its three real audio-element creators are initial playback, metadata duration,
and the soft-loop clone. The last uses `new Audio(audio.src)`, so passing a
previously resolved HTTP grant through `encodeFilePath` again cannot recover the
lost fetch mode: that URL is intentionally returned unchanged.

The [Web Audio security rule](https://www.w3.org/TR/2021/REC-webaudio-20210617/#MediaElementAudioSourceNode-security)
requires silence from a MediaElementAudioSourceNode for a CORS-cross-origin
resource. Advancing media clocks and a resolved `play()` therefore do not prove
that the graph receives samples. The [Audio constructor](https://html.spec.whatwg.org/multipage/media.html#dom-audio)
can initiate loading from its source argument; setting `crossOrigin` afterwards
does not establish correct ordering before that fetch.

## Narrow behavior

Both initial creators resolve once. A local path mapped by the Rust API to its
loopback asset service creates an empty element, sets `crossOrigin="anonymous"`,
installs the original listeners, then assigns its source once. Electron, direct
URLs, data/blob URLs, unmapped paths, and provider virtual elements retain the
original constructor and event timing.

A weak map stores element to the exact initially resolved native resource. A
soft-loop clone copies the parent's non-null mode before assigning the source
only when its current source still equals that recorded resource. The weak map
retains no element, context object, or native grant lease. It is a CORS hint, never byte authority.
Changing a formerly native element to a different resource takes the original
clone branch, even if the replacement happens to have the native URL shape.
Empty/null modes, existing crossfade reuse, gain updates, loop timing, and source
swaps otherwise retain the original behavior. The native registry and server
continue to validate/revoke capabilities.

## Tests

`tests/parity/video-player-cors.test.mjs` executes the actual pinned class and
the modified class with controlled DOM/native boundaries. It also executes the
real media URL registry for accepted, missing, and stale grants. Six suites cover:

- Eighteen Electron/direct/unmapped creator traces, with original load/cleanup.
- YouTube, Vimeo, and unknown provider virtual branches without an Audio fetch.
- Both native creators' first-fetch CORS/listener order and source error effects.
- Actual soft-loop clone, existing crossfade reuse, gain/play/pause/swap order,
  and a single original registry lookup.
- Three non-null modes, null mode, and changed/direct resources retaining the
  original clone semantics. Credentials mode preservation is not a claim that
  the native server supports credentials.
- Missing and stale grants producing the original empty-source error result,
  with no file URL/path fallback.

Before the production change, three suites passed and the three native-CORS
suites failed. With the change all six pass. These controlled DOM traces are
separate from the real browser evidence below.

## Actual Linux full-fork browser evidence

`tests/desktop/video_cors/build.mjs` builds the complete retained Svelte app and
unchanged remote clients. Its test plugin adds an early delegating observer;
it replaces no application module except the explicitly selected immutable
VideoPlayer control. AudioAnalyser remains the original eager implementation
(`ff000b9ddcd06eb88069a210abe09fb293e75a34e116bc702de0045768146eff`).

`hook.ts` calls the real public VideoPlayer duration/start/update/stop APIs,
observes real browser elements and the production graph, and uses real
`MAIN.READ_FOLDER` results and their native grants. It explicitly configures
fixture stores/routing through the real routing manager and uses a trusted
WebDriver button click for start. It is not evidence that an untouched retained
Svelte video button handler was exercised. Declared seek/store controls drive
the actual source's soft-loop implementation; no gain, source samples, native
response, or codec result is fabricated.

`run.py` creates a private profile/library, isolated Xvfb/driver ports, and a real
virtual PulseAudio sink. One small generated VP8/Vorbis WebM is frozen and copied
into each control. The verified pair uses 36,558 identical bytes, SHA-256
`6dc66b604c0a9c05f71cc2b6d304863cd66884ca550fbbfaeb2ba191ceeb00c7`.
The graph observations are functional silence/nonzero checks, not comparable
CPU measurements or exact PCM/codec parity assertions.

The immutable VideoPlayer control runs inside the Rust fork, not the official
Electron executable. It passed:

- Three silent samples while media time advanced `1.082→1.551s` and context time
  `0.926→1.402s`; source/main/speaker all reported the real `-60dB` floor.
- Reloading the same element and exact same granted URL with anonymous CORS
  yielded all three graph levels near `-24.09dB`, with routing and gains retained.
- Its genuine soft-loop clone kept the same URL, lost CORS, and returned all
  three graph levels to `-60dB` despite advancing playback.
- Source stop removed playing/source state and ceased the graph signal; normal
  native CLOSE exited the actual app before fixture termination. Driver/Pulse
  joined, and the controller had no owned children left.

The corrected candidate passed the ordinary path without a diagnostic reload:

| Actual browser phase                                                   | Element / CORS            | Source / main / speaker |
| ---------------------------------------------------------------------- | ------------------------- | ----------------------- |
| Initial playback                                                       | initial / anonymous       | all `-20.938dB`         |
| Real soft-loop swap, same resource                                     | new / anonymous           | all `-20.935dB`         |
| Replace that same live element with another already-granted direct URL | same / anonymous          | all `-21.185dB`         |
| Real clone of the changed resource                                     | new / null                | all `-60dB`             |
| Source stop                                                            | no playing/source element | all `-60dB`             |

The direct replacement is an explicit test control, not a claim that an
unmodified Svelte action changes a live element's resource this way. Its
negative result establishes that a formerly native weak key cannot apply the
correction to a different resource. Both the metadata and initial native source
assignments were observed with anonymous CORS already configured. Duration was
`12.003s`, readyState `4`, and playback/graph clocks were live. Fourteen bounded
graph observations sufficed. The app exited on normal native CLOSE before
fixture termination; driver/Pulse joined, with no controller-owned children.

The corrected binary SHA-256 was
`5e001c5bda6fd07c859d0db633bd14df9092bc15467141fca44771d27b132249`;
the immutable-control binary was
`685d8609f2e39c462f297cede19833aef1b375df29f459fc96479c13e69c1b05`.
The corrected VideoPlayer input SHA-256 was
`4336e54d32395b0c0242ef4950895235772ff8509883b429a7026d9aacb8c006`.

Raw receipts/build hashes/logs are under the worktree's ignored
`test-output-rust/video-cors/`. The original control is preserved in
`original-control/` and `corrected-control/`. Earlier failed receipts are retained: initial clock sampling
did not wait for distinct media times, startup capability diagnostics were not
yet classified, and the first changed-resource controls copied a live fixture store item while
the source's actual 300ms `isSwapping` guard remained active. Its real timeout
still targeted the old object. The final controller preserves that live item
identity, as production `updateProperties` does, and observes actual guard
release before its second seek. No production loop logic is changed to
accommodate the test.

The fixture's `READ_AUDIO_METADATA` reports the exact independent capability gap
`audio_unsupported_variant: unsupported audio variant: ASF/Matroska metadata`.
The original ReplayGain error path uses multiplier 1. This is not successful
Matroska metadata or recording evidence. Exact startup DEVICE_ID,
PROVIDER_CONNECTIONS, and SPOTIFY_GET_STATE unsupported-command diagnostics are
recorded separately; any other runtime/browser error fails the probe.

## Running and limits

Use Node `22.21.1` and the existing dependency installation:

```sh
node --test tests/parity/video-player-cors.test.mjs
FREESHOW_VIDEO_CORS_ORIGINAL=1 node tests/desktop/video_cors/build.mjs
# Build the private custom-protocol native binary, then run the control:
WEBKIT_WEBDRIVER=/path/to/WebKitWebDriver xvfb-run -a -s '-screen 0 1280x900x24' dbus-run-session -- python3 tests/desktop/video_cors/run.py
# Preserve its ignored receipts, build without FREESHOW_VIDEO_CORS_ORIGINAL,
# rebuild the same private binary, and run the corrected candidate.
node tests/desktop/video_cors/build.mjs
```

The verified build used jobs 1, incremental/debug info disabled, offline Cargo,
the extracted native headers, `--manifest-path src-tauri/Cargo.toml --features
custom-protocol`, and this worker's independent external target. Root caches,
binaries, manifests, lockfiles, and other workers' files were not changed.

The probe bounds source/error records to 256/64, graph observations to 128,
WebDriver requests to 512, each response to 2MiB, and the frozen fixture to
256KiB. Each observation has a deadline; no hardware or performance result is
inferred. It verifies Linux WebKit's audio graph for this supported browser
codec fixture, not video pixels, remote services, physical delivery, capture,
all codecs, Windows/macOS runtime, or broad Electron playback parity. Native
retirement/context and grant authority remain owned by existing hosts.
