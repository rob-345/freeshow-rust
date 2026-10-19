# Native ordered library phases

This implements the narrow queue seam identified by [the now-playing host plan](now-playing-host-plan.md). It does not implement NOW_PLAYING, its scheduler, metadata/image preparation, filesystem helpers, renderer delivery or a new IPC command. The immutable source is `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`; the source phase/overlap investigation is recorded in plan commit `25d1c32b`.

## Native API and ordering

`LibraryService::begin_phase(request, window).await -> Result<LibraryPhase>` obtains a whole phase lease immediately, then uses existing FIFO admission to enqueue a phase marker behind earlier ordered work. Actual begin validates the captured full `RuntimeContext` under the existing filesystem barrier. The sealed capability has private fields, no constructor, no Clone, no Serialize and no Deserialize. The returned `context()` is immutable.

`LibraryPhase::submit(request, closure) -> Result<PhaseTicket>` admits immediately or returns an explicit error. It does not reacquire normal admission. Its private channel is consumed by the **same existing library IO thread**, inside the phase marker. Each subjob validates both the captured phase context at submission and the actual Runtime context on the IO owner, including SAVE's embedded snapshot authority and recovery behavior. It executes its synchronous trusted closure under the existing exclusive filesystem barrier, refreshes context/show-reader/media-settings snapshots even after an ordinary operation error, advances the ordinary mutation dependency fence, and tags returned events with the resulting context.

`PhaseTicket::finish().await -> Result<Vec<Event>>` collects that accepted job's actual result. Its lease includes queued, running and completed-but-uncollected ownership. Dropping a ticket, or its waiting future, does not cancel accepted IO.

`LibraryPhase::close()` synchronously seals new submissions without consuming a work/result/observer slot. `finish().await` also closes, then waits for real accepted subjob completion and normal admission release. A timeout/drop releases only that observation. Retry retains the same owner, work and completion channel. Successful completion is idempotent. Dropping the capability closes it; accepted subjobs still run.

Example native use, with already bounded native preparation and typed effects supplied by the host:

```rust
use freeshow_core::{
    protocol::{Event, Request}, runtime::Runtime, service::LibraryService, Result,
};

async fn apply_source_phase(
    library: &LibraryService,
    begin_request: Request,
    effect_request: Request,
    operation: impl FnOnce(&mut Runtime) -> Result<Vec<Event>> + Send + 'static,
) -> Result<Vec<Event>> {
    let phase = library.begin_phase(begin_request, "main").await?;
    let accepted = phase.submit(effect_request, operation)?;
    let result = accepted.finish().await;
    phase.finish().await?; // Wait even after an ordinary accepted effect error.
    result
}
```

Multiple actual effects may be submitted/collected within one phase, including a bounded asynchronous dependency between submissions. The IO owner waits on the private channel while idle; it holds **no filesystem barrier** then. Never call ordinary `commit`/`dispatch` and await it while retaining a phase: normal admission deliberately waits behind that phase. Use `submit` for its own filesystem effects. Do not execute codecs, GTK/UI, async-runtime waits or arbitrary IPC on the library owner.

Earlier normal mutations finish before successful begin; later normal jobs wait until close plus accepted subjob drain. Completed-but-uncollected tickets, retained closed phase handles, and finish observers retain their quotas, **not normal admission**. A retained result cannot certify IO completion; `finish` only signals after the actual owner drains and releases admission. Snapshots/status remain responsive; SHOW uses the existing independent pool and fences only accepted mutations. An idle open phase adds no mutation fence. A SHOW admitted after a subjob waits for that subjob's real filesystem/snapshot phase. Other normal queued commands still wait behind normal admission.

The host must split at actual source asynchronous phase boundaries. This API does not justify whole-request FIFO, latest-only cancellation, or changed metadata/title/text ordering. A host that accepts source work must retain its phase and actual dependency futures through caller drop if source continuations remain to be submitted. Dropping a capability intentionally rejects those **unsubmitted** continuations; it never claims they ran.

## Quotas and failure observations

