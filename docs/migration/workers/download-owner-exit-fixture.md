# Download shutdown fixture: actual owner exit

The macOS core job in run `39255050656` failed the existing download shutdown fixture: the controlled network task panicked as requested, but the final shutdown returned `download_shutdown_timeout` instead of `download_worker`. The fixture configured a 20ms observer deadline and released the task immediately before that final assertion. Joining the network task is followed by closing the filesystem queue, observing actual native-thread completion, and joining those threads. A released task does not prove those later owners have exited within 20ms.

This correction changes only test support and the affected fixture. The production shutdown implementation, 20ms fixture policy, task/thread counts, sticky failures, and parallel execution remain unchanged.

The fixture now proves these boundaries for both ordinary exit and a genuine controlled task panic:

1. With the network gate held, the first shutdown times out and retains the actual remaining task handle. Dropping a polled retry does not dispose it. Both filesystem owners remain alive.
2. Releasing the network gate eventually makes that retained task's `is_finished()` true. Observing this does not remove its handle or publish the join failure. Both filesystem owners are still alive.
3. An actual shutdown joins that task and closes the filesystem queue. One fixture-local gate holds a real native thread after its receive loop ends. The unchanged 20ms shutdown must time out, retaining both native join handles even after the other native thread finishes. The controlled task failure is already sticky at this point.
4. Releasing the native-thread gate and observing both retained handles finished permits the original strict final assertion: success for ordinary exit, or the exact retained `download_worker` error for panic. Actual shutdown must remove all joined handles. A further shutdown must preserve the same result.

The observation reads only the retained Tokio and native `JoinHandle::is_finished()` values. It never joins, removes handles, closes queues, publishes failures, or substitutes logical work counters. The test's bounded five-second condition wait allows native scheduling to reach a specified real boundary; it does not retry shutdown until success or enlarge its production deadline. The native gate is one per fixture, releases its queue mutex before waiting, and exists only under `cfg(test)`.

Focused verification on Linux, Rust 1.99.1, using one Cargo job and the existing private cache:

```sh
export CARGO_TARGET_DIR=/mnt/data/cache/freeshow-cover-native-portability
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo +1.99.1 test --offline --locked -p freeshow-ffmpeg-download --lib shutdown_timeout_and_abandoned_retry_retain_the_real_remaining_join_and_sticky_fault -- --nocapture
cargo +1.99.1 test --offline --locked -p freeshow-ffmpeg-download --all-targets
cargo +1.99.1 clippy --offline --locked -p freeshow-ffmpeg-download --all-targets -- -D warnings
```

The focused case and all 16 library tests pass; the parity binary has zero unit tests. Strict Clippy and formatting pass. The controlled panic output is expected and asserted through the real task join. This is Linux execution evidence for the fixture correction; actual macOS execution remains the next native CI check. No application, GUI, release build, network outside controlled test loopback, or performance comparison was run.
