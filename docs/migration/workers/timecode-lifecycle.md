# Terminal timecode context observers

The native host previously published a terminal drain failure to its progress
watch while replying `Ok(())` to the pending context-change observer. That could
let frontend lifecycle code adopt a new context even though the actual timecode
host had closed after a publisher/delivery failure. Later observers could receive
a lifecycle-busy/worker error from a closed channel instead of the original
failure.

This refinement changes only `src-tauri/src/timecode.rs`, its focused tests and
this note. Terminal `actual_drained` sends the same bounded first error to active
and queued Change observers. A clean terminal host sends `timecode_shutdown` to
those observers; it cannot renew a context. Ordinary nonterminal retirement and
reopenable source TIMECODE_STOP keep their existing behavior.

`context_changed()` retains its existing single lifecycle deadline across queue
observation and any required terminal drain/join. Terminal channel closure,
observer contention and a failure racing an ordinary lifecycle reply all use
the existing retained `shutdown()` path. They do not turn a known terminal error
into an immediate success or abandon cleanup. While actual accepted work or the
supervisor remains blocked, the caller may receive
`timecode_retirement_timeout`; owners and the live JoinHandle remain retained
for retry. Only after actual drain and supervisor join does a failed terminal
context call return the original sticky error code/message. A successful clean
terminal drain still returns `timecode_shutdown` for context renewal.

No thread, command, renderer success ACK, service replacement or context reset is
added. Internal lifecycle admission still has one active observer and one queued
control record. The coordinator owns the existing bound on public lifecycle
observers. Aborting or timing out an observer does not cancel accepted native
close/join work. The existing test-only supervisor exit gate and its root-added
dead-code annotation are unchanged by this patch.

Three new regressions exercise actual MidiService ownership over controlled
native port handles:

- A real input close is blocked after a genuine missing private publication
  completion. An observer is aborted, a retry times out while the port remains
  owned, and later retries preserve the original delivery failure after close.
- The actual native publisher returns a closed-channel failure. Real port close
  is blocked, observers are abandoned/retried, then the real supervisor remains
  gated after drain progress. Context observers cannot return the original error
  before the actual supervisor join; releasing the gate permits the retained
  join and all later calls return the same original publisher error.
- After a clean terminal shutdown, repeated context changes return the explicit
  closed error and never renew the process-global source run.

Failure-path test guards release their actual blocked port and exit gate even if
an assertion fails. The full strict host suite passes 21 tests, including existing
actual immutable source/original LTC byte comparisons. Linux, Windows GNU and
macOS ARM all-target Clippy, Rustfmt and focused document formatting were checked
with the worktree-local target, offline cached dependencies and one Cargo job.
Cross compilation does not add hardware/platform-runtime claims. No root/native
GUI build, shared bridge/helper edit or new packaging capability is involved.