| Resource                                       | Bound                                                              |
| ---------------------------------------------- | ------------------------------------------------------------------ |
| Whole phase leases                             | 4, including pending begin and completed retained handles/results  |
| Accepted + running + ready/uncollected subjobs | 4 per phase                                                        |
| Lifetime subjob admissions                     | 32 per phase                                                       |
| Pending finish observers                       | 4 per phase; immediate rejection, no waiting admission queue       |
| Request JSON plus window UTF-8 bytes           | 64 KiB per begin/subjob                                            |
| Reply serialized JSON                          | 1 MiB per subjob, at most 32 events                                |
| JSON values                                    | 16,384 aggregate nodes, maximum nesting depth 32 per request/reply |
| New threads, runtimes, timers                  | 0                                                                  |

Strings and keys are checked against an aggregate UTF-8 budget before the JSON writer scans/escapes them; serialized escaping and structure must also fit the exact byte cap. Context paths must be Unicode. Reply checks cover the retained event payload and the current protocol's private JSON fields. Captured native closures and buffers are trusted: the caller must separately bound their allocation, synchronous IO/CPU, handles, diagnostics and cooperative deadlines. This API cannot inspect a closure's captures or preempt a syscall. An open live phase has no automatic expiry; its native owner must close it. Terminal control can always seal it.

`library_phase_resource` distinguishes quota/JSON rejection; `library_phase_closed` distinguishes sealed admission. Context failures retain `StaleLibrary`; actual stopped/poisoned-owner paths remain `WorkerUnavailable`. An accepted closure can apply effects and then fail. Reply-size validation also runs **after** the operation; a rejected oversized result does not roll back or certify absence of effects. The host must bound its intended result before effects and retain actual error receipts. There is no invented source success ACK.

## Terminal and cancellation races

`finish_writes` first seals the bounded phase registry through independent control, then follows its existing normal admission/drain behavior. This cannot deadlock on host-retained phase handles, saturated private work queues or uncollected results. Actual accepted subjobs finish before the admission guard is released. Pending begins are sealed and reject; earlier ordinary queued admission attempts keep their FIFO ordering, including final SAVE. Phase admission remains terminally closed even if that first terminal waiter is canceled before the ordinary fence acquires admission. Normal admission retains its existing behavior until that fence takes it.

The host must drain accepted now-playing preparations/dependencies/continuations **before** calling this terminal library fence, as required by the plan. The fence does not invent or accept jobs which the host has not submitted. It remains a repeatable terminal **write fence**, not thread termination. Existing LibraryService startup discards the IO/SHOW `JoinHandle`s; this change does not repair that separate lifetime contract or claim actual thread joins. A failed phase completion likewise does not prove whole-thread exit.

Admission ownership sits in sealed control state. Before actual IO-owner entry, closing a pending begin releases its guard because no published capability or accepted subjob can exist. After entry, only actual owner drain/unwind releases it. This also covers a reserved mpsc send that publishes after the receiver exits, which can otherwise retain a queued marker and its guard. Begin independently observes receiver closure. A closed late marker performs no validation, effects or sequence update: only real submitted subjobs allocate mutation sequences. It cannot regress progress or strand SHOW fences when a later normal mutation was sent first. Ordinary trusted-closure panic retains existing worker-failure behavior; it is never reported as successful phase drain.

## Verification

Linux executed `cargo test -p freeshow-core --offline`: 63 checks (24 unit tests, 9 context, 10 location, 16 new phase integration tests, 4 storage) plus one compile-fail seal doctest. The two new private reservation regressions live in `service.rs` because they must control real mpsc permits and worker exit. The tests use actual temporary filesystem writes and the existing IO thread, with controlled blocked closures rather than fake completion receipts. They prove earlier/phase/later order and thread affinity, responsive snapshots/idle SHOW, SHOW-after-mutation fences, caller/ticket/phase/finish-waiter drop, retained quotas, lifetime job limits, saturated close/control, pending/enqueued begin cancellation, context changes/foreign/embedded SAVE authority, actual final SAVE persistence, ordinary errors, and actual worker unwind.

Existing ZIP-copy/compression/read concurrency, shutdown, final-save/context/storage tests remain part of that same full core run. Strict Clippy, formatting, and Windows/macOS cross checks are reported with the sealed commit; cross checking is build evidence, not execution on those OSes. This slice adds no package dependencies, root manifests, frontend changes, apps, profiling or performance claim.
