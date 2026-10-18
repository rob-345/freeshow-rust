# Inbound API worker

Scope: `WEBSOCKET_START`, `WEBSOCKET_STOP`, `API_TRIGGER`, inbound REST,
Socket.IO and OSC from unchanged `src/electron/utils/api.ts` at
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`. This is one network service slice;
it does not establish full application, remote, stage or controller parity.
The existing outbound `EMIT_OSC` crate is separate and unchanged by this slice.

## Host interface

`ApiService::new(ApiConfig)` returns the service and a bounded receiver of
`ApiEvent`. `start(&serde_json::Value, context: u64).await` stops and joins the
previous run, binds TCP `port`, TCP `port + 1` and UDP `port`, then returns
`BoundEndpoints`. Default base port is 5505; numeric zero takes the original
truthiness default. Defaults use Node's dual-stack wildcard TCP binding and
IPv4 wildcard UDP binding. Ordinary tests explicitly use loopback; the single
authorized production-binding exception is described below.

`stop().await` / `shutdown().await` close admission, cancel pending replies and
clients, await tracked tasks and destroy the isolated network runtime before
returning success. All endpoint binds succeed before start reports success.
Failure rolls back the candidate. A failed new configuration leaves the old
service stopped, matching the original stop-before-start order.

Each start has `ApiGeneration { context, run }`, with a new UUID even when the
application context is unchanged. Every event includes that generation:

| Event                                 | Host routing                                                       |
| ------------------------------------- | ------------------------------------------------------------------ |
| `Connected`                           | Existing `ToMain.WEBSOCKET`, data `"connected"`                    |
| `Trigger { data }`                    | Existing `ToMain.API_TRIGGER2` renderer event                      |
| `DynamicValues { listener_id, keys }` | Existing `MAIN.GET_DYNAMIC_VALUES` request, preserving listener ID |
| `Diagnostic { code, message }`        | Host diagnostics/error reporting                                   |

Correlated actions replace caller `returnId` with a private
`api:<run UUID>:<request UUID>` before the renderer event. `get_` actions always
correlate. Other truthy caller IDs also correlate. The retained frontend echoes
that private ID in `MAIN.API_TRIGGER`; the host calls
`api_trigger(&generation, &payload)`. The service removes `returnId` and returns
**the entire remaining envelope** to the network caller. It never resolves an
old renderer reply against a reused caller ID in a new run.

Dynamic IDs are `GET_DYNAMIC_VALUES:<run UUID>:<request UUID>`; route unchanged
renderer replies to `dynamic_values_reply(&generation, listener_id, value)`.
Host routing must retain event provenance, reject events from an obsolete
context and intercept these backend-initiated MAIN replies before ordinary
library dispatch. `cancel_pending()` cancels current host RPCs during a host
context change. The coordinator owns this shared IPC/context integration.
Unknown, duplicate, wrong-run and stopped-run completions return explicit
`unknown_reply`, `duplicate_reply` or `stale_generation` errors.

Action response timeout is 10 seconds; dynamic timeout is 15 seconds. An action
timeout produces no Socket.IO reply / REST 204. Dynamic timeout or a false host
value produces `{ isVariable: true, values: {} }`. A renderer disconnect,
shutdown or explicit cancellation removes its pending registrations.

## Behavior retained

- Socket.IO uses real Engine.IO v4 polling, WebSocket and polling-to-WebSocket
  upgrade, with the root namespace and `data`/`error` events. A namespace connect
  reports `connected` before action authentication. REST works on both TCP ports.
- Password trimming, source precedence, object field order, object/array/string
  coercion and bearer quirks are ported. An incorrect truthy object token blocks
  later sources; object `authorization` is not bearer-decoded. Empty password
  accepts requests without a token. Authentication is per action, as upstream.
- REST JSON parsing precedes auth. Body action wins; query `data` JSON spreads
  after query action and can override it. Invalid query JSON falls back to the
  query action. Arrays and BMP strings retain JavaScript spread behavior.
  Query parsing matches installed Express `qs` extended defaults, nesting,
  duplicates, sparse compaction, overflow arrays, malformed escapes and encoded
  bracket parsing. Missing/fire-and-forget actions return 204; replies return
  200; unauthorized returns the original 401 JSON.
- JSON UTF-8, UTF-16LE/BE, UTF-16 BOM/heuristic detection, UTF-7 and UTF-7-IMAP,
  gzip and deflate are supported. Original unsupported UTF-32 remains 415.
  The default inflated JSON body limit is 102400 bytes. REST has no CORS;
  Socket.IO CORS uses the actual original defaults, including preflight methods.
- Accepted finite JSON numbers use an API-local bounded `RawValue` traversal
  and Rust binary64 conversion. This preserves fractional/exponent/subnormal
  rounding, 53-bit integer rounding, negative zero, duplicate keys and original
  object key insertion positions. No `arbitrary_precision` or `float_roundtrip`
  feature is enabled. `raw_value` alone does not change other crates' parsing.
- OSC decodes supported `i/f/h/t/d/s/b/T/F/N/I` tags and nested bundles, Unicode
  strings, byte blobs and source timestamp rounding/parent-child ordering.
  The original `/freeshow/*` matching and `/freeshow` removal are preserved,
  including suffix matches and the leading slash preventing automatic `get_`
  correlation unless JSON overrides action. First-argument JSON and auth across
  all arguments follow the original precedence.
- OSC responses still target **127.0.0.1:41235**, never the sender. Unauthorized
  responses encode the original JSON string. Successful API reply envelopes
  are objects which actual osc-js cannot encode: the service emits
  `osc_response_encoding`, instead of inventing JSON replies.

## Resource ownership and lifecycle

Default bounds: 32 admitted lifecycle calls, 256 host events, 128 TCP
connections, 64 Engine.IO sessions, 128 active requests, 8 active requests per
Socket.IO client, 128 pending host replies, 1000000-byte Socket.IO frame/message
and aggregate binary attachments, 32 binary attachments/output packets per
session, and 128 JSON levels. OSC limits bundles to 16 levels and 256 messages
per datagram. Configuration validates finite upper bounds and positive timeout
limits. Admission happens before data-task spawning; synchronous flood errors
use bounded socket queues. Queue/session/request saturation fails explicitly.
Diagnostic queue saturation increments `ApiStats.dropped_diagnostics`.

An Engine.IO session lease lives in its original request parts, independent of
namespace connection and the number of polling TCP requests. Initial handshakes
are tracked through SID registration even if HTTP cancellation occurs after
session creation. Reserved query keys are decoded, validated and canonicalized
before dependency dispatch. An empty `sid` remains an initial **leased**
handshake; invalid and duplicate IDs cannot bypass admission.

Each run owns one dedicated standard thread/current-thread Tokio runtime.
Serialized restart permits at most one live run. Stop joins TCP/UDP listeners,
waits admitted handshakes and request tasks, disconnects namespaces, closes raw
polling sessions, then destroys the runtime. This also cancels dependency-owned
heartbeat/connect-timeout/upgrade tasks which SocketIo's public namespace close
does not fully expose. Stop checks that all Engine.IO/TCP leases were reclaimed.
The completion notification occurs **after runtime destruction**; the final
thread join covers only thread return and native TLS destructors. There are no
DNS or blocking-pool workers in this crate. Cancelling a start/stop future keeps
its Active owner retained for the next stop/restart to finish joining.

The task admission mutex covers both close and `TaskTracker::spawn`, avoiding
late spawns after tracker wait. Reply bounds use a counting writer before
cloning/encoding a host envelope; an oversized reply fails both completion and
the awaiting network request, releases its slot and permits recovery.

### Explicit dual-stack configuration

TCP socket construction now uses socket2 to clear **IPV6_V6ONLY = 0** before
binding, rather than inheriting the host OS default. It enables SO_REUSEADDR on
Unix and leaves it disabled on Windows, matching pinned libuv. Default wildcard
binding tries IPv6 first, falls back to IPv4 for unavailable IPv6/setup failures,
and uses Node's backlog request of 511. Bind/listen failure still rolls back the
entire candidate group. This follows pinned Node's
[listen setup](https://raw.githubusercontent.com/nodejs/node/v22.21.1/lib/net.js),
[Unix/macOS bind](https://raw.githubusercontent.com/nodejs/node/v22.21.1/deps/uv/src/unix/tcp.c)
and [Windows bind](https://raw.githubusercontent.com/nodejs/node/v22.21.1/deps/uv/src/win/tcp.c).

The unit regression reads the configured IPV6_V6ONLY option before bind and
checks IPv6 loopback connectivity and per-OS reuse configuration. Linux can force
V6ONLY true when binding specifically to ::1, so that loopback-bound value does
not represent wildcard behavior. The separately authorized real wildcard
comparison ran on Linux: original Node 22.21.1 and Rust bound [::] and accepted
both 127.0.0.1 and ::1 requests on both TCP ports, with matching authenticated
204 and unauthorized 401 results. Its output records actual dynamic ports and
client families without logging the password. Physical Windows/macOS results
remain CI/platform integration evidence; their source configuration is verified,
not described as physically tested by this worker.

## Vendored dependency

`vendor/socketioxide` is the required source/package subset of registry
socketioxide **0.18.7**, checksum
`6d23ebfa62e73bcc05c40301f8b4cb8400489400c5e794e789300a04fd28a3b8`,
upstream commit `708ea9fc5ee549229d7236924e4975ec5977a59c`.
Its original MIT license and exact source URL are retained alongside
`FREESHOW-NOTICE.md` and `freeshow-connect-guard.patch`.

The sole behavior change is a per-engine-session/known-namespace CONNECT guard.
Upstream starts a task for each raw CONNECT before public middleware can gate
it. FreeShow registers only `/`; duplicate pending/already-connected packets now
reuse that admission. The guard releases on completion/cancellation. Parser
and namespace routing are unchanged. Packaging-only Cargo.toml pruning and a
vendor-local workspace are captured separately in
`freeshow-package-manifest.patch`. No shared root patch is required.

**Required integration layout:** coordinator moves this scoped vendor directory
from `crates/freeshow-api/vendor/socketioxide` to root `vendor/socketioxide`,
changes the API dependency path to `../../vendor/socketioxide`, removes only the
API package's temporary standalone `[workspace]`, and adds
`vendor/socketioxide` to root workspace exclusions. Keep the vendor-local
`[workspace]`. A temporary equivalent virtual root verified both correct
`workspace_members` (API only) and unchanged `cargo fmt --all -- --check`.
Cargo treats a vendor nested beneath an explicitly listed member as included
despite the nested exclude; moving it outside the member subtree avoids that.
The standalone worker layout remains scoped under its owned API prefix.

The raw-packet regression posts four batches of 2000 CONNECT packets against one
real polling session. It checks exactly one session/connected event and compares
Tokio `num_alive_tasks` after each completed HTTP batch with its first stable
batch (allowing one transient cleanup task). It then closes/reconnects normally.
These measurements establish retained task/session bounds at request boundaries;
they are not an allocator/RSS benchmark or a claim about all network behavior.

## Checks and limits of evidence

Focused checks passed: **9 Rust unit tests, 23 Node source/live tests**,
clippy with warnings denied, Rust formatting and Prettier. Use
`CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=target`:

```sh
cargo test --manifest-path crates/freeshow-api/Cargo.toml --offline
cargo clippy --manifest-path crates/freeshow-api/Cargo.toml --all-targets --offline -- -D warnings
cargo fmt --manifest-path crates/freeshow-api/Cargo.toml -- --check
cargo build --manifest-path crates/freeshow-api/Cargo.toml --example api-parity --offline
node --test tests/parity/api.test.mjs
```

The test harness executes the **complete original api.ts**, injecting only
loopback OS binding, renderer IPC, logs and shorter timers. Original Express
4.22.3, Socket.IO 4.8.1 and osc-js 2.4.1 are actual installed dependencies. Source
hashes are fixed to the pinned commit and work in shallow checkouts. Ordinary fixtures bind/send exclusively on controlled loopback. One explicitly
authorized production-binding test temporarily opens wildcard listeners on high
dynamic ports with an unpredictable password, uses only local IPv4/IPv6 clients,
and guarantees teardown. Network namespace isolation was attempted but unavailable
(`unshare -Urn` was denied), so this exception is explicit and narrowly bounded.

Source tests ran on standalone Node **22.21.1**. This is Electron 37.10.3's Node
version, but standalone V8/ICU/Unicode tables differ from packaged Electron.
This corpus checks numeric, explicit ECMAScript trimming and auth cases; it does
not assert packaged Electron equivalence for every Unicode or V8 edge case.

The Node corpus covers auth precedence/Unicode, finite binary64 boundaries,
248 generated numbers, 160 generated conflicting query structures, real REST
and Socket.IO clients, raw packets, variable/action timeouts, private correlation
across restart, aborted clients, abandoned handshakes, limits/failure recovery,
CORS/path behavior, OSC variants/default reply/encoding failure and scheduled
OSC cancellation. Rust unit tests deterministically pause after engine session
creation but before SID registration during stop; cancel a waiting stop and
restart; exhaust/cancel lifecycle admission; and exercise host queue/reply
bounds, stale completion and task admission.

Deliberate deviations / remaining gaps:

- Nonfinite JSON (`1e309`) returns `json_number_overflow` instead of original JS
  Infinity. Native JSON IPC cannot faithfully carry Infinity. Raw Socket.IO
  overflow emits a diagnostic and performs no host action. Arbitrary-precision
  shared parser changes are intentionally avoided.
- Native strings cannot carry lone UTF-16 surrogates. Such JSON is rejected;
  astral query-string spread explicitly returns `json_surrogate_spread` because
  JS creates lone code units. Ordinary non-BMP strings are supported intact.
  Malformed UTF-16 text uses replacement characters; full typed-array/Buffer
  fidelity through the shared JSON renderer bridge is not established by this
  slice. Binary attachment resources are bounded and tested.
- Invalid action types, malformed inputs, oversized messages/replies, closed
  host channels and saturated resources return explicit errors/diagnostics in
  place of upstream throws, unhandled async rejections or indefinite work.
  Start validates typed integer ports/string passwords rather than accepting
  out-of-contract JavaScript coercions. Bind failures return errors rather than
  advertising a partially started group of servers.
- Error pages are sanitized JSON rather than Express HTML/stack traces; server
  metadata, byte-for-byte JSON formatting and every Express response header are
  not claimed. Only default Engine.IO v4/root namespace is implemented, matching
  current options; custom clients' legacy EIO3, JSONP, WebTransport and served
  Socket.IO JavaScript assets are outside this service contract.
- OSC validates bundle element boundaries/depth and uses Rust regex. Malformed
  osc-js packet size quirks and JS-only regex features are not reproduced. No
  network-wide parity claim follows from this listener or the outbound handler.

Shared runtime/IPC/frontend wiring, root/native manifests/locks and canonical
migration status remain coordinator-owned integration work.

## Native host integration

`src-tauri/src/api.rs` owns one bounded FIFO lifecycle task and one generation-filtered renderer event pump. Thirty-two leases cover complete public requests; one private control slot and a guard carried by its queued job preserve context/shutdown access under saturation or caller cancellation. Accepted lifecycle effects finish even if their IPC reply future disappears. Capacity is released before waking a completed caller. A publication mutex keeps connections arriving immediately after bind from losing their Connected event before the host binding is available.

Full frozen RuntimeContext accompanies ApiGeneration, rather than retagging old replies with current context. Commands/replies originate only from main. Backend dynamic IDs and private action IDs must match the current run before completion. Existing live listeners automatically stop/restart on a committed library change with the same options and a fresh run, cancelling pending old requests. An unchanged context preserves connections. Restart disconnects clients, an observable difference from the original surviving listener; rebind failure is reported without suppressing an already committed library reply. Later asynchronous Svelte action side effects and native bridge binary/rare numeric fidelity remain outside this guarantee.

Normal closing fences public admission, drains accepted lifecycle jobs, stops the owned network runtime and awaits both owner and event-pump completion. Host payload caps are 16 KiB for startup options, 1 MiB for other API messages and 256 bytes for correlation IDs. There is no renderer bind-address option; trusted host FREESHOW_RUST_API_BIND_IP can select loopback for isolated native tests, with original wildcard defaults otherwise. The API-local RawValue number parser does not globally alter serde_json features beyond raw_value.

The vendor now resides at root vendor/socketioxide and is excluded from the root workspace; its local workspace is retained. Core Node source/live tests pass 23/23 after fixing a probe race that selected a prior-run DynamicValues event before the current stdout event arrived. The probe now waits for the exact current run and asserts full provenance. Five native host tests and the full 102-step real editor workflow now pass. The editor exercises external REST on both ports and genuine Socket.IO polling/WebSocket actions/dynamic replies before and after selection and during compression, then verifies TCP/UDP closure after actual Quit. The combined local suite passes 256 tests plus 23 native host tests, menus and drops at both scales. Remote integration CI remains pending.
