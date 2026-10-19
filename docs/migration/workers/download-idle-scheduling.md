# Download owner idle scheduling

The native download service previously selected a 20 ms sleep on both fixed network owners, including when no download existed. Each expiration scanned the job registry and reentered the loop. That permits about 100 empty timer wakeups per second across the two owners; it is a source scheduling observation, not measured CPU use or evidence about the archived application regression.

The replacement keeps the same two network tasks, two filesystem/DNS/archive threads, two whole-job permits, bounded queues, and existing staging authority. No runtime, task, or thread is created per request. Public download APIs, source progress/completion plans, HTTP/gzip handling, and host installation obligations are unchanged. The only manifest addition is the existing Tokio dependency's `test-util` feature for development tests.

## Wakeup and ownership rules

Each idle network owner now waits for actual queue work, service cancellation, phase notifications, or a pending prepared job's cancellation/deadline. A fixed-size snapshot copies at most two sets of cancellation tokens and deadlines. It retains no `Job` or prepared artifact owner: otherwise an idle wait could prevent the last prepared value's `Drop` from queueing cleanup. With no prepared job, the expiry future stays pending and creates no timer. With prepared jobs, each owner has at most one timer for their earliest deadline, plus their bounded cancellation waits.

The shared notification is enabled **before** inspecting phases. Newly prepared jobs, accepted installs, and queued cleanup notify both owners. A phase change between inspection and the select therefore causes a new snapshot instead of leaving an obsolete timer or losing a newly required deadline. Ordinary queue receive wakes the first real job after idle; it does not depend on a periodic scan.

Queued/streaming jobs remain owned by the real transfer and archive work. An unaccepted prepared value remains revocable by its original absolute deadline, caller cancellation, ticket cancellation/drop, or shutdown. Expiry and cancellation still await the actual fixed filesystem owner's cleanup receipt. That receipt is published after cleanup and permit release. An accepted install intentionally has no reaper timer: the trusted final IO owner pins its artifact until `finish` or guard disposal, including across shutdown timeout. No output is installed or executed by this service.

## Terminal retry correction

The audit also found that shutdown awaited all network handles and cleared their vector only afterward. If the first handle joined and shutdown timed out on the second, retry could poll the first completed handle again. Shutdown now borrows each live handle through its await and removes each completed handle immediately. Dropping or timing out an observer retains the actual pending handle for retry. A pump fault is recorded while the remaining real owners continue to drain; repeated shutdown retains the failure after successful joins. Finished filesystem threads are also all joined before reporting a join fault.

The existing shutdown order remains network drain, whole-job permit drain, filesystem sender close, actual filesystem thread exit, and joins. Its 1 ms waits while checking terminal permits/thread exits remain; these are bounded shutdown waits, not ordinary idle polling. OS filesystem/DNS calls still have the preexisting cooperative cancellation limitations. Callers must await shutdown while Tokio is alive; dropping an observer is not drain proof, and dropping the entire service is not a substitute for that contract.

## Focused evidence

The Rust tests retain all eleven existing download cases and add five:

- A no-job service exposes only test-local fixed-size owner-loop counters. Advancing a paused Tokio clock by one second, across fifty former poll periods, produces no extra loop iterations. This test admits no jobs and makes no virtual-time claim about `std::time::Instant` expiry.
- A separate ordinary-clock service witnesses both idle owners, then performs its first actual loopback HTTP/gzip transfer, verifies staged bytes, and awaits cleanup plus real service shutdown.
- A real absolute deadline expires while the caller retains a prepared output. Without another request, the actual cleanup receipt arrives, the artifact disappears, the permit is released, and acceptance reports `download_deadline`.
- Three ordinary-clock variants cancel a retained prepared output, drop a successful but uncollected accepted ticket, and drop a collected prepared value after both owners observed it. Each requires actual cleanup before the original deadline, without a second job. Wait snapshots cannot hide abandoned ownership until expiry.
- A private gate holds the second actual network task before return. Shutdown joins the first, times out retaining only the second, and a directly polled/dropped retry cannot abandon that handle. Releasing the gate permits real network/filesystem joins; a controlled owner panic remains sticky across subsequent shutdown calls. Existing accepted-install and real slow-archive cancellation tests also remain.

These tests measure scheduling and ownership directly. They do not establish whole-application CPU, latency, hardware, production-network, or packaging performance. Loopback test bytes are inert and never executed. No GUI or application comparison was performed for this slice.

Linux checks passed: all 16 crate tests and the parity binary target; ten consecutive complete test-binary runs with eight test threads (160 passing test executions); warnings-denied all-target Clippy; crate Rust formatting, document Prettier, and `git diff --check`. The existing seven immutable-source Node22.21.1 suites also passed with zero skips, covering 25 URL cases, 26 actual loopback downloads, 20 progress samples, seven resource cases, and two padding/chunk cases. No macOS or Windows execution was performed for this followup.

Negative controls temporarily restored each old branch independently: the empty 20 ms poll made the idle counter assertion fail (`[2, 2]` versus `[1, 1]`); retaining all completed joins until the end made the gated shutdown assertion fail (two handles instead of one). The exact final source was restored, rebuilt, and passed all checks afterward. These controls establish that the new tests detect the targeted scheduling and ownership regressions, without interpreting elapsed wall time as drain proof.
