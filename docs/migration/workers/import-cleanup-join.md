# Import cleanup owner and actual joins

Based on `56e614d5`. The import transport/source oracle remains pinned to
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`; this patch changes native ownership,
not import selection, payloads, renderer callbacks, or final media installation.

`ImportService` retains the cleanup thread's real `JoinHandle`. The thread holds
only its receiver and separate progress state: it never holds its sender, join
owner, or an `ImportService`. Live stages and accepted removals retain the original
whole prepared-result permit. A native stage guard survives until actual directory
cleanup returns; the removal frame disposes its permit before advancing the
ordinary cleanup barrier, including during unwind. The same fixed parser workers
(default two, maximum eight) and one cleanup thread remain; there is no new task,
thread, runtime, timer owner, or polling while idle.

The public lifecycle has three distinct operations:

- `finish_cleanup()` waits for removals already released when its barrier is
  captured. It neither cancels preparation nor waits for live plans or thread exit.
- `close_preparation()` cancels admission/work, observes and joins actual parser
  threads, then applies the ordinary cleanup barrier. Live staged plans may remain.
  This preserves the old nonterminal shutdown behavior under an explicit name.
- `shutdown()` closes preparation, waits for all live staged plans/removals, closes
  the cleanup channel, observes actual native exit, and joins the cleanup thread.
  Only `Ok(())` proves these actual joins. A retained plan without a stage can
  outlive this join, and still retains its original prepared-result permit.

There are four shared lifecycle-observer slots, including observers waiting for
serialization; excess requests return `lifecycle_busy`. Dropping/timing out an
observer releases its observer slot, not native ownership. Accepted cleanup still
runs. A later observer resumes the same retained handles; there is no detached
async shutdown continuation. No shutdown deadline or resource cap was increased.
Hosts may impose an observer timeout and retry. Pending native joins require a
Tokio runtime; already-finished joins work without one. The 1ms exit observation
runs only during an active shutdown observer. OS filesystem calls are not
preemptible by Rust cancellation or an observer timeout.

The barrier uses submitted/disposed counters rather than queued fence messages.
Abandoning barrier observers cannot consume removal queue slots. The queue is
bounded by `max_prepared` (at most 16), as are real stages. Unexpected send failure
retains the stage and its original permit in a bounded stranded-work collection
and fails admission closed; it does not discard the stage path or release its
permit as a substitute for cleanup. Cleanup/parser panic and cleanup IO failures
remain sticky. A cleanup error is not proof of directory absence or joined drain;
release native plans and retry terminal shutdown. Stranded work after receiver
failure remains owned until final disposal, and cannot produce a successful drain.

Final owner destruction closes admission and synchronously joins remaining native
threads. This emergency path can block on nonpreemptible IO; normal async shutdown
joins only handles whose `is_finished()` is already true. Ordinary plan Drop only
queues cleanup while the service owner is retained. Dropping the last service and
plan owners without completing the lifecycle can invoke that synchronous safety
join, including on the last plan's releasing thread. The host must retain its
service owner through actual plan release and terminal shutdown. There is no
ownership cycle or possible cleanup-thread self-join.

## Required native-host composition

This patch does **not** wire `NativeImports`. Its first call in the current
`NativeImports::shutdown` must change from `service.shutdown()` to
`service.close_preparation()`. It must still close host admission, cancel admitted
host requests, drain the actual host active registry/plan closures, and then call
`service.shutdown()` for the terminal join. Attempt the final drain even if the
first phase returns an error; retain failures rather than reporting joined success.
Calling terminal shutdown before releasing the live staged plans it awaits can
otherwise deadlock the host's own sequential shutdown composition.

```rust,ignore
// Native host admission is already closed, and accepted requests are canceled.
let preparation = service.close_preparation().await;
// Await real host closures and release their PreparedImport/MediaCommitPlan values.
drain_actual_host_requests().await;
let terminal = service.shutdown().await;
preparation.and(terminal)?;
```

`finish_cleanup()` remains available for ordinary successful import completion;
its result must never be presented as terminal cleanup-thread join evidence.
No shared manifest, lock, IPC, native host, or desktop fixture is changed here.

## Checks and evidence

Private manifest/source links and all build output live below
`/mnt/data/cache/freeshow-rust-build/import-cleanup-join`; the manifest reuses the
crate's dependencies and a private copy of the existing root lock. Commands use
`CARGO_TARGET_DIR` there, `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`, and
`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`:

```sh
cargo +1.99.1 test --offline --manifest-path /mnt/data/cache/freeshow-rust-build/import-cleanup-join/check/Cargo.toml
cargo +1.99.1 clippy --offline --all-targets --manifest-path /mnt/data/cache/freeshow-rust-build/import-cleanup-join/check/Cargo.toml -- -D warnings
cargo +1.99.1 clippy --offline --all-targets --target x86_64-pc-windows-gnu --manifest-path /mnt/data/cache/freeshow-rust-build/import-cleanup-join/check/Cargo.toml -- -D warnings
cargo +1.99.1 clippy --offline --all-targets --target aarch64-apple-darwin --manifest-path /mnt/data/cache/freeshow-rust-build/import-cleanup-join/check/Cargo.toml -- -D warnings
```

Linux: 28 Rust tests pass, including all 21 previous import/parser/text tests and
seven new lifecycle cases. Native gates hold actual cleanup before remove and
before real thread exit. Tests witness live stage/permit retention, four abandoned
observers and fifth-request rejection, canceled barrier reuse without queue growth,
ordinary fence success while physical exit is held, actual later joins, sticky
worker panic, actual disconnected-receiver retention through final disposal,
failed-barrier waiting for other accepted removals, final-owner
safety join, and pending/finished no-runtime behavior.

A real negative control temporarily returned terminal success after channel close
without waiting for the held thread's exit/join. The barrier-versus-exit test failed
at its strict pending-shutdown assertion. The source was restored and the complete
suite rerun; the negative log is private `negative-control.log`, not product code.

The private rebuilt `freeshow-imports` driver passes all 14 existing Node source
cases from `imports.test.mjs` and `imports-text.test.mjs`, using pinned Node
22.21.1 and `FREESHOW_IMPORTS_BIN` pointing to the private target. These exercise
actual immutable handlers/converters with controlled filesystem/DOM boundaries;
they are not WebView/GUI acceptance. Strict all-target Clippy passes for Linux,
Windows GNU and macOS ARM; foreign compile evidence is not native execution.
Formatting passes. Native Windows/macOS lifecycle execution and the coordinator's
host-composition integration remain required before claiming application shutdown
parity or closing its recorded actual-cleanup-join gap.
