# Native ordinary MIDI host

This preparation adds only `src-tauri/src/midi.rs`, its direct tests and this
note. It wraps the existing `freeshow-midi` and `freeshow-midi-ports` foundations
without changing their algorithms, native backend, shared protocol, application
routes, frontend, manifests or CI. The coordinator integrates those boundaries.
MTC's global router and LTC remain separate; this host routes only five ordinary
MIDI commands. Controlled handles and source fixtures do not certify hardware
open, write, receive, latency or OS device-name fidelity.

## Native API and source replies

```rust,ignore
let midi = NativeMidi::new(
    library.clone(),
    MidiHostConfig::default(),
    || Ok(Box::new(freeshow_midi_ports::NativeBackend)),
    |publication| bounded_main_publisher.try_send(publication).map_err(|e| e.to_string()),
)?;
let replies: Vec<NativeMidiPublication> = midi.dispatch(request, invoking_window).await?;
midi.complete(invoking_window, private_delivery_id, accepted)?;
midi.context_changed().await?;
midi.shutdown().await?;
```

Construction requires an entered Tokio runtime. The native factory creates a
cheap `Backend` value only, without OS/device work; it is never renderer input.
Only `MidiService`'s fixed native thread enumerates, opens, writes and closes.
The publisher is a synchronous, nonblocking bounded sink; rejection is an explicit
error. Do not perform synchronous renderer dispatch, wait for IPC or reenter
host lifecycle methods from the publisher. The direct private `complete` path
only takes its bounded nonce registry and is safe for transport receipts.

The immutable oracle is FreeShow 1.6.7-beta.2 commit
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, specifically
`src/electron/IPC/responsesMain.ts`, `IPC/main.ts` and `utils/midi.ts`.
`receiveMain` replies only when its handler returns a defined result:

| Command            | Native result                                                                                            |
| ------------------ | -------------------------------------------------------------------------------------------------------- |
| `GET_MIDI_INPUTS`  | One `MAIN` / `GET_MIDI_INPUTS` event with `{name}` array, original listener ID and exact request context |
| `GET_MIDI_OUTPUTS` | Same getter contract for outputs                                                                         |
| `SEND_MIDI`        | No source response event; actual native open/write or explicit error/diagnostic                          |
| `RECEIVE_MIDI`     | No source response event; opens/listens through the existing binding rules                               |
| `CLOSE_MIDI`       | No source response event; uses the existing exact ID coercion and close rules                            |

Getters ignore their payload within finite bounds and preserve actual native
order, duplicate names and Unicode. Unavailable/denied backend initialization is
an error, never a fabricated empty list. Send/listen/close native `Done` and
`Diagnostics` are not exposed as successful renderer acknowledgements. Recoverable
close diagnostics remain in bounded host status; fatal ownership/backend failures
reject the internal native operation and remain sticky. Source caught/logged
errors and native explicit errors are deliberately distinguishable.

`CLOSE_MIDI` maps `data.id` through the reviewed foundation, including numeric IDs,
falsy close-all and the source's unknown-nonempty-ID close-all quirk. Null/missing
`data` fails at the source property-access boundary, before touching a backend.
Packet validation, selector fallback, caching, note/control labels, zero velocity,
reserved input behavior and failed-open ordering remain in the tested foundations.
No copy of JZZ's selector/message algorithm is implemented in the host.

## Publication and private delivery completion

`NativeMidiPublication` contains `event: Event`,
`hydration: freeshow_midi::Hydration` and optional
`delivery: NativeMidiDelivery { id, context, run, listener }`. Hydration paths are
relative to `event.message.data`. Source callbacks remain `MAIN` /
`RECEIVE_MIDI2` with the original `WireData` payload and **no listener ID**. The
native listener/run metadata is separate from caller-controlled payload `id`.
Getter replies have no private delivery metadata.

The coordinator serializes an optional top-level `nativeMidi` envelope carrying
hydration and private provenance. It must accept the exact full context/run
before applying hydration or calling retained handlers, then report private
completion in `finally`. Preserve callback data and its absent fields; do not
reuse `nativeExif`, substitute null for undefined or expose this envelope to the
ordinary source handler. The existing MIDI hydration type has undefined paths
and special numbers but no object-key placement metadata. A generic assignment
restoration does not preserve original insertion order for omitted `id` or note/
velocity fields. Exact restoration therefore needs the known source MIDI object
schema order (`id`, `values`, `type`; note/velocity/channel or controller/value/
channel), or a separately reviewed shared hydration refinement. This preparation
preserves the foundation metadata and does not claim generic restoration fixes
own-key placement. General undefined/non-finite renderer input transport remains
outside this JSON-only host slice.

