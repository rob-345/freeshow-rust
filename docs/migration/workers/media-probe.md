# Native media probe worker

This slice implements `FFMPEG_CHECK`, `MEDIA_CODEC`, and `MEDIA_TRACKS` in the independent `freeshow-media` crate. It does not change Electron, the Svelte frontend, shared IPC handling, or packaging.

The oracle is FreeShow 1.6.7-beta.2, commit `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`. The worker verified that `src/electron/IPC/responsesMain.ts`, `src/electron/utils/files.ts`, `src/electron/streaming/ffmpegManager.ts`, and `src/electron/data/media.ts` are unchanged from that commit. Node parity tests extract and execute those actual function declarations, MIME data, resolver declarations, and the actual `FFMPEG_CHECK` handler arrow. They inject filesystem/process boundaries; they do not copy the oracle algorithm.

## Integration contract

Dependencies are `serde` with derive, `serde_json` with preserve_order, `thiserror` 2, `tokio` with fs/process/io-util/sync/time/rt/macros, and `tokio-util` 0.7 with rt. The initial crate has its own temporary `[workspace]` and lock so worker development cannot mutate shared manifests. The coordinator removes that workspace/lock, adds the crate to the root workspace and native app dependencies, and changes the feature test harness's default binary path to the integrated target directory.

```rust
use freeshow_media::{MediaConfig, MediaService, COMMANDS};

let media = MediaService::new(MediaConfig {
    local_bin_dir: Some(user_data.join("bin")),
    ..MediaConfig::default()
})?;
// Route only COMMANDS; handle context/payload/path policy at the shared boundary.
let reply = media.dispatch(command, payload).await?;
// reply.data is the original handler reply shape.
// reply.diagnostics must be surfaced/logged as explicit capability/process failures.
// Preserve existing reply/event channel and listener ID at the integration layer.
media.shutdown().await; // Before Tokio runtime/application exit.
```

`MediaService` is cheap to clone and independent of Tauri/AppState. `dispatch` requires an active Tokio runtime and returns `Result<MediaReply, MediaError>`. Unknown commands return `unsupported_command`; non-object metadata payloads return `invalid_request`; cancelled, shut down, and capacity-exhausted requests return explicit errors. `FFMPEG_CHECK` ignores its payload as the original handler does.

`MediaReply { data, diagnostics }` separates compatibility data from capability failures. Missing files, unreadable/non-file inputs, inputs without `ftyp` in the first 256 KiB, and ffprobe's normal rejected-input exit (code 1) return the original empty metadata object without diagnostics. Unavailable binaries, timeouts, pipe limits, abnormal process failures, invalid JSON, and detected unsupported codec variants attach explicit diagnostics. The coordinator must not advertise successful metadata support after discarding those diagnostics. The installed check remains `{ "installed": false }`, with `path` absent, when no usable FFmpeg is found. This command checks FFmpeg independently; it does not promise that ffprobe exists.

`MediaConfig` accepts explicit executable candidates before PATH discovery, OS-specific conventional paths, and optional user-data/bin candidates. `discover_system: false` disables PATH/conventional paths; an explicit local-bin directory still supplies candidates. Each candidate must exit successfully on `-version`. Successful paths are cached; failed resolution is retried on later calls. Like the original resolver, `FFMPEG_CHECK` keeps reporting a previously resolved path if that binary is subsequently removed. A cached ffprobe that disappears produces a process diagnostic on the next metadata request. Nothing downloads or installs executables.

`clear_ffmpeg_path_cache().await` invalidates the cached FFmpeg selection after a native installation. It waits for the same mutex held through an actual resolver process: a probe already in flight settles before invalidation completes, and cannot later restore its old cache entry. Calls admitted before invalidation may therefore return the old selected path; subsequent resolution uses the current candidates. Configuration itself is immutable.

## Preserved behavior

- Payload fields are spread into replies, preserving unknown fields; computed `codecs`, `mimeType`, `mimeCodec`, and `tracks` overwrite stale input fields.
- The original narrow extension MIME map is retained, including `application/octet-stream` for `.m4a` and other audio extensions. MIME type is inferred from the input path independently of the file contents.
- The original MP4/ISO-BMFF header gate remains in place. A valid WAV still produces empty metadata. ffprobe accepting another container is insufficient to broaden this handler.
- AVC codec strings use avcC profile/compatibility/level bytes; common AAC uses AudioSpecificConfig object type; HEVC and AV1 use their configuration records; simple sample entries such as `mp4v`, `tx3g`, `Opus`, or `ac-3` retain their entry tag spelling. Codec order follows stream order.
- Subtitle extraction uses ffprobe raw packet bytes with edit-list handling disabled. It retains MP4Box's ASCII-only text cleanup, printable mov_text length-prefix artifacts, style-box fragments, floored millisecond timestamps, cue numbering, language names and first-two-character `lang`, default `und`/`un`, and `{ embedded: true }`. Non-ASCII-only cues can vanish. `wvtt` remains excluded because MP4Box classifies that sample entry as metadata.
- Only native absolute local paths are passed to ffprobe, using an argument vector, no shell, null stdin, and a file-only protocol whitelist. The original payload path remains unchanged in the reply.

## Process and resource bounds

The default service admits at most two jobs, including discovery and file header checks. Additional calls fail immediately with `media_busy`, so there is no unbounded waiting queue, thread-per-request design, or unbounded spawn-blocking pattern. Admitted jobs run as bounded Tokio tasks. Default stdout/stderr caps are 8 MiB/64 KiB per child, read concurrently. Each child has a ten-second timeout. Configuration rejects zero/oversized resource limits.

