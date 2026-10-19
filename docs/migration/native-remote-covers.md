# Native remote now-playing covers

The Rust desktop now configures a native fixed-cover source for each retained
MAIN START run. REMOTE, STAGE and CONTROLLER use the same bounded source contract.
The current anchored filesystem backend is verified on Linux. Other platforms
return an explicit unsupported preparation; compilation is not filesystem or GUI
parity. Complete remote-cover parity remains unfinished.

`remote_cover.rs` captures the full native library context, the existing ordered
library owner, fixed-file preparation and independent folder notifications. No
HTTP target, query, header or renderer field supplies a filesystem path. Each
request prepares only `Audio/NowPlayingCover.png` under that captured library.
Opaque and empty existing files remain ordinary representations; image decoding
is not an HTTP prerequisite. Missing is the actual existence-guard outcome,
separate from later stat/open/read/authority errors.

Preparation and pre-header validation both await actual accepted ordered jobs and
their library phase fences, including error paths. The holder retains its opened
root/Audio/file authority and its one validation continuation through body work.
The original signed POSIX timestamp is projected with Node's separate binary64
casts. Unsupported representation projection becomes a retained failed state;
it cannot erase a genuine earlier folder-mkdir observation. This is not an
immutable snapshot guarantee after headers: later file mutation can still fail
body reads.

The source-compared HTTP planner preserves tested GET, HEAD, single/combined
ranges, conditional requests, weak ETag, Last-Modified, fixed PNG MIME, zero bytes,
missing-file responses and automatic OPTIONS. Production finalhandler HTML is
selected only by captured exact native `NODE_ENV=production`, matching Express's
environment boundary. Absent/development/other values retain explicit unsupported
412/416 rendering; Rust debug/release compilation does not select semantics.
The bounded supported date grammar does not cover every V8 Date.parse spelling.

The transport receives Hyper's parsed URI. Its 8,192-byte matcher limit applies
after URI parsing; the complete HTTP head has a separate 16 KiB cap. Literal
fragments can be discarded before matching. Actual raw TCP comparisons confirm
source/native agreement for ordinary and long fragments, while a backslash before
`#` remains a source-200/native-400 difference.
[Measured framing boundary](workers/remote-cover-raw-target.md).

Two existing fixed file workers own preparation, validation, body streaming and
unconditional native finish. Eight queued file jobs, 32 KiB chunks, two queued
chunks and the existing whole-request admission bound remain in place. A shared
lease survives both actual HTTP-body disposition and native holder cleanup.
Body/run cancellation closes its owned connection and observes actual body drop;
it does not select-drop entered native futures. The same runtime remains alive
while file futures drain, before the actual file threads join. A stop timeout
retains unfinished ownership for retry. Trusted callback panic and non-preemptible
OS/native calls remain explicit limitations.
[Transport contract](workers/remotes-fixed-cover.md).

A real fixed-Audio mkdir failure publishes the original MAIN ALERT before the
HTTP continuation. That notification uses its own native token/deadline after the
actual observation, so a later HTTP cancellation cannot erase it. Headers/404 do
not await UI acknowledgement. A process-wide ordered enqueue lane and four
native receipt slots preserve private sequence/provenance outside the original
`{channel,data}` callback. The main-only completion command validates the saved
full context and nonce; a historical acknowledgement disposes a receipt without
adopting that context or authorizing a path operation.

Native finish independently observes accepted notification delivery for up to
five seconds, then disposes actual file/validation ownership. Unknown UI delivery
stays charged in the process-lifetime registry. Ordinary context replacement does
not prove a queued WebView event was consumed and cannot release that quota.
After four permanently unknown deliveries, new folder notifications fail at the
explicit bounded capacity until genuine completions or terminal shutdown.
[Receipt ownership](workers/remote-cover-publications.md).

The retained shared bridge checks private cover events before generic context
adoption, forwards only original callback fields, and retains four completion
promises across context changes and helper replacement. Remote STOP leaves its
library-context admission open. Terminal CLOSE still attempts native retirement
after a frontend observation timeout. Native library change retires obsolete
publication observations before remote drain; terminal CLOSE wakes them before
dependent shutdown. START marks the new context ready only after the old actual
remote/body/file owners have drained. The emitter belongs only to the accepted
START job and actual run; no permanent source factory/AppHandle is stored in
AppState or the publication registry.

Focused evidence includes six actual library-source tests, an actual TCP desktop
host test serving all three ports and holding STOP through an independent folder
receipt, plus the earlier eleven anchored-file and eight publication tests. A
Weak source witness confirms joined STOP releases the actual per-run emitter.
Eight shared-bridge integration tests run the real exported bridge and helpers.
Configured transport tests compare 84 complete actual Express/native HTTP
responses, and separate raw-TCP controls exercise the parser boundary. Strict
all-target compiler/lint checks of the actual native modules pass on Linux,
Windows GNU and macOS ARM; those cross checks do not establish native OS runtime
or browser behavior.

The rebuilt full Linux editor passes 187 steps, four GTK menus and native drops
at both scales. Fourteen actual cover phases make 141 HTTP requests on all three
ports: 60 status-200 responses (including OPTIONS), 63 missing responses, nine ranges and
nine not-modified responses. Independent Python reads compare actual bytes and
hashes, MIME/length, one-byte ranges and ETags. OPTIONS runs before folder effects.
A real regular-file Audio blocker produces exactly three source-shaped alerts,
with undefined listener/private-field stripping and the actual visible popup;
its independently read bytes remain unchanged. A library switch returns missing
on all ports while preserving the old library's opaque cover sentinel. Every
phase observes zero cover runtime/browser errors, no source ACK, no reveal
callback advance and final ordinary zero file/publication retention. Separate
native STOP/lifetime tests provide the actual join/owner evidence described
above. The whole-editor receipt retains 55 expected diagnostics from deliberate
capability, stale, backend and filesystem cases. Actual backup/pending-write Quit
and process exit pass. Artifacts include `remote-covers.json`, `now-playing.json`
and the full editor receipt under `test-output-rust/desktop`.

No new application performance improvement is claimed. Source image/metadata,
PNG compressed-byte, resize and platform-specific gaps remain separate from
ordinary fixed-file HTTP serving.
