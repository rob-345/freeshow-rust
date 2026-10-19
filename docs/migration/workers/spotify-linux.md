# Native Linux Spotify state foundation

This slice executes the original Linux desktop Spotify query using real
`dbus-send` children. It does not use the Spotify Web API, credentials, a Node
runtime sidecar, or a replacement disconnected-state constant. Main/IPC/renderer
integration remains coordinator-owned; broad Spotify/provider parity is pending.

## Native API

```rust
use freeshow_spotify::{Backend, Limits, Outcome, Owner, SpotifyService};
use std::time::{Duration, Instant};

let owner = Owner::new(&context.runtime_id, context.library_generation, &context.data_path)?;
let backend = Backend::capture(trusted_absolute_dbus_send_path)?;
let service = SpotifyService::start(owner.clone(), backend, Limits::default())?;
let capability = service.ready().await?;
let ticket = service.begin_state(&owner, Instant::now() + Duration::from_secs(2))?;
let outcome = ticket.finish().await?.into_reply(&owner)?;
let wire = outcome.wire();
```

`Owner` captures all three RuntimeContext fields and a private Arc identity seal.
An equal-looking foreign owner cannot submit, retire, or consume a receipt.
Owner/backend/receipt have no Deserialize implementations. The host must derive
them from native authority, revalidate the actual full library/window context
before publication, and actually retire/join an old service before replacing its
native owner. The library identity is diagnostic provenance; it never selects a
bus, file or Spotify account. Linux source has no cached Spotify account/state
to reset.

A host may retain one private process-lifetime Owner/service across library
switches. It must separately capture and validate each real request's full
RuntimeContext before and after the query, cancel stale Tickets, and publish only
under that request context. Receipt/status context then describes the private
process owner, never the current library request. Ordinary library retirement
must not call `retire`/`close` on this global service: those operations permanently
seal it. Ticket cancellation/drop retains real child cleanup on the same owner;
terminal application shutdown still awaits its actual join. Backend, not Owner,
contains all native IO authority.

`Backend::capture(&Path)` requires an absolute native Unicode executable path.
It captures only `DBUS_SESSION_BUS_ADDRESS`, `LANG`, `LC_ALL`, `LC_CTYPE`, and
`HOME`, at most 16 KiB in aggregate. Values must be Unicode without NUL. A single
existing `unix:` session-bus address is required; address-list fallback, TCP and
libdbus autolaunch are explicitly unsupported. No per-query input can change
path, environment, destination, object, interface or properties. Backend and
State Debug output redact native configuration/metadata; diagnostics are static
bounded strings and never include output, addresses or tokens.

`ready()` returns `Capability::{Pending,Available,Unavailable{code}}` observations
(it waits past Pending). Available means an actual bounded `--help` child entered,
returned the supported usage marker and was waited, not that Spotify is running
or that a server accepted a request. Real dbus-send1.14.10 returns exit1 with help
on stderr, which is deliberately accepted. Failed capabilities retry only when a
new actual query is admitted. Binary disappearance updates capability; restoring
the same trusted path allows a request-driven reprobe. No background retry runs.

`Outcome::State(State)` carries original native f64/string values.
`Outcome::SourceNull(NullReason)` distinguishes missing title, genuine
ServiceUnknown and ordinary process failure internally. Unavailable binary or
session, unsupported platform/encoding, cancellation, deadline and resource
rejection remain explicit native errors. They never become fake successful nulls.

`Outcome::wire() -> WireState` has public fields:

- `data`: original state object or null;
- `undefined_fields`: currently `albumArt`, when that original own property is
  undefined; its JSON placeholder is null;
- `special_numbers`: `{field,kind}` for NaN/PositiveInfinity/NegativeInfinity/
  NegativeZero, again with explicit null placeholders.

The serialized private field names are `undefinedFields` and `specialNumbers`.
`NumberRestoration` exposes its `field` and typed `NumberKind`. Restore only those
explicit entries; do not guess undefined or NaN from null. Source object order is
`isPlaying,title,artist,positionSec,durationSec,albumArt,volume,platform`.

Root must return the original `SPOTIFY_GET_STATE` reply (including source null)
with its listener/full context and appropriate private restoration metadata.
This API introduces no source success ACK, provider connection event or ALERT.
Spotify's source errors are console diagnostics. Album-art strings are returned
unchanged; this foundation performs no image/network fetch or WebView delivery.

