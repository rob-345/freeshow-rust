# Native Spotify reply hydration

`src/frontend/rust/spotify.ts` exports
`hydrateSpotify(data: unknown, restoration: unknown): SpotifyState | null`, plus
the `SpotifyState`, `SpotifyRestoration`, `SpotifyNumberField` and
`SpotifyNumberKind` types. It accepts only the Linux native `Outcome::wire()`
schema from `freeshow-spotify`, not arbitrary provider objects.

The coordinator must accept the actual selected full RuntimeContext before
calling it, assign the returned DTO to the original `SPOTIFY_GET_STATE` reply,
and strip the private restoration envelope before source callback delivery.
The private process-lifetime native Owner context is not the selected request
context. The hydrator has no context, listener, replay or native IO authority and
introduces no source success ACK. Placeholder checks reject inconsistent stale
metadata; matching metadata from another request still requires the coordinator's
real context/response pairing.

The private envelope is exactly:

```ts
type SpotifyRestoration = {
    undefinedFields: "albumArt"[]
    specialNumbers: {
        field: "positionSec" | "durationSec" | "volume"
        kind: "NaN" | "PositiveInfinity" | "NegativeInfinity" | "NegativeZero"
    }[]
}
```

Source null requires both lists empty. Non-null data must have exactly eight
own enumerable data properties, in original Linux order:
`isPlaying,title,artist,positionSec,durationSec,albumArt,volume,platform`.
`isPlaying` is boolean and platform is exactly `linux`. Only albumArt can restore
to own undefined, from an explicit null placeholder and `undefinedFields` entry.
Each special number requires its own explicit null placeholder. Entries follow
the native producer's positionSec/durationSec/volume order. Unknown, duplicate,
out-of-order, missing and inconsistent entries reject. Bare null, undefined,
nonfinite or negative-zero numeric values are never guessed into source values.

Title, artist and present albumArt must be nonempty Unicode scalar strings, each
at most 1 MiB UTF-8 (at most 3 MiB aggregate). This explicit conservative envelope
covers every capture from the native parser's maximum 1 MiB combined stdout;
it does not claim three simultaneous maximum captures are source-reachable.
UTF-8 accounting scans the existing strings without allocating encoded copies,
and rejects lone UTF-16 surrogates. NUL is retained because neither source nor
native parser excludes it. Empty artist captures become `Unknown`, and empty
album-art captures become own undefined in the actual current source regexes.

Lists are dense, at most one undefined entry and three numeric entries. Only
fixed known fields are visited; unknown nested objects are not projected or
rewritten. JSON records with foreign prototypes, getters, symbols or hidden
fields reject. The boundary expects already bounded deserialized native JSON,
not adversarial executable JavaScript proxies or unbounded hidden-key objects.
The result is a fresh fixed-order DTO; original data and metadata remain
unchanged on success and rejection. Strings are reused as immutable values.

Seven focused Node22 suites execute the actual pinned original helper and the
real native `WireState` parser driver, comparing Object.is (including IEEE
specials), own undefined, exact key order and empty-capture behavior. Additional
controlled envelopes exercise all four numeric kinds on all three fields,
frozen inputs, malformed/null/unknown/duplicate/sparse shapes, late validation
failure, stale tags, exact UTF-8 boundaries and oversized-list rejection. The
negative-infinity and negative-zero envelope controls certify restoration, not
reachability through the current source Linux regex parser.

The unchanged helper is loaded by `tests/parity/spotify-oracle.mjs` from immutable
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/utils/spotify.ts`;
CRLF-normalized SHA256 is
`75d1ceab68e46c2a21dfc5a61f4e667f1d7d130e7b34b377938b7a97082918fd`.
The native driver is the foundation from `a6ba915d`, built privately; no D-Bus,
real Spotify, GUI, network or playback is needed by these hydration suites.

```sh
FREESHOW_SPOTIFY_BIN=/private/target/debug/freeshow-spotify-parity \
  node --test tests/parity/spotify-hydration.test.mjs
node node_modules/typescript/bin/tsc --noEmit --strict --target ES2022 \
  --module ESNext --lib ES2022 src/frontend/rust/spotify.ts
```

The default test binary is root `target/debug/freeshow-spotify-parity`, with `.exe`
on Windows. Final checks: seven Node22 suites, standalone strict TypeScript,
repository Prettier and diff checks. This worker does not edit shared bridge,
CoreEvent, main, manifests, credentials or runtime stores. Root owns actual
command/context/hydration delivery integration. macOS and Windows Spotify state
schemas remain unsupported by this helper until their backends are implemented.
