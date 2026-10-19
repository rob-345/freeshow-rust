# Windows browse stat / FILE_INFO foundation

This independent slice adds the private `stats/windows.rs` backend, its Windows
unit/native fixtures, and an immutable-source Node oracle. Shared stat dispatch,
engine, paths, manifests, IPC, and native host are unchanged. It is based on
`f63ecd6cc14f3d7b487bc6adfe1fa449848ee81a`. Local evidence is compilation/lint;
actual Windows behavior requires the native CI tests below.

The private API remains `stat(path, &mut Budget) -> Result<Option<Stat>>`.
Preparation runs synchronously on the existing fixed browse workers and adds no
thread, task, timer, subprocess, content read, descriptor transfer, grant, or
write. Each native API is followed by the existing cancel/operation/deadline
check; handle ownership remains RAII-local until the metadata result is built.
Cooperative budgets cannot preempt a blocking native call. Existing worker
admission and joined shutdown remain the service's responsibility.

Supported inputs are bounded, normalized absolute local drive-letter paths,
including ordinary `/` separators or normalized `\\?\C:\` spelling, and one
trailing separator. Raw caller spelling remains the output path. Relative,
UNC/device/pipe/ADS, dot/parent, ambiguous/reserved names, unresolved reparse,
offline/recall, non-fixed volumes, and uncertified identity/API variants return
`unsupported_stat_backend`. The open follows source stat's real symlinks and
junctions; this read-only metadata capability is not protected-root or media
descriptor authorization. Initial and resolved handle paths must identify fixed
local disks. Following a link can already enter an OS path before final-volume
validation; this is not a promise that the OS never contacts a link target.

Implementation uses maintained `windows-sys 0.61.2` bindings to documented
[CreateFileW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew),
[GetFileInformationByHandle](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfileinformationbyhandle),
extended basic/standard/ID queries, and
[GetFinalPathNameByHandleW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew).
Metadata opens request only read-attributes, share read/write/delete, and permit
directories. No reparse-open flag is used because genuine source stat follows
links. Resolved-path/API failures are explicit unsupported outcomes, not guessed
missing files.

The full volume/128-bit
[FILE_ID_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info)
is retained through identity certification before JS-number conversion. Leaf
IDs with nonzero upper 64 bits, mismatching legacy index/volume, unavailable ID
information, and inconsistent observations are unsupported. No truncated ReFS
identity is claimed. The ordinary Node Stats projection intentionally converts
the certified 64-bit inode to binary64, including rounding above `2^53`.
This metadata observation is not an immutable or transactional filesystem
snapshot.

The 14 fields retain source insertion order. Directory size is source zero;
mode uses file/directory type plus readonly `0444` or writable `0666`, without
invented executable bits. Link count and allocation size come from actual
[FILE_STANDARD_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_standard_info).
Blocks use actual allocation in 512-byte units. Device is the source 32-bit
volume serial, `uid/gid/rdev` are source zero, and source `blksize` is 4096. These
rules follow pinned Node22's
[libuv Windows stat assignment](https://raw.githubusercontent.com/nodejs/node/v22.21.1/deps/uv/src/win/fs.c)
and [ordinary Stats constructor](https://raw.githubusercontent.com/nodejs/node/v22.21.1/lib/internal/fs/utils.js).

All four native time fields must have true canonical seconds in `0..=u32::MAX`.
Within that interval, native 100-nanosecond fields project through the source
binary64 milliseconds expression, including the post-2038 interval. Pinned
[Node22 Windows time-field conversion](https://raw.githubusercontent.com/nodejs/node/v22.21.1/src/node_file-inl.h)
casts through 32-bit unsigned long; pre-epoch and beyond-u32 behavior is explicitly
unsupported here. Tests record actual Node exceptional results without deriving
guessed wrapping from native seconds. Dates/prototype methods are absent at the
source structured-clone boundary; no Date hydration is added.

Only actual open `FILE_NOT_FOUND`/`PATH_NOT_FOUND`, including a regular-file
ancestor or dangling link, become source null. Empty string also follows the
actual source failure. Other open/metadata failures remain explicit unsupported:
Node can use a newer by-name stat or directory fallback even when this metadata
handle cannot open. In particular an actual share lock is not fabricated as
missing. This is a documented capability gap for denied/sharing/filter cases.

## Coordinator wiring

The exact suggested patch is separately available at
`/tmp/freeshow-browse-windows-check/parent-wiring.patch`; it is not applied to
shared files. It adds a Windows child and private stat wrapper, narrows the old
unsupported stat cfg, and adds target-Windows `windows-sys` features
`Win32_Foundation`, `Win32_Security`, `Win32_Storage_FileSystem`, and
`Win32_System_WindowsProgramming`.

Permit only Windows `Request::FileInfo` in the engine platform gate. Leave
`READ_FOLDER` and `GET_SIMILAR` explicitly unsupported: their shared POSIX
join/parent/basename/ordering semantics are outside this slice. FILE_INFO must
split both native separators before existing basename-extension logic; otherwise
`C:\parent.with.dot\leaf` incorrectly picks a dot in its parent. The supplied
patch applies that narrow split only to FILE_INFO's already-certified paths; it
does not claim a general Node-win32 path implementation. The native source test
includes this regression. Merge the independent macOS cfg without enabling
Windows folder/similarity operations.

## Evidence

The temporary standalone harness copies the actual crate at the base commit,
imports these actual new modules, and applies only the suggested parent wiring.
No root Cargo files or builds are touched. Passed locally:

```sh
CARGO_TARGET_DIR=/mnt/data/cache/freeshow-cover-native-portability \
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 \
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
cargo +1.99.1 clippy \
  --manifest-path /tmp/freeshow-browse-windows-check/Cargo.toml \
  --offline --all-targets --target x86_64-pc-windows-gnu -- -D warnings
```

Rustfmt, Prettier, and whitespace checks pass. The oracle helper was executed
with Node22.21.1 on Linux against one real ordinary file, one nonexistent file,
and empty string; immutable-source hash/extraction, real clone keys/types,
binary64 receipts, and source error capture worked. This helper smoke is not
Windows stat evidence.

After integration, run on genuine Windows with installed pinned Node22.21.1,
TypeScript/node_modules, and the upstream git object:

```sh
cargo test -p freeshow-browse --lib stats::windows::tests -- --nocapture
```

Ten native tests cover ordinary/empty files, directories, Unicode, hardlinks,
allocation, readonly modes, raw path variants, fractional/post-2038/u32-boundary
times, deliberately gated exceptional times, actual missing/non-directory
errors, junction following, sharing failure, full identity/53-bit rounding,
cancel/budgets, actual worker FILE_INFO/rejected other commands, and joined
service shutdown. Only file-symlink fixtures may narrowly skip actual Windows
privilege error1314; mandatory junction tests still execute.

Each source comparison executes the actual immutable FreeShow `getFileStats` and
`getFileInfo` in one realm with actual native `fs`, then observes real
`structuredClone`. The helper checks source SHA256
`98e563c55026d2dbbdd217ee9b94962ed7fc8b82d893795355bd7dc0f54fc5de`
against the pinned git object. Its bounded receipts include exact Number bits,
not a duplicated expected-stat algorithm or a tolerance comparison. Child
fixtures are sequential per invocation, deadline-bound, and killed/reaped on
failure; pipe buffering cannot deadlock the test because receipts use private
files. The production backend creates no child. No native Windows runtime,
full browse, GUI, or application parity is claimed before actual CI execution.