## Exact source boundary

Pinned source: `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`,
`src/electron/utils/spotify.ts`, CRLF-to-LF SHA256
`75d1ceab68e46c2a21dfc5a61f4e667f1d7d130e7b34b377938b7a97082918fd`.
The exact `[Main.SPOTIFY_GET_STATE]` property AST-text hash is
`8d655e635c64c162bbf128b08089ec4f48a0f9a13af8979c4874e5f5f6069944`
in original responsesMain.ts (whole-file hash
`70372b48b698f74f239b82f92e48dde69880a7c4d78af1a9f0e1723313fb57a3`).

Every query executes Metadata, PlaybackStatus, Position, Volume **in that order**
using Properties.Get against fixed `org.mpris.MediaPlayer2.spotify`,
`/org/mpris/MediaPlayer2`, `org.mpris.MediaPlayer2.Player`. Four direct children
replace the source shell's four `&&` commands. Nonzero status short-circuits the
remaining calls; partial stdout is discarded, just as the source callback does.
Successful stdout is concatenated before the original regex semantics apply:

- First artist only; absent artist becomes `Unknown`.
- Empty/missing title becomes source null, after all four successful queries.
- Only uint64 length matches duration; the **first positive int64 anywhere**
  matches position. A signed length120000000 therefore produces duration0 and
  position120 seconds, even when the actual Position is3 seconds.
- Any `Playing` substring in concatenated output makes isPlaying true, including
  a paused track named `Playing title`.
- Negative position does not match and defaults0; negative volume defaults1.
  Printed exponent/dot and quote quirks remain source-faithful.
- ECMAScript whitespace and ASCII digit/word-boundary semantics are explicit;
  ordinary Rust Unicode regex defaults would produce different results.

Native calls are serialized. Original arbitrary concurrent Linux requests could
interleave four-call sequences; this finite owner does not reproduce every such
race. The retained frontend normally has one fetch in flight and already owns
its 1s/3s polling and50ms interpolation; do not add a native idle polling loop.
The four reads are not an atomic Spotify snapshot in either implementation.

## Ownership, bounds and shutdown

- Exactly one fixed Rust owner thread/current-thread Tokio runtime is allowed per
  process until actual join. No thread, task or runtime is created per request.
  At most one direct dbus-send child is alive at a time.
- Four whole queued/accepted/completed-uncollected jobs are charged until the
  actual receipt is consumed/dropped. The four-slot semaphore is process-global,
  so retiring a service with held results cannot create a fresh four-slot budget.
- Queue capacity4, immediate admission. A dropped ticket cancels queued/accepted
  work but does not release its lease before the owner settles actual cleanup.
- Four process-global lifecycle observers total across ready/shutdown and scoped
  replacements; even unpolled old observers remain charged. Excess observers reject
  immediately. `close()` seals and cancels independently of every job/observer
  permit, so full quotas cannot prevent real cleanup.
- Default total query deadline2s, maximum10s, including queue wait and any
  request-driven capability reprobe. Checks run before entry, during IO and after
  bounded parsing. Startup help has its own bounded deadline.
- Default aggregate query stdout256KiB/stderr64KiB; configured maximum1MiB/256KiB.
  Help output is separately capped8KiB per stream. Reads use4KiB chunks. Output
  caps, deadlines and cancellation kill then actually wait for the same child.
- Regex work is linear over at most1MiB; patterns are fixed and initialized once.
  UTF-8 output is required. Original Node's replacement of malformed byte output
  is not certified; native returns an explicit unsupported-encoding error.
- The actual Child slot and work lease live outside fallible query futures. A
  controlled query/probe panic is caught; the retained child is killed/waited
  before the original lease/result settles. A genuine wait fault is sticky and
  retains that child for cleanup retry, rather than certifying drain.
- `retire(&owner)`/`shutdown()` cancel and drain accepted work, then actually join
  the owner. Aborted/timed-out observers leave the same handle owned for retry.
  Sticky first failures survive repeated shutdown after actual cleanup.
- `status().joined` is actual thread join. `drained` also requires zero active PID
  and spawned==reaped. `retained` is separate and can include already completed
  old-context results. `running` reports owner-body completion, not a join proof.
  Status is diagnostic, not a reservation.

