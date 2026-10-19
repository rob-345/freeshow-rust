# Windows fixed-cover backend

This slice adds only the private Windows backend, its dedicated Windows test
target, and this handoff. It does not install a route or publish an ALERT. The
parent API remains `CoverFiles::submit(&LibraryPhase, Control)` and opaque
`HeldCover`/`Observation` revalidation. Local evidence is Windows GNU compilation
and strict lint, not execution on Windows or verified Windows HTTP parity.

## Native authority and limits

The only leaf is `Audio/NowPlayingCover.png` below the native full-context data
root. There is no request-selected path. The existing ordered library IO owner
performs preparation and revalidation. The backend adds no worker, task, thread,
timer, service, handle clone, or public descriptor extraction. The existing four
process-wide whole-operation leases remain held by actual work and its returned
observations, descriptors, and ready validation results. Cancel, ticket Drop,
phase completion, and an observer timeout do not prove descriptor disposal or
OS-work completion. The surrounding host must retain and drain its actual IO and
HTTP owners.

Root opens support absolute drive-letter and verbatim drive-letter paths on
fixed local drives. The root is bounded to 16,384 UTF-16 code units, 128 normal
components, and 255 code units per component. UNC, device/pipe namespaces,
drive-relative paths, parent traversal, ADS, reserved device components, trailing
dot/space names, and ambiguous components are explicitly unsupported. Unicode
components are retained as native UTF-16. Reparse, device, offline, and recall
attributes are rejected from the actual opened handle. No canonicalization or
fallback open follows a rejected object.

The drive is opened once per walk. Every later open supplies one validated
component relative to a retained directory handle using the maintained
`windows-sys 0.61.2` bindings. The user-mode API is linked from `ntdll.dll`.
`NtCreateFile` documents relative `RootDirectory` opens and
`FILE_OPEN_REPARSE_POINT`; the latter opens the point itself rather than following
its target. The same returned handle is inspected before use. Synchronous opens
include `SYNCHRONIZE`. See Microsoft's
[NtCreateFile contract](https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntcreatefile).

Root and Audio comparisons retain the full volume serial and 128-bit file ID.
The current shared `FileStat` has a 64-bit inode field, so a leaf whose upper
64 file-ID bits are nonzero is explicitly unsupported. It is not truncated and
does not silently fall back to the older 64-bit identifier. Required identity
information unavailable on a volume is also unsupported. Microsoft's
[FILE_ID_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info)
defines the full identity pair; its
[older file-information documentation](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/ns-fileapi-by_handle_file_information)
specifically warns about weaker ReFS 64-bit identifiers. No ReFS support is
claimed by this slice.

