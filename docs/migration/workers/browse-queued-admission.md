# Bounded queued browse admission

The retained editor starts several MediaTabs/AudioTabs counts and visible folder listings concurrently. The native browse preparation previously offered only fail-fast worker admission: a request rejected with `browse_busy` could leave that visible listing blank until another navigation. This crate slice adds an explicit waiting API. It does not yet wire the native host or demonstrate the editor behavior changing.

The existing `dispatch`, `dispatch_with_cancel` and `prepare_listing_with_cancel` methods and their parsing/resource tests retain their fail-fast behavior. The new methods are:

```rust
service.dispatch_queued(command, payload).await?;
service.dispatch_queued_with_cancel(command, payload, cancel).await?;
service.prepare_listing_queued_with_cancel(payload, cancel).await?;
```

`READ_FOLDER` hosts that complete thumbnail effects must use the queued preparation method. Ordinary queued dispatch still reports `thumbnail_unavailable` with the original intents instead of silently omitting those effects. Listings, diagnostics, field shapes, options and source traversal are unchanged.

The new API requires a Tokio runtime with timers. No entered runtime returns `runtime_required` before admission. One configured total timeout starts when the queued future is first polled, before parsing, covers the FIFO semaphore wait and result wait, and is passed unchanged into the actual worker's existing `Budget`. Expiry returns `browse_resource_limit`; request cancellation returns `cancelled`; service shutdown/closure returns `browse_shutdown`; actual disconnected workers return `browse_worker_error`. A ninth waiting request returns `browse_busy` before retaining parsed owned paths, even if its payload is malformed.

There are eight separate waiting slots and the original configured execution slots (default two, configuration still bounded to at most eight). A waiting slot is acquired with `try_acquire_owned` before parsing. The existing worker semaphore supplies FIFO waiting order; it is acquired once, with no retry loop. Its permit transfers into `Work` only after cancellation/deadline/closure checks. No request spawns a task, blocking job or native thread. Default construction still creates exactly two filesystem owners. The bounded native work channel is unchanged.

Dropping/canceling a waiting future removes its semaphore waiter and releases its waiting slot. Once native work has transferred, caller cancellation/Drop/deadline cancels the private cooperative token and releases the observer, while the actual worker keeps the execution permit. The worker now releases that permit after sending or disposing the real result, including failed-send disposal for an abandoned receiver. A successful reply alone is not an observation that its worker frame has returned. No hard syscall cancellation is claimed. The existing shutdown still awaits real work and native thread joins; an abandoned shutdown observer leaves those owners retained for retry.

Results already returned to a caller are outside these execution/waiting counts. The coordinator must retain and bound complete native host requests, including finished/uncollected results and thumbnail continuations, to ten. It also owns frozen full-context/listener checks and stale-result publication. Queued futures must be driven or dropped during retirement; this crate deliberately does not spawn hidden tasks to drive a caller's future. A canceled/expired observer is not proof that an entered filesystem operation has finished.

## Native host test seam

`test-harness` is an opt-in feature with no default features. Under that feature or `cfg(test)` only:

```rust
let (mut entered, releases) = service.hold_workers_for_test()?;
// Dispatch two real requests, then await two distinct worker indices on `entered`.
// Submit eight more and await service.queued_waiters_for_test() == 8.
// A whole-host eleventh request should fail the host's ten-request budget.
for release in releases {
    let _ = release.try_send(());
}
```

Arm before dispatch. There is one gate per actual configured owner, one installation per service, and a channel bounded to that owner count. Each gate blocks a real entered `Work`; unique worker indices index the release senders, rather than claiming OS thread IDs or a universal signal order. Sending once or dropping each release sender unblocks that actual owner. A second arm reports `browse_busy`. The observed waiting count is eight minus actual available waiting permits; it does not dispose work or imply completion/join. Only the native integration's dev-dependency should enable this feature. Ordinary production builds contain neither the hooks nor these observers.

The richer crate-only tests register eight semaphore waiters by explicitly polling them in known order, hold the second physical worker throughout, and drive the first lane one request at a time. Actual entry paths prove FIFO order independently of Tokio task scheduling. Each actual `Work.started` must remain inside its original registration interval. A negative control that reset `started` at worker handoff failed this assertion, then the original code was restored.

## Focused evidence

Linux, Rust 1.99.1, one Cargo job, debug info/incremental disabled, existing private cache:

```sh
export CARGO_TARGET_DIR=/mnt/data/cache/freeshow-cover-native-portability
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo +1.99.1 test --offline --locked -p freeshow-browse --all-targets --features test-harness
cargo +1.99.1 clippy --offline --locked -p freeshow-browse --all-targets --all-features -- -D warnings
cargo +1.99.1 clippy --offline --locked -p freeshow-browse --all-targets --all-features --target x86_64-pc-windows-gnu -- -D warnings
cargo +1.99.1 clippy --offline --locked -p freeshow-browse --all-targets --all-features --target aarch64-apple-darwin -- -D warnings
cargo +1.99.1 build --offline --locked -p freeshow-browse --bin freeshow-browse
FREESHOW_BROWSE_BIN="$CARGO_TARGET_DIR/debug/freeshow-browse" FREESHOW_TEST_PACKAGED_BROWSE=0 node --test tests/parity/file-browse.test.mjs
```

All 17 library tests pass with default features and with `test-harness`: nine existing cases plus eight new cases covering bounded/FIFO admission and shared deadlines, cancellation/Drop, no late dispatch, retained owners/joins, post-process permits, thumbnail preparation, runtime rejection and the public test seam. The binary and macOS-only target have zero Linux test bodies. All nine existing Node source comparisons pass under pinned Node 22.21.1; packaged Electron is not launched. Strict Clippy passes for Linux, Windows GNU and macOS ARM, including the opt-in seam. Formatting/diff checks pass. Cross-compilation is not actual Windows/macOS execution; those new scheduling cases still require native CI. No GUI, release build, benchmark or shared host edit was performed.
