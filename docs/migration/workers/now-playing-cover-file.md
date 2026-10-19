# Fixed now-playing cover descriptor foundation

This native-only IO module prepares the selected library's fixed `Audio/NowPlayingCover.png` on the existing ordered `LibraryPhase`. It does not install `/nowplayingcover`, emit an ALERT, decode an image, construct an HTTP body, start an owner, or create a thread/task. The future remotes host owns those continuations and must retain their accepted work through disconnect, context retirement and shutdown. The route integration design is in [remote-cover-plan.md](remote-cover-plan.md).

The pinned source is `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`: `audio/nowPlaying.ts::getNowPlayingMediaPath` calls `utils/files.ts::getDataFolderPath("audio")`, which runs `createFolder`; `servers.ts` then tests `doesPathExist` before `sendFile`. Thus preparation can have a real folder side effect even for an empty404. A regular file at `Audio` causes an actual failed mkdir, one folder-failure observation, and an exists-false Missing result. An inaccessible Audio path likewise attempts mkdir. Once the leaf existence probe succeeds, a later open error is Failed, never Missing. The module preserves a previously observed folder failure even if a later identity check fails.

## Native API and authority

```rust
let files = CoverFiles::default(); // All instances share the process-wide quota.
let ticket = files.submit(&phase, control)?;
let prepared = ticket.finish().await?; // Actual ordered/context receipt first.
let (observation, outcome) = prepared.into_parts();
// At most one observation.folder_failure(), derived from the real mkdir error.
// A host must accept any corresponding bounded source notification before its
// HTTP continuation. Renderer acknowledgement must not gate the source404.
match outcome {
    Outcome::File(held) => {
        let validation = held.revalidate(&phase, Control::default())?
            .finish().await?;
        validation.stat().map_err(Clone::clone)?; // Check before headers.
        // Read on the existing bounded remotes file worker, never GTK/Tokio IO.
        let mut chunk = [0u8; MAX_CHUNK];
        let count = held.read_at(0, &mut chunk)?;
        // Keep held + observation while their independent effects/body live.
    }
    Outcome::Missing => { /* Source existence guard was false. */ }
    Outcome::Failed(error) => { /* Explicit native error, not empty404. */ }
}
```

Inputs are an already admitted native `LibraryPhase` plus a cancellation/deadline control. There is no request path/name, `Deserialize`, renderer grant, executable or arbitrary callback. The submitted request carries the phase's exact complete `RuntimeContext`; the ordered owner validates it again. The fixed paths derive only from that context. Native `FolderFailure` exposes the fixed folder and bounded diagnostic, with no notification or delivery claim.

`HeldCover` is opaque and cannot expose or clone its raw `File`. It retains the root, Audio and leaf descriptors with the operation lease. `read_at` uses the held leaf without shared seek offsets, accepts at most32KiB per call and never reads beyond the4MiB domain. It is synchronous filesystem IO and belongs on an existing file worker. `Observation::revalidate` also supports Missing namespace guards. Missing revalidation does not promise continued absence or repeat the original existence decision.

Revalidation is another actual ordered phase operation. One continuation/result per authority can be live; attempts to accumulate more return `now_playing_cover_busy`. Its guard remains charged through accepted work, ready/uncollected results and the collected `Validation`, including failures. Dropping a waiting ticket cannot remove the worker's guard. `Validation::stat()` must be inspected; merely collecting the receipt does not imply a successful check. Foreign contexts reject before admission. The host must also check its current complete context before publishing headers/effects.

## Verified backend and limits

The Linux backend uses the public `libc` ABI: `openat`, `fstatat(AT_SYMLINK_NOFOLLOW)` and `mkdirat`. It walks the absolute selected root from a held `/` with `O_PATH|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC` for every component, anchors Audio without following links, and opens the fixed regular leaf with `O_RDONLY|O_NONBLOCK|O_NOFOLLOW|O_CLOEXEC`. It verifies device/inode observations around opens and again before returning File/Missing. Revalidation freshly walks the selected path and checks the root, Audio, current leaf and held leaf. Directory timestamps are not treated as identity; ordinary unrelated directory entries may change.

Symlink ancestors, Audio/leaf symlinks, special files, directory covers, relative roots, parent components and unverified OS backends are explicit unsupported variants. This is deliberately narrower than Node's path-following behavior. Windows/macOS compilation checks cover the explicit unavailable branch; actual anchored backends remain unimplemented there. Linux execution was verified on x86_64, not every Linux architecture or filesystem.