There is at most one private callback delivery in flight. Its fresh
`native-midi-` + 32-hex UUID nonce is 44 bytes and is never chosen by source JSON.
Only the actual main window can call `complete(window, id, accepted)`. Unknown,
late, duplicate and malformed completions fail explicitly. While a receipt is
pending, the owner stops reading its native event stream, preserving the existing
bounded queues instead of filling an unbounded WebView event queue. Ordinary
commands can still run. A completed successful CLOSE fences an already-published
callback receipt; revoked listeners cannot publish later queued callbacks.
This fence covers native publication and synchronous dispatch completion, not
the time the renderer sends its fire-and-forget command or changes local state.
The source also queues callback IPC asynchronously and supplies no CLOSE success
reply. A renderer send-time epoch that discarded such earlier callbacks would
change that timing behavior. Same-context CLOSE retains its pending receipt;
later native RECEIVE installation waits behind CLOSE. Context and terminal
cancellation instead revoke that receipt and rely on full-context rejection of
an already-queued old callback. Neither fence undoes asynchronous retained actions.
The coordinator may serialize private run/listener values as decimal strings
before JSON; source payload IDs and source listener IDs must remain unchanged.

Default receipt budget is five seconds, with native configuration allowing only
shorter positive budgets. Ready receipts and their deadlines precede ordinary
queued commands; command floods cannot starve them. An already-entered native
operation can delay cooperative observation of that deadline until it returns.
Rejection or timeout of a current delivery is a sticky
failure that closes admission and cancels/drains native handles. Context/terminal
cancellation revokes its pending nonce directly, bypassing public command capacity.
A stale-context rejection racing the native context hook is cancellation, not a
fatal error for the replacement library. Receipt completion proves synchronous
retained callback dispatch only; it does not await asynchronous retained actions
or undo a write already inside an OS API. It is never a MIDI command success ACK.

## Ownership, limits and cancellation

One fixed Tokio command/event actor and one fixed join supervisor own the host.
A service is created lazily for a full `RuntimeContext` plus monotonically fresh
native run. Its opaque `freeshow-midi-ports::Owner` is never deserialized. There
is exactly one active native `MidiService` thread per host. Old cancellation,
actual IO/handle close, thread join and event-stream drop precede replacement;
the old receiver stays alive through real join and any timeout retries so a
normal in-flight producer send does not become a false event-overflow failure.
`eventStreamRetained` reports that resource ownership, including terminal drain.
context refresh alone never creates a new service. A late context hook preserves
an already-current run. Library selection deliberately retires old MIDI listeners;
retained code must issue new listen requests for the new library. No old action
is silently rebound to new data.

Sixteen host requests cover active, queued and completed-but-uncollected replies.
Each completed oneshot retains its real permit until collected or dropped. A
caller dropping its IPC future or hitting the default five-second request
budget does not drop the actor-owned native ticket, detach OS work or release
that admission early. Raw backend execution can exceed a deadline after entering
a blocking driver call; cancellation cannot undo an already-entered write.
Queued commands check deadlines before native effects. The source's private
payload is an owned snapshot; send mutations do not become a new renderer reply.

One reserved context job and a separate terminal waiter/control revision keep
cleanup independent of public saturation. Context changes immediately gate old
callbacks and cancel that native owner. An accepted cleanup job survives caller
Drop. A context timeout retains the old service and prevents overlapping
replacement. Terminal shutdown cancels callback gates and pending receipt before
waiting for command drain. The supervisor joins the fixed actor and the real
native service. A timed-out native close leaves its binding/thread owned and
`teardown.complete: false`; an explicit later shutdown revision retries the real
join. Dropping a shutdown waiter cannot discard that ownership. Actual backend,
close, actor or publication failures stay sticky across shutdown/context retries,
while other handles still get their cleanup attempts. A lost supervisor cannot
be reported as success and still attempts the independent native service drain;
missing host task-join proof remains a failure.

Default request/lifecycle budgets are five seconds; validated native config caps
those and native shutdown at 30 seconds. Inputs are at most 64 KiB encoded, 1024
nodes, depth 10 including the command envelope, 256-byte listener/runtime IDs and
32768 encoded OS-path bytes. Payload-specific bounds remain enforced by the
foundations (depth eight, names, selectors and packets). Full output publications
have a 128 KiB counting-writer limit without another serialized allocation.
Diagnostics retain a bounded 2048-byte message plus counters, not an unbounded
history. Non-UTF-8 owner paths are explicitly unsupported. Run values remain
within the existing 53-bit owner bound.

