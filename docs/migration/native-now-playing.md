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

- Full portable workspace: 459 Rust tests pass. Five ignored subprocess entry
  fixtures are invoked by their supervisor tests.
- Full source-parity suite: 485 pass; four existing platform/capability skips.
  Twenty retained frontend unit tests pass. Workspace and native all-target
  Clippy deny warnings; formatting and focused TypeScript checks pass.
- Full native run passes 307 executions (297 host integration plus ten desktop
  binary unit cases), including repeated shared modules and the four owner tests
  with actual terminal-error persistence after joined cleanup and retry.
- Real Linux custom-protocol desktop: 160 editor steps, four GTK menu steps and
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

The GUI receipt is `test-output-rust/desktop/now-playing.json`, alongside the
complete editor receipt. The latter retains 54 diagnostics in this local run,
including the deliberate image capability rejection and expected unsupported,
stale, backend and filesystem cases; it is not a zero-error application claim.

## Remaining acceptance gaps

Native JPEG input, broader PNG/color/alpha/chunk handling, certified resizing,
Electron compressed-byte fidelity and arbitrary nativeImage formats remain
incomplete. The GUI checks PNG existence/dimensions; independent foundation
tests provide the narrower measured pixel evidence. No GUI pixel or encoded-byte
equivalence is claimed here. Source Buffer backing-allocation layout is not a
transport fidelity claim.

Common `picture` insertion position relative to other root tags remains
unverified: GUI evidence preserves both raw observations and compares all typed
values/nested field order while explicitly excluding that root placement.
Metadata formats/limits beyond the certified parser corpus, non-Unicode source
variants, unrestricted resource sizes, physical audio, packaged codecs,
Windows/macOS GUI and successful OS folder reveal remain open.

Failed Audio-directory creation currently omits the source's caught folder
ALERTs even though path/false filesystem behavior continues. The verified counts
are four for the stated SET fixture, two for UNSET and one before OPEN's missing
file alert. Repair requires bounded multiple publications without replacing a
pending metadata receipt; see the [source-backed plan](workers/remote-cover-plan.md).
The remote `/nowplayingcover` endpoint still returns an explicit unsupported
response. Those gaps remain in the acceptance inventory.

These changes establish no new performance gain. Concurrent ownership and
resource limits are tested; comparable release measurements remain required.
