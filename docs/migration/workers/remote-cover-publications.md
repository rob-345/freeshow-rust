# Native fixed-cover folder publications

This foundation is not registered in the desktop binary. It does not serve an
HTTP endpoint. The native HTTP adapter, main-window IPC and retained bridge
subscription remain integration work.

`remote_cover_publications.rs` consumes a sealed `Observation` from actual
ordered fixed-cover preparation. Only its genuine fixed-Audio mkdir failure can
produce the original MAIN ALERT text. Runtime ID, generation and captured data
path must all match the ready native context. No request supplies a file path,
folder or message. The private receipt and positive decimal u64 sequence remain
outside the original `{channel, data}` callback; the callback has no listener ID.

One process-lifetime registry retains four publication slots across replacement
handles. A process-wide synchronous publication lane covers sequence allocation
through the actual trusted transport enqueue. This prevents concurrent HTTP
owners from enqueueing sequence N+1 before N and causing the frontend's monotonic
replay guard to reject a genuine earlier alert. The registry retains no emitter
or AppHandle. The per-call emitter must synchronously enqueue and cannot reenter
publication/lifecycle operations. Completion may reenter independently because
the registry state mutex is released before enqueue.

The returned receipt owns its actual observation. HTTP completion, cancellation,
observer timeout, a dropped waiter and UI acknowledgement do not release its
native file lease. Actual receipt disposal drops the observation before releasing
publication admission. If enqueue succeeded but no UI completion was observed,
receipt disposal releases actual IO resources while the registry retains a
bounded delivery tombstone. A genuine late, one-use, full-context acknowledgement
or terminal closure with admission permanently sealed can dispose that tombstone. This
prevents successive timed-out HTTP requests from accumulating an unbounded
WebView event queue. A known synchronous enqueue failure is rejected immediately.

`ready(context)` must follow actual retirement of obsolete native work.
`retire(context, terminal)` seals admission and retires matching observations;
it is not an IO-thread join or a queued-WebView disposal fence. Ordinary context
retirement retains unknown delivery slots. A genuine late full-context ACK can
dispose its slot without making the retired observation successful. If a stalled
WebView never consumes old-context events, the four slots can remain exhausted
until terminal shutdown; a native queue-disposal fence is still absent. Cancellation tokens from an old remote run must remain
cancelled through same-context restart. Remote STOP does not permanently seal the
frontend's library-context helper. Terminal closure is process lifetime and
cannot be undone by constructing another native handle.

Eight Linux integration tests use real ordered library phases and actual Audio
regular-file blockers. They check source-only construction, exact payload/private
provenance, one-use full-context completion, four retained observations, timeout,
caller drop, cancellation, rejection, context replacement, actual disposal,
late acknowledgements and process-lifetime terminal closure. Completion state is
read before retirement so a concurrent late ACK cannot revive a retired
observation; an explicit interleaving test rejects the old atomic read order. One test pauses the
first actual enqueue while a replacement handle attempts a second; both enqueue
and complete once in sequence order. Another drops all four timed-out HTTP-side
receipts, proves native file ownership has ended while publication capacity is
still exhausted, then proves context replacement cannot release unknown quota. Genuine late ACKs
release it; terminal closure permanently prevents re-admission. The
terminal test runs in a separate real child process so it cannot reset the
process-lifetime seal for another case.

The frontend helper is documented in [cover-bridge.md](cover-bridge.md).
No GUI, real native IPC, complete remote serving or performance gain is claimed
for this foundation.

Controlled negative variants remove synchronous enqueue serialization, release
unknown quota at ordinary context retirement, and reverse the atomic observation
read order. Each fails its corresponding regression; the restored implementation
passes. These controls do not change production code or skip cases.