Final thread join is synchronous only after the owner body has settled real IO;
final TLS/destructors remain nonpreemptible. Last-Service Drop closes and joins
as an emergency fallback and can block: the host should explicitly await
shutdown before releasing dependencies. No await occurs after taking the sole
JoinHandle, so observer cancellation cannot detach it. OS spawn/kill/wait and
bounded parser execution have cooperative limits, not a hard syscall deadline.
Child process RSS/internal libdbus allocations are not an OS-enforced memory
limit. Fatal runtime/allocator faults and external SIGCHLD/wait interference are
not a recoverable supported backend contract.

Cleanup covers owned direct client children. The existing session daemon and
the OS provider are external owners; a real daemon may activate an installed
service as the original calls do. This is not ownership of a desktop Spotify
process, a process-tree kill guarantee, physical playback or server acceptance.

## Evidence and integration prerequisites

Nine Rust tests include portable parser/configuration/unavailable-owner tests
that require no dbus installation. Linux controls use immutable executable shell
fixtures as real process fixtures, not a production backend substitute. They
exercise stdout/stderr caps, deadline/cancel, startup-probe cancellation, a
witnessed post-spawn panic held before real wait, caller drop, source-context
retirement, old ready or lost-sender results/global quota, four observer saturation
and aborted shutdown retry held before genuine owner exit. There are no ignored
test cases.

Six Node22 suites run the actual immutable TS helper, plus real native children,
on a private Gio/GDBus MPRIS service. They cover nine actual media/error profiles,
ServiceUnknown, exact property order/short-circuit, own undefined, IEEE values,
quoted/Unicode strings, held real methods, quota/drop/cancel/deadline/CLOSE, and
native Unicode executable-symlink disappearance/restoration. Every successful
native terminal case checks actual child PID disappearance or waited counters
and join. Fixture setup/cleanup owns and awaits every daemon/service/driver;
failure cleanup is not counted as successful native drain.

The private bus has an explicit session config, same-user EXTERNAL auth and no
service directories/includes. A missing-name test cannot activate any ambient
installed Spotify service. No actual Spotify/account/auth is used. The test
service owns its fixed name before READY; held methods are released by explicit
control after witnessed entry, not by a sleep scheduling assumption.

Local real-tool evidence: dbus1.14.10, PyGObject3.48.2, GLib2.80.0, Node22.21.1,
Rust1.99.1. Linux source tests require `/usr/bin/dbus-send`, `dbus-daemon`, and
`/usr/bin/python3` with `gi.repository.Gio/GLib` (Ubuntu packages `dbus`,
`dbus-bin`, `python3-gi`, `gir1.2-glib-2.0`). Missing Linux prerequisites fail the
tests; they do not silently become parity skips. Mac/Windows skip only the four
explicit Linux real-bus suites and still run the portable source/parser and
explicit unsupported-backend suites. No native macOS/Windows runtime is claimed.
Final focused checks: nine Rust tests, six Node22 suites with no Linux skips,
and warnings-denied all-target Clippy on Linux, aarch64-apple-darwin and
x86_64-pc-windows-gnu. The latter two are compile evidence only.

```sh
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo build --manifest-path crates/freeshow-spotify/Cargo.toml --offline --locked
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test --manifest-path crates/freeshow-spotify/Cargo.toml --all-targets --offline --locked
FREESHOW_SPOTIFY_BIN=/private/target/debug/freeshow-spotify-parity \
  node --test tests/parity/spotify.test.mjs
```

The Node default binary is root `target/debug/freeshow-spotify-parity` (`.exe` on
Windows). The binary is a bounded test driver, never a runtime sidecar. Root must
add the crate to its workspace and remove its temporary nested `[workspace]` and
standalone lock when integrating. Dependencies are already present in root's
lock graph: serde/serde_json(preserve_order), regex1, futures-util0.3,
tokio(rt/process/io-util/sync/time/macros), tokio-util(rt). No core/native/IPC/CI
files are changed by this worker.

Remaining: actual root command/hydration/context/shutdown binding; retained
WebView UI/album-art integration; Spotify desktop runtime evidence; commands,
macOS AppleScript/TCC, Windows libspotifyctl/bridge lifecycle, OAuth/provider
startup imports; packaging/tool discovery and missing-session/autolaunch variants.
