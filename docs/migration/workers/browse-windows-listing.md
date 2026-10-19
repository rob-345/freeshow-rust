# Bounded Windows READ_FOLDER and GET_SIMILAR engine

This slice adds only `src/engine/windows.rs`, `windows_scandir.rs`,
`windows_tests.rs`, the new immutable-source listing oracle and this note. It is
based on `ef7fadfb`, with private cherry-picks of sealed stat
`ddf6ff48`/`6a070b32` and lexical `cdf7cf6b`/`b11ce847`. No shared dispatcher,
manifest, lockfile, native runtime, IPC or canonical parity file is changed.
Windows runtime evidence remains pending native CI; local cross-checks do not
certify an OS run.

## Capability and API

The coordinator can route Windows `Request::Folder` and `Request::Similar` to
`engine::windows::process(request, &mut Budget) -> Result<BrowseReply>`. The
module preserves the existing typed thumbnail intents and `noMedia:null`
own-undefined wire placeholder. It does not generate a thumbnail, serve bytes,
authorize a media path, create a filesystem entry or emit source success.
`FILE_INFO` continues through the separate stat backend; this engine deliberately
rejects it rather than replacing that foundation's wider verbatim-path subset.

The certified namespace is ordinary absolute drive-letter paths with paired
Unicode and either native separator, normalized nonreserved components, and at
most one trailing separator. Raw root spelling and original-similarity identity
remain untouched. Child paths use the sealed fallible Node-win32 join; it
normalizes their separators independently. Relative/current-drive, UNC/device,
verbatim, ADS, dot/parent, repeated separator, trailing-dot/space, reserved-name,
offline/recall and uncertified stat/identity/date cases where metadata is
required, and nonfixed-volume cases remain explicit unsupported outcomes. Discovered unpaired UTF-16 names are rejected,
not lossy replacements that could name a different file. No Unicode
normalization or case folding is added to path identity or stems. The existing
parsed-request contract is unchanged: bounded string roots, numeric/null depth
and boolean/null flags; broader source JavaScript coercions are not certified.

The existing Windows stat path/capability gate is reused before each listing.
The actual enumeration handle is independently checked as a disk object with a
resolved DOS drive path and, more strongly, its actual volume GUID root classified
as fixed. Both use the same live handle, before enumeration. The GUID check does
not assume the first DOS drive letter identifies a mounted folder's underlying
volume. The stat foundation's older DOS-prefix-only volume check is a scope
caveat for separate coordinator followup; it is unchanged here. This listing
engine retains the stronger actual-volume check.

Source stat follows real junctions. The backend therefore follows an ordinary
fixed-local junction too, with explicit post-open volume checks. Opening a path
can already contact a link target; this is not a protected-root confinement,
no-network-contact or immutable-snapshot promise. Actual volume-GUID lookup
failure is unsupported. A concurrent rename or write can change later path
stats. The held directory is enumerated while returned children remain source
path joins; they do not become descriptor-backed grants.

## Exact source and native enumeration

