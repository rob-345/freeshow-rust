# Network snapshot reply and disposal ordering

Scope: `src-tauri/src/network_snapshot.rs` and its inline regressions only.
No shared manifests, main routing, IPC, OS capture algorithms or source payload
semantics changed. The service still uses one fixed OS thread, the same bounded
channel/admission quota and actual join/retry shutdown. There is no new task,
thread, polling service, native network send, listener or fabricated IP result.

## Windows CI observation versus production ownership

Run39228374634, revision84822efc, Windows native `network_queries` failed
`completed_uncollected_work_and_values_keep_real_admission` at old line1061:
admitted was2 after an unpolled future was dropped, where the test expected1.
The same inline test had already passed in the main/discovery executables.
The relevant downloaded receipt is
`/tmp/freeshow-folder-ci-windows-native.log`.

The test waited for `active == 0 && queued == 0`. The worker decrements queued
when it dequeues Work and clears active after capture, **before** reply handoff.
That observation can precede `oneshot::Sender::send`. Dropping the receiver then
does not synchronously dispose the still-worker-owned Completion. Admission2
in this window is conservative, correct ownership, not an admission leak.
The counters are independent diagnostics, not a ready-result or shutdown fence.

The original test now observes a cfg(test)-only `settled_replies` Release/Acquire
fence. It advances only after an explicit inner Work scope ends: successful
handoff or failed-send Completion destruction, plus all budget/failure/frame
guards. A successful uncollected oneshot still holds its original permit and
native guard; the fence does not dispose that result. This is not a production
status/drain API, nor a claim that reply admission means the worker has joined.

## Actual cleanup-order correction

A separate real ordering gap existed: Work, Completion and Captured declared
their semaphore permit before the consumer keepalive. Rust destroys struct
fields in declaration order. The permit could therefore become available while
the actual native consumer guard was still being destroyed. All three now
declare the permit last. Result/value and consumer-guard disposal precede quota
release on queued, failed-send and collected-result paths.

Shutdown still observes actual thread completion and joins the same handle.
Timeout retains that handle for a later retry; caller Drop, capture-idle
counters, reply-settled test state and the worker's done notification are not
substitutes for the join. Externally retained Captured values keep their own
leases until actually consumed/dropped; their consumers must drain before
dependency teardown. No immutable snapshot or native syscall preemption is
introduced.

## Deterministic evidence

- A gate after actual capture/active0 but before reply send recreates the exact
  CI observation window. Dropping its caller retains admission, rejects another
  request as busy and makes actual shutdown time out. Releasing the gate and
  retrying shutdown joins the worker and releases the owned slot.
- A dropped caller's real failed-send Completion enters a controlled blocking
  consumer destructor on the existing OS thread. A oneshot witness records the
  permit is still held inside that destructor. While held, admission remains
  full, the Work-frame fence stays0 and shutdown times out. Only release plus
  actual join makes teardown complete. No auxiliary thread simulates cleanup.
- A collected Captured value records available permits from its actual consumer
  Drop, proving that result handoff also retains quota through that destructor.
- The actual worker-panic test records the queued Work guard's destructor before
  its permit release, after sticky failure publication; shutdown still joins and
  retains the original failure across retries.
- Temporarily restoring just Completion's original field order makes the new
  blocking-destructor regression fail deterministically: available permits1,
  expected0. The fixed file was restored in a finally block, then the full
  focused suite was rerun. This demonstrates the production ordering gap
  independently of the intermittent CI scheduling window.

The source comparison is unchanged: actual pinned MAIN.IP/getLocalIPs executes
under Node22.21.1 and uses actual native interface snapshots. The focused Linux
headless `network_queries` target passes15 tests (9 snapshot owner and6 native
query cases), including the genuine source/native comparison and held ordered
library owner case. Rustfmt, Prettier, diff whitespace and strict Linux plus
Windows GNU cross-Clippy checks passed. The cross-check is compilation/lint
evidence, not a native Windows run.
The next coordinator Windows CI must establish the native result.

The temporary headless manifest at
`/tmp/freeshow-network-snapshot-ci-check/Cargo.toml` references the committed
core/discovery crates, this exact `src-tauri/tests/network_queries.rs`, Tokio,
serde/tempfile, Unix libc and the existing Windows network features. It avoids
unrelated Tauri/ALSA builds and does not alter shared manifests. Checks use
jobs1/debug0/incremental0 and the authorized root target cache. Production CI
uses the normal target:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --features custom-protocol \
  --test network_queries --locked
```

Focused local checks:

```sh
cargo test --manifest-path /tmp/freeshow-network-snapshot-ci-check/Cargo.toml \
  --test network_queries --offline
cargo clippy --manifest-path /tmp/freeshow-network-snapshot-ci-check/Cargo.toml \
  --test network_queries --offline -- -D warnings
cargo clippy --manifest-path /tmp/freeshow-network-snapshot-ci-check/Cargo.toml \
  --test network_queries --target x86_64-pc-windows-gnu --offline -- -D warnings
```
