# SystemService native owner teardown

The original native service discarded its `std::thread::JoinHandle` at startup and returned successful shutdown when its worker sent a done watch value. That message was emitted before the thread returned, so success did not prove native thread teardown or report a teardown panic.

The service now retains one native handle through its actual join. `shutdown()` first closes admission, then admits at most four observer futures without queueing; excess observers receive `system_busy`. Each observer has a two-second wait budget. Pending native work is polled asynchronously every millisecond; timeout returns `system_shutdown_timeout`, preserving the actual handle for retry. Dropping an admitted observer releases only its observer slot. It leaves closed admission, accepted native work and the join handle owned by the retained service.

`workerRunning` reports the handle's actual `is_finished` observation; `workerJoined` becomes true only after the real join returns. These are distinct from `activeJobs`, `queuedJobs` and the internal logical done watch. A named owner-lock guard covers taking and joining the handle, so concurrent shutdown/status callers cannot observe a temporarily empty handle collection as completed teardown. A caught request panic is retained before its reply is sent. Both that failure and actual native join panic remain sticky across subsequent shutdown calls; failures do not skip the actual join.

The service starts without a Tokio runtime. Already-finished shutdown can also join and return without Tokio; pending shutdown returns explicit `system_runtime` if no runtime is entered, retaining the actual handle. Pending shutdown otherwise requires a Tokio runtime with time enabled. The parity executable now creates one current-thread Tokio driver and reuses it for its existing probe futures. It adds no worker/helper thread and preserves source reply projections. The crate explicitly enables the existing Tokio `rt` and `time` features.

No OS API call is forcibly canceled. The final join is taken only after `is_finished`, which Rust documents as supporting a nonblocking join pattern; Rust also discloses a possible brief final native teardown interval. That last join interval and OS destructor work cannot be preempted by the async observer deadline. [Rust JoinHandle documentation](https://doc.rust-lang.org/std/thread/struct.JoinHandle.html#method.is_finished) The service's final `Drop` closes admission but cannot prove a join if the host discards its last retained service handle. The native host must keep the service and retry shutdown until actual joined proof or an explicit teardown failure. There is no hidden shutdown task or replacement owner.

GET_OS, device-name first `.local` replacement, RAM sampling/provenance, Number arithmetic, queue capacity, job deadlines and original response shapes are unchanged. DEVICE_ID preparation and IPC/main remain separate coordinator work.

## Verification

Five new lifecycle cases use bounded, fixture-local real worker controls:

- A real worker is paused after its logical done notification. Active/queued counters are zero, but shutdown times out and an abandoned observer leaves the actual native handle retained. Only witnessed native exit followed by actual shutdown joins it.
- Four concurrently polled shutdown observers fill the observer budget; the fifth fails truthfully. Dropped observers reclaim only their own slots. Two later observers safely share one real join.
- A genuine caught worker panic is sticky before the consumer receives its reply. A held native-exit gate still forces timeout; releasing it permits the real join and retains the original failure on retry.
- A genuine panic at native thread teardown is detected by the actual join, then remains failed after its handle has been consumed.
- With no Tokio runtime, pending shutdown explicitly fails while retaining its handle; after actual native exit is observed, shutdown joins successfully on the same non-Tokio executor.

All 13 focused Rust cases passed (eight existing plus five new lifecycle cases), as did all six unchanged pinned-source system Node cases using the real native OS boundary. Strict all-target Clippy passed for Linux, Windows GNU and macOS ARM. A negative control temporarily restoring done-watch-as-success failed the held-exit regression with unexpected success; the real implementation was restored and all 13 tests passed again. Cross-target checks establish compilation/lint only, not Windows/macOS runtime behavior.

```sh
export PATH=/path/to/node-v22.21.1/bin:$HOME/.cargo/bin:/usr/bin:/bin
export CARGO_TARGET_DIR=/mnt/data/cache/freeshow-cover-native-portability
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo +1.99.1 test --offline --locked -p freeshow-system --all-targets
cargo +1.99.1 build --offline --locked -p freeshow-system --bin freeshow-system-parity
FREESHOW_SYSTEM_PARITY="$CARGO_TARGET_DIR/debug/freeshow-system-parity" node --test tests/parity/system-info.test.mjs
cargo +1.99.1 clippy --offline --locked -p freeshow-system --all-targets -- -D warnings
cargo +1.99.1 clippy --offline --locked -p freeshow-system --all-targets --target x86_64-pc-windows-gnu -- -D warnings
cargo +1.99.1 clippy --offline --locked -p freeshow-system --all-targets --target aarch64-apple-darwin -- -D warnings
```
