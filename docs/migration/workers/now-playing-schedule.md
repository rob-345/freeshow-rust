# Bounded native now-playing phase schedule

This is pure scheduling state for a future native host. It owns no preparation,
filesystem job, image, socket, publication, task, thread, timer or runtime. It adds
no source success ACK, IPC route, renderer authority or GUI/performance claim.

## Native API

`Schedule::new(frozen_full_runtime_context)` creates a fresh opaque native run.
Its public operations are:

- `admit_set()` reserves a pending-preparation record.
- `admit_unset()` / `admit_open()` reserve a record and queue the synchronous phase;
  neither cancels pending SET preparations.
- `ready(&id, has_metadata)` reports a genuinely settled preparation. `true` queues
  `PublishMetadata`; `false` means the original source's normal parser-null result
  and queues `PostMetadata`, preserving fallback artist/title/text work.
- `preparation_failed(&id)` reports an actual settled unsupported, authority,
  resource, cancellation or native failure; it does not invent source parse-null
  behavior or filesystem effects.
- `take_next()` returns a sealed `PhaseToken`, or None while any current phase is
  held. Its borrowed `operation()`, `context()` and `kind()` identify trusted work.
- `complete(&actual_token)` queues PublishMetadata → PostMetadata → Text, then
  finishes. UNSET and OPEN finish after their one phase.
- `fail_current(&actual_token)` stops that operation's future phases after actual
  failed effects/cleanup settle. Independent records continue.
- `retire()` synchronously seals admission and removes unstarted queued phases.
  Current tokens remain active; pending preparations remain pending until their
  actual result arrives. Reporting ready after retirement settles that preparation
  without scheduling effects. Retirement is repeatable and does not fake drain.
- `release(&id)` removes only a finished record. This is the trusted host's explicit
  assertion that real image cleanup, publications and all other retained work
  finished. The scheduler cannot verify resources that it never owned.

`OperationId` is Clone + Eq + Hash for a **bounded host map**. Cloning an ID clones
identity only, never a resource lease. IDs retain a private per-schedule Arc seal
and monotonically increasing non-reused native sequence. `PhaseToken` has a new
private per-dispatch seal. No ID/token has Deserialize or a public constructor.
Equal full context values in a new schedule do not validate old IDs/tokens.
Unknown/released IDs, duplicate readiness, replayed phase tokens, foreign runs,
early release and retired admission return explicit errors without advancing work.
Counter exhaustion fences admission instead of reusing an ID.

Runtime ID, generation and data-path spelling are frozen and compared through that
native run identity. Context input bounds are runtime ID ≤256 UTF-8 bytes, absolute
Unicode data path ≤32768 bytes, no NUL, and a JavaScript-safe generation. This pure
schedule does **not** query LibraryService; the actual host must validate current
full context at admission/effect/publication and retire an obsolete run.

## Bounds and actual ownership obligations

There are exactly **four whole-operation records**, counting pending prep, queued,
active and finished-unreleased results. Each record has at most one queued/current
phase, so queued + active ≤4 (and the FIFO alone ≤4). There is only one active phase.
No unbounded diagnostic, payload, continuation or deferred task list is stored.
Full capacity is explicit and becomes reusable only after a valid release.

The host must retain actual futures/tokens/resources independently of dropped
callers and observer timeouts. It may call complete/fail_current only after actual
phase effects settle, and ready/preparation_failed only after actual prep settles.
PublishMetadata completes after actual source publication enqueue, independently
of a private renderer delivery receipt. That receipt can retain the whole slot
until release, but must not invent a source ACK or stall the source continuation.
It must keep the current token through accepted native work and retain finished
records until actual image/publication cleanup. Token Drop and an empty phase
queue are **not** drain receipts. `Status::drained()` means zero scheduler records
only; its relationship to actual native drain depends on honest host releases.
Terminal control and retryable actual owner joins belong to the host.

