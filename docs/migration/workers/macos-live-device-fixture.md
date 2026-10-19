# Darwin shared-device source fixture

Published `07c89581` CI run `39274111800` first lost both macOS runners before
any steps. Its second attempt reached the actual Darwin tests and exposed a
fixture race: `/dev/null` changed mtime between the source and native reads.
The source observed `1792423746446.884` ms; Rust observed
`1792423747450.698` ms. The failure occurred in
`symlinks_follow_real_targets_and_missing_loops_fifo_and_devices_match_source_stat`.
This is not recorded as a passing CI run.

Private file, symlink, FIFO, directory and controlled timestamp comparisons stay
exact, including original captured binary64 bits. Only the shared `/dev/null`
observation moves outside the batch: actual pinned source before, actual native
query, then actual pinned source after. An actually changed device timestamp
must be finite and lie within its two source observations. An unchanged timestamp
still requires exact original bits. Every other property, including stable
device/inode identity, remains exact across both source observations and Rust.
There is no generic floating-point tolerance, fixture retry or production change.

Source JSON numeric text remains type/shape evidence. Source endpoint and stable
comparison values come from their separately captured binary64 bits, avoiding a
second decimal parse becoming an expected source value. Changed fields must
exist and have numeric type in both source receipts before being removed from
the remaining exact comparison.

Three portable Rust tests exercise the comparator. The old exact-only helper
rejects a valid intermediate observation; the bracket accepts it and rejects
values outside either endpoint. Stable one-ULP changes, altered size, changed
kind, same-kind inode replacement, missing source bits, missing timestamp fields
and string-valued source timestamps all fail. A separate control preserves the
captured source bits when transported numeric text parses differently.

Focused Linux execution and strict Linux/Darwin cross-Clippy are local checks.
The corrected actual Darwin filesystem test must run in the next CI milestone;
cross-compilation does not supply that runtime evidence.
