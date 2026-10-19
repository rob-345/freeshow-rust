# Native device identity integration

`MAIN.DEVICE_ID` uses the existing ordered library/configuration owner. Its
snapshot, native preparation, actual global `machineId` setter and final source
value stay in one trusted commit. The path comes from native `Stores::path`
for global app-data `config`, never the selected library or renderer payload.
Only the main window is supported. Full runtime/library context and original
listener correlation are checked before admission and again before publication.
The original handler ignores its payload; this host discards it before queueing.

Four shared leases bound admitted observers and accepted configuration jobs.
An accepted closure and its observer share the lease: caller Drop requests
cancellation but cannot free entered/queued work's capacity. The two-second
observer deadline returns an explicit error while the actual accepted closure
remains owned. CLOSE seals admission and cancels outstanding controls; the
existing later library `finish_writes` fence waits for accepted ordered work.
This is a configuration-work fence, not a claim that the existing library's
native threads have been joined or that a renderer has acknowledged delivery.
No worker, thread, child process or idle timer is added by this host.

The global file is opened/read once on that owner with a regular-file check,
32KiB chunks, an 8MiB encoded cap and cooperative deadline/cancellation checks.
Unix uses nonblocking open so a FIFO cannot hang at that boundary. A conservative
32,768 punctuation cap limits general JSON structural work before allocation;
punctuation inside strings can also count toward this resource limit. Native
open/read/write/flush/rename calls are not forcibly interruptible. The identity
value additionally has the preparation module's 8KiB/depth-eight/256-node bounds.
The locked JSON parser recursion limit is explicitly a resource error, including
valid deep unrelated configuration accepted by the actual source Conf getter.
The locked parser diagnostic is classified before the source fallback policy.
Unsupported representations and resource/cancellation/deadline errors remain
explicit; they are not converted into the legacy fallback literal.

Existing truthy IDs, including finite JSON numbers, booleans, arrays and objects,
return without a setter. The original bytes remain unchanged. The bounded raw
identity JSON uses correctly rounded Rust binary64 parsing instead of default
serde decimal reparsing: the actual source decimal `1792415704000.0015` must keep
bits `427a1544d23c0006`, including within nested values. Negative zero, strict
shape/order and last duplicate-key value are preserved. Nonfinite JSON numerals
are an explicit unsupported capability. Other configuration fields retain the
existing store's JSON parsing behavior; this does not certify every global
storage numeric or external-edit case.

Missing/falsy IDs use the separately tested native legacy provider/UUID policy.
The actual setter preserves the complete captured global object and existing
atomic-write behavior. The encoded output bound uses the same pretty JSON as
the actual setter, preventing a rewrite beyond the next query's 8MiB read cap.
A genuine read/parser failure or setter failure follows
the preparation module's source outer-fallback policy. A successful value is
published only after actual setter return and another control check. No ID,
provider token, raw configuration or whole environment is logged. Real generated
IDs in tests use boolean equality assertions so failures do not print them.

`LibraryService::commit_without_media_refresh` preserves the media-folder
snapshot and avoids the generic commit path's additional unbounded CONFIG read.
It retains the existing context validation, ordered admission, mutation fence,
accepted caller-Drop ownership and terminal drain. Existing `commit` still
refreshes media settings. The new entrypoint is trusted native code only and
requires callers to preserve `mediaFolderPath`.

Eight real native-host tests cover global persistence/unknown fields, library
selection, truthy JSON and exact fractional/negative-zero bits, role/stale/resource
and actual parser failures, pretty-output expansion rejected before any write,
valid deep configuration parser-limit rejection, real read-only setter failure,
four actually queued jobs through observer abort/seal/drain, and the real observer deadline retaining
its accepted queued job. A separate core test makes the actual configuration
unreadable, verifies the narrow commit leaves the cached media snapshot intact,
then verifies ordinary commit still refreshes it. These and strict native
all-target Clippy pass locally. They do not establish foreign runtime, complete
GUI startup, or all shutdown/error combinations.

The original Conf getter reads its file on each query and normalizes root values
with Object.assign onto a plain object. The existing native store rejects
non-object roots at startup; corrupt-profile startup and concurrent external writers remain storage compatibility gaps.
The native provider's Linux corpus, Apple ABI provenance, foreign-runtime
obligations and bounded-provider gaps remain in [the foundation](device-id.md).
Provider connection queries are a separate subsequent binding; this commit
does not authenticate providers or implement Spotify.
