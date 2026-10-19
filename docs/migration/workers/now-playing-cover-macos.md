# macOS fixed-cover filesystem backend

This slice adds only `now_playing_cover_file/macos.rs` and a dedicated native
macOS integration target. The coordinator must select it in the existing parent
module. It does not enable the route by itself. Local evidence is ARM macOS
cross-compilation and strict Clippy, **not execution on macOS**. The native tests
must run on a real macOS CI runner before claiming that platform's route parity.

## Authority and supported behavior

The private API matches the existing native parent:

```rust
pub(super) fn prepare(
    observation: &mut Observation,
    lease: Arc<Lease>,
    control: &Control,
) -> Result<Outcome>;
pub(super) fn revalidate(
    authority: &Authority,
    control: &Control,
) -> Result<Option<FileStat>>;
```

An accepted ordered `LibraryPhase` supplies the full context and root. Only the
fixed `Audio` and `NowPlayingCover.png` names are used. The module accepts no
request-selected path or deserialized authority and starts no worker or task.
The parent's four process-wide whole-result leases and bounded validation guard
remain unchanged. The opaque holder continues to own root, Audio and leaf FDs;
coordinator wiring enables its existing Unix positional `read_at` on macOS.

Every root component opens relative to the preceding live FD using
`O_RDONLY|O_DIRECTORY|O_NONBLOCK|O_NOFOLLOW|O_CLOEXEC`. Audio is held as a directory
or an actually observed regular-file blocker. The leaf opens read-only with
`O_NONBLOCK|O_NOFOLLOW|O_CLOEXEC`. `fstatat(AT_SYMLINK_NOFOLLOW)` probes the named
entry, and `fstat` verifies opened identity. No production canonicalization or
fallback path traversal is performed. Existing symlink aliases such as `/var`
or `/tmp` are explicitly unsupported; fixture setup alone canonicalizes its
native temporary base. Other mount/firmlink, TCC/sandbox and filesystem variants
remain subject to actual API results and real macOS validation.

The pinned Electron source (`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`) calls
`getNowPlayingMediaPath` -> `getDataFolderPath("audio")` -> `createFolder` before
`servers.ts` tests `doesPathExist`, whose implementation calls `fs.existsSync`.
The module preserves the actual Audio mkdir attempt and at most one typed
folder-failure observation. Missing is produced only by an actual failed parent
or fixed-leaf existence probe followed by successful held-namespace validation.
An existing readable regular file at Audio produces the real mkdir error and
ENOTDIR existence-false. An error opening/statting a leaf after an exists-true
probe is Failed, never Missing. Earlier genuine mkdir failure survives later
identity, deadline or cancellation failure; this module emits no ALERT itself.

Readable directory anchors are a supported-subset requirement: permissions that
prevent obtaining one yield an explicit error, even where Node could report
exists-false. The module does not substitute `O_EVTONLY` for authority: Apple's
manual restricts its purpose to monitoring and notes its different unmount
behavior. `O_SEARCH` and newer resolution flags are not assumed available or
verified for this backend. No claim of complete Node filesystem behavior follows
from using the public POSIX-like API.

## Limits and observations

- Root at most 32768 OS bytes and 128 components, with no parent traversal.
- Regular leaf length at most 4 MiB; opaque and empty bytes are supported.
  Preparation allocates no image/body buffer. The parent limits each read to
  32 KiB and its absolute endpoint to 4 MiB.
- At most three persistent FDs per held authority and at most six total during
  its preparation/revalidation. Root walking retains at most the previous and
  next directory, rather than every ancestor. Four original leases remain the
  whole-operation limit; validation cannot accumulate another unbounded result.
- One folder failure plus one exists-false diagnostic at most. Native errno is
  retained; parent diagnostic strings remain limited to 1024 UTF-8 bytes.
- Control checks precede and separate filesystem operations. They do not
  interrupt a blocked kernel call, establish a cancellation fence or roll back
  a completed mkdir. Caller drop does not discard accepted ordered work.
- `LibraryPhase::finish()` fences its actual accepted callbacks and closes that
  phase's admission. It does not join the process's library IO thread or prove
  remote-body/publication retirement. Those owners remain coordinator-owned.

File observations preserve the actual signed Darwin mtime/ctime seconds and
nanoseconds without normalization. Identity comparisons retain native signed
`dev_t` plus `ino_t`; the parent's device observation is a u64 cast, not a claim
about every numeric Node Stats field. Pinned libuv's Apple `uv__to_stat` likewise
copies the raw time components. Gnulib records negative `tv_nsec` on macOS; the
pre-epoch test therefore compares actual native components and exact integer
time, without assuming Linux's representation. Existing HTTP `from_posix`
rejects noncanonical nanoseconds. Such a Darwin observation remains an explicit
header-projection failure until separately verified; normalizing it here could
change the required binary64 projection.

Before-header revalidation checks a fresh root walk, Audio identity, current
fixed leaf identity/version and the held leaf's stat. Missing validation does
not assert continued absence. Held FDs do not make byte snapshots: in-place
writes remain visible, and changes after validation, restored/coarse timestamps,
privileged ABA/mount changes and filesystem stalls are not solved by this seam.
No post-header immutability, image-decoding or HTTP lifecycle proof is claimed.

