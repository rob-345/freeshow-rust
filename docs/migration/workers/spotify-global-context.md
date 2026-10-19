# Accepted Spotify controls survive library selection

Based on `679ac0b1`. This corrects the library-selection cancellation behavior in
the initial native command host; it does not wire the MAIN command or renderer
adapter. The earlier `spotify-command-host.md` cancellation expectations and its
cancellation-removal negative control are superseded by this source evidence.

The pinned `2d94defcf98175e4f5c9504e7f4fd1d33c35c975` source
`responsesMain.ts` UPDATE_DATA_PATH handler only calls
`config.set("dataPath", data.newPath)` and `createStores(data.oldPath)`. The Linux
Spotify seek callback captures its requested value, waits for Metadata, and then
invokes SetPosition; it has no selected-library cancellation input. Playback
controls address the process-global player. The original `true` callback means
accepted scheduling, not child exit, successful peer receipt or audible playback.

## Native behavior and integration boundary

The existing main-role, complete current LibraryService context, listener and
payload/numeric checks still execute before admission. A stale request is rejected
before spawning or accepting effects. Once accepted, library selection does not
cancel the control or reject its original early reply. The event preserves its
original request context and optional listener; it never adopts the newly selected
library. `context_changed` still cancels obsolete state-query observers and prunes
only commands whose real child/continuation has settled. Explicit seal/CLOSE still
cancels all pending controls and joins their actual owner. The existing fixed
operation deadlines remain a resource limit, not source cancellation parity.

The same four whole-job slots and at most four strong, lease-free registry controls
remain. Event/observer Drop neither cancels accepted intent nor releases active
native work. A cloned event can still own its receipt quota after effects finish;
serde cannot reconstruct that private lifetime guard. The process owner, backend,
thread and captured executable/session persist across library selection. No new
thread, task, runtime, completion observer or idle polling is added.

Coordinator's separately owned publication/bridge adapter must match replies to
the **original** origin and listener. An old-generation playback reply may be
accepted only within the same current process runtime; it must neither advance
current generation nor retag its data path. This exception is command-specific;
state-query results retain their existing full-context final fence. Native admission
must still use the actually current LibraryService context.

## Real private-bus and source evidence

The headless suite composes the actual NativeSpotify, SpotifyService,
LibraryService and real UPDATE_DATA_PATH filesystem mutation. An independent
private Python GIO peer holds actual Metadata calls. Cases verify:

- A held seek survives real library selection and event disposal, later delivers
  SetPosition, rejects a genuinely stale new admission without another spawn, and
  reuses the same process owner for a new-library Pause.
- A reply deliberately held after admission is returned with its exact original
  full context and listener after selection, and its actual seek continues.
- Caller abortion after accepted work and real selection leaves the native effect
  owned until the actual continuation finishes.
- Four held controls remain charged across selection and dropped events, reject
  fifth command/state admission, and all deliver their actual continuations.
- Explicit terminal CLOSE still kills/waits a genuinely held child and prevents
  SetPosition. Ready receipts/clones remain charged through actual join/replacement.

For the four selection cases, a separate private peer runs the actual immutable
executeSpotifyCommand handler/receiveMain plus the actual extracted UPDATE_DATA_PATH
handler. The latter's config/createStores boundaries are controlled and labeled;
its exact call order is checked while actual source Metadata callbacks are held.
After release, complete source/native peer effect values and order match. This
combines real native LibraryService mutation with controlled original store effects;
it is not an Electron GUI or account test. Other continuation/numeric cases retain
their complete original-function comparisons.

The draft saturation check once observed only three peer records immediately after
all four no-reply children had exited. Its failure is preserved as
`commands-peer-observation-draft-failure.log`. The corrected test explicitly waits
for all four **actual peer effects** before asserting success. Child exit is never
substituted for peer delivery.

## Focused checks and remaining scope

Private headless manifest/cache only:
`/mnt/data/cache/freeshow-rust-build/spotify-global-context/{check,target}`.
One Cargo job, Rust 1.99.1, offline dependencies, debug/incremental data disabled.
The final native suite has five top-level tests: all seven existing state/source
scenarios plus eight command scenarios pass. Command scenarios observe 41 actual
private-peer effects; six scenarios independently compare 36 of those effects to
original source. All fixture children and real native owners are waited/joined.

Two deliberate negative controls restored the old context-notification cancellation
and the old post-admission final-generation fence separately. The first prevents
the required real SetPosition continuation; the second rejects the held accepted
reply as stale_library. Both tests fail, and the corrected source is restored and
the full focused suite rerun. Logs are `cancel-negative.log`,
`final-fence-negative.log` and `host-tests.log` in the private directory above.

Strict all-target Clippy passes for Linux, Windows GNU and macOS ARM; rustfmt,
configured Markdown formatting and `git diff --check` also pass. Foreign Clippy
is compile evidence only. Linux private buses establish scheduling/peer effects and
owned teardown; there is no real Spotify account, playback, native macOS/Windows
transport, packaged GUI or shared bridge proof in this slice.
