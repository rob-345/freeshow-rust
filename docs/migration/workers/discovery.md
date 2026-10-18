# FreeShow discovery publisher worker

This independent crate ports the publication helper in unchanged
`src/electron/data/bonjour.ts` from Electron oracle
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`. Its normalized source SHA256 is
`6ff78829b6f8a383d7584fdbec69e134892bb3299fbd7189901e41bcd4c5e869`.
It closes a specific transport capability gap. The separate native host wrapper
below is prepared and tested without shared routing/manifests; packaged application
discovery and LAN interoperability are not claimed here.
The existing `freeshow-api` already implements the pinned OSC receiver/action
routing; this crate does not duplicate that listener.

## Host API and authority

```rust
let config = DiscoveryConfig::new(host_random_three_bytes);
let (service, events) = DiscoveryService::new(config)?;
let receipt = service.begin(generation.clone(), frozen_network_snapshot).await?;
service.publish(&generation, DiscoveryName::Remote, actual_bound_port).await?;
service.stop(&generation).await?;
service.shutdown().await?;
```

`DiscoveryGeneration { context: u64, run: String }` uses the trusted listener
owner's epoch and fresh 32-hex run nonce. The host retains the full
`RuntimeContext` beside it. Use a fresh nonce for every begin, serialize lifecycle
submission, and never accept a generation, interface snapshot, executable,
transport destination, or instance ID from client JSON. Publish/stop require the
current exact generation; replacing/stopping it makes old requests fail with
`stale_generation`. Begin replaces an existing run only after its teardown.
A caller dropping an accepted reply does not cancel the owned job.

`DiscoveryConfig::new` requires the process's random three-byte instance ID;
there is no all-zero production default. Retain that ID across library runs.
`DiscoveryName::{Remote, Stage, Controller, OutputStream}` maps to the original
uppercase names. Supporting the OUTPUT_STREAM discovery name does not implement
its HTTP server or frame capture: publish only an actually listening endpoint.

`NetworkSnapshot` contains a hostname and named interfaces with address, family,
internal and MAC fields. Interface names must be unique and supplied as the
native interface mapping. Numeric interface names use JavaScript own-key order.
The first valid begin freezes the TXT IP list for this service's process
lifetime, matching the source module-level `ips`. A replacement run can supply a
new record/membership snapshot while retaining those initial TXT values. Capture
the initial snapshot at host startup when matching source startup timing matters.

On non-macOS, `DiscoveryTransport::Multicast` binds IPv4 UDP 5353 with address
reuse, joins the supplied interfaces, sets multicast IP TTL 255, enables local
multicast loopback and sends to 224.0.0.251:5353. There is no hostname/DNS
resolution or hidden interface enumeration. The explicit `Loopback` transport
requires loopback endpoints and a nonzero peer port, and is used by every live
test. An absent usable membership returns an error rather than advertising
availability.

On macOS, the host must supply an absolute trusted native `dns-sd` executable
through `DiscoveryPlatform::Darwin`; absence is `dns_sd_unavailable`. Do not
fall back to raw mDNS there: the source deliberately avoids hostname conflicts
with mDNSResponder. The crate passes the exact source argv without a shell and
uses ignored stdin/stdout/stderr. `Spawned` means OS process creation succeeded;
it does not prove dns-sd registration. Missing/non-executable/replaced paths and
early exits surface errors. The path is host authority, not an atomic executable
identity or replacement guard. Windows native dns-sd execution is explicitly
unsupported; Windows uses the non-mac publisher.

The original has no frontend discovery ACK. `PublishReceipt::Scheduled` and
`AlreadyPresent` are internal admission results. `Published` means a UDP
announcement was accepted by the local OS, not receipt by another machine.
Suppression and diagnostics are bounded generation-tagged events. The host can
log them like the source and must not manufacture a successful client discovery
or a new renderer authorization flow.

## Source behavior retained

- Instance names are `freeshow-NAME-6hex` and service type is
  `_freeshow._udp.local`, including for the TCP-based remote/stage/controller
  servers. The SRV target is the supplied hostname with no invented `.local`.
- `getLocalIPs` prioritizes en0, eth0, Wi-Fi, then repeats those addresses during
  the full interface traversal, and always appends `localhost`. Internal and
  non-IPv4 addresses are omitted. Both string IPv4 and numeric 4 families count.
  The dependency's A/AAAA records, separately, accept only string family values
  and omit internal and zero-MAC addresses. Unicode names, duplicated addresses,
  numeric own-key order and the no-interface metadata fallback have actual source
  comparisons. Inherited `results` names such as `constructor` produce an
  explicit source-shape error rather than being normalized into success.
- Non-mac publication browses for one second. A positive PTR plus matching SRV
  in the same packet can suppress another name beginning with the exact
  case-sensitive `freeshow-NAME-` prefix. A PTR alone does not suffice. Source
  DNS name comparisons remain ASCII case-insensitive.
- A successful browse begins three ANY probes, 250ms apart after the supplied
  0..250ms jitter. A matching answer/additional name, even TTL zero, conflicts
  after probing starts. This preserves the real dependency's behavior.
- PTR/SRV/TXT/A/AAAA record TTLs are respectively 28800/120/4500/120/120 seconds.
  TXT `ip` and comma-joined `ips`, record order and UTF-8 bytes match actual
  bonjour-service/dns-packet output. The first reannouncement follows three
  seconds later, then intervals grow by three until the source's one-hour cap.
- Query responses preserve registered-record deduplication and type grouping,
  source short-name and ANY matching, PTR SRV/TXT/host additionals, authoritative
  flags and default group destination. No unsupported service enumeration record
  is invented. A published name ignores later port changes until restart.
- Stop emits TTL-zero records for active probes/publications, closes the socket,
  and terminates/reaps native children. Exact source goodbyes, name conflicts,
  shared registry queries, duplicate publication and port retention are tested.

## Bounds, ownership and deliberate differences

Default bounds are 16 admitted jobs (including the active job), 64 events,
4 named publications/direct children, one UDP socket, and 64 query responses per
second. Limits have finite checked configuration ceilings. Network snapshots
have at most 32 interfaces and 16 addresses per interface with bounded fields;
run IDs are validated before cloning/queueing. Aggregate registered records and
outgoing packets are validated before admitting an additional publication.

DNS parsing accepts at most 9000 bytes, 16 questions and 64 aggregate records.
Expanded names are at most 253 content bytes/63-byte labels; compression permits
at most 32 jumps, including cycle detection by the finite jump budget. Record
boundaries, TXT strings, all truncations and mutated offsets are checked. A
single 9001-byte receive buffer detects oversized datagrams. Responses are sent
serially with a 250ms send deadline; no per-packet tasks or response backlog are
created. Queue overflow, malformed packets and dropped diagnostics/responses are
observable, and event saturation never blocks stop.

There is one dedicated current-thread Tokio runtime/OS thread per service, no
blocking worker pool and no application/Node sidecar. Native children use Tokio's
process driver; direct child exits are checked at most every 100ms while native
children exist. There is no idle periodic timer in the non-mac publisher: the
owner sleeps until actual publication deadlines or incoming work. The known
trusted dns-sd process has no output pipes or reader tasks. Unix child process groups receive SIGTERM, then SIGKILL after a 250ms
cooperative grace period, and each direct child is reaped. This is stronger than
the source's fire-and-forget SIGTERM. It is not a sandbox for arbitrary executables
or escaped descendants. Windows native dns-sd is not advertised as available.

Terminal cancellation bypasses the public FIFO, rejects queued jobs and performs
owned teardown. Shutdown awaits runtime destruction and completion, then joins
the finished thread. The final TLS/thread-destructor join interval and OS
spawn/filesystem/wait syscalls are not preemptible Rust deadlines. One active
query can perform at most sixteen bounded sends before observing terminal
cancellation; kernel child reaping cannot be promised to finish under a hard
wall-clock deadline. Dropping the last service requests cancellation; callers
requiring a teardown guarantee must await `shutdown`.

Intentional differences are explicit:

- Source delayed browse/probe work can publish after `unpublishPorts`; an actual
  UDP regression reproduces that bug. Rust cancels pending work on stop/restart.
  Duplicate pending calls coalesce instead of allocating unbounded browsers and
  timers. Fresh run provenance prevents old publication work from reaching a new
  binding.
- Source package socket memberships refresh every five seconds. This service
  uses frozen host snapshots and requires begin/replacement to refresh them.
  Jitter is an explicit frozen host input; polling adds ordinary scheduler
  latency. It does not promise identical cross-runtime scheduling races.
- Malformed DNS compression, oversized names/TXT/packets/snapshots, invalid
  zero bound ports and aggregate records beyond the caps fail explicitly.
  Malformed dependency packet quirks and unlimited traffic are not reproduced.
- Actual native macOS dns-sd registration, OS multicast sharing/permission and
  Windows/macOS LAN behavior were not exercised on this Linux worker. Controlled
  UDP validates wire semantics, not physical-network discovery or firewall reach.

## Prepared native owner

`src-tauri/src/discovery.rs` contains a native-only `NativeDiscovery` wrapper;
`src-tauri/tests/discovery.rs` imports it directly. Neither file adds an IPC
handler, changes remote/API ownership or emits a renderer success callback.
The coordinator owns routing and application setup/teardown integration.

```rust,ignore
// Once during native setup, outside request paths. Keep these inputs if a
// failed construction is retried; never recapture/reseed on library changes.
let frozen = FrozenDiscoveryInputs::capture()?;
let config = NativeDiscovery::production_config();
let (discovery, native_events) = NativeDiscovery::new(library.clone(), config, frozen)?;
// Listener owner has completed actual binding and still owns these listeners.
let run = discovery.begin(&full_context, &bound_servers.generation.run).await?;
for (name, actual_endpoint) in trusted_started_endpoints {
    discovery.started_port(&run, name, actual_endpoint).await?;
}
discovery.stop(&run).await?; // before closing those listeners, even same-context restarts
// Library continuation: only retires obsolete full contexts; never starts one.
discovery.context_changed().await?;
discovery.shutdown().await?;
```

`FrozenDiscoveryInputs` and opaque `DiscoveryRun` have private fields and no
Deserialize. Inputs hold the startup hostname, ordered native interface snapshot
and UUID entropy's first three random bytes. Config's instance ID is replaced by
that frozen suffix. macOS production config selects `/usr/bin/dns-sd` directly;
it does not use renderer paths or PATH lookup. Controlled tests provide a private
helper executable or loopback transport through trusted native construction only.
No production wrapper method accepts a `Request`/JSON command. The coordinator
must call `started_port` from the native listener owner's actual bind receipt,
never from requested config ports or client messages. A `SocketAddr` parameter is
native provenance supplied by that owner, not a kernel listener liveness proof
inside this independent wrapper. OUTPUT_STREAM still needs its real listener
owner before that name may be published.

The wrapper stores the full `RuntimeContext`, the listener owner's run nonce and
a separate fresh discovery nonce. Admission and execution both validate full
current context; every publication validates the exact binding token. Reusing a
listener run string cannot authorize an old token. Stop accepts the exact old
binding after selection so cleanup remains possible. Delayed context hooks leave
an already-current fresh binding alone. Status's `actualPorts` records the first
actual endpoint accepted for each publication name; it is scheduling evidence,
not evidence of peer receipt or dns-sd registration. Existing names retain their
first port until restart, matching the source.

There are 16 admitted FIFO native jobs, one separately owned context cleanup,
four coalesced native cleanup waiters and 64 forwarded native events, in addition
to the independent service's configured bounds. Accepted jobs survive caller
Drop without per-request tasks. Coalescing revisions and completion share a
mutex, preventing a selection arriving at completion from being lost, including
when its caller is dropped. Sustained dropped-caller churn is capped at sixteen
cleanup passes, then returns terminal `discovery_refresh_limit`. Terminal
cancellation bypasses both queues. There are three fixed host Tokio tasks:
owner, event pump and supervisor, plus the service's one runtime/thread. No
blocking-pool snapshot task or per-address task is hidden. Setup snapshot calls
are synchronous and cannot be preempted.

The supervisor joins the owner, always shuts down/joins the service, explicitly
cancels and joins the pump, and preserves the first bounded terminal failure.
Shutdown joins the finished supervisor; retry/context refresh cannot turn an
owner panic or teardown failure into success. Lost supervisor completion triggers
best-effort independent-service shutdown and pump cancellation while retaining a
terminal failure; it does not claim that missing host task joins succeeded. Successful shutdown has no service
socket or direct child. Dropping the final host requests cleanup; orderly CLOSE
must await `shutdown`. Final OS/thread destructors and kernel calls remain outside
hard wall-clock cancellation guarantees.

Native events carry the original full context, listener run and service run.
The pump checks current exact provenance before forwarding and uses `try_send`;
receiver congestion drops bounded diagnostics with counters rather than blocking
teardown or filling a WebView queue. These are native log/status evidence, not
retained client actions. The eventual consumer must not retag already-forwarded
old events or treat them as current availability. Keep logging bounded. A
publication/start/stop failure must be logged explicitly while unrelated remote
listener closure and accepted connection cleanup still complete.

### Native OS snapshot boundaries

Linux/macOS capture uses native `getifaddrs`, preserving returned candidate order,
UP+RUNNING filtering, loopback flags and physical-address matching (including
Linux alias prefixes). IPv4/IPv6 addresses are grouped by first interface name,
then JavaScript integer-own-key order is applied. Windows uses native
`GetAdaptersAddresses(AF_UNSPEC)` with the source libuv skip-anycast,
skip-multicast and skip-DNS-server flags; Up status, FriendlyName, unicast order,
physical-address length and loopback adapter type follow the inspected Node
22.21.1 implementation. Raw hostname follows the native system service approach;
the source's `.local` removal for display names is not applied to Bonjour hosts.
No native collector subprocess, network request or multicast socket is used.

Native output retains the crate's 32 interfaces/16 addresses per interface/name
bounds. Unix list traversal is capped at 1024 entries; `getifaddrs` itself allocates
its OS-owned list before Rust can check it, so that allocation and the syscall
have no application-enforced memory/time cap. Windows supplies an aligned buffer
capped at 256 KiB and makes at most three bounded-buffer calls. Native interface
names/hostname with lone UTF-16 surrogates and unexpected address families fail
explicitly. Calls are not atomic against adapter churn. The test brackets capture
with Node reads, retries at most three changing snapshots, and compares only a
stable pair rather than skipping a mismatch.

The first actual Linux source/native capture matches hostname, ordered IPv4/IPv6
candidates, family, internal flag and MAC, plus actual hash-pinned
`getLocalIPs` output. The Node subprocess records its Node/libuv versions and is
bounded to 15 seconds and 1 MiB per output stream. Native Windows/macOS order,
adapter semantics and hostname behavior remain pending native CI; compilation
is not evidence of those OS behaviors. The collector was informed by primary
[Node OS bindings](https://github.com/nodejs/node/blob/v22.21.1/src/node_os.cc),
[Linux libuv](https://github.com/nodejs/node/blob/v22.21.1/deps/uv/src/unix/linux.c),
[macOS libuv](https://github.com/nodejs/node/blob/v22.21.1/deps/uv/src/unix/bsd-ifaddrs.c)
and [Windows libuv](https://github.com/nodejs/node/blob/v22.21.1/deps/uv/src/win/util.c).
No source body was vendored into this wrapper.

The wrapper freezes the whole snapshot for its lifetime. Source TXT startup IP
freezing is preserved; live source membership refresh and later per-publication
hostname/interface changes remain deliberate unsupported refresh behavior.
Native macOS registration, Windows/macOS LAN multicast/firewall reach, unusual
adapter providers, exotic IPv6 formatting and network changes during capture are
not certified. `Spawned` remains only process-creation evidence.

### Coordinator dependency/wiring additions

The final commit contains only the new module, new tests and this worker doc.
The temporary standalone check manifest is outside the repository and is not an
application integration. Required coordinator additions are:

- Add the independent `freeshow-discovery` workspace member/native dependency,
  removing its temporary workspace/lock as usual.
- Add native Windows `windows-sys = "0.61"` features `Win32_Foundation`,
  `Win32_NetworkManagement_IpHelper`, `Win32_NetworkManagement_Ndis`,
  `Win32_Networking_WinSock`. Existing native Unix `libc`, UUID v4, Tokio and
  tokio-util dependencies suffice; discovery's features supply runtime/net/time.
- Add native test-only `socket2 = "0.6"` for the actual Windows exclusive-bind
  failure fixture. All its sockets are loopback.
- Construct once at setup, retain the bounded native event receiver for logging,
  and pass an opaque run from actual remote listener start through stop/restart.
  Refresh during accepted library continuation; explicitly await terminal CLOSE.
  Do not add a renderer discovery ACK or port/path command.

## Integration order

The native remote owner creates one discovery service, then begins a run and
publishes only after actual listener binding succeeds. Use actual returned ports,
not requested config values. Source `closeServers` unpublishes before closing
listeners: retire the corresponding discovery generation during stop/restart,
without holding the remote publication mutex across teardown awaits. Failed
listener startup must not create a discovery advertisement.

On library change, stop/drain the old discovery run alongside old listeners;
retained SETTINGS can start the new run after DATA_PATH is accepted. Discovery
messages never supply client actions or bypass the retained connection cleanup
ledger. Terminal CLOSE explicitly awaits discovery shutdown as well as the
remote owner/pump. All shared/native wiring remains coordinator-owned.

## Verification

The new `tests/parity/discovery-oracle.mjs` transpiles the hash-pinned actual
TypeScript and uses installed bonjour-service 1.3.0, multicast-dns 7.2.5 and dns-packet 5.6.1.
Only OS interface/hostname/entropy/socket/process boundaries are replaced.
Every outgoing original socket send is routed to a private loopback peer on
random high ports; no test joins or advertises on the shared network. Mac source
process calls are inspected as an OS boundary. Separate genuine helper children
exercise failures, ignored SIGTERM, force kill and reaping, without claiming native
macOS discovery availability.

The source runtime is standalone Node 22.21.1, matching the packaged Electron
Node version; no packaged Electron/macOS multicast execution was performed.
A Rust example is solely a private test adapter. The standalone Cargo workspace
and lockfile are temporary packaging; the coordinator removes/merges them on
workspace integration. No shared runtime, vendor, root manifest, frontend,
canonical status or release binary was edited by this worker.

Local focused result: **9 Rust tests and 12 Node source/live tests passed**;
Clippy `--all-targets -- -D warnings`, Cargo formatting, Prettier and whitespace
checks passed. Checks used `CARGO_BUILD_JOBS=2`, `CARGO_INCREMENTAL=0`,
`CARGO_PROFILE_DEV_DEBUG=0` and the existing worker-local
`crates/freeshow-remotes/target`, avoiding another heavyweight target.

```sh
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test --manifest-path crates/freeshow-discovery/Cargo.toml --offline
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo build --manifest-path crates/freeshow-discovery/Cargo.toml --example discovery-parity --offline
FREESHOW_DISCOVERY_BIN="$PWD/crates/freeshow-remotes/target/debug/examples/discovery-parity" node --test tests/parity/discovery.test.mjs
```

Native wrapper focused preparation: eleven Linux host tests pass, covering actual
UDP records/goodbyes/first-port retention, frozen full-context events, late timer
cancellation, stale queued admission and reused listener IDs, dropped begin and
coalesced retirement callers, overload/non-starvable terminal cleanup, an actual
owner task panic with sticky retry failure and socket rebind, lost-supervisor fallback that remains failed while reclaiming the independent runtime, genuine controlled
child force kill/reap, a removed executable spawn failure/recovery and the actual
Node OS/source comparison. Unix helpers `exec /bin/sleep` so no test descendant
escapes direct-child ownership; they never execute system dns-sd or advertise.
Root native/GUI integration and physical network tests remain coordinator work.

The wrapper checks above used the uncommitted temporary manifest
`/tmp/freeshow-discovery-host-check/Cargo.toml`, depending only on this worktree's
core/discovery libraries plus the native dependencies listed above, with its
`[[test]]` pointing to the new `src-tauri/tests/discovery.rs`. No Tauri/GUI/native
application binary or root assets were compiled. The equivalent integrated check
is `cargo test --manifest-path src-tauri/Cargo.toml --test discovery` after the
coordinator adds those dependencies. Exact local focused commands:

```sh
FREESHOW_TEST_NODE=/home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --test discovery
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --all-targets -- -D warnings
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --all-targets --target x86_64-pc-windows-gnu -- -D warnings
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --all-targets --target aarch64-apple-darwin -- -D warnings
rustfmt --edition 2021 --check src-tauri/src/discovery.rs src-tauri/tests/discovery.rs
npx prettier --config config/formatting/.prettierrc.yaml --check docs/migration/workers/discovery.md
```

Coordinator workspace integration removes the temporary child workspace/lock and adds the native module tests/dependencies to the desktop test target and CI. Integrated focused checks pass nine crate tests, twelve actual-source tests and eleven Linux native host tests. Full local checks pass 296 Rust / 302 pinned-Node / 20 frontend and 100 native host tests. Production listener/setup/retirement wiring and actual Windows/macOS OS snapshot execution still need their integration evidence; the coordinator is not claiming multicast reach or native registration from these loopback/build checks.