One active phase preserves source synchronous atomicity among these commands even
when native effects await workers. The host also needs its genuine ordered library
phase capability to preserve ordering against other library writes; this queue
alone does not create that filesystem fence.

## Actual source comparison

The source-only fixture executes the entire immutable
`src/electron/audio/nowPlaying.ts` from
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`. Its SHA256 is
`0a203138d9c283b3e5fa37f0aa46cd33df596d8ee7183ac6cd73d3c59cf3bc7c`.
It uses TypeScript transpilation and `new Function(require, exports, console)` in
the **same Node realm** as the deferred real Promises. No cross-realm Promise
assimilation or rewritten scheduling algorithm is substituted for the source.
Only parser/filesystem/image/IPC/Icecast module boundaries are controlled and
traced. These are source-only traces, not real disk/image/network success evidence.

The real source awaits metadata, publishes it before its outer continuation, runs
post-metadata work synchronously, then yields at its async dynamic-value function
before text output. Selected observed traces (`M` metadata, `P` post, `T` text):

| Controlled source turns                               | Observed phases               |
| ----------------------------------------------------- | ----------------------------- |
| Resolve B then A before yielding                      | BM, AM, BP, AP, BT, AT        |
| Resolve A then B before yielding                      | AM, BM, AP, BP, AT, BT        |
| Resolve B; await one setImmediate; resolve A          | BM, BP, BT, AM, AP, AT        |
| Resolve B/A; call UNSET synchronously before yielding | UNSET, BM, AM, BP, AP, BT, AT |
| Call UNSET then OPEN while A parser is pending        | UNSET, OPEN, AM, AP, AT       |
| Parser rejection caught by actual helper              | AP, AT; no metadata event     |
| Missing common from actual helper                     | AP, AT; no metadata event     |
| Constructor failure after metadata/title              | AM, AP; rejected SET, no text |

For the same-turn cases the host observes all currently ready prep results before
taking the next phase. In the synchronous-UNSET case it observes incoming source
jobs before prep outcomes because the source invokes UNSET in the current call
stack before Promise continuations run. A separate turn may finish all of B before
A becomes ready. These rules reproduce the eight controlled cases; they do **not**
establish universal Node/Tokio turn equivalence, ordering for every mixed real IO
race, or hardware behavior. The host must document its actual batching boundary.
The fixture also asserts actual SET settles to undefined, UNSET/OPEN return undefined,
and a source image failure occurs after metadata/title but before writes/text.

## Focused verification and integration

**10 native tests passed** on Linux with pinned standalone Node22.21.1. They include
the eight-case actual-source differential, both FIFO ready orders, four complete
slots through finished-unreleased, synchronous current-phase atomicity, duplicate
readiness/replay/foreign/context seals, retirement with active and pending work,
dropped-token false-drain prevention and 64 rounds of bounded capacity reuse.
Strict Clippy (`--all-targets -- -D warnings`) passed for production and test
configurations; rustfmt, Prettier and staged whitespace checks passed.
No app, GUI, performance comparison or broad build was run.

Private headless validation uses the existing cached target, jobs1/debug0/incremental0:

```sh
cargo test --offline --manifest-path /tmp/freeshow-osc-now-playing-schedule-check/Cargo.toml --test now_playing_schedule
cargo clippy --offline --manifest-path /tmp/freeshow-osc-now-playing-schedule-check/Cargo.toml --all-targets -- -D warnings
node tests/parity/fixtures/now-playing-overlap.mjs
```

Native CI runs the same Node source fixture on each actual OS; no OS-specific cases
are skipped. The native test resolves the checkout from the src-tauri manifest's
parent. Only a temporary standalone manifest needs
`FREESHOW_NOW_PLAYING_ORACLE_ROOT`. The module's only dependency is the already
declared freeshow-core RuntimeContext type. Coordinator integration needs the
module/test registration, bounded fixed host owner/map, genuine prepare/effect
futures, original metadata publication, library phase fencing and terminal cleanup.
No shared manifest, main, bridge or existing module was changed in this slice.
