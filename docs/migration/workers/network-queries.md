# Native live network address queries

The coordinator has added production main-window routing, shared-owner setup/status, context retirement and terminal drain. Linux's expanded desktop verifies seven original retained `MAIN.IP` callbacks both normally and during the same real 64 MiB ZIP workload; every request performs fresh native enumeration, preserves correlation/current context and ignores spoofed payload authority. Root review added a real ordered-library barrier test and corrected panic failure publication before reply/consumer-lease destruction, with an actual destructor witness. The combined Linux native target executions now include six shared-worker tests in each importing target. Actual Windows/macOS live-query execution remains pending the new CI run.

This preparation implements the immutable Electron `MAIN.IP` handler from
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975` (1.6.7-beta.2), using the actual
`getLocalIPs` helper in `src/electron/data/bonjour.ts`. It adds no application
routes, shared manifests, frontend flow or discovery ACK. The discovery host
milestone `2bbb1a1f` is preserved; this follow-up extracts its collector into one
shared native owner and updates startup capture to await it.

## Source contract

The original `[Main.IP]: () => getLocalIPs()` ignores its payload and calls
`os.networkInterfaces()` on every invocation. It never reads the hostname or
reuses Bonjour's module-level frozen `ips`. The result is a string array, including
the original priority order `en0`, `eth0`, `Wi-Fi`, followed by all matching
interfaces again and finally `localhost`. Duplicates, native adapter ordering,
JavaScript integer own-key ordering, internal-address filtering and the IPv4
string/numeric family distinction are preserved by the reviewed discovery helper.
Its inherited-name error behavior is retained as an explicit source-shape error.
`localhost` is appended only after successful native enumeration and validation;
an OS failure never becomes a successful localhost-only response.

`NativeNetworkQueries::dispatch` returns exactly one ordinary context-tagged
`MAIN` / `IP` reply with the original listener ID after successful work. It rejects
other commands, non-main callers, missing/obsolete context and bounded-input
violations. Payload values are ignored within those limits; renderer JSON cannot
select interfaces, hostname, paths, executables or ports. Nothing binds a socket,
resolves a name or checks address reachability. These are local address strings,
not claims that a LAN peer can connect.

## Shared native API

```rust,ignore
let snapshots = NetworkSnapshots::start(SnapshotLimits::default())?;
let frozen = FrozenDiscoveryInputs::capture(&snapshots).await?;
let queries = NativeNetworkQueries::new(library.clone(), snapshots.clone());
let (discovery, events) = NativeDiscovery::new(
    library.clone(), NativeDiscovery::production_config(), frozen,
)?;

let replies = queries.dispatch(request, invoking_window).await?;
// Called after the accepted library continuation has published its full context.
queries.context_changed(&library.context()?);
discovery.context_changed().await?;

