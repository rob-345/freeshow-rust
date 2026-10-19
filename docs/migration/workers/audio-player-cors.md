# Native AudioPlayer asset CORS

This correction is independent of the lazy AudioAnalyser candidate. It changes
only the two file-element creators in `src/frontend/audio/audioPlayer.ts`, their
focused source tests, and this note. It adds no dependency, native command,
filesystem grant, DSP policy, resampling or encoder reset.

The complete retained Svelte application exposed a boundary absent from the
earlier isolated HTTP WebKit harness: its document is `tauri://localhost`, while
authorized native media bytes arrive from a private loopback HTTP asset server.
The retained AudioPlayer used `new Audio(encodeFilePath(path))` without CORS.
An advancing HTML media clock is insufficient proof of readable Web Audio
samples. The [Web Audio specification](https://www.w3.org/TR/webaudio-1.0/#MediaElementAudioSourceNode-security)
requires silence for a source whose media fetch is labeled CORS cross-origin.

Actual private full-app controls establish the failure separately from the
analyser optimization. With the immutable eager analyser, a readyState 4 WAV
advanced to 2.16 s, its real 44,100 Hz context advanced to 2.548 s, and the source
gain was 1. `crossOrigin` was null; source, main and speaker meters were all −60 dB.
Reloading the same element and granted URL with anonymous CORS before `src`
made all three meters −9.718187 dB in the same context/route. The lazy candidate
independently showed the same null-CORS silence and a −7.374981 dB result after
that controlled reload. These are actual browser/graph observations, without
an assertion that WebKit emitted a particular console diagnostic. The eager
control is the original analyser inside this Rust full application, not an
original packaged Electron application run.

`createAudioElement` now resolves the source exactly once. Only an existing
native resolver with a changed loopback asset URL selects an empty Audio
element with anonymous CORS. Playback and exported `loadAudioFile` install their
original listeners, then assign that source once. The latter covers the native
duration element's first fetch mode; it is not evidence that the duration
element caused the routed element's silence. Electron, direct remote/data/blob,
ordinary file URLs and unresolved/revoked empty results retain the original
`new Audio(source)` behavior. There is no fallback from a denied empty native
result to `file://`, and no second authority lookup or new URL selection API.

Two Node 22.21.1 tests execute the actual original and current AudioPlayer class,
element helper and duration-loader declarations with a controlled DOM boundary.
They compare both creators' legacy constructor/listener traces and actual
load/error return paths, and require CORS/listeners before exactly one native
source assignment. Original frontend source SHA256, normalized for CRLF:
`0731f9e0de7a001e9bf71ff650b4b4e19bcd10e59474c54d258719d6d3fda918`.
These controlled tests do not substitute for the real browser evidence.

The separate full-fork harness/evidence note records ordinary corrected eager
and lazy runs, real WAV playback/routing, SoundTouch and source reuse, actual
retained worklet PCM/native Opus/Ogg/source comparison and normal CLOSE.
Both ordinary runs pass without a diagnostic reload: twelve real 7,056-byte
44,100 Hz worklet packets reach native Opus; actual original-source/original
NAPI libopus 1.4 replay matches the captured HTTP/Ogg stream, with fourteen raw
CRCs checked before serial/idle normalization and one source encoder. Native
accepted/completed counters each increase by twelve. Real sender/native leases
drain, and normal CLOSE exits before fixture process termination; the driver,
virtual Pulse process, peer thread and owned descendants actually join/reap.
Its source recorder MIME failure stays explicit; physical audio, capture,
recording success, hardware output, server acceptance, platform-wide parity
and CPU improvement remain unclaimed. VideoPlayer's independent initial,
duration and soft-loop element creators require separate coverage; this change
does not certify them.

```sh
node --test tests/parity/audio-player-cors.test.mjs
```
