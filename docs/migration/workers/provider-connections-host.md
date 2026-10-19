# Native stored provider connection queries

`MAIN.PROVIDER_CONNECTIONS` now runs the separately source-compared projection
against the actual global app-data `ACCESS` file on the existing ordered
`LibraryService` owner. The renderer supplies neither its path nor the registry
or scopes. Only the main window is admitted. Full context and original listener
correlation are checked before admission, during ordered execution and before
publication. The ignored source payload is discarded before queueing.

Four shared leases bound accepted queries and observers together. Dropping or
timing out an observer cancels its control but retains the accepted closure and
its capacity until the real owner returns. The observer deadline is two seconds.
CLOSE synchronously seals admission and cancels existing controls; the later
existing `finish_writes` fence drains accepted ordered work. This host adds no
worker, thread, process, OAuth listener, provider initialization or idle timer.
That fence does not prove a native library-owner thread join or renderer ACK.

The reader opens the actual trusted ACCESS path once, accepts regular files,
reads in 32KiB chunks and caps encoded bytes at 1MiB before parsing. Unix open
is nonblocking so a FIFO cannot hang at open. A conservative 32,768 punctuation
cap bounds structural allocation; punctuation inside strings also counts. The
projection independently applies its complete-snapshot compact-byte, node and
depth budgets, including unused secrets. Filesystem calls and parsing are not
forcibly preemptible. Resource, read, parse, unsupported, stale, cancellation,
shutdown and admission errors remain explicit, with static diagnostics that
contain no path, credentials or store contents. A genuine missing file is a
successful empty object, matching the actual Conf missing-file getter; an actual
throwing read/parse never becomes a fabricated empty success. No query writes.

`commit_without_media_refresh` avoids an additional unbounded CONFIG reread
and leaves the committed media-folder snapshot intact. Provider queries retain
the same context checks and ordering/fence as ordinary native commits.

Six real host tests pass locally: exact source key order/truthiness, secret
omission and unchanged bytes, conflicting selected-library ACCESS isolation and
library switching, live file transitions and actual missing file, role/stale/
parse/nonobject/nonregular failures, whole-file/node/depth resource bounds,
actual Unix permission denial, four accepted jobs retained through observer
abort/seal/drain, and the actual two-second observer deadline retaining queued
work. Strict all-target native Clippy passes. The original eight Node suites
and five Rust projection tests remain independent source evidence; combined
workspace/GUI/foreign-runtime verification is a subsequent integration gate.

This is a stored indication, not authentication or provider availability.
Cloud-provider workflows, token refresh, backend calls and service imports
remain incomplete. Raw nonobject Conf roots normalize into plain objects in the
source; existing native Stores rejects them at startup, and this query returns
an explicit invalid-snapshot error for a later nonobject external replacement.
Concurrent external file changes between the original helper's repeated reads
are not reproduced by one stable ordered snapshot. No full storage or provider
parity is claimed. See [the foundation](provider-connections.md) for exact pinned
source hashes, scopes, initialization observations and remaining boundaries.
