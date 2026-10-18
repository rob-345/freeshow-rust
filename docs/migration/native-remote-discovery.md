# Native discovery ownership for remote listeners

Coordinator integration now captures one frozen network snapshot through the shared fixed OS owner at desktop setup and installs this owner beside the actual remote listeners. Native setup failures are logged and leave ordinary TCP listeners available without fabricated discovery inputs. The default uses production discovery transport; a trusted process-only loopback target is used by the Linux GUI harness. The expanded native tests pass with strict native Clippy. The **148-step Linux editor** verifies actual packets before and after a library change: each phase has 15 exact original record payloads for three actual bound TCP ports, including frozen TXT/address data. The harness retains raw packets, compares the original dependency's real wire encoding/defaults and normalizes only random instance suffixes. It waits for all three current SRV ports on the already-bound private UDP descriptor, rather than treating cumulative status counters as current packet receipts. This is loopback evidence; LAN/native registration and actual Windows/macOS execution still require their own checks.

This integration connects the reviewed native remote owner to the reviewed
discovery owner. It changes only `src-tauri/src/remotes.rs`, its direct tests and
this note. Application setup, IPC, dependencies, CI and shared snapshot ownership
remain coordinator work. It does not claim LAN discovery, native macOS registration
or complete remote/stage/controller feature parity.

## Native construction and authority

```rust,ignore
// Once at setup. The shared OS owner also serves fresh MAIN.IP requests.
let snapshots = NetworkSnapshots::start(SnapshotLimits::default())?;
let frozen = FrozenDiscoveryInputs::capture(&snapshots).await?;
let (discovery, discovery_events) = NativeDiscovery::new(
    library.clone(), NativeDiscovery::production_config(), frozen,
)?;
let remotes = NativeRemotes::new_with_discovery(
    library.clone(), remote_config, discovery, discovery_events, main_sink,
)?;
```

`new_with_discovery(library, remote_config, NativeDiscovery,
NativeDiscoveryEvents, sink)` takes native lifecycle/event ownership. The legacy
`NativeRemotes::new(library, remote_config, sink)` remains available for transport
users and existing tests. The two constructors share the same bounded owner and
renderer pump. No extra logger or per-publication task is spawned. Construct all
native services from the same `LibraryService`; never supply discovery snapshots,
executable paths, run tokens or endpoint evidence from renderer JSON.

The caller may retain a native discovery clone while constructing if it needs to
await cleanup after a constructor error. Dropping a constructor argument requests
its existing native cleanup; a failed synchronous constructor does not promise
that asynchronous teardown was joined. After successful construction, the remote
owner is the exclusive discovery lifecycle owner. A monitoring clone must not
start/stop competing runs. Do not register a second application event consumer.

The coordinator routes the existing commands unchanged. `MAIN.START` retires the
previous run then starts requested listeners, `MAIN.STOP` retires them,
`MAIN.SERVER_DATA` updates server data, and outer `REMOTE`/`STAGE`/`CONTROLLER`
messages retain their existing renderer-send path. There is no new discovery
command and no successful renderer or Socket.IO ACK. Retained authorization,
client-supplied message IDs, trusted lifecycle origins, subscription rules and
static resource roots are unchanged.

## Bound ports and source behavior

