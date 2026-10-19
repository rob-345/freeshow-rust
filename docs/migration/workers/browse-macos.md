# Darwin browse stat backend

This independent slice adds `crates/freeshow-browse/src/stats/macos.rs`, six
macOS integration fixtures, two private module tests, and a pinned source oracle
with two portable Node tests. It changes no shared crate file, manifest, host or
frontend. Base: `f63ecd6c`; branch/worktree: `agent/browse-macos` /
`freeshow-worktrees/browse-macos`. Earlier worker branches remain intact.

## Production seam

The new child exposes the existing crate-private
`stat(&str, &mut Budget) -> Result<Option<Stat>>`. It calls the maintained
`libc::stat` Darwin binding, following symlinks exactly as the original
`fs.statSync` / `fs.stat` helpers do. It creates no thread, task, descriptor,
renderer authority, fallback metadata or Node runtime dependency. The existing
two filesystem workers, admission, output/entry/depth/path limits, cancellation
and shutdown owner remain responsible for the operation.

Each call checks the existing budget before/after the syscall and between
interrupted-call retries. CString allocation is bounded by the existing path
limit. Actual ordinary stat failures return `None`, matching the original
null/skip behavior; `ENOSYS` is an explicit `unsupported_stat_backend`, never a
successful empty stat. Syscalls cannot be preempted while blocked. There is no
new shutdown/join policy or claim of syscall deadlines.

The projection preserves exactly the 14 numeric own Stats properties in source
order. It does not add Date values or methods. Darwin's true birthtime and ctime
are distinct fields. Raw signed seconds/nanoseconds are converted separately
to binary64, then multiplied/divided/added in the actual Node order; negative
Darwin subsecond representations are not normalized. Ordinary fields first
follow libuv's assignment to `uint64_t`, including wrapping of signed native
dev/size/blksize/blocks, then binary64 rounding. Values beyond 2^53 are never
advertised as exact integers.