- Four process-wide whole-work/result leases, shared across all `CoverFiles` instances and library contexts. Reservation precedes context copies, private result channels and ordered work admission. A failed phase admission releases its reservation.
- Queued/active work, completed/uncollected tickets, collected Missing/Failed observations, held descriptors and validation results retain the same whole-operation lease. Splitting `Prepared` cannot release FD quota while the file remains alive. Releasing the observation can free the notification data, while `HeldCover` independently keeps its descriptor guards and lease.
- At most three persistent FDs per successful cover authority; bounded transient descriptors while walking/revalidating. Preparation uses no cover-byte allocation. Validation has one live continuation/result per authority, so retained private observations cannot grow through repeated revalidation.
- Root path cap32768 OS bytes and128 components; cover length at most4MiB, including opaque non-PNG and zero-byte regular files. Reads are at most32KiB, with a4MiB absolute endpoint bound. The source permits larger files; rejection is an explicit native resource limit.
- At most one actual folder-mkdir failure and one caught exists-false diagnostic per preparation. Diagnostic messages are at most1024 UTF8 bytes, with native errno retained. Folder path length is the bounded context path plus the fixed Audio leaf.
- Default control deadline10seconds; the native caller may supply a different deadline. Cancellation/deadline checks occur before admission and between bounded filesystem operations. They do not preempt a blocked kernel/filesystem syscall. No timeout is presented as disposal or drain.
- Existing phase limits still apply: four accepted/ready job results and32 operations per phase. `LibraryPhase::finish()` is a real accepted-job fence and admission release, not termination or join of the process's library IO thread. A future host must keep the phase open for required validation continuations and independently drain its network/publication owners.

`FileStat` reports device, inode, length, and raw signed modification/change seconds plus nanoseconds. A real pre-epoch fixture verifies that `-1.25s` is reported as seconds`-2`, nanoseconds`750000000`. There is no guessed `mtimeMs`, ETag or Last-Modified conversion here. The HTTP host must separately reproduce Node's binary64 Stats projection before calling the pure response planner.

Held identity and version evidence does not certify an immutable snapshot. A writer can modify the held inode after validation or while a body streams. Replaced paths are detected before headers when checked; the held descriptor continues to reference its original inode. No atomic namespace transaction, rollback of an earlier mkdir, post-header mutation guarantee or restored/coarse-timestamp detection claim is made. General filesystem latency remains an OS dependency.

## Focused evidence

Eleven Linux native tests use real temporary files and the real library ordered owner. They cover actual absent Audio creation, missing cover, unchanged regular-file blocker and typed failure, a mkdir failure surviving a later parent swap, inaccessible-root exists-false behavior, opaque/empty bytes, exact4MiB and oversized files, raw pre-epoch times, cross-context shared quota, accepted caller drop, ready/uncollected ownership, actual exists-true→ENOENT and EACCES open failures, controlled root/Audio/leaf swaps, held-inode reads after replacement, truncation/version failure, wrong full context, no-follow symlinks/FIFO/directory rejection, cancellation, deadline, and bounded validation continuation/result ownership. Permission-denied cases execute as the non-root test user; they are conditionally omitted if run as root. Controlled pause hooks exist only under `cfg(test)` and block the actual ordered IO callback; no production hook is exposed.

Focused checks use a private temporary test-only Cargo manifest pointing at the new test and existing `freeshow-core`, avoiding shared manifest/runtime edits:

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/home/rob-admin/GitHub/freeshow-rust/target \
  cargo test --manifest-path /tmp/freeshow-native-menu-cover-file-check/Cargo.toml --offline --test cover_file
# 11 passed
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/home/rob-admin/GitHub/freeshow-rust/target \
  cargo clippy --manifest-path /tmp/freeshow-native-menu-cover-file-check/Cargo.toml --offline --all-targets -- -D warnings
rustfmt --edition 2021 --config skip_children=true --check \
  src-tauri/src/now_playing_cover_file.rs src-tauri/tests/now_playing_cover_file.rs
```

The source's actual file-helper/folder-failure suite remains separate from these native IO tests. These checks establish this fixed-file preparation seam, not completed HTTP route, header/range/body lifecycle, codec, GUI or cross-platform parity.