The oracle is unchanged Electron commit
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975` (1.6.7-beta.2).
`src/electron/servers.ts` invokes `publishPort` from each enabled listener's
`onStarted`; `closeServers` invokes `unpublishPorts` before closing Socket.IO.
The discovery helper has no success response and logs publication failures.

The host waits for `RemoteService::start` to return actual `BoundServers`, checks
the full current `RuntimeContext`, and installs that frozen binding while early
connection publication is gated. When there is at least one bound endpoint, it
begins discovery with the actual remote generation's run nonce. It retains the
opaque `DiscoveryRun` beside that exact bound/context and maps only returned
endpoints to `REMOTE`, `STAGE` and `CONTROLLER`. Disabled/unbound servers and a
completely disabled run create no advertisement. A listener-start failure cannot
reach discovery begin. Endpoint paths/ports in `SERVER_DATA` remain ordinary data
and cannot replace native bind evidence.

Names remain `freeshow-NAME-6hex`, with the frozen startup suffix and TXT IP list.
The type stays `_freeshow._udp.local`, although these three listeners use TCP.
Repeated publication retains its first port until restart; an actual restart has
a new listener/discovery generation and uses the new actual endpoints. The
source's configured-port shortcut is deliberately replaced with kernel-bound
port evidence. In particular, a trusted ephemeral-port test option publishes the
returned nonzero port, whereas original `onStarted` passes its configured value.
Ordinary nonzero successful bindings have the same port. No requested zero port
is advertised as a successful endpoint.

`Published` evidence means a local UDP send was accepted; `Spawned` means process
creation. Neither proves peer receipt or dns-sd registration. Discovery bind,
spawn and publication errors warn and remain in bounded status without disabling
successfully bound remote HTTP/Socket.IO listeners. A source-stage frame, output
stream capture, now-playing cover or additional listener is not invented here.

## Lifecycle, bounds and teardown

Discovery events are consumed in the existing command owner after terminal,
context and public command priorities. Its queue remains the native discovery
owner's bounded 64-event queue. A successful evidence event must match the current
full library context, remote listener run and opaque discovery generation. Old
events are counted and discarded; they are never retagged. The host retains only
one last accepted evidence event, one bounded 2048-byte warning and saturating
counters. Service status retains its existing bounds and dropped-event counts.
These diagnostics never enter the renderer delivery queue.

The existing remote admission bounds remain 32 accepted public jobs, one owned
context control, one renderer completion in flight and bounded remote sessions.
Discovery keeps its existing 16-job native admission, four named publications,
socket/child limits, bounded DNS parsing and bounded response/event queues. There
is one remote service runtime/thread, one discovery service runtime/thread and
the existing native owner/pump/supervisor tasks. The shared snapshot worker is
created once by application setup; this integration creates no OS collector.

Start/publication, stop and terminal discovery waits each have a three-second
cap. The one lifecycle slot stores an owned future before awaiting it:
`Starting -> Ready(opaque token) -> Stopping`. Caller Drop, terminal cancellation
or operation timeout cannot discard an accepted begin's future or its eventual
token. STOP/restart can collect that result and stop the exact run. A timed-out
stop retains its same future/token for a later attempt. Another discovery begin
cannot replace unfinished ownership. These are bounded native continuations,
not detached tasks. Operation timeouts warn as
`remote_discovery_operation_timeout`; they do not claim the outstanding work was
canceled or free its resources.

Normal retirement first marks the remote binding retiring, stops the exact
discovery run, then stops listeners and drains the existing accepted connection
ledger. Discovery errors cannot skip either later phase. Publication locks are
released before retirement waits, so old-context disconnect completions can
progress. Across library selection, existing accepted genuine disconnects keep
their original context and reach retained cleanup; stale client actions remain
filtered. No context advance or cleanup retagging is added.
The same accepted context continuation also invokes discovery's reserved owned
`context_changed` control even when the remote binding has already disappeared.
This covers outstanding token-less begins and timed-out stops; timeout/drop does
not lose that coalesced cleanup, and its full-context check protects a fresh run.

Terminal cancellation first requests discovery's complete shutdown, which also
cancels accepted begins whose token has not yet returned. The actual shutdown
future is stored separately and survives a timeout or dropped shutdown waiter.
Remote listener/runtime shutdown and retained renderer cleanup still proceed.
Remote owner/pump completion only proves those loops finished. Public
`NativeRemotes::shutdown` must additionally collect the retained real discovery
shutdown, and sets discovery status `joined: true` only after success. A
`remote_discovery_shutdown_timeout` keeps ownership and is retryable even when
the command/pump loops are done; it takes precedence over a retained unrelated
failure so the need to retry is visible. An actual native teardown/panic failure
stays sticky across later shutdown attempts. Keep `NativeRemotes` in application
state through awaited teardown; last-handle Drop is not a successful drain proof.
Kernel syscalls and final thread/TLS destruction cannot be hard-preempted.

The coordinator should call `remotes.context_changed()` inside the existing
accepted library continuation and await `remotes.shutdown()` during CLOSE. Do not
separately refresh/stop the transferred discovery service. Drain live network
query consumers and the shared `NetworkSnapshots` owner in the existing global
shutdown sequence, with the OS owner last. No main/IPC/manifest change is included
in this preparation.

## Focused acceptance evidence

The direct module tests retain the six existing remote/context tests and add:

- Three actual loopback TCP listeners produce UDP SRV/PTR/TXT/host records equal
  to the hash-pinned original `bonjour.ts`, actual `bonjour-service.Service`
  records and `dns-packet` encoding/decoding at those returned ports. Restart
  preserves frozen TXT/name suffix and obtains new run provenance. `SERVER_DATA`
  cannot alter the bound/discovered ports. All packets target a private loopback
  receiver; no test joins multicast or advertises on the shared network.
- STOP and a library-context change before the one-second browse deadline produce
  no delayed positive announcements over a 2.1-second observation. Accepted
  Socket.IO cleanup still delivers the exact old-context disconnect and drains
  the existing renderer ledger before retirement returns.
- A real occupied UDP bind produces an explicit discovery warning while the real
  remote HTTP fixture remains reachable. The socket is rebound after cleanup.
  Enabled subsets advertise only their actual endpoint, all-disabled starts own
  no discovery socket, and genuine old native tokens cannot stop or publish into
  a replacement run.
- A held native begin times out, its IPC caller is aborted, and STOP still collects
  and retires its eventual run. Terminal shutdown also reclaims a second accepted
  begin before any token is returned.
- On Unix, three genuine private helper children are held behind the native
  owner while a terminal drain is paused. A dropped shutdown waiter and explicit
  timeout leave `joined: false` and those children owned even after TCP cleanup.
  Releasing the owned drain kills/reaps them and later shutdown succeeds. Helpers
  `exec /bin/sleep`, so they create no unmanaged child descendants. This is process
  lifecycle evidence, not native macOS registration.
- A genuine discovery owner task panic stays a terminal error while all remote
  listeners still close; a retry cannot convert it to successful teardown.
- After a timed-out begin and STOP remove the remote binding, a library change
  still submits the reserved discovery context control. Releasing the actual
  held owner retires old work and a fresh current-context START recovers.

The source record helper uses standalone Node **22.21.1**. It imports the existing
hash-pinned `tests/parity/discovery-oracle.mjs` with `noIO: true`, so actual source
record construction opens no socket. Its child has a 15-second deadline and
1 MiB stdout/stderr bounds with kill/reap on failure. The UDP side is the real Rust
publisher. Existing transport/source tests continue to cover client polling,
WebSocket, upgrade and retained access semantics separately; this host suite uses
real Engine.IO/Socket.IO polling packets for its connection cleanup cases.

Linux focused execution passes **19 tests**: six prior remote/context tests, eight
new integration cases and five imported shared-snapshot tests. The shared tests
must not be counted as new remote feature cases. Windows/macOS native execution,
physical LAN reach/firewalls, native dns-sd registration and retained client GUI
flows remain pending coordinator CI/application work. Compilation is not native
OS behavior evidence.

Local validation uses the temporary standalone manifest
`/tmp/freeshow-discovery-host-check/Cargo.toml`, with the existing core, remotes and
discovery crates and previously documented native dependencies. No new application
dependency is needed. The integration's test target imports both owned native
modules directly. No root/Tauri/GUI/release build was run.
Strict Clippy also checks the normal library code path without `cfg(test)`, and
cross-checks Windows GNU/macOS arm64 builds. These are compile checks only.

```sh
FREESHOW_TEST_NODE=/home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --test remotes
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --lib --test remotes -- -D warnings
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --lib --test remotes --target x86_64-pc-windows-gnu -- -D warnings
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --lib --test remotes --target aarch64-apple-darwin -- -D warnings
```
