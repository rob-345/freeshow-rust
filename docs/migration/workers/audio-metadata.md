# Native audio metadata worker

This bounded slice implements `READ_AUDIO_METADATA` in `crates/freeshow-audio`. It leaves Electron, Svelte, IPC/context handling, shared manifests, and the previous media crate untouched. The reference is `getAudioMetadata` in `src/electron/audio/audio.ts` at pinned FreeShow 1.6.7-beta.2 (`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`), with the installed original `music-metadata` 7.14.0. The worker verified that this handler is unchanged from that commit.

## Integration

```rust
use freeshow_audio::{AudioConfig, AudioService, COMMANDS};

let audio = AudioService::new(AudioConfig::default())?;
let reply = audio.dispatch(command, payload).await?;
// Preserve reply.data as the existing handler reply; surface reply.diagnostics.
// Shared IPC retains channel/listener ID and performs application context/path policy.
audio.shutdown().await;
```

`COMMANDS` contains only `READ_AUDIO_METADATA`. `dispatch` consumes the original `{ filePath }` payload. An empty/missing/non-string path returns `null`, as observed at the original parsing boundary. Unknown commands and non-object payloads fail explicitly. Paths over 32 KiB or containing NUL are rejected before cloning them into work. Other request fields are ignored, matching the handler.

`AudioReply { data, diagnostics }` returns `null` without diagnostics for ordinary missing, malformed, unreadable, and non-file inputs. Resource limits, detected unsupported variants, and worker failures return `null` with explicit diagnostics (`audio_resource_limit`, `audio_unsupported_variant`, `audio_worker_error`). Cancellation, shutdown, busy admission, and request validation return errors. Integration must retain those distinctions rather than treating an unsupported variant as verified metadata parity.

`AudioService` is independent of Tauri and cheap to clone. It starts a fixed parser pool (two threads by default), owns a bounded queue, and holds a job permit until native work and reply construction finish. Excess requests return `audio_busy` immediately. A request future's drop guard cancels its token. Cancellation is cooperative at reads/seeks and checked again after native parsing. Shutdown cancels work, closes the sender, drains permits, waits for worker completion notifications, and collects thread handles. Service creation, dispatch, and shutdown work without a Tokio runtime; a portable unit test uses a small ordinary executor to verify this. The JSONL harness uses Tokio only to drive its futures. `WorkerExit` signals immediately before thread return, so final `join` can briefly wait for thread-local destructors/termination. This is not a strict nonblocking-join guarantee. Dropping the last service cancels/closes work, but callers should await shutdown for complete cleanup.

Dependencies: `lofty = 0.24.0` with default features disabled, `serde` with derive, `serde_json` with preserve_order, `thiserror` 2, Tokio sync/rt/time/macros, and `tokio-util` 0.7 rt. Lofty 0.25.4's disabled-compression configuration failed to compile because its disabled branch references unimported error types; 0.24.0 supports that configuration. No Node sidecar, runtime FFmpeg dependency, downloaded executable, or per-request thread/spawn-blocking pattern is used. [Lofty API documentation](https://docs.rs/lofty/0.24.0/lofty/) describes its format parsers.

The initial crate uses a temporary standalone `[workspace]` and lock. The coordinator removes those, adds root/native dependencies, routes the command, applies context checks before/after the reply, awaits shutdown, and updates the feature harness binary default from the worker-local target. The pure `project_metadata(Value)` helper is for compatibility tests/trusted parser output; the bounded production endpoint is `AudioService::dispatch`.

## Compatibility behavior

The reply contains the complete projected `common` object and optional `replayGainMultiplier`, without the parser's `native`/`format`/quality data. Default common fields are `track: { no: null, of: null }`, `disk: { no: null, of: null }`, and `movementIndex: {}`. Covers are skipped. Known native tags are mapped using data extracted from the actual 7.14.0 mapper classes and common-tag multiplicity rules, with its MIT notice retained in `MUSIC-METADATA-LICENSE.txt`. Tests compare the embedded mapping data against those original classes. Using concrete Lofty tag structures retains native tag identities/order that a generic conversion would discard.

Observed behavior retained includes Unicode text; artist/artificial-artists behavior; unique genres/composers/artists versus repeated comments; track/disc totals and zero-to-null handling; date/year insertion; common ReplayGain `{ dB, ratio }` objects; Vorbis ratings and duplicate ISRC handling; and the original seven-bit ASCII decoding of WAV INFO text, including its Unicode corruption. Unknown native tags are omitted because the actual original parser does not put them in `common`. Unknown fields already present in `common` are preserved by the pure handler projection.

ReplayGain walks metadata in JavaScript key order, including numeric keys, arrays and nested objects. It takes the first successfully non-null gain and peak independently, so album gain can precede track gain, and a track peak can clamp that album gain. Strings use the handler's character stripping and `parseFloat` quirks. Ratio objects use the requested property, then `ratio`, then `dB`, with nullish fallback. An object with no such fields yields undefined rather than null, affecting later matches and causing a final JSON null multiplier. Nonfinite arithmetic remains nonfinite until JSON serialization, matching `JSON.stringify`'s null behavior. Positive peaks limit gain to `1 / peak`; zero and negative peaks do not clamp. Tests preserve all object keys and order and allow only `2e-14` relative/absolute tolerance for native floating-point math.

