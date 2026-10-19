# Ordinary retained Spotify bridge verification

This slice changes only `tests/parity/bridge.test.mjs` and this note. The dynamic
inline-module loader now resolves the production bridge's `./spotify` import;
the test continues to execute the actual `createRustApi` implementation and its
actual hydrator rather than replacing either with a copied bridge algorithm.
No production bridge, native owner, shared protocol, manifest or lock changes
are included.

Five new tests exercise the ordinary retained API:

- Four real immutable-source/native-parser reply cases pass through
  `api.send("MAIN", {channel:"SPOTIFY_GET_STATE", ...}, listenerId)`, the injected
  asynchronous transport, and the receiver registered after send. They compare
  the actual Linux source result against the actual native `WireState` after
  bridge hydration. Cases include source null with paired empty restoration,
  Unicode text, own undefined album art in its original key position, ordinary
  finite values, a decimal precision boundary, and source-reachable Infinity/NaN.
  `Object.is` compares every primitive, including exact finite binary64 values.
  Send returns undefined immediately; callback arguments retain the original
  listener, and the message has only `channel,data`. Native placeholders,
  restoration, context and `autoProfile` do not leak or mutate the wire fixture.
- A pending Spotify transport remains held while an actual retained
  `UPDATE_DATA_PATH` request completes. The old full context's malformed reply
  is silently discarded before hydration; the same malformed restoration in a
  new request's current context reports `spotify_publication` without delivery.
- Absent, foreign-runtime, older-generation, future-generation and exact-path
  mismatches are discarded before malformed restoration is examined. A separate
  valid future source-null reply cannot advance the selected context.
- Actual send/receive requests reject wrong outer/inner request channels,
  mismatched or absent listener IDs, wrong reply routes and missing restoration.
  Rejection preserves the selected context and delivers no callback.
- An unsolicited asynchronous Spotify envelope cannot impersonate a response.
  Spotify replies also cannot initialize an unknown selected runtime context.

The source is loaded by the existing `spotify-oracle.mjs` helper from immutable
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/utils/spotify.ts`, checking
the CRLF-normalized SHA256
`75d1ceab68e46c2a21dfc5a61f4e667f1d7d130e7b34b377938b7a97082918fd`.
Only the source OS/exec boundary is controlled. The native parity executable's
real `parse` operation produces each wire fixture; no native Spotify service or
D-Bus connection is started. The fixture driver is actually closed and awaited
in a `finally` block. The other new tests inject response envelopes at the real
bridge transport seam to certify rejection and lifecycle ordering, not native
backend execution.

The existing seven `spotify-hydration.test.mjs` suites cover the wider restoration
schema, including negative-zero/negative-infinity tags which are not claimed to
be source-reachable through the current Linux regex parser. These new tests add
the retained request/receiver boundary rather than duplicating that entire
helper corpus. Native host admission, real DBus child ownership, macOS/Windows
Spotify backends, desktop GUI and live playback remain separate evidence.

Focused Node22.21.1 commands (using an already built native fixture driver):

```sh
FREESHOW_SPOTIFY_BIN=/private/target/debug/freeshow-spotify-parity \
  node --test --test-name-pattern=Spotify tests/parity/bridge.test.mjs
FREESHOW_EXIF_PARITY=/private/target/debug/freeshow-exif-parity \
FREESHOW_SPOTIFY_BIN=/private/target/debug/freeshow-spotify-parity \
  node --test tests/parity/bridge.test.mjs
```

The default driver path remains `target/debug/freeshow-spotify-parity` (`.exe` on
Windows). Local verification used the existing coordinator-built immutable
snapshot driver through `FREESHOW_SPOTIFY_BIN`, without building or mutating its
cache. Production code is loaded from the fresh `c0456366` worktree.
The full existing suite additionally needs its original EXIF fixture driver;
local validation supplied that existing binary through `FREESHOW_EXIF_PARITY`.
All five focused Spotify tests and all 42 bridge tests passed, along with
configured Prettier and `git diff --check`. No Cargo, app, GUI or live-provider
run was performed for this slice.
