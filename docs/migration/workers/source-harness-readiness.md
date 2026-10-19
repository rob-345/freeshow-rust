# Source harness readiness

A full local source run exposed two fixture races while the application editor
and other tests ran concurrently. Neither correction changes a production
application handler or substitutes an expected source value.

The original API fixture returned after a fixed 20ms sleep. Its actual wildcard
observation contained only the REST listener; the subsequent WebSocket-port
request failed with ECONNREFUSED. The fixture now observes both real HTTP
servers' listen completion, with a two-second deadline and actual error/close
propagation. Temporary observation listeners and timers are removed on every
outcome. No request is retried. Wildcard port-pair preflight uses the actual
Node wildcard binding family, while ordinary fixtures remain loopback-bound.
The original API module and real Express/Socket.IO/OSC behavior stay intact.

The standalone browse CLI used try-only admission. A source corpus of nine
sequential GET_SIMILAR requests received `browse_busy` when completed replies
outran the actual workers' permit release. The production native host already
uses bounded queued admission. The CLI now uses that same entrypoint and keeps
its original deadline, capacity and supervised shutdown. Existing real held-owner
queue/cancellation tests and the complete source comparison corpus remain in
force; no expected result, resource limit or production owner is changed.

The corrected required source suite passes all 610 checks with four existing
platform/capability skips. The run requires the real private Opus/LTC source
oracles; an earlier direct invocation omitted that required environment and had
two failures/eight skips, so it is not accepted as a parity run. Its API refusal
log is preserved locally. The intervening required run's browse diagnostic was
observed in the session; its full log was overwritten by the corrected run and
is not claimed as an archived replay artifact. Strict all-target browse Clippy
also passes. The current passing full log and tool scripts are hashed in the
integration receipt.