Pinned source is commit `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`.
[Node22/libuv Windows fs\_\_scandir](https://raw.githubusercontent.com/nodejs/node/v22.21.1/deps/uv/src/win/fs.c)
opens a synchronous directory for list/synchronize access, sharing read/write/
delete. It queries `FileDirectoryInformation` in an aligned 8 KiB buffer and
retains returned native record order. There is no Windows qsort/strcmp step.
[Node's ReadDir](https://raw.githubusercontent.com/nodejs/node/v22.21.1/src/node_file.cc)
and [uv_fs_scandir_next](https://raw.githubusercontent.com/nodejs/node/v22.21.1/deps/uv/src/uv-common.c)
forward that order. The Linux byte-sort is deliberately not reused.

The new implementation uses maintained `windows-sys 0.61.2` bindings to
[NtQueryDirectoryFile](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntquerydirectoryfile)
and the exact public
[FILE_DIRECTORY_INFORMATION](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_directory_information).
Offsets come from `offset_of!`, not a copied private record layout. Each header,
name byte count, even UTF-16 length, forward/aligned next offset and record span
is checked before slices or copies. Terminal NUL trimming and dot-entry removal
follow libuv. A fixed 255-unit stack name buffer rejects wider certified
components. The native call receives initialized, 8-byte-aligned 8192-byte
storage and a local `IO_STATUS_BLOCK`.

`CreateFileW` does not use `FILE_FLAG_OVERLAPPED`; its synchronous-handle contract
keeps actual query completion ahead of stack-buffer/handle disposal. Only
`STATUS_SUCCESS` decodes records, and `STATUS_NO_MORE_FILES` ends enumeration.
Unobserved informational statuses, buffer overflow/empty-success, malformed
records and projection/capability failures are explicit unsupported outcomes.
Known actual access-denied/sharing/deleted/missing/non-directory OS failures
become the source's empty list; unknown statuses do not. Partial names are
never returned after an enumeration failure. Existing stat failures retain the
stat foundation's explicit boundary rather than guessing Node fallback success.

Actual-handle final path and volume checks follow
[GetFinalPathNameByHandleW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew),
[GetDriveTypeW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getdrivetypew)
and [volume GUID naming](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-volume).
No user-provided drive/device handle or dynamic-library choice is accepted.

Folder traversal retains the source depth ternary, capture's depth-two cutoff,
first four media selections in native files order, noMedia shape, root-media
intent and complete source Stats values/order. Similarity uses the shared
bounded UTF-16 Levenshtein helper, native candidate order, case-sensitive exact
source path exclusion, includes-based stem deduplication and stable score ties.
Its source allows directories as candidates. ASCII media extensions in the
source corpus use the retained tables; no general new Unicode case-table parity
claim is made. The original async stat/recurse and thumbnail completions may
reorder the outer map or callback order, especially overlapping roots. This
synchronous bounded worker preserves complete values, each files array and
selected intent set, without claiming every concurrent source schedule.

## Ownership and limits

No thread/task/timer/child is added in production. Execution reuses the existing
fixed `BrowseService` workers, queue, admission and cancellation. Default bounds
remain 2 workers, 64 roots, 10,000 total directory entries, requested depth16,
32,768 path bytes, 16 MiB accumulated path/serialized-result bytes, 100,000
operations, 10 million similarity cells/substrings and 10 seconds. Existing
trusted configuration maxima remain unchanged. Native directory scratch is
8 KiB plus a reused 64 KiB final-path buffer and 510-byte name buffer; names and
joined paths are charged before retention. Finite native component decoding is
also bounded by aggregate entry count. Nothing grows to the whole OS directory
before the entry budget is enforced.

Checks surround real OS calls and record iteration. Cancellation/deadlines are
cooperative and cannot preempt a stuck OS/driver call. RAII retains the real
handle until that call/worker returns. An observer timeout or dropped request is
not a native cancellation fence or joined shutdown receipt.

The inherited service releases its worker permit before sending a completed
reply (`service.rs`); already-ready/collected replies are not whole-result
leased by that semaphore. The native host must retain its own bounded result,
thumbnail/effect and publication owners. This slice neither changes that shared
contract nor claims result-disposal proves OS cancellation.

## Coordinator wiring

Declare `#[cfg(windows)] mod windows;` in shared `engine.rs`. At the beginning of
`process`, before the platform gate:

```rust
#[cfg(windows)]
if matches!(&request, Request::Folder { .. } | Request::Similar(_)) {
    return windows::process(request, budget);
}
```

Retain the earlier Windows FILE_INFO-only gate and its narrow separator split.
Declare `#[cfg(windows)] pub(crate) mod windows;` after the shared `paths.rs`
module doc. Preserve the Windows stat child/wrapper and unsupported fallback cfg
from the sealed stat foundation. Add `Wdk_Storage_FileSystem` and
`Win32_System_IO` to that foundation's target-Windows `windows-sys` features
(`Win32_Foundation`, `Win32_Security`, `Win32_Storage_FileSystem`,
`Win32_System_WindowsProgramming`). No lock/package version change is required.

A suggested shared-file wiring diff is at
`/tmp/freeshow-browse-windows-listing-parent.patch`; shared files in this worktree
remain untouched. Integrate alongside the independent macOS cfg rather than
expanding the POSIX helper globally. The historical stat worker test explicitly
expects both other commands to return `unsupported_platform`; update that
assertion during coordinator dispatch integration. Its verbatim fixture remains
outside this new ordinary-drive listing subset, so substituting a success
assertion there would be incorrect. The new engine tests below use explicit
ordinary-drive fixture spellings and require actual success.

## Verification and pending evidence

Private temporary parent wiring imports these actual owned files, the exact
sealed stat/lexical additions and existing crate code. Passed locally:

```sh
CARGO_TARGET_DIR=/home/rob-admin/GitHub/freeshow-worktrees/native-menu/target/assets \
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 \
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
cargo +1.99.1 clippy \
  --manifest-path /tmp/freeshow-browse-windows-listing-check/Cargo.toml \
  --offline --all-targets --target x86_64-pc-windows-gnu -- -D warnings

/home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node \
  tests/parity/lib/browse-windows-listing-oracle.mjs --portable-source-control
```

The portable source control executes actual functions for empty roots/similarity,
verifies exact Node22.21.1/libuv1.51, normalizes both source transports CRLF→LF,
checks fixed pinned SHAs and proves a genuine non-line-ending mutation fails.
It is source-loader/control evidence, not Windows IO evidence. Rustfmt,
Prettier and whitespace checks pass. No Windows executable was linked/run on
Linux, no Wine substitution, GUI, performance or hardware claim is made.

After integration, execute on actual Windows with pinned Node22.21.1,
node_modules and the immutable git object:

```sh
cargo test -p freeshow-browse --lib engine::windows -- --nocapture
```

Thirteen new Rust test bodies compile: eleven genuine Windows fixtures and two
checked native-record/capability controls. They include 18 actual-source
operations: a 186-entry multi-buffer Unicode/order tree, complete recursive
Stats and Number bits, raw/mixed/trailing caller spelling, fractional depths,
capture/noMedia/first-four intents, stable similarity ties/directory candidates/
raw identity, real missing and locked-directory empty results, and a mandatory
fixed-local junction. Separate fixtures cover unpaired UTF-16 rejection,
namespace/capability rejection, entry/byte/math/deadline/cancellation bounds,
actual open-directory replacement, and accepted handle ownership across caller
Drop, shutdown-observer timeout and genuine retry/join. Test hooks are private,
one-shot and used only after an actual opened/verified directory handle.

The helper extracts the actual immutable functions and media declarations,
uses real native `fs`/`path`, and records complete outputs, own-undefined keys,
Stats insertion order/Number bits and input mutation. Only thumbnail generation
is controlled and labeled, returning unresolved empty slots. Source child
receipts use private files, 4 MiB output/256 KiB input limits and a 15-second
kill/reap guard; no pipe-buffer deadlock or detached child is accepted. Native
filesystem/ABI/order/sharing/GUID behavior, actual source concurrency races,
non-NTFS/remote/removable volumes, application dispatch and GUI remain uncertified
until genuine platform integration tests run.
