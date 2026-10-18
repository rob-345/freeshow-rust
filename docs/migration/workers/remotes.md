# REMOTE / STAGE / CONTROLLER transport slice

This independent crate ports the three Socket.IO transports in the unchanged
Electron `src/electron/servers.ts` at
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975` (1.6.7-beta.2). It does not establish
complete remotes, network, or application parity. The retained renderer still owns
password checks, permissions, control actions and responses. Actual production
client bundles and native host wiring remain coordinator work.

## Native host contract

`RemoteService::new(RemoteConfig)` returns a clonable service and one
`RemoteEvents` receiver. It does not require an entered Tokio runtime. Each active
run owns one dedicated current-thread Tokio runtime and OS thread, shared by the
three listeners. Public async methods can be called on the host's runtime.

```rust,ignore
let (service, mut events) = RemoteService::new(config)?;
let bound = service.start(&start_payload, library_epoch).await?;
service.renderer_send(&bound.generation, ServerKind::REMOTE, &message).await?;
service.broadcast_backend(&bound.generation, ServerKind::STAGE, &message).await?;
service.send_stage_subscribers(&bound.generation, &frame_from_host).await?;
service.stop().await?;     // restart remains possible
service.shutdown().await?; // terminal; future starts fail
```

The pinned main command mapping is `START -> start`, `STOP -> stop`, and
`SERVER_DATA -> update_server_data`. `update_server_data(&Value)` and
`get_server_data(ServerKind)` update/read server configuration without restarting.
`endpoints()` reports actual bound ports, including an ephemeral port selected by
the source-compatible string `"0"`. `stage_subscribers(&generation)` prunes expired
subscriptions using the configured host clock. `stats()` is resource observation,
not an event-delivery barrier.

Every run has `RemoteGeneration { context: u64, run: UUID }`. The host must retain
its full trusted runtime/library context alongside that generation. Outgoing
commands must use the corresponding generation; stopped/previous generations fail
with `stale_generation`. Never derive provenance from a client message's `id`.
`RemoteEventKind::Renderer` contains the **unchanged** message plus a separate
`source_socket`. A client can omit, spoof, or choose a non-string payload ID, just
as in Electron. Do not inject the real socket ID into that message. Renderer events
go to the retained **main** window, using their original server channel.

The event pump must keep running during stop/restart. CONNECTION and DISCONNECT
have reserved control capacity, and a per-run sequence merges control/data events
in their accepted order. Before retiring an old host binding, drain its accepted
DISCONNECT events through the retained connection-cleanup flow. Filtering all old
events immediately after publishing a new generation would strand renderer
connection IDs. The host must reject stale action/data delivery according to its
frozen context policy while still completing that cleanup. Stop returning means
disconnects have been enqueued; it does not mean the host has consumed them.
Maintain the accepted connection set per generation to identify the disconnect
drain. Sequence numbers order one run, not arbitrary cross-run event consumption.

Lifecycle calls serialize through one owner; at most eight ordinary callers are
admitted. Terminal shutdown has a separate path, cancels immediately and prevents
queued/future starts from reopening listeners. A dropped start future cancels its
run but retains ownership in the service; the next stop/restart awaits that run
before rebinding. Dropping a service cancels it; hosts must explicitly await
shutdown for the completed teardown guarantee. An output command admitted to the
bounded queue remains owned if its caller drops its reply future, but cancellation
can discard pending output. Host IPC lifecycle ownership must likewise survive a
dropped IPC reply. A method's `Result<()>` is a local admission/send result. The
original protocol sends **no success ACK**, and the host must not invent one.

## Preserved source behavior

- Only `disabled.remote/stage/controller === false` enables a listener. Omitted,
  null, numeric zero and string `"false"` stay disabled. Fresh starts use defaults
  5510/5511/5512 unless a truthy configured port overrides them. Numeric zero uses
  the default; truthy string `"0"` requests a real ephemeral TCP port.
- Raw `max` participates in JavaScript comparison coercion. A missing value or
  NaN-producing string/object imposes no source soft limit; null, false, zero,
  negative values, empty string and empty array reject all clients. Fractional
  `1.5` admits two clients. Native hard limits still apply. Over-limit ERROR goes
  to existing clients and the rejected newcomer, which then disconnects without
  renderer CONNECTION/DISCONNECT events.
- The first UA match wins, including Generic Linux before Android and before
  other OS labels. CONNECTION/DISCONNECT payloads retain the source fields.
- Renderer sends target a truthy `id` room, or a union of string rooms for an ID
  array; an empty array produces the original broadcast quirk. Backend sends
  ignore payload ID and broadcast. Stage subscriber sends use real subscribed
  socket IDs, ignoring payload ID. Missing IDs and truthy primitive/array incoming
  payloads pass through unchanged; false, zero and empty string are ignored.
- Stage subscribe/unsubscribe uses the real socket ID. Renewal sets expiry to
  clock + 10,000 ms, equality remains subscribed, and pruning occurs on subscriber
  reads/sends. Renewal retains the original subscription insertion order. Disconnect removes
  the subscription; restart clears subscriptions.
- Backend STREAM uses the shared response gate keyed by JS-converted `data.id`.
  STREAM_DONE is consumed and reopens the gate without renderer forwarding. Gate
  state survives restarts across these three services. The `__proto__` key retains
  the original ignored-assignment behavior and always allows a send. Native
  limits bound distinct keys and key bytes.
- Exact finite raw Socket.IO JSON tokens are converted to JS binary64 before
  losing token spelling: 53-bit rounding, subnormals, negative zero, a known
  one-ULP decimal, escapes and duplicate-key replacement/insertion position are
  covered. Output negative zero follows JSON.stringify's zero wire form. The
  crate uses `serde_json/raw_value`; it does not enable `float_roundtrip` or
  `arbitrary_precision` and does not alter the shared parser's semantics.

## Bounds and completed teardown

Default limits are global across all enabled listeners, except the source soft
max per server: 256 TCP connections, 192 retained Engine.IO sessions, 192 accepted
Socket.IO clients, 256 data events, 64 output commands, 32 buffered output packets
per socket, 1,000,000 bytes per Socket.IO message, 256 stream-response keys and
1,024 bytes per key. Native output checks depth 128 and one million JSON nodes
with a counting writer before cloning. Configuration validates explicit upper
limits; defaults are bounds, not a promise of low worst-case memory under load.
Control capacity is `2 * max_clients + 16`; admission reserves CONNECTION and its
future DISCONNECT together without awaiting a permit. A full data queue emits a
bounded diagnostic and disconnects the sending client. Diagnostics can themselves
be dropped; their count remains observable. Full client/session/command/output
capacity is an explicit error, not a no-op success. A failed multi-client send can
have delivered to other clients before reporting a full output queue.

The Engine.IO lease lives in the engine request extensions/session, not in a TCP
request or joined namespace. Unjoined polling sessions, upgrades, and sessions
using multiple TCP requests consume the same bound. Empty, percent-encoded,
duplicate and invalid reserved query fields are validated/canonicalized before
dependency dispatch. The existing reviewed socketioxide 0.18.7 CONNECT guard is
reused without changes. It bounds pending namespace CONNECT work per engine,
including duplicate raw CONNECT packets before namespace completion.

TCP headers have a 10 s deadline, 16 KiB parser buffer and 100-header cap. POST
Engine.IO bodies have a 10 s default deadline; idle polling GETs retain normal
heartbeat semantics. The default bind uses socket2 to explicitly disable
`IPV6_V6ONLY` for Node/libuv's dual-stack policy, with IPv4 fallback for unavailable
unspecified IPv6. The earlier API slice verified actual wildcard IPv4 and IPv6
connections on this Linux host. This slice's ordinary tests use only loopback;
Windows/macOS execution remains CI evidence to obtain, not a claim from Linux.

Stop closes task admission under the same mutex used to spawn tracked handshakes,
cancels TCP I/O, joins listener dispatch, waits for admitted handshake registration
to finish, disconnects joined clients, reclaims known raw polling sessions, closes
Socket.IO, waits for session leases, joins file workers and destroys the dedicated
runtime. Only then does it notify the caller and join the OS thread. No
`spawn_blocking` or dependency runtime task survives successful stop. The final
thread join still includes a small unmeasured TLS/destructor interval after the
completion notification. Filesystem syscalls and final OS destruction are not
preemptible. Failed cleanup reports `shutdown_incomplete`/worker errors rather
than advertising successful shutdown.

## Explicit static assets and remaining capabilities

`RemoteConfig.static_roots` accepts trusted normalized absolute non-symlink asset
directories per server. No root is guessed, copied or synthesized. Missing roots
return HTTP 503. The coordinator must provide actual retained compiled client
assets for application use. The small HTML/assets in tests identify themselves as
fixtures and are never product assets.

Static serving has two fixed file threads and eight queued requests by default,
32 KiB chunks, two buffered chunks per response, a 16 MiB file limit (configurable
up to 64 MiB), a cooperative 10 s admission-through-stream deadline, and stop
cancellation. Worker count is configurable up to four. File work does not use the
runtime's blocking pool. GET/HEAD, index files, redirects, tested MIME types,
weak-stat ETag/Last-Modified, conditional requests and single/suffix byte ranges
are compared with the actual Express/send middleware. Node's rounded fs.Stats
Date millisecond value is used for ETag parity. Parent directories below the
trusted root are checked, symlinks/dotfiles/traversal are rejected, and Unix opens
use `O_NOFOLLOW|O_NONBLOCK` plus identity checks. This is static path validation,
not protection against an external ancestor rename race; Windows open identity
checks are weaker than Unix inode/device checks. Treat roots as trusted and
immutable while serving.

Intentional differences/gaps:

- Start validates and binds all enabled listeners atomically before reporting
  success. A bind failure closes all candidates and leaves no partial run. The
  original starts independently, logs some bind errors, and restarts without
  awaiting old ownership. Malformed payloads/unsupported port types are explicit
  errors; named-pipe paths and object-form listen options are unsupported.
- OUTPUT_STREAM enablement is rejected (`unsupported_output_stream`). Incoming
  OUTPUT_FRAME is consumed with `unsupported_output_frame`. Native output/frame
  acquisition, capture, Bonjour advertisement and now-playing cover lookup are
  absent; `/nowplayingcover` returns 501. The host can supply real stage frames to
  the subscriber transport, but this crate never fabricates frames or cover data.
- Binary Socket.IO attachments and JSON numbers overflowing finite binary64 are
  explicit diagnostic/disconnect failures. Electron can forward Buffer/Infinity;
  the native JSON renderer contract cannot faithfully represent them. Null client
  messages produce an explicit error instead of the source async-handler failure.
- Static symlinks are intentionally forbidden even though Express follows them.
  Error/redirect response bodies, all Express auxiliary headers, its complete MIME
  registry, legacy permissive Date.parse forms, multipart/coalesced ranges and
  automatic Socket.IO client-script serving are not claimed. Actual client bundles
  need their packaged client dependency; an absent script is not fabricated.
- No renderer action handling, passwords, controller permissions, full remote or
  stage UI exercise, capture data, or application parity is claimed here.

## Focused evidence and integration

The Node oracle transpiles the **entire actual pinned** `servers.ts`, asserts its
normalized SHA256, and injects Electron IPC, clock, Bonjour/capture/cover boundaries.
Real installed Express, HTTP, Socket.IO and socket.io-client perform transport.
Only the config/default unit corpus uses mocked listen; every live case uses
actual controlled loopback sockets. Standalone Node 22.21.1 is the tested source
runtime. Electron 37.10.3 embeds that Node version with different V8/ICU tables;
these tests are not a packaged-Electron UI run. Source dependencies are Express 4.22.3, Socket.IO/socket.io-client 4.8.1,
Engine.IO 6.6.11 and TypeScript 4.9.5 from the unchanged project lock/install; no
Node sidecar is introduced.

Rust checks cover deterministic shutdown after an engine session exists but before
its SID is registered; a dropped startup future followed by same-port restart;
closed event receiver with an outstanding disconnect reservation; control/data
ordering; size/depth validation; and terminal shutdown while all ordinary
lifecycle slots are occupied. Live checks cover polling, WebSocket and upgrade;
message/provenance/UA, no ACK, ID routing, coerced soft max, stage TTL, STREAM gates,
finite numeric tokens and explicit binary/overflow gaps, abandoned sessions,
restart/stale generations, bind failure, real output saturation/recovery, and
static fixture interoperability/slow-reader saturation/stop cleanup.

The raw CONNECT regression sends 2,000 packets in 20 batches **before** namespace
confirmation and samples the dedicated runtime's live-task count after each
batch (peak <= engine baseline + 4). It sends another 2,000 after a normal connect
(post-flood count <= connected baseline + 3), then verifies messages, disconnect
and reconnect. These are observed runtime task boundaries, not a process-wide
allocation proof. Ordinary socketioxide engine/task bounds and teardown tests
cover retained sessions separately. Tests await actual event messages; neither a
stats response nor an HTTP acknowledgement is treated as event-flush evidence.

Local result: **7 Rust tests and 19 Node source/live tests passed**, with Clippy
`--all-targets -- -D warnings`, Rust formatting and Prettier checks passing. No
production host/GUI or Windows/macOS execution was performed by this slice.

Standalone commands (local target, at most two compile jobs):

```sh
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" cargo test --manifest-path crates/freeshow-remotes/Cargo.toml
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" cargo clippy --manifest-path crates/freeshow-remotes/Cargo.toml --all-targets -- -D warnings
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" cargo build --manifest-path crates/freeshow-remotes/Cargo.toml --example remotes-parity
FREESHOW_REMOTES_BIN="$PWD/crates/freeshow-remotes/target/debug/examples/remotes-parity" node --test tests/parity/remotes.test.mjs
cargo fmt --manifest-path crates/freeshow-remotes/Cargo.toml --check
rustfmt --edition 2021 --check crates/freeshow-remotes/src/transport.rs
npx prettier --config config/formatting/.prettierrc.yaml --check tests/parity/remotes.test.mjs tests/parity/remotes-oracle.mjs docs/migration/workers/remotes.md
```

For coordinator integration, remove this crate's temporary standalone `[workspace]`
and lockfile, add it to the shared workspace/native dependencies, and repoint
socketioxide from `../freeshow-api/vendor/socketioxide` to the coordinator's already
reviewed `../../vendor/socketioxide`. Do not modify the vendor or enable new shared
serde number features. The example is a loopback JSON-lines driver, independent of
Tauri and AppState; the host owns main-only admission, frozen contexts, event
publication ordering, old-run disconnect cleanup, and real asset roots.