The leaf is limited to 4 MiB. Existing parent checks cap each offset read at
32 KiB and the observed file length. Windows `FileExt::seek_read` uses the
explicit offset but also changes the current cursor; the sealed HTTP worker
must continue sequential use of this held file, with metadata-only validators.
No public clone is introduced. See the
[Rust Windows FileExt contract](https://doc.rust-lang.org/std/os/windows/fs/trait.FileExt.html).

Preparation records an actual attempted Audio mkdir failure before continuing.
A regular-file Audio blocker remains a typed folder failure followed by an
observed missing guard. A raced, already-created actual directory follows the
existing source-derived handling. Missing is permitted only for known namespace
statuses (`OBJECT_NAME_NOT_FOUND`, `OBJECT_PATH_NOT_FOUND`, `NO_SUCH_FILE`,
`NOT_A_DIRECTORY`). A denied or share-locked metadata-handle open is an explicit
IO failure: it does not prove that Node's by-name exists guard was false. This is
a conservative capability difference for ACL, sharing, and filter cases, not
complete emulation of all Node `existsSync` error outcomes.

`Failure.os_code` contains the real Win32 code. For a failed `NtCreateFile`, the
bounded diagnostic also records the original hexadecimal NTSTATUS. Conversion
uses [RtlNtStatusToDosError](https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-rtlntstatustodoserror),
including its explicit unmapped-status result; it is not presented as a lossless
inverse. Failed later validation preserves an already-recorded folder failure.

Version guards use length and true signed native LastWriteTime/ChangeTime,
converted from 100-nanosecond ticks since 1601 into canonical signed POSIX
seconds/nanoseconds. [FILE_BASIC_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_basic_info)
distinguishes data modification from metadata change. Revalidation compares the
held root, current root, Audio identity, held leaf version, and current namespace
leaf version. Reads retain the original file after namespace replacement.
These observations do not make the file an immutable snapshot. External parent
renames/drive remapping, identity reuse, delayed/restored timestamps, and writes
during body reads are not an OS raceproof guarantee. Native synchronous calls
cannot be forcibly preempted by a cooperative deadline.

## Required source/header timestamp gate

Do not directly enable the current POSIX header projection for every Windows
timestamp. Pinned Node 22.21.1
[node_file-inl.h, lines 85–95](https://raw.githubusercontent.com/nodejs/node/v22.21.1/src/node_file-inl.h)
selects this Windows time-field operation:

```cpp
SET_FIELD_WITH_STAT(stat_offset, static_cast<unsigned long>(stat))
```

For the first production integration, the coordinator will gate the Source
header projection to true native seconds `0..=u32::MAX` with canonical
nanoseconds `0..1_000_000_000`. Exceptional pre-epoch or post-2106 values remain
accurate raw guard fields in this backend, while HTTP header projection is
explicitly unsupported. No guessed generic wrapping of these native values is
implemented here. The exact FILETIME-to-libuv exceptional arithmetic needs
separate verification before widening that interval.

The dedicated Windows target invokes actual pinned Node `fs.statSync` and the
installed source `etag` package against the same real file. It compares ETag for
ordinary time, fractional values below/above the rounding boundary, and the
upper supported seconds boundary. For pre-epoch and beyond-u32 dates it records
the real Node result and signed native time without constructing POSIX headers
or asserting guessed overflow behavior. Node must be 22.21.1. The default oracle
root is the repository above `CARGO_MANIFEST_DIR`; temporary harnesses may set
`FREESHOW_COVER_WINDOWS_ORACLE_ROOT` and `FREESHOW_COVER_WINDOWS_NODE`.

## Coordinator integration

The commit intentionally changes no parent file or manifest. The minimal merge
with the separate macOS backend is:

1. Declare this Windows child module; expose existing `Authority.root` and
   `Authority.audio` on Windows as well as the supported Unix platforms.
2. Dispatch Windows preparation to `windows::prepare(&mut observation, lease,
&control)`, validation to `windows::revalidate`, and held-file offset reads to
   `windows::read_at`. Retain all parent admission, chunk/length, full-context,
   and ready-validation guards. The mutable Observation binding must include
   Windows. Linux's fourth test-hook argument stays Linux-test-only.
3. Narrow the unsupported dispatch branches and platform-only dead-code
   annotations coherently; do not add a blanket `allow(dead_code)`. Windows has
   its own private test pause slot and `CoverFiles::test_pause_windows_*`
   methods, so it needs no parent pause-type export.
4. Add the target-Windows `windows-sys = "0.61.2"` features
   `Wdk_Foundation`, `Wdk_Storage_FileSystem`, `Win32_Foundation`,
   `Win32_Security`, `Win32_Storage_FileSystem`, `Win32_System_IO`, and
   `Win32_System_WindowsProgramming`. Retain the coordinator's other required
   Windows features.
5. Run `now_playing_cover_windows` as a genuine Windows native test target.
   The old `now_playing_cover_file` unavailable-on-non-Linux test must exclude
   newly implemented Windows/macOS. Gating that function alone leaves its old
   fixtures unused under strict lint, so gate the old target/helpers coherently.
   Likewise update `remote_cover`'s platform-capability test rather than still
   expecting Unsupported on every non-Linux OS.
6. Implement the Source/header gate above before enabling production Windows
   cover responses. Keep the raw backend identity/version observations intact.

## Evidence and pending native execution

The worktree base is `b2347f857d19698fffc457409ba99ec60162a50c`. The final
temporary compile harness reads actual root parent/Source/publication modules
from root `8516cbe45d00b36330254ce1b87baec717f5a28c`, makes only the proposed
Windows parent cfg/dispatch changes in its private copy, and imports this actual
Windows module and dedicated test. Its production-shaped private API fixture
reaches Source construction, publication ready/complete/retire/retained, and
CoverFiles retention without a blanket dead-code allowance. It changes no root
source or Cargo file.

Passed locally on Linux:

```sh
CARGO_TARGET_DIR=/mnt/data/cache/freeshow-cover-native-portability \
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 \
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
cargo +1.99.1 clippy \
  --manifest-path /tmp/freeshow-cover-windows-check/Cargo.toml \
  --offline --all-targets --target x86_64-pc-windows-gnu -- -D warnings

rustfmt +1.99.1 --edition 2021 --config skip_children=true --check \
  src-tauri/src/now_playing_cover_file/windows.rs \
  src-tauri/tests/now_playing_cover_windows.rs
```

The dedicated target contains 12 async Windows fixtures plus four private
backend unit tests. Cross lint compiles them; it has not executed those tests.
Fixtures cover actual mkdir/blocker outcomes, held bytes and offset reads,
size/quota/ready-result retention, root/Audio/leaf replacement, positive guard
then failed open, sharing failure, in-place changes, foreign context,
folder-error ordering, caller Drop and real ordered-IO fencing, actual directory
junctions, and the Node stat/ETag comparison. Test children are sequential,
bounded to five seconds, and killed/reaped on failure; no product child is added.
Only the genuine leaf-symlink case may skip on Windows privilege error 1314;
mandatory root/Audio junction tests do not depend on that privilege. A phase/IO
fence is described as ordered-work completion, never as a joined library thread.

Actual Windows fixture execution, HTTP route/header behavior through the gated
Source, and native client coverage remain coordinator CI obligations. No LAN,
GUI, immutable-file, or full remote-route parity claim follows from this slice.
