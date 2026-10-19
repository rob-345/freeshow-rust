# Native now-playing integration

The retained Svelte API's `NOW_PLAYING`, `NOW_PLAYING_UNSET` and `OPEN_NOW_PLAYING`
now use the Rust desktop host. The source baseline remains FreeShow
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`. This is a bounded, partially verified
feature milestone; complete migration acceptance still fails intentionally.

One lazy Tokio supervisor overlaps at most four metadata preparations and runs
one source continuation phase at a time. It composes the existing authorized
asset descriptor owner, fixed audio parser and cover/image workers, typed source
planner, phase scheduler, ordered library filesystem jobs and actual shared
Icecast pipeline. Source metadata publication precedes title/cover/text effects;
private UI completion retains resources without blocking the continuation.
UNSET preserves the source's pending-SET behavior. Source commands remain void.

Actual failed Audio-folder creation now publishes the original caught ALERT
before its source continuation advances. Each operation retains all of its
publications independently: at most one metadata receipt plus four folder alerts,
with twenty native publications and twenty entered UI completion promises across
the four whole operations and context replacement. A rejected sibling cannot
release another pending receipt. The fixed folder comes only from the actual
ordered filesystem observation; private kind/path provenance stays outside
original callbacks. See the [alert contract](workers/now-playing-alerts.md).

The desktop registry retains one sealed four-operation budget across host and
library replacement, including old completed/uncollected results. Accepted work
survives abandoned IPC callers. Actual cleanup and the retained supervisor join
precede dependency retirement. A timeout keeps the same owners for retry; a
joined terminal failure remains observable after result disposal and retry.
Native callbacks own application handles only inside the actual supervisor,
and drop on its exit. Full context fences every native operation/publication.

Raw command framing preserves numeric duration, including NaN, infinities and
negative zero, with a send-time snapshot. Metadata restoration preserves actual
Uint8Array picture contents, own undefined, special numbers and supplied field
order before original callbacks. Private IDs, sequence, hydration and completion
receipts stay outside source payloads. See the [wire contract](workers/now-playing-wire.md),
[native ownership tests](workers/native-now-playing.md) and
[image capability evidence](workers/cover-images.md).

## Local evidence, 2026-10-19

- Full portable workspace: 493 Rust tests pass. Five ignored subprocess entry
  fixtures are invoked by their supervisor tests.
- Full source-parity suite: 563 pass; four existing platform/capability skips.
  Twenty retained frontend unit tests pass. Workspace and native all-target
  Clippy deny warnings; formatting and focused TypeScript checks pass.
- Full native run passes 352 executions (339 host integration plus thirteen desktop
  binary unit cases), including repeated shared modules and the four owner tests
  with actual terminal-error persistence after joined cleanup and retry.
- Real Linux custom-protocol desktop: 187 editor steps, four GTK menu steps and
  native drops at both scales pass. Original LTC and continuous Opus source
  comparisons, actual ZIP workload, library switches and real Quit still pass.
- Four actual WebView SET/UNSET cases execute pinned-source metadata/formatter
  oracles on real WAV/FLAC/MP3 files. They preserve invalid YEAR as NaN, infinite
  duration formatting (`Infinity:NaN`), send-time input, all three picture
  buffers/own undefined fields, exact per-picture order, source text and actual
  cover creation/deletion. Native admitted/completed counters plus real idle
  fields prove work entered and settled; initial zero is not a drain receipt.
- The invalid-image case delivers metadata first, then explicitly reports
  `cover_unsupported`. It never fabricates an empty successful image/text write.
  No listener-specific source success ACK occurs in any case.
- Two more actual WebView cases use unchanged JPEG MP3 APIC and FLAC PICTURE
  files. Their packaged Electron recording includes actual nativeImage, complete
  SET effects and UNSET. Native text and typed metadata match; an independent
  bounded PNG decoder reads each actual installed cover and verifies the source
  RGBA hash at 17×11 and 64×48. UNSET empties text/removes the cover. PNG compression
  bytes differ. The fixed Linux x64 GdkPixbuf 2.42.10 subset, entropy/metadata
  preflight and explicit rejection boundaries are documented in
  [JPEG capability](workers/cover-jpeg.md) and
  [desktop recording evidence](workers/jpeg-desktop-evidence.md).
- Three further real WebView cases independently block Audio with a regular
  file. SET preserves metadata then four folder alerts, UNSET two, and OPEN one
  then the missing-text alert. Exact public callback envelopes, visible final
  popup text, counter advances, ordinary idle and unchanged blocker bytes pass.
  The trusted reveal callback counter never advances; this does not assert OS
  reveal success or an IO-thread join.

The GUI receipt is `test-output-rust/desktop/now-playing.json`, alongside the
complete editor receipt. The latter retains 55 diagnostics in this local run,
including the deliberate image capability rejection and expected unsupported,
stale, backend and filesystem cases; it is not a zero-error application claim.

Published HTTP integration `a57338c1` passes all six jobs in
[CI 39239768321](https://github.com/rob-345/freeshow-rust/actions/runs/39239768321),
including Windows/macOS native and the 181-step Linux desktop workflow.
Independently validated downloaded artifacts preserve its seven now-playing
cases, ten cover phases/93 HTTP responses, exact visible messages, callbacks,
counter advances, unchanged blocker/old-library hashes, menus/drops and exit.
Its complete editor receipt retains 54 expected diagnostics. The current
187-step local JPEG/parser run additionally verifies two JPEG cases and fourteen
cover phases/141 HTTP requests; this newer milestone needs its own complete CI.
[Native remote-cover contract and evidence](native-remote-covers.md).

## Remaining acceptance gaps

JPEG outside the checked Linux x64 native consumer and strict certified subset,
broader PNG/color/alpha/chunk handling, certified resizing, Electron compressed-byte
fidelity and arbitrary nativeImage formats remain incomplete. The two new JPEG
GUI cases establish exact source RGBA pixels for their real files; this does not
certify unrestricted JPEG inputs or other OS decoders. Source Buffer
backing-allocation layout is not a transport fidelity claim.

The unchanged original JPEG fixtures exposed invalid ID3 timestamp text and
multiple FLAC comment-block differences before image decoding. A private bounded
ID3 read view preserves plain timestamp text, and the existing FLAC preflight
retains physical comment order before the original priority Collector. Original
files are never rewritten. Alternate timestamp encodings/flags, noncanonical
Vorbis keys/UTF8 and source-accepted malformed/resource cases remain explicit
gaps. See [ID3 evidence](workers/audio-id3-recovery.md) and
[FLAC evidence](workers/audio-flac-comments.md).

Common `picture` insertion position relative to other root tags remains
unverified: GUI evidence preserves both raw observations and compares all typed
values/nested field order while explicitly excluding that root placement.
Metadata formats/limits beyond the certified parser corpus, non-Unicode source
variants, unrestricted resource sizes, physical audio, packaged codecs,
Windows/macOS GUI and successful OS folder reveal remain open.

Folder-failure counts above apply to the stated valid fixture; a formatter error
can stop earlier. The remote `/nowplayingcover` endpoint now serves the fixed
selected-library file on Linux through a retained native source. Tested ordinary
GET/HEAD/range/conditional/missing/OPTIONS behavior, original folder alerts,
context changes and actual native disposal are documented in the
[remote-cover contract](native-remote-covers.md). Broader parser/date/development
error rendering, unrestricted filesystem variants and foreign OS backends remain
in the acceptance inventory.

These changes establish no new performance gain. Concurrent ownership and
resource limits are tested; comparable release measurements remain required.
