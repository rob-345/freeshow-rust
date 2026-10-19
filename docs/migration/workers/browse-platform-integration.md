# Native browse platform integration

The coordinator composes the reviewed Darwin stat backend and Windows ordinary
drive-file stat backend with the existing two filesystem owners. Darwin enables
`FILE_INFO`, `READ_FOLDER` and `GET_SIMILAR` through the existing POSIX engine.
Windows enables only `FILE_INFO`; folder and similarity requests still return
`unsupported_platform`. No worker, renderer file authority, metadata fallback
or Node sidecar is added.

The target dependencies add Darwin to the existing libc condition and enable
the already-pinned windows-sys package for Windows stat APIs. Both workspace and
desktop lockfiles record the browse dependency. Linux statx and POSIX behavior
remain unchanged. The shared source suite now tests the narrower platform gate
and compares Windows' actual original empty-path result.

Windows FILE_INFO preserves caller path spelling, including the stat backend's
accepted verbatim drive paths. Its extension projection splits both Windows
separators before the existing basename logic. The independently source-tested
Windows lexical module deliberately rejects verbatim paths and is not wired
into this command or used to enable folder operations.

Review corrected two test assumptions before integration: exclusive data-read
sharing can still allow metadata access, and Darwin filesystems may backdate
creation time with modification time. Windows now proves the real data-read
sharing failure while comparing successful attribute access against Node.
Windows date fixtures additionally require actual retained filesystem times
before comparing or rejecting fractional, u32-boundary and exceptional dates;
source/native equality after a clamp cannot masquerade as boundary evidence.
Darwin retains the complete source IEEE-bit comparison and a synthetic fixture
with distinct timestamps, without imposing an unsupported real-filesystem
inequality. Both source oracles normalize CRLF and reject actual source edits.

The private integrated worktree passes strict all-target Clippy for Linux,
Windows GNU and macOS ARM, nine existing Linux Rust tests, and nineteen focused
pinned-Node source suites with no skips. The Node suites include 8,192 generated
Windows lexical cases; they execute the actual pure Rust helper, not a Windows
filesystem simulation. Both native target lockfile resolutions succeed.

Main integration passes 493 Rust, 573 pinned-Node and twenty frontend checks,
plus all 352 Linux native executions across 36 targets. Five ignored Rust entry
fixtures remain driven by supervisor tests, and four existing source capability
skips remain explicit. Strict workspace and native all-target Clippy pass. The
rebuilt 187-step Linux editor, four menus and both drop scales pass again.
Independent validation preserves both exact source JPEG pixel hashes, all six
ordinary and three folder-error now-playing cases, fourteen cover phases/141
HTTP responses, blocker/old-library sentinel bytes and real process exit.
The complete editor retains 55 expected other-feature diagnostics.
[Durable local receipt](../baselines/browse-platforms-linux-local.json).

Actual core CI at `d0e88f3b` passes all six Darwin integration fixtures/two
module tests and all eleven Windows stat cases. The complete macOS core job
passes. Windows core later fails only the separate lexical source oracle's CRLF
hash. Both native desktop jobs stop at a stale host assertion that expects
all non-Linux systems to be unsupported; actual replies have the correct Null,
context and listener. The coordinator corrected that assertion and added strict
CRLF/mutation controls, with focused local checks. Follow-up `a3cbb911` passes
both native jobs, Windows core and Linux core/desktop. All eighteen Windows cover
cases, three Source cases and the actual TCP/STOP test pass. A separate macOS
core download shutdown fixture still fails, so complete corrected CI remains
required. [Native CI receipt](../baselines/platform-cover-native-ci.json).
Existing shared Linux source/native checks pass in the main integration.
Foreign GUI/IPC behavior, physical media,
unrestricted Windows paths/volumes, external mutation races and performance
improvement are not established here.

Backend limits and source details are in [Darwin](browse-macos.md),
[Windows stat](browse-windows.md) and [Windows lexical paths](browse-windows-paths.md).