## Resource evidence

Defaults are two jobs, 16 MiB of actual bytes read per request across preflight plus native parsing, 50,000 read/seek operations, ten seconds of cooperative parse time, 4 MiB per metadata block/item allocation, 4,096 native tags/metadata atoms, and a 4 MiB serialized reply. Configuration rejects zero/oversized limits. A counting serializer checks reply size without allocating a second complete JSON buffer. Audio payload sizes are skipped with seeks where possible; total audio file size is not capped at the metadata read limit.

A read cap alone cannot stop an allocation made from a declaration. Preflight therefore runs before Lofty:

- ID3 tag/frame sizes and frame count are checked before native parsing. Compressed/encrypted frames, chapters/table-of-contents recursion, ID3v2.2, extended headers and tag-wide unsynchronization are explicit unsupported variants in this slice.
- FLAC comment blocks and Ogg Vorbis/Opus header packets have size/vendor/comment-count/field-length checks. This guards Lofty 0.24.0 `ogg/read.rs::read_comments`, whose `Vec::with_capacity(number_of_items)` bypasses its item allocation setting. Native ID3/MP4/block allocations also use Lofty's thread-local allocation ceiling, set separately on each fixed worker.
- MP4 boxes are inspected iteratively with a 32-level nesting limit and an atom-count cap. This guards the source-backed `AtomInfo::read -> parse_freeform -> freeform_chunk -> AtomInfo::read` recursion on nested `----` before the expected mean/name checks. `ilst`, mean, and name allocation declarations are capped. Ordinary media data is skipped.
- WAV LIST/INFO and embedded ID3 outer chunk sizes, nested ID3 declarations, field counts and lengths are checked. Lofty's `iff/chunk.rs::id3_chunk` reads the outer chunk before parsing its ID3 header, so both declarations matter.

Portable end-to-end tests verify oversized ID3/FLAC/Opus/WAV declarations and nested MP4 freeforms yield resource diagnostics. A sparse FLAC fixture has a 4 MiB picture block but succeeds with a 128-byte item ceiling and 512-byte read budget. Lofty `flac/read.rs` passes picture blocks to `Block::read` with its read predicate false when `read_cover_art(false)`; `flac/block.rs` then seeks over their bytes without allocating. The test verifies that actual path.

The deadline is cooperative, not a hard CPU/process timeout. Parser/text conversion work between reads cannot be preempted; read/seek/tag/depth/block bounds constrain it, and expiry is checked again when parsing returns. Filesystem metadata/open/read/seek calls can block in the OS and cannot be interrupted by a token. Network filesystems, device/path replacement races, native allocator behavior, and final TLS/destructor joins are not given hard wall-clock guarantees. These remain explicit OS/integration limits. This is not an exhaustive audit of every dependency allocation or a process memory sandbox.

## Checks and remaining scope

Verified with Rust 1.99.1, pinned oracle Node 22.21.1, original `music-metadata` 7.14.0, and system FFmpeg 6.1.1 for test fixture generation. Twelve portable Rust tests pass with no ignored tests. Four Node tests pass with no skips: 220 pure source-handler ReplayGain cases, original schema comparisons, empty/missing/malformed/non-file replies, and 19 nonempty real tagged/untagged files across MP3, FLAC, OGG, Opus, M4A and WAV. Real cases include ID3v2.3 slash lists, ID3v2.4 multi-value text, repeated artists/genres/comments, numeric genre syntax, Unicode, M4A freeform ReplayGain, malformed gain strings, order/clamping, ratings and unknown native tags. FFmpeg is only a test fixture generator; a missing generator marks real format coverage unexecuted and does not count as parity.

```sh
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-audio/target cargo test --manifest-path crates/freeshow-audio/Cargo.toml
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-audio/target cargo build --manifest-path crates/freeshow-audio/Cargo.toml
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=crates/freeshow-audio/target cargo clippy --manifest-path crates/freeshow-audio/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path crates/freeshow-audio/Cargo.toml --check
/home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node --test tests/parity/audio-metadata.test.mjs
npx prettier --config config/formatting/.prettierrc.yaml --check tests/parity/audio-metadata.test.mjs docs/migration/workers/audio-metadata.md
```

Nineteen fixtures establish the observed subset, not complete equivalence to music-metadata. MP3 ID3v1/APEv2 priority, ID3v2.2, flagged/recursive ID3 variants, complex mapped ID3 frames such as POPM/UFID/IPLS/PRIV, and ID3 podcast flag semantics are explicit unsupported variants when detected. Other source-parser formats (AIFF, raw ADTS AAC, ASF/WMA, Matroska, APE, WavPack, DSD and others), mixed/duplicate tag containers, chained Ogg streams, malformed-but-tolerated structures, alternate native encodings, unusual MP4 atom data types and rare common-tag postprocessing are not exhaustively supported/proven. Unknown formats can still be indistinguishable from corrupt inputs. QuickTime's known null fallback is retained for `qt  ` brands. Windows/macOS, 32-bit systems, packaged distribution, real-world music libraries and broad parser fuzzing remain unverified. Shared runtime/frontend integration belongs to the coordinator.