// CLOSE: stop/drain native consumers, then retain and drain the shared owner last.
queries.shutdown().await?;
discovery.shutdown().await?;
snapshots.shutdown().await?;
```

`network_snapshot.rs` owns the only native collector implementation. One fixed
`std::thread` receives a bounded synchronous queue; async callers use oneshots.
There are no per-request tasks, blocking-pool jobs, subprocesses or extra runtime
threads. `NetworkSnapshots` clones share the same thread. Discovery captures one
full snapshot plus hostname for its existing frozen startup contract; live IP
queries capture only interfaces and skip the hostname API entirely. Stopping or
restarting either consumer cannot close or multiply this shared owner.

The raw native-only methods are `snapshot(SnapshotControl)` and
`addresses_owned(SnapshotControl, guard)`. Both return `Captured<T>`, which keeps
real admission until collection/drop, including a completed oneshot whose waiter
has not resumed. The latter method also keeps the supplied native guard. The query
wrapper moves its full-context request lease into that guard: caller Drop,
deadline or context cancellation can stop waiting but cannot free the lease while
real OS work remains. Context is rechecked after capture, before reply creation
and after bounded serialization. Old results are never retagged as current data.

The wrapper has no background task of its own and admits eight whole requests.
Its shutdown cancels and drains only those requests. A blocked real syscall keeps
the lease; after three seconds shutdown returns
`network_query_shutdown_timeout`, remains closed and can be retried. The process
owner must keep the wrapper/snapshot state after that error rather than replacing
it with another OS worker. Snapshot shutdown similarly closes admission, cancels
queued work, and joins only after the thread reports finished. Its three-second
timeout returns `network_snapshot_shutdown_timeout` with the join handle retained.
An actual worker panic is sticky across capture, consumer shutdown and shared
shutdown retries. Queued work, guards and permits are reclaimed on thread exit.
Last-handle Drop requests cleanup but is not a substitute for awaited shutdown.

## Bounds and OS boundaries

Defaults are eight total snapshot admissions (active, queued and completed but
uncollected), one active OS call, two-second end-to-end request budgets, 128 KiB
encoded snapshot output and two million cooperative operations. Config validation
caps admission at 32, request/shutdown budgets at 30 seconds, output at 512 KiB
and work at four million operations. The collector retains at most 32 interfaces,
16 addresses per interface and 128-byte UTF-8 names; Unix traversal is at most
1024 list records. Windows supplies at most a 256 KiB aligned adapter buffer over
three `GetAdaptersAddresses` calls. Counting writers enforce output bounds without
allocating another serialized buffer; diagnostic messages retain at most 2048
bytes.

Ignored IPC payloads still have finite limits: 64 KiB encoded data, depth 16,
1024 nodes, listener IDs/runtime IDs of 256 bytes and encoded OS paths of 32768
bytes. The complete output event, including its frozen context, has a 128 KiB
limit. Saturation is an explicit error rather than an unbounded wait. These bounds,
context/window restrictions and cooperative cancellation deliberately constrain
the unbounded original handler.

Linux/macOS use `getifaddrs`, retaining UP+RUNNING filtering, native order, loopback
flags, physical address matching and Linux alias prefixes. Windows uses native
`GetAdaptersAddresses(AF_UNSPEC)` with libuv's skip-anycast, skip-multicast and
skip-DNS-server flags, Up status, FriendlyName, unicast order, physical-address
length and loopback type. The unchanged collector's full-snapshot hostname path
uses `gethostname` or `GetHostNameW`; the live address path never calls either.

Budgets are checked around OS calls and bounded traversal/serialization. A kernel
call, allocator or final thread/TLS destructor cannot be preempted. Unix
`getifaddrs` allocates its OS-owned list before application limits can inspect it;
that native allocation has no application-enforced memory cap. Windows buffer
allocation is bounded. Interface collection is not atomic against adapter churn.
Unexpected families, excessive native data and unsupported UTF-16 strings return
explicit errors. Non-Linux/macOS/Windows collectors are unsupported.

## Evidence and integration requirements

The new source helper `src-tauri/tests/support/network_oracle.rs` hash-checks and
extracts both actual TypeScript functions through the TypeScript AST. It executes
the original handler against genuine native Node interfaces, with a hostname
boundary that throws if `MAIN.IP` tries to read it. The hashes are:

- `responsesMain.ts`: `70372b48b698f74f239b82f92e48dde69880a7c4d78af1a9f0e1723313fb57a3`
- `bonjour.ts`: `6ff78829b6f8a383d7584fdbec69e134892bb3299fbd7189901e41bcd4c5e869`

Source/native comparisons bracket every actual Rust capture with real Node
captures and permit three attempts for observed adapter churn. Payload cases are
null, booleans, numbers, strings, arrays and objects containing spoofed interfaces,
hostname, path and port. A native capture counter verifies fresh enumeration on
every live request. The existing discovery comparison verifies full ordered
IPv4/IPv6 candidates, MACs, internal flags, hostname and helper output. Node helper
children have a 15-second deadline, 1 MiB retained stdout/stderr bounds, and are
killed/reaped on timeout/overflow. They open no network listeners or clients.

The focused suites also cover admission held by completed/uncollected results,
queue saturation, caller abort during held OS work, separate operation/output/
deadline limits, retryable terminal drain, consumer isolation, context retirement,
injected interface/hostname errors and a genuine worker panic with queued work.
Five shared worker tests run in each integration target; report them once when
counting unique cases. No real hardware/provider failure is fabricated as success.

Linux execution uses standalone Node **22.21.1**, matching Electron's Node version
but not certifying its ICU/V8 tables. Native Windows/macOS ordering, unusual
adapter providers, Unicode interface names, exotic IPv6 formatting and OS error
behavior remain pending actual native CI. Cross-compilation alone is not evidence
of those behaviors. All generic real-OS source comparisons are enabled on those
platforms; no blanket OS skip is introduced. No LAN availability or multicast
registration claim follows from these tests.

Coordinator changes required outside this owned preparation:

- Declare `mod network_snapshot; mod network_queries;`, keep one snapshot owner
  in process state, and add `NativeNetworkQueries` to native state/routing.
- Add native direct `serde = "1"` for generic bounded counting serialization.
  Existing discovery dependencies supply `freeshow-discovery`, UUID v4, Tokio
  sync/time/macros and tokio-util. Reuse Unix `libc` and Windows `windows-sys 0.61`
  features `Win32_Foundation`, `Win32_NetworkManagement_IpHelper`,
  `Win32_NetworkManagement_Ndis`, `Win32_Networking_WinSock`.
- Await shared startup capture in the existing setup runtime. Never capture OS
  interfaces directly on Tokio, freeze live IP responses or reconstruct the shared
  owner on library refresh.
- Route known `MAIN.IP` to `dispatch` with the real invoking window and full
  request context. Add the context continuation hook and terminal drain in the
  order above. Keep errors explicit and retain ownership after drain timeouts.
- Run both native test targets on actual Linux, Windows and macOS with the pinned
  source Node runtime and installed TypeScript. No GUI or Tauri binary is needed
  for the direct module suites.

Focused Linux execution passed **16 discovery-target tests and 10 query-target
tests (21 unique cases)**. Strict Clippy passed all targets on Linux and during
Windows GNU/macOS arm64 cross-compilation; Rust formatting, Prettier and whitespace
checks passed. No native application, GUI, root assets or release was built.

The local temporary manifest `/tmp/freeshow-discovery-host-check/Cargo.toml`
depends on worktree core/discovery crates and the dependencies above. It is not
committed and does not integrate the application. Focused commands reuse the
existing worker target with one job and incremental compilation disabled:

```sh
FREESHOW_TEST_NODE=/home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --tests
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --all-targets -- -D warnings
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --all-targets --target x86_64-pc-windows-gnu -- -D warnings
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --all-targets --target aarch64-apple-darwin -- -D warnings
```

CI additionally exposed the independent cancellation wakeup path in failure retention. The unwind guard already preceded reply/consumer destruction, but `retain` cancelled waiters before storing the cause. It now stores the first failure before publishing closing/cancellation, so both wakeup paths observe the sticky cause. The unchanged real-panic witness regression passes 64 consecutive local executions; the complete focused discovery/query/remotes set also passes.
