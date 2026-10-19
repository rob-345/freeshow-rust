# Typed now-playing folder-failure observation

`now_playing_files::Observation.folder_creation_failed` is true only when the
actual ordered `Intent::EnsureFolder` calls `fs::create_dir_all` and receives an
error. It is false for path-only observations, successful/existing directories,
Exists/Delete/Write failures and equal-text skips. Every independent mkdir attempt
produces a fresh observation; earlier failures are not cached or deduplicated.
The existing returned paths, `success`, diagnostics, write behavior, operation
order, resource limits and accepted library ticket ownership are unchanged.

The files owner publishes nothing. The native host may derive the original alert
only from this typed flag and **trusted** `observation.paths.folder`:

```text
ALERT "Error: Could not create folder at: <trusted Audio folder>!"
```

An ordinary false write/delete result or diagnostics entry does not grant that
publication intent. `Paths` performs no mkdir and cannot imply a folder alert.
Native host publication ownership, ordering, multiple private receipts and frontend
retained delivery are separate coordinator/host work, outside this patch.

The source regression executes actual pinned `files.ts` functions and the complete
`nowPlaying.ts` in the same Node realm, with genuine music-metadata parsing of a
real WAV. A real regular file obstructs `selected/Audio`; all mkdir/write failures
come from actual private filesystem calls. Only renderer publication, Icecast and
shell boundaries are traced. No shell is opened. A transparent microtask marker
queued at the genuine title boundary distinguishes the synchronous post region
from the source's awaited text continuation; it does not establish universal
Node/Tokio scheduling equivalence.

Actual source evidence:

- SET: metadata first, then title, three folder alerts during PostMetadata and one
  during Text. Both artwork replacements resolve the folder even when the chosen
  format contains only `{title}`. The caught text write returns false separately.
- UNSET: empty title, two folder alerts and a separate caught false text write.
- OPEN: folder-creation alert, then `This does not exist!`; no reveal/system open.
- Repeated path-helper calls return the same folder spelling and alert for every
  actual failed attempt. The obstruction's bytes remain unchanged.

Immutable revision: `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`.
Both source blobs are SHA256-checked in the test. Local evidence: **8 native file
tests and 4 Node source tests passed** on Linux with Node22.21.1; strict Clippy,
rustfmt, Prettier and whitespace checks passed. The new native obstruction test and
all source regressions are OS-generic; actual Windows/macOS runs remain root CI.
Two preexisting native symlink/FIFO tests remain Unix-only.

Cargo ran only after coordinator confirmed the cache move complete, using the
authorized shared root target, jobs1/debug0/incremental0 and a temporary headless
manifest. No shared manifests, lockfiles, native host, main, bridge or CI files
changed. No GUI/build artifacts or archived binaries were modified.

```sh
cargo test --offline --manifest-path /tmp/freeshow-osc-now-playing-files-check/Cargo.toml --test now_playing_files
cargo clippy --offline --manifest-path /tmp/freeshow-osc-now-playing-files-check/Cargo.toml --all-targets -- -D warnings
node --test tests/parity/now-playing-folder-alerts.test.mjs
```
