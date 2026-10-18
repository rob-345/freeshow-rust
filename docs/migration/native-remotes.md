# Native remote, stage and controller integration

The unchanged Svelte clients and main-window handlers use the bounded Rust
`freeshow-remotes` Socket.IO transport. This covers text and control traffic;
the network feature area remains partial. The source oracle is FreeShow
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975` (1.6.7-beta.2).

`MAIN.START`, `MAIN.STOP` and `MAIN.SERVER_DATA` retain their original payloads
and produce no invented success reply. Outer `REMOTE`, `STAGE` and `CONTROLLER`
sends retain the source ID targeting and broadcast rules. Native lifecycle and
renderer sends are main-window-only. Existing Svelte code still handles passwords,
stage layout authorization, show queries, actions and replies.

## Concurrency and ownership

One native lifecycle task admits at most 32 requests, including the active
request. Accepted jobs retain their leases after an IPC caller disappears.
One event pump merges the transport's reserved lifecycle and bounded data queues.
The three listeners share the transport's dedicated current-thread Tokio runtime;
static file work uses its two fixed workers. Library reads and backup compression
use their existing independent owners.

The event pump permits one private renderer delivery at a time. Its unpredictable,
one-use nonce is completed through `remote_delivery_complete`, a separate native
command which never queues behind lifecycle work. Completion means synchronous
retained receiver dispatch was accepted, including the original receiver-error
reporting behavior. It is neither a Socket.IO acknowledgement nor completion of
asynchronous renderer actions. A five-second delivery failure stops admission and
initiates terminal transport cleanup instead of continuing to fill a WebView queue.

Every binding captures the full runtime ID, library generation and data root,
plus the transport's private run ID. The separate `nativeRemote` envelope carries
the actual socket identity, trusted lifecycle origin and unchanged raw message.
Truthy primitive/array messages and client-supplied/spoofed IDs keep their source
behavior. A payload shaped like CONNECTION or DISCONNECT cannot create native
lifecycle authority. Numeric JSON wire fidelity is described in
[the transport contract](workers/remotes.md).

The host conservatively records a genuine connection before synchronous
publication, then distinguishes confirmed delivery from potential delivery when
completion is lost. This prevents a timeout from silently forgetting a connection
which already reached the interface. An explicit rejection releases potential
ownership. Retiring a run marks it under the publication mutex, stops and joins
the transport, then serially completes retained DISCONNECT cleanup for owned IDs.
Publication holds that mutex only through validation and synchronous sink
acceptance; completion waits and stop/drain work run without it.

The bridge permits only an exact native `{channel: "DISCONNECT", id: realSocket}`
with matching frozen ledger ownership to cross a library switch. Cleanup neither
advances nor retags the current context. New old-run actions are rejected. The
host keeps its retiring binding until cleanup completes. Retirement has a
15-second aggregate renderer-cleanup deadline, in addition to the individual
delivery deadline. Pending completion ownership clears even on timeout.
Unresponsive or failed cleanup remains an explicit failure; the deadlines do not
preempt an operating-system filesystem call or prove a global shutdown bound.

Context refresh stops and drains the previous run. It does not start a replacement
before the UI receives DATA_PATH. The unchanged SETTINGS handler then starts the
listeners using the selected library's settings. Root selection runs in one of
four bounded, owned continuations, so IPC cancellation after the library commit
cannot skip its cleanup hooks. Close stops root-change admission and drains those
continuations before finishing library writes and shutting down services.
Terminal cancellation has a separate control path and can cancel ordinary queued
remote work; it cannot reopen listeners. The host explicitly ends its event pump
after cleanup rather than depending on channel EOF while service clones exist.

## Real client resources

The independent frontend build compiles the original remote, stage and controller
entries with their original server Vite configuration. It copies the original
HTML, manifest, icon, service worker and stage auxiliary files into
`dist-rust/remotes/{remote,stage,controller}`. The native resource mapping copies
those files next to development binaries and into platform bundle resources.
The host uses Tauri's platform resource directory and supplies explicit absolute
roots. It does not assume embedded frontend assets are filesystem paths or serve
test fixture HTML in production. Development startup builds these client resources
before starting the desktop Vite server.

## Verification and remaining work

The immutable source/live transport suite exercises real polling, WebSocket and
upgrade clients, ID routing, soft limits, subscriptions, numeric tokens, malformed
messages, static HTTP behavior, saturation, restart and completed teardown. Native
host tests exercise real polling clients, targeted Unicode replies while library
IO is blocked, trusted origin, old-context cleanup, caller cancellation, full
public admission, non-starvable shutdown and lost completion cleanup. Bridge
tests exercise raw messages, send-time snapshots, ordering, and exact owned
cleanup across context changes.

The desktop acceptance probe uses real external clients against the retained
main-window handlers, including wrong/correct remote and stage passwords, show
queries, controller replies, and library switching. A separate ephemeral WebKit
view loads each actual client page; its DOM and resource observations are recorded
in `test-output-rust/desktop/remotes.json` when the workflow succeeds.

Still unported or uncertified: OUTPUT_STREAM and native OUTPUT_FRAME/capture,
stage mirror frame acquisition, Bonjour advertisements, now-playing covers,
binary attachments and nonfinite/lone-surrogate wire cases, complete static HTTP
edge behavior, broad remote/controller action and stage-item workflows, actual
Windows/macOS browser and hardware behavior, service-worker lifecycle, IPv6 and
cross-platform packaged installation. Already-running asynchronous retained
actions can finish after a library switch; synchronous delivery completion does
not cancel or fence them. Raw client lifecycle-shaped messages can still alter
the source connection store; the private ledger guarantees cleanup only for
genuine native connections. No whole-application performance gain or complete
network/application parity is claimed.
