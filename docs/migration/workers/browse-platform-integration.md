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
Darwin retains the complete source IEEE-bit comparison and a synthetic fixture
with distinct timestamps, without imposing an unsupported real-filesystem
inequality. Both source oracles normalize CRLF and reject actual source edits.

The private integrated worktree passes strict all-target Clippy for Linux,
Windows GNU and macOS ARM, nine existing Linux Rust tests, and eighteen focused
pinned-Node source suites with no skips. The Node suites include 8,192 generated
Windows lexical cases; they execute the actual pure Rust helper, not a Windows
filesystem simulation. Both native target lockfile resolutions succeed.

Actual macOS and Windows execution remains pending. The new platform fixtures
must run under their real native CI jobs before their filesystem corpora are
claimed: six Darwin integration fixtures plus two module tests, and eleven
Windows module tests. Cross-compilation is compilation evidence only. The
existing shared Linux source/native tests remain independently required at the
main integration milestone. Foreign GUI/IPC behavior, physical media,
unrestricted Windows paths/volumes, external mutation races and performance
improvement are not established here.

Backend limits and source details are in [Darwin](browse-macos.md),
[Windows stat](browse-windows.md) and [Windows lexical paths](browse-windows-paths.md).
