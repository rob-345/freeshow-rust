# Outbound OSC worker

Scope: the current `EMIT_OSC` handler, independently of the shared IPC/runtime
integration. Source oracle: unchanged Electron revision
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975` (`1.6.7-beta.2`),
`src/electron/utils/api.ts::emitOSC/convertToType`,
`src/electron/utils/oscPatch.ts`, installed `osc-js` 2.4.1, and retained
`src/frontend/components/actions/apiOSC.ts` / `emitters.ts` callers.
No Electron source or frontend behavior was changed.

## Integration API

`freeshow-network` has no dependency on Tauri, AppState, the core runtime, or Node.
The local `[workspace]` and crate lock keep development isolated; the coordinator
can remove that stanza and add this crate to the root workspace.

```rust
use freeshow_network::{OscSender, OscSenderConfig};

// Call while entered into a Tokio runtime with I/O and timer drivers enabled.
let sender = OscSender::start(OscSenderConfig::default())?;

// After coordinator-owned command/context validation:
let outcome = sender.emit(&request_data).await?;

// During coordinator-owned shutdown:
sender.shutdown().await;
```

`emit(&Value)` awaits bounded queue admission and the local OS send. The default
queue holds 64 pending messages plus one active message; the default three-second
deadline includes admission, queueing, DNS and sending. Configuration allows
1..=1024 pending messages and a positive timeout up to 60 seconds.

`try_emit(&Value)` immediately returns either a ticket or an explicit error such
as `queue_full`. Await `OscTicket::wait()` to observe completion. Dropping a ticket
or cancelling an `emit` future cancels an unsent message. Clones share one FIFO
worker and shutdown state. `prepare_emit(&Value)` and `encode_message(&str)` are
pure APIs for inspection. Prepared messages cannot be externally constructed or
mutated to bypass bounds.

Successful outcomes are `Ignored` (non-string data, matching the old handler) or
`Sent { host, port, address, bytes }`. `Sent` means the local UDP stack accepted the
packet; it does not claim remote reception. Errors serialize as `{ kind, message }`.
The original command has no renderer reply/event, only logging. The coordinator
owns result logging, stale-context handling and any shared event/error contract.
Avoid spawning an unbounded task per request around this bounded sender.

## Preserved source quirks

- `data` must be a string; arrays, JSON objects, numbers, booleans, null and missing
  data are ignored. A null outer payload errors. Data is never JSON-parsed.
- `signal.host || "0.0.0.0"` and `signal.port || 8080` use JavaScript truthiness.
  The outdated frontend type's `url` property is ignored; actual emitter inputs
  use `host`. Numeric port zero defaults to 8080, while truthy string `"0"` reaches
  the send boundary as port zero and fails in the pinned Node sender. The Rust
  sender preserves that failure even when bare UDP send_to accepts zero on
  Windows. Numeric port strings,
  including hex/exponents and ECMAScript whitespace, use Node-compatible coercion.
- Split only on literal ASCII space. Repeated/trailing spaces produce empty string
  arguments. Exact lowercase `true`/`false` become booleans. Nonempty numeric
  tokens use JavaScript Number grammar/whitespace and binary64 rounding; NaN-like
  tokens stay strings. Tabs/newlines/Unicode whitespace can become integer zero.
- Address normalization removes one trailing slash, then prepends a slash only
  when the remaining JavaScript UTF-16 length exceeds one. Consequently `x`, `é`
  and `a/` are invalid, while an astral character without a slash is normalized.
  Embedded null/control characters remain accepted by the original encoder even
  though such strings may not decode meaningfully on the receiving side.
- Reachable OSC tags are `i`, `f`, `s`, `T`, `F`. All integral binary64 values use
  int32 wrapping, including large values after JS rounding. `-0` encodes integer
  zero. Nonintegral values use float32; `Infinity` and `-Infinity` get `f`, because
  osc-js checks its float predicate before its infinity predicate.
- Strings use UTF-8 byte counts with zero termination/alignment. Type tags include
  a leading comma; integers/floats use network byte order. Boolean tags carry no
  argument bytes. One worker serializes actual UDP sends in admission order.

## Lifecycle and intentional operational differences

Each active send binds an ephemeral IPv4 UDP socket to `0.0.0.0:0`, matching the
original udp4 sender. The socket drops after the awaited send completes or its
future is cancelled; the old fixed 100 ms close timer is unnecessary once Rust
awaits local send completion. At most one socket exists per sender. There is no
Socket.IO/REST server, sidecar, response listener or delivery acknowledgement.

Errors that Electron logs through osc-js's error listener (or sometimes leaves
silent in the native UDP send without a callback) are explicit Rust outcomes.
Validation occurs before allocating a socket. Work is bounded: commands at most
1 MiB, IPv4 UDP packets at most 65,507 bytes, destination host strings at most
4096 bytes with no embedded null byte. Oversize inputs, saturation, deadlines and
shutdown return explicit errors. These operational bounds are intentional additions
and can reject oversized numeric inputs that would encode to a small packet.

`shutdown()` stops admission, cancels active/queued work and awaits the worker's
socket cleanup. Concurrent/repeated shutdown is safe. Dropping the last sender
signals cancellation; call `shutdown()` when cleanup must finish before continuing.
Drain never awaits a Tokio receiver with an outstanding reservation, and tickets
observe shutdown independently so late reservation sends cannot hang a caller.
Cancellation cannot retract a datagram already accepted by the kernel.

Hostname resolution uses the OS resolver with IPv4 selection. One lazily created,
process-wide resolver thread and one pending slot bound DNS work across sender
creation, timeouts and shutdown. Full DNS capacity returns `resolver_busy`.
**An in-progress OS getaddrinfo cannot be interrupted**: it may outlive cancellation
or sender shutdown, with at most that single thread still running and one queued
request. It holds no UDP socket, never delays sender shutdown, skips cancelled
pending requests and releases results to dropped receivers. The shared resolver
remains available for later senders instead of accumulating detached DNS threads.

## Gaps and boundaries

Patch-only typed OSC branches `h`, `t`, `d`, `b`, `N`, `I`, explicit type descriptors,
arrays, bundles and timetags are not accepted by the current `EMIT_OSC` string
handler and are not exposed by this crate. OSC receiving/API-trigger responses,
Socket.IO, REST, web/remote/stage listeners and network discovery are separate work.
This feature does not establish overall application or network-server parity.
Rust/serde JSON strings cannot represent isolated UTF-16 surrogates; JavaScript
TextEncoder would replace those with U+FFFD. That shared JSON transport limitation
is outside this sender; valid Unicode, including combining/astral characters, is
covered differentially. IPv6 is rejected, matching the original udp4 capability.

## Focused checks

From this worktree, without changing shared manifests or node_modules:

```sh
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=target cargo test --manifest-path crates/freeshow-network/Cargo.toml --offline
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=target cargo clippy --manifest-path crates/freeshow-network/Cargo.toml --all-targets --offline -- -D warnings
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=target cargo build --manifest-path crates/freeshow-network/Cargo.toml --example osc-parity --offline
node --test tests/parity/osc.test.mjs
cargo fmt --manifest-path crates/freeshow-network/Cargo.toml --check
npx prettier --config config/formatting/.prettierrc.yaml --check tests/parity/osc.test.mjs docs/migration/workers/osc.md
```

Rust tests cover FIFO, bounded queue and DNS saturation, async backpressure,
cancellation, deadlines, shutdown with a held reservation, error recovery, actual
UDP loopback delivery, DNS localhost, port-zero failure, unavailable receivers and
sender recreation/cleanup semantics. The Node suite executes actual pinned
TypeScript and actual osc-js with only dgram/timer boundaries mocked, checks 2,805
generated numeric messages plus focused edge/default/validation cases, then
receives real Rust UDP packets and decodes them with actual osc-js. All packets
sent by tests target controlled loopback addresses. The JSON-lines harness itself
rejects non-loopback destinations and is an example executable, not runtime IPC.
