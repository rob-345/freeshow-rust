# Windows fixed-cover backend

This slice adds only the private Windows backend, its dedicated Windows test
target, and this handoff. It does not install a route or publish an ALERT. The
parent API remains `CoverFiles::submit(&LibraryPhase, Control)` and opaque
`HeldCover`/`Observation` revalidation. Local evidence is Windows GNU compilation
and strict lint. The partial actual Windows CI results and subsequent unexecuted
mkdir correction are recorded below; they do not establish Windows HTTP parity.

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
its target. Existing opens inspect that same returned handle before use. The
separate directory-create options and object-parser guard are described below.
Synchronous opens include `SYNCHRONIZE`. See Microsoft's
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
A regular-file Audio blocker retains a typed folder failure; its continuation
uses the existing narrow missing-status set or returns an explicit IO failure.
The strict source-compatible Missing assertion awaits the actual post-fix CI
execution. A raced, already-created actual directory follows the existing
source-derived handling. Missing is permitted only for known namespace
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
upper supported seconds boundary. Every case first asserts 100ns alignment and
the exact requested epoch nanoseconds in both real filesystem metadata and the
signed native seconds/nanoseconds, including the exceptional dates. Quantized or
clamped timestamps cannot masquerade as rounding-boundary evidence. The sharing
fixture requires actual `ERROR_SHARING_VIOLATION`, not just any IO failure.
For pre-epoch and beyond-u32 dates it records
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

## Actual Windows CI diagnosis at `90fda94e`

The genuine Windows native job executed 16 tests: 14 passed and two failed.
The real Node stat/ETag fixture, byte reads, identity/namespace guards, resource
retention, sharing, cancellation/disposal, and junction cases passed. This is
partial native evidence, not a green backend or full HTTP claim.

The absent-Audio case returned Missing but did not create Audio. The original
assertion did not print its already-retained FolderFailure diagnostic, so the
actual mkdir NTSTATUS is not yet observed in that log. The diagnostic follow-up
prints bounded actual outcome, folder error, OS code and at most four bounded
diagnostics if either the initial preparation or real directory assertion fails.
It also reports any unexpected regular-Audio-parent continuation outcome. No
Missing status is added and no failed mkdir is hidden.

The creation call currently inherits `FILE_OPEN_REPARSE_POINT` and
`FILE_OPEN_NO_RECALL` from ordinary opens. The primary NtCreateFile directory
options contract restricts compatible options; this is a concrete candidate,
not proof of the unreported native error. This diagnostic-first follow-up leaves
production options/access unchanged until the next actual Windows receipt.
Metadata/traverse handles are not speculatively widened to add-child rights.
Any subsequent create-option change must separately test a junction raced after
the real absent probe and before FILE_CREATE, including an absent external
target, and an unchanged ordinary collision. Reparse rejection at the initial
probe does not certify that seam.

The other failure was test setup: renaming a parent containing the held regular
Audio child returned native AccessDenied5. The corrected deterministic fixture
pauses after the actual failed mkdir, replaces the blocker before Audio opens,
then replaces the genuinely held Audio directory at the existing Audio pause.
This preserves an actual folder failure followed by identity Changed without
requiring parent rename or guessing a nondirectory RootDirectory failure. The
former blocker bytes are checked unchanged. It uses the single bounded test
pause slot sequentially and introduces no product task or behavior.

## Observed mkdir status and creation-only correction

Diagnostic milestone `ef7fadfb`, actual run `39249175360`, Windows native job
`118048476728`, executed 16 tests: 15 passed. The corrected Audio-namespace
fixture passed. The only failure was the real absent-Audio mkdir:

```text
outcome=Missing
folder_failure=now_playing_cover_io os=Some(87)
Audio mkdir: NTSTATUS 0xC000000D, Win32 87 (parameter incorrect)
continuation: NTSTATUS 0xC0000034, Win32 2 (Audio remains absent)
```

The actual invalid-parameter result agrees with the documented restricted
`FILE_DIRECTORY_FILE` option combination. The sole `FILE_CREATE` call now uses
`FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT`, without the incompatible
`FILE_OPEN_REPARSE_POINT`/`FILE_OPEN_NO_RECALL` create options. It also sets
`OBJ_DONT_REPARSE` in `OBJECT_ATTRIBUTES`, whose documented purpose is to reject
reparse traversal while parsing the supplied relative name. This preserves a
no-follow policy for creation rather than relying on a later rejection after
an external target might already have been created. See the primary
[directory options and create disposition contract](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntcreatefile)
and [OBJECT_ATTRIBUTES contract](https://learn.microsoft.com/en-us/windows/win32/api/ntdef/ns-ntdef-_object_attributes).
Existing opens retain their original reparse/no-recall flags. Ancestor access
rights, sharing, Missing status classifications and public authority API do not
change. There is no added Cargo dependency or feature.

The new private `BeforeMkdir` stage uses the existing single cfg(test) pause slot.
It is reached only after an actual absent-or-nondirectory metadata probe and
immediately before the actual create call. Two mandatory native Windows fixtures
start with genuinely absent Audio and install a conflicting entry at that seam:

- A real directory junction points at an absent private external target. After
  preparation, the target must still be absent; the junction's full volume and
  128-bit identity, reparse tag, attributes and stored target must be unchanged.
  Snapshot handles close before production resumes. The original FolderFailure
  must retain an actual name-collision (`0xC0000035`) or reparse rejection
  (`0xC000050B`) status, never the former invalid-parameter result. The next
  no-follow probe must reject the junction, return Unsupported and expose no
  revalidation authority. A candidate that follows and creates the external
  target, replaces the junction, or returns an authority fails independent
  filesystem/result assertions. A collision before reparse processing is also
  a valid non-creation outcome; this fixture does not observe invisible filter
  callbacks or claim a kernel syscall trace.
- An ordinary file installed at the same seam must retain its full identity and
  exact bytes, with a genuine bounded mkdir failure. Its later continuation may
  only use the unchanged certified Missing set or expose actual IO failure;
  this collision fixture does not preemptively certify another native status.
  The existing combined mkdir/blocker fixture still requires Missing and prints
  the real NTSTATUS if that expectation fails in the next Windows run.

Both fixtures retain the actual original status in their assertions and bounded
test output. They add no runtime skip, product task, thread or authority. Junction
setup uses the existing one-child, five-second kill/reap fixture helper. A phase
and ordered IO fence still proves only real work completion, not a library
thread join.

The refreshed private composition imports the unchanged parent from
`90fda94e5da6717b98366e5ee456e5236c4c70ab`, this actual backend and dedicated target,
and the current root Source/publication modules with production-shaped API use.
The following passed on Linux after the correction:

```sh
CARGO_TARGET_DIR=/mnt/data/cache/freeshow-cover-native-portability \
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 \
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
cargo +1.99.1 clippy \
  --manifest-path /tmp/freeshow-cover-windows-ci-check/Cargo.toml \
  --offline --all-targets --target x86_64-pc-windows-gnu -- -D warnings

rustfmt +1.99.1 --edition 2021 --config skip_children=true --check \
  src-tauri/src/now_playing_cover_file/windows.rs \
  src-tauri/tests/now_playing_cover_windows.rs
```

This compiles 14 async Windows fixtures plus four private backend unit tests; it
does not execute them. Actual corrected mkdir, regular-file parent continuation
and both new race fixtures must pass genuine Windows CI before runtime support
is claimed. No root source, parent dispatch, manifest, lock, CI or app is changed
by this correction.
