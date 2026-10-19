# Native browse admission

The retained application can request audio/media folder counts and the visible
folder at the same time. Its original two live host slots rejected ordinary
startup requests with `browse_busy`; the UI could remain blank until navigation.
The actual full-app audio receipt preserves that earlier failure.

NativeBrowse now admits at most ten live host futures and uses the service's
new queued dispatch and prepared-listing entrypoints. The service retains its
two actual filesystem owners and separately bounds eight waiting admissions.
The existing configured deadline includes admission wait and filesystem work.
No retries, extra native threads or library-compression dependency are added.
Existing original thumbnail preparation, public replies and frozen context
checks remain in place.

The host lease covers its future and prepared thumbnail work. Dropping that
future releases the host lease and cancels its token; it does not terminate an
entered filesystem operation. The independent service execution permit remains
owned until actual return/result disposal. The bounds are ten live host futures,
at most eight service waiters, and two actual filesystem operations, including
operations whose original host future has disappeared. They are not a claim
that renderer delivery has been acknowledged or that all results stay within
one end-to-end operation lease.

Three new native integration tests use the actual feature-gated worker entry
boundary. They witness both physical owners held and the exact retained waiting
count before asserting behavior. Ten requests retain each original callback,
context and listener; the eleventh receives the explicit capacity error. Actual
library selection cancels old queued/public observations while a fresh context
still waits for the held old owners. Queued caller abort recovers capacity;
shutdown cancels observers but remains pending through held real filesystem
owners, including a dropped shutdown observer, before the eventual actual joins.
All five native browse tests pass on Linux, including the two existing tests.

Strict native all-target Clippy and the normal custom-protocol binary build
pass. The new dev dependency enables `freeshow-browse/test-harness` only for
test targets. Actual compiler-artifact output from the ordinary build records
the browse crate features as `[]`; worker gates are absent. The private compiler
cache is an independent by-value copy of an idle functional cache, with the
desktop package explicitly cleaned before rebuilding normal frontend assets.
No performance samples use this cache or binary.

The complete retained UI startup still needs a dedicated no-renavigation run.
Windows/macOS execution, full integration CI, broad resource saturation and
performance remain unverified. Existing explicit unsupported path/format/volume
and thumbnail errors remain unchanged.

[Service queue contracts and tests](browse-queued-admission.md).