Dropping a dispatch future cancels its task token. Timeout, cancellation, and pipe-limit errors explicitly kill and wait for the direct child before its job permit is released. Shutdown cancels admitted work and waits for all job permits. `kill_on_drop` provides additional protection during runtime teardown; normal shutdown must still be awaited while Tokio is alive. Process-tree cleanup is not implemented: a configured wrapper that starts descendants can leave those descendants alive. Supported direct ffprobe/ffmpeg processes normally do not need wrappers.

The timeout is per child, including each `-version` attempt. Whole-request discovery can take the number of candidates multiplied by that timeout, followed by the metadata process timeout; it has no separate overall deadline. Failed concurrent discovery is serialized by the per-binary cache mutex and can repeat after an earlier failure. Filesystem metadata/open/read/canonicalize operations use Tokio filesystem APIs and inspect at most 256 KiB of file content. They have no independent timeout, and cancellation cannot interrupt an OS filesystem call already in progress. Filesystem hangs on unusual/network paths can therefore delay request cancellation and shutdown. These limits are explicit integration/OS gaps.

## Evidence and remaining gaps

Verified on Linux with Rust 1.99.1 and system FFmpeg/ffprobe 6.1.1. The portable Rust suite does not require media executables: ten tests cover projection/default behavior, MIME fallback, malformed dumps, command/payload errors, busy admission, shutdown before/after permit acquisition, process timeout, independent stdout/stderr caps, nonzero exit, and cancellation with Linux `/proc` evidence that the child was reaped. Five ignored test functions are executable child fixtures invoked by those tests, not omitted parity assertions.

The Node suite has ten tests. All passed locally with zero skips. Its main real-process corpus compares 28 original metadata replies across AAC audio-only, H.264/AAC, MPEG-4 video, subtitles with Unicode/style/printable-prefix quirks, multiple languages, default language, uppercase and unknown extensions, a dash filename, WAV, corrupt/truncated/missing inputs, and a late `ftyp` header. Additional real HEVC and AV1 cases compare nonempty codec strings. Resolver tests compare installed/unavailable replies and the cached result after PATH executable removal. A real VP9 fixture establishes the explicit unsupported-variant diagnostic rather than counting it as parity. When binaries or optional encoders are unavailable, test names include a clear reason and real-process coverage is marked unexecuted.

Standalone checks:

```sh
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-media/target cargo test --manifest-path crates/freeshow-media/Cargo.toml
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-media/target cargo build --manifest-path crates/freeshow-media/Cargo.toml
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-media/target cargo clippy --manifest-path crates/freeshow-media/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path crates/freeshow-media/Cargo.toml --check
node --test tests/parity/media-probe.test.mjs
npx prettier --config config/formatting/.prettierrc.yaml --check tests/parity/media-probe.test.mjs
```

VP8/VP9, VVC, encrypted entries, and `stxt` configuration are explicitly unsupported by the codec projection where ffprobe does not expose sufficient equivalent configuration. Non-AAC `mp4a` descriptors are explicitly unsupported. AAC mapping assumes the usual MPEG-4 Audio OTI `0x40`; MPEG-2 AAC descriptor OTI values are not exposed by ffprobe and can differ from that assumption. There is no claim of exhaustive parity for fragmented MP4, unusual edit lists, malformed-but-tolerated MP4Box inputs, alternate text subtitle types, cover-art pseudo-streams, zero-sample tracks, or uncommon sample-entry configurations. Large subtitle metadata can intentionally hit output caps and return a resource diagnostic. macOS/Windows spawning/path/cancellation behavior and bundled executable availability are unverified. Bundled binaries, licensing/distribution, download UI, and replacing the old MP4Box dependency in a packaged app remain separate work. No bundled-distribution capability is claimed here.

## Resolver fixture synchronization followup

Ubuntu core job in CI run `39225346104` failed at the cache test's **initial** selection, before any invalidation: the second Unicode candidate was selected instead of the first. The resolver intentionally skips a candidate whose execution fails, and the CI log did not retain that candidate's underlying errno. This is not evidence that invalidation overtook an in-flight probe.

A controlled Linux reproduction established a matching fixture hazard: a fork retaining a writable descriptor prevents execution with `ETXTBSY` (errno 26), even after the parent closes its own descriptor. Concurrent test subprocess creation can overlap creation of a freshly written executable. This explains a possible first-candidate rejection, but the original CI errno remains unverified. Production resolver behavior is unchanged; no retry conceals candidate failures.

The Unix cache test now creates Unicode symlinks to the committed executable `tests/fixtures/version-probe.sh`, without writing the executable inode during tests. It verifies cached-path survival after removal, explicit invalidation, and an actual first-candidate child held by private ready/release witness files. After that child reports its PID, the test removes its candidate symlink and directly polls invalidation to `Pending` while the resolver owns the cache mutex. Releasing the child must return the first path to the old request; completed invalidation must then force a new request to select the second path. Polling a child witness has a deadline, but no assertion relies on sleeping to infer task admission or invalidation ordering.

A separate Linux test holds a real writable executable descriptor, confirms the kernel's errno 26, and exercises the production resolver selecting the usable second candidate. It does not depend on ambient fork timing. Both tests use local temporary paths and `/bin/sh`, without installed FFmpeg. Windows does not execute the Unix fixture; Windows resolver lifecycle parity remains a separate gap.

Linux followup checks passed: `cargo test --locked -p freeshow-media --lib` (12 passed, five executable child-fixture entries), then 25 consecutive complete runs of that test binary with `--test-threads=8` (300 passing test executions). Each run exercises both resolver regressions concurrently with the existing process supervisor tests; no test retries failures. `cargo clippy --locked -p freeshow-media --all-targets -- -D warnings`, crate `cargo fmt --check`, `sh -n` on the fixture, document Prettier, and `git diff --check` also passed. No macOS or Windows run was performed for this followup.