Underlying MIDI limits remain 32 commands, 256 raw callback messages, 256 parsed
events, eight physical inputs, sixteen physical outputs and 32 logical bindings.
An overflow closes/faults the real owner rather than reporting a fake successful
listen or allocating more tasks. The host adds no backend enumeration timer:
only explicit getters/open selection and the foundation's existing one-second
health checks while handles exist enumerate. Driver open/send/close, native
allocation and final thread/TLS destructors cannot be hard-preempted. Last-handle
Drop requests cleanup; orderly CLOSE must retain the host through awaited drain.

## Coordinator integration

Add `mod midi`, native `freeshow-midi` / `freeshow-midi-ports` dependencies with
the default real backend, one process `NativeMidi`, its known-command routing,
private main-only completion path, accepted context hook and terminal drain.
Existing serde/JSON, UUID v4, Tokio sync/runtime/time and tokio-util dependencies
suffice. The source envelope remains unchanged; optional `nativeMidi` belongs to
the shared protocol and bridge, which this preparation does not edit. Root must
bound publisher acceptance and forward at most the one host-issued private
receipt at a time. Do not interpret Tauri emit acceptance as completion. Main
private completion must bypass ordinary library IO/command admission.

## Focused checks and remaining evidence

Fifteen direct native module tests cover controlled real deferred opens/closes,
caller Drop, post-open accepted writes, native reply deadlines, full/stale contexts,
late context hooks, stale ready-reply collection, ignored payload bounds, actual
queue saturation, sixteen completed-uncollected admissions, one held receipt,
CLOSE fencing, receipt priority over an already-queued native write, terminal
receipt cancellation, receiver lifetime through join retries, output escaping limits, actual raw
queue overflow, unavailable backend errors, sticky close failure with other handle
cleanup, actual native worker and host publisher panics, and a supervisor-held
real close through dropped shutdown waiters/timeouts/retry. They reuse the
foundation's existing bounded controlled backend. Factory active-count assertions
prove old backend destruction precedes replacement; controlled handles are not
physical device evidence.

The unchanged two foundation Node suites execute their actual pinned source/JZZ
fixtures: 20 cases passed using standalone Node 22.21.1 and JZZ 1.9.5. They include
selector, byte, callback and close quirks, plus existing MTC regressions and one
read-only native enumeration check. Passing those unchanged MTC tests does not
add MTC routing to this host. No physical MIDI open/send/delivery is claimed; the
existing denied-ALSA probe evidence remains documented in
[midi-ports.md](midi-ports.md). No permissions, devices, installations or system
settings changed. Mandatory actual Windows/macOS native CI, hardware/loopback
routing and retained UI hydration remain coordinator work; foreign compilation
alone is not OS/device evidence.

Local direct tests use the uncommitted temporary manifest
`/tmp/freeshow-discovery-host-check/Cargo.toml`, importing this module directly
with the worktree core/MIDI/ports libraries and default native feature. Linux
uses the existing private ALSA `.pc` descriptor documented by the port foundation;
it points to the installed real runtime library and changes no system install.
The existing worker target is reused with incremental disabled and one build job.
No root native application, GUI, frontend asset or release build was run.

```sh
PKG_CONFIG_PATH=/home/rob-admin/GitHub/freeshow-worktrees/native-menu/target/assets/alsa-runtime-pkgconfig CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --test midi
PKG_CONFIG_PATH=/home/rob-admin/GitHub/freeshow-worktrees/native-menu/target/assets/alsa-runtime-pkgconfig CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --lib --test midi -- -D warnings
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --lib --test midi --target x86_64-pc-windows-gnu -- -D warnings
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --manifest-path /tmp/freeshow-discovery-host-check/Cargo.toml --offline --lib --test midi --target aarch64-apple-darwin -- -D warnings
FREESHOW_MIDI_PARITY="$PWD/crates/freeshow-remotes/target/debug/freeshow-midi-parity" FREESHOW_MIDI_PORTS_PARITY="$PWD/crates/freeshow-remotes/target/debug/freeshow-midi-ports-parity" /home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node --test tests/parity/midi.test.mjs tests/parity/midi-ports.test.mjs
```
