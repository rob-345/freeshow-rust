# Retained desktop explicit Nemotron status fixture

This followup extends the existing AI inventory/Spotify desktop checkpoint at
`normal`, `restarted` and `switched`. Each existing checkpoint now sends three
original commands. It still records one smoke step per checkpoint, so the total
step count is unchanged. No native host, production UI, manifest, cache, account,
setup/download/inference, forged model stamp or model data is added. Actual
native/WebView execution belongs to the coordinator; this worker ran none.

The additional retained call is exactly:

```js
window.api.send("MAIN", { channel: "AI_GET_STATUS", data: { engineId: "nemotron", customPath: ignoredRoot } }, "ai-spotify-probe-" + phase + "-AI_GET_STATUS")
```

There is no `modelId` own property. `ignoredRoot` is the existing renderer-root
decoy, also supplied to the source oracle. The native global AI fixture remains
exactly README (27 bytes) and `models/模型.bin` (5 bytes). No pinned model files
exist, so the exact original Linux response is:

```json
{ "nemotron": { "ready": false, "error": "The local model is missing" } }
```

The browser checks literal boolean false/string value and records `dataKeys:
["nemotron"]`, `engineKeys: ["ready", "error"]`. As for the two prior commands,
it requires original message keys `["channel", "data"]`, exactly two callback
arguments, exact phase/channel listener, callback-time full native context and
void send. Runtime ID, library generation and dataPath must all remain current;
the context contains exactly those three own keys. No raw identity is copied.
Null, unknown-channel and wrong-correlation callbacks are ignored before matching.
Matching duplicates, extra arguments and private fields reject. A 50 ms bounded
tail observes already queued duplicates/errors after all three replies, within
the existing six-second callback deadline. It is not proof against all future
duplicates. Both timers and the named MAIN/runtime-error listeners are removed
on success or failure.

The source oracle imports the existing hash-verified `ai-status-oracle.mjs`, which
executes the immutable status, manager, Nemotron, DownloadManager and original
MAIN response/receive functions at pin
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`. Actual Node fs is used. The first
pinned file is missing; no hash, successful file stat or stamp write occurs.
This is a real missing-file result, not an assumed constant helper. The oracle
asserts the exact result/key order, original three-argument outer reply and
listener, native userData capture once, the explicit customPath payload and empty work/error
evidence. It records the four status-related normalized source checksums.

The original inventory execution, native global root/32-byte sum, actual private
Linux D-Bus ServiceUnknown query, no Spotify owner/activation, real source shell
close/wait, suppressed expected source console error and before/after bus checks
remain intact. Source expectations are executed before **and** after each native
phase. Smoke compares all three callbacks, key arrays and contexts, checks all
three command-specific runtime-error channels, and verifies the exact original
global fixture tree/bytes remain unchanged (including absence of a new stamp).
Both source observations and native callbacks stay in `ai-spotify.json`.

The missing-only status case does not independently distinguish every possible
wrong path selection. Global inventory/root assertions remain separate evidence;
the foundation's valid-stamp/customPath differential tests establish that stronger
branch. These GUI fixtures never manufacture a ready stamp or usable models.

## CLI and receipt changes

Oracle syntax is now `ai_spotify_oracle.mjs USER_DATA PHASE [IGNORED_ROOT]`.
The optional third argument defaults to `/ignored-renderer-root` for existing
source-only callers; smoke explicitly passes its actual decoy. No other arguments
are accepted. All prior phase names, including `source-check`, remain valid.
The inherited bus must be private, with Spotify neither owned nor activatable.

The oracle's new `aiStatus` member contains `data`, `dataKeys`, `engineKeys`,
`messageKeys`, `outer`, `listener`, `sourceReplyArgs`, `payload`, and `evidence`.
Its provenance adds `aiStatusSha256`, `nemotronSha256` and
`downloadManagerSha256` (manager hash already existed). Browser receipt adds
`aiStatus` with the common callback metadata/data and the two key arrays;
`callbacks` and `sendUndefined` now include `AI_GET_STATUS`. No prior inventory or
Spotify receipt fields are removed. No raw bus address or machine identity is
archived.

## Focused local checks

- Node22.21.1 syntax and actual probe function construction; Python smoke compile;
  repository Prettier and diff checks.
- One owned `dbus-run-session` source-only run covers three phases, before/after
  each: six genuine original Spotify shell queries, all ServiceUnknown and waited;
  exact unchanged 32-byte AI fixture and explicit missing Nemotron response.
- The actual exported probe is evaluated under controlled JS boundaries using
  those original-source values: three positive phase controls plus twelve
  negative controls. Null/unknown/wrong correlation is ignored; message/status
  private fields, key order, numeric ready, extra argument, each full-context
  component, runtime error, final relevant duplicate, nonvoid send and missing
  status callback reject. Listeners and timers are verified removed in every
  case. Only the deadline control shortens the six-second timer in its private
  test environment. These are helper controls, not native/browser evidence.

Local receipts are `/tmp/freeshow-ai-status-desktop-source-controls.json` and
`/tmp/freeshow-ai-status-desktop-probe-controls.json`, with explicitly labelled
scope. Their bounded private runners are alongside them. No Cargo, native
binary, GUI, performance run or shared build cache was used. Coordinator still
must run the actual normal/restarted/switched smoke against its built native app.