Source evidence: immutable FreeShow `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`
`files.ts` SHA256 `98e563c55026d2dbbdd217ee9b94962ed7fc8b82d893795355bd7dc0f54fc5de`;
actual Node 22.21.1 extracted `getStatsFromBinding`, `msFromTimeSpec`, and their
constants SHA256 `56779a34e11fa086c3a0f6d9719962542c6b4e654350de81f1ac0b96df5e4a94`.
Both checks normalize CRLF. Primary native references:
[libuv stat field widths](https://github.com/libuv/libuv/blob/v1.51.0/include/uv.h#L366),
[Apple field copies and syscall path](https://github.com/libuv/libuv/blob/v1.51.0/src/unix/fs.c#L1348),
[Node scalar conversion](https://github.com/nodejs/node/blob/v22.21.1/src/node_file-inl.h#L78),
[Node Stats construction](https://github.com/nodejs/node/blob/v22.21.1/lib/internal/fs/utils.js).

## Coordinator-owned wiring

The exact proposed patch is retained separately at
`/tmp/freeshow-browse-macos-check/parent-wiring.patch`; it is not a committed
shared-file edit. Its three changes are:

1. In `stats.rs`, add the `#[cfg(target_os = "macos")]` child at
   `stats/macos.rs` and `pub(crate) use macos::stat`; exclude macOS from the
   existing unsupported fallback. Leave Linux statx unchanged.
2. In `engine.rs`, permit the macOS POSIX command paths alongside Linux.
   Preserve a Windows sibling's FILE_INFO-only restriction when merging both
   platform patches; do not broadly enable Windows folder/similarity paths.
3. Widen only the libc dependency target to
   `cfg(any(target_os = "linux", target_os = "macos"))`. No new package/version.

Update the old unsupported-platform diagnostic wording coherently. No new Date
hydration is required: existing `hydrateFileBrowse` remains responsible only for
its precise `noMedia` own-undefined placeholder. Thumbnail options/intents and
capture scheduling are unchanged; these tests never substitute generated images.
The shared `file-browse.test.mjs` final `if (!linux)` assertion also needs its
platform guard narrowed when integrating the newly supported contracts.

## Tests and limits of evidence

The six native fixtures compare actual public BrowseService results with the
immutable source functions, actual pinned Node filesystem results, and V8's
numeric-own-property Stats projection. They cover files/directories/hardlinks,
a real sparse 4GiB+17-byte file (bounded physical allocation), symlink following,
dangling/cyclic links, FIFO/device stat, missing/ENOTDIR, Unicode POSIX listing,
duplicate roots, fractional depth/capture/noMedia shapes, and similarity order.
Ordinary failed stat is compared to actual source null; malformed/resource and
cancellation errors remain separate explicit native failures.

Real signed/fractional times are asserted to survive in native integer time
before source comparison. The `1700000000sec +12348ns` fixture exposes the actual
rounding error caused by premature integer-nanosecond aggregation. Permission
tests require non-root and fail under root; there is no silent permission skip.
The existing actual `BrowseService.shutdown` is awaited and retried in the
lifecycle fixture; this is no new UI/context ownership proof.

Two module tests cover resource/cancel/NUL behavior and synthetic native field
projection. The latter invokes the **actual extracted Node conversion**, rather
than a duplicated timestamp formula. It covers unsigned wrapping, >2^53 values,
signed unusual subseconds, independent ctime/birthtime, and time extremes. It is
pure arithmetic evidence, not an observation of an enormous real file/time.

Both `--project` and `--requests` capture each actual Node Number's IEEE-754
binary64 bits as 16-digit big-endian hex before JSON serialization. The bounded
per-result `numberBits` object uses escaped JSON pointers. Rust comparisons use
source JSON numbers for type/shape only and compare native bits directly to
these strings; they never trust decimal numbers re-parsed by serde_json as the
numeric oracle. Every bit entry must be consumed, and every native numeric
field requires one. The existing synthetic projection test corrupts one IEEE
bit and requires rejection with the source JSON unchanged. Portable controls
verify emitted bits for actual Node arithmetic and real source filesystem
fields, JSON transport preservation, and pointer escaping. Test counts remain
six native integration fixtures, two module tests, and two portable Node tests.
An additional worker-private Linux harness executed this same Rust comparison
helper against actual Node projection receipts: changed decimal data with intact
bits passes, while a one-bit change and missing/extra bit entries are rejected.
That control verifies the comparison machinery only, with no Darwin filesystem
claim or additional committed test count.

The portable Node suite validates this exact source extraction and executes the
actual immutable POSIX handlers on a bounded local tree. Its local filesystem
platform is reported without a Darwin/native-parity label. The native oracle's
`--requests` mode requires Darwin and Node 22.21.1. Test child ownership is
sequential, with a ten-second observer deadline and actual kill/reap on failure;
regular private stdio files avoid pipe backpressure, with bounded input/output.
No product child or per-request worker is introduced.

Coordinator review removed an unnecessary real-filesystem assertion that
backdated modification and creation times must differ. Apple's
[HFS implementation](https://raw.githubusercontent.com/apple-oss-distributions/hfs/main/core/hfs_vnops.c)
can move creation time back with modification time; APFS behavior was not
reproduced locally. Real fixtures compare the actual source birthtime bits,
while the synthetic projection retains deliberately distinct field values.
The backend does not substitute one timestamp for another or require their
inequality.

Actual [macOS core CI at d0e88f3b](https://github.com/rob-345/freeshow-rust/actions/runs/39251699143/job/118056813501)
passes all six native fixtures and both private module tests, including real
permission failures, sparse metadata, signed/fractional timestamps, symlinks,
folder/similarity shapes and exact source IEEE bits. The native desktop job
stopped earlier at a stale host assertion expecting all non-Linux browsing to
be Unsupported; its actual reply was the correct FILE_INFO Null/context/listener.
That assertion is corrected separately. Local strict Apple ARM all-target
Clippy also verifies the actual new module/tests with shared wiring.
Required test environment: Node 22.21.1, project npm dependencies, ordinary
non-root user, local filesystem with exact fixture timestamps and sparse-file
support. No official Electron executable is required for these tests.

```sh
FREESHOW_BROWSE_MACOS_ORACLE_ROOT=/absolute/repository \
FREESHOW_BROWSE_MACOS_NODE=/absolute/node22.21.1 \
cargo test -p freeshow-browse

node --test tests/parity/browse-macos-source.test.mjs
```

Local private harness: `/tmp/freeshow-browse-macos-check/Cargo.toml`, with
`CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0`, debug0 and the existing worker-private
external target. No root Cargo or GUI run. The two portable Node suites pass.
Complete native host verification, GUI/IPC, packaged macOS Electron transport, TCC/sandbox,
network/unusual filesystems, non-Unicode names and races during external mutation
remain outside this runtime evidence. Async original Map insertion order is
deliberately excluded; exact array order, nested values and stat field order are
retained. There is no whole-platform parity or performance claim.