## Coordinator wiring

No existing file was edited in this commit. Apply these narrow parent changes
alongside any Windows backend wiring:

1. Widen the existing Linux-only `Failure::os`, `Authority.root/audio`, held
   Unix `read_at` branch, mutable preparation observation, preparation dispatch
   and revalidation dispatch to `any(target_os = "linux", target_os = "macos")`.
   Widen the complementary unsupported branches to its inverse. Leave the
   existing inline Linux backend and Linux pause fields/implementation untouched.
2. Change just the existing preparation call's optional hook argument to
   `#[cfg(all(test, target_os = "linux"))] &hooks`. Darwin's private entrypoint
   takes the same first three arguments and owns its own test-only hook.
3. Add the platform selection and test-only type export:

    ```rust
    #[cfg(target_os = "macos")]
    #[path = "now_playing_cover_file/macos.rs"]
    mod backend;

    #[cfg(all(test, target_os = "macos"))]
    #[allow(
        unused_imports,
        reason = "Dedicated macOS integration target uses this test-only hook type"
    )]
    pub use backend::MacTestStage;
    ```

4. Exclude the existing `now_playing_cover_file.rs` Linux/unavailable-OS test
   crate on macOS once the new dedicated target replaces it. Excluding just its
   unavailable-OS function leaves unconditional fixture helpers unused under
   strict Clippy. Its Linux cases stay unchanged; the new
   `now_playing_cover_file_macos` target exercises macOS. Update the existing
   dead-code annotation explanations to describe all implemented platforms when
   integrating.

The one bounded global pause slot exists only under `cfg(test)`. The native
`CoverFiles::test_pause_macos(MacTestStage)` lets tests stop the actual ordered
callback at root/Audio anchoring, before leaf open or after leaf open. It creates
no new IO thread. Dedicated tests serialize access to this hook/global quota.

## Evidence and pending execution

Twelve macOS-only integration tests compile against the actual parent types
and real `LibraryService`/`LibraryPhase`. They cover mkdir/Missing/blocker errno,
opaque/empty bytes, native stat fields and pre-epoch time, exact/oversized files,
cross-context global quota, completed-uncollected results, accepted caller drop,
controlled root/parent/leaf swaps, later ENOENT open failure, preserved mkdir
failure after cancellation/replacement, held-inode versus in-place changed reads,
symlinks/FIFO/directory rejection, real permission failures, work/deadline bounds
and validation guard lifetime through queued/collected/caller-dropped receipts.
Permission cases require the ordinary non-root CI user; they fail explicitly
when the process is root so a bypassed permission check cannot count as proof.
The in-place change regression preserves the same inode and three-byte length,
sets and checks two explicit real mtimes, verifies the held descriptor observes
the replacement bytes, and requires before-header revalidation to reject the
changed version. No sleep is used to manufacture timestamp changes.

A private temporary harness copies the unchanged parent and applies only the
wiring above, selecting the actual new module by absolute path. It points to
the actual owned test file with only its parent-module path rewritten. A small
binary target checks the non-test module too; no Tauri GUI build is involved.
The harness manifest uses the existing core, serde_json, Tokio, tokio-util,
libc and tempfile dependencies, with no new dependency or shared-lock edits.

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 \
CARGO_TARGET_DIR=/home/rob-admin/GitHub/freeshow-worktrees/native-menu/target/assets \
cargo clippy --offline \
  --manifest-path /tmp/freeshow-native-cover-macos-check/Cargo.toml \
  --all-targets --target aarch64-apple-darwin -- -D warnings
rustfmt --edition 2021 --config skip_children=true --check \
  src-tauri/src/now_playing_cover_file/macos.rs \
  src-tauri/tests/now_playing_cover_file_macos.rs
```

Both checks passed locally. They establish type/ABI-binding compatibility and
test compilation, not successful macOS execution. After parent wiring, run the
dedicated target on native macOS (`cargo test --manifest-path src-tauri/Cargo.toml
--test now_playing_cover_file_macos`) and then the coordinator's real route/body
and GUI evidence before expanding the platform parity statement.

The test-only peer-review follow-up also passed strict Apple ARM all-target
Clippy in `/tmp/freeshow-native-cover-macos-peercheck`, using a copy of the
current coordinator parent with the supplied wiring and the actual owned
backend/tests. This remains cross-compilation evidence, not macOS execution.

Primary references: Apple's pinned XNU `xnu-12377.121.6`
[open/openat manual](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/bsd/man/man2/open.2),
[fstat/fstatat manual](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/bsd/man/man2/stat.2),
[mkdir/mkdirat manual](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/bsd/man/man2/mkdir.2),
[public flags](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/bsd/sys/fcntl.h)
and [public stat layout](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/bsd/sys/stat.h);
[libuv 1.51.0 Apple stat adapter](https://github.com/libuv/libuv/blob/v1.51.0/src/unix/fs.c#L1348);
[Gnulib's own negative-nanosecond compatibility code](https://github.com/coreutils/gnulib/blob/master/lib/stat-time.h#L202).
