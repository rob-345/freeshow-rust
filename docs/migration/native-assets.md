# Native local media delivery

The native host now connects the bounded asset server and typed grant planner to the retained Svelte interface. The original Electron source remains pinned at `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`. This milestone supplies local media bytes and a synchronous URL resolver; it does not establish complete media, codec, protected-content or thumbnail parity.

## Publication and ordering

`NativeAssets` has four whole-pipeline leases. Two fixed filesystem workers serve bounded authorization/read work independently of the library IO owner; the existing Tokio runtime owns the loopback HTTP listener. Defaults admit eight HTTP connections and eight queued filesystem jobs, use 16 KiB chunks, and retain at most 1,024 grants / 8 MiB of registry entries. Host authorization batches contain at most 128 paths / 1 MiB of path bytes and share a ten-second cooperative deadline. No per-request runtime, process or unbounded thread pool is created.

The host constructs candidates at these explicit boundaries:

- Full committed `SHOW` replies and successfully committed `SAVE.showsCache` snapshots. Backup-only snapshots are excluded. Reset saves publish even though their successful response uses SETTINGS rather than SAVE2. Asset capacity or planning failure never prevents the underlying save.
- Accepted native `OPEN_FILE2.files`, completed native file drops, committed media-location results and accepted nonfolder browse records. FILE_INFO request paths do not authorize media. Drop Enter/Over/Leave and obsolete gestures cannot publish grants.
- Final media destinations returned after ordered import effects commit. Private staging paths never become byte grants. Windows imports publish the exact native spelling and the source converter's forward-slash alias when needed.

Every candidate and publication retains the captured runtime/library generation/data path. The host checks the context before and after asynchronous work. An event's optional `nativeAssets` envelope is accepted before the original retained callback runs; its source payload and listener correlation are preserved. Registry validation is atomic. Stale envelopes cannot populate a new library's registry.

New output windows receive a snapshot of already authorized grants in their STARTUP.TYPE response. Subsequent output deliveries include a bounded snapshot only after the native registry changes. Opaque Arc-backed revisions make the unchanged check constant-time without walking paths or doing filesystem work on a cue. Output JSON is not recursively scanned to authorize new paths. A library switch revokes server grants and streams and clears renderer lookup state through the new context. Quit closes asset admission and awaits server cleanup after the final library write fence.

## Local paths and browser behavior

The retained media encoder first performs an exact synchronous lookup. Committed paths use the existing source-compatible local-path normalization; accepted native paths preserve literal percent signs. Remote/data/blob URLs and bundled relative resources keep their existing handling. An unregistered absolute local path produces an explicit diagnostic and no invented byte URL. There is no public authorize-path IPC command.

The server binds only ephemeral IPv4 loopback and serves opaque capability IDs. Allowed browser origins are the Tauri production origins and the configured development origin, excluding `null`. The real Linux WebView reports `tauri://localhost`. GET/HEAD and single ranges stream through bounded reads, with no video-sized materialization. File identity is checked when opening a stream. Replacing a file requires renewed native authorization; same-inode edits remain possible. Reauthorizing a source key replaces its older grant and cancels old streams. Known protected-cache roots, including canonical symlink targets, cannot acquire plaintext grants. Protected decryption/licensing is still unimplemented.

Local image canvas helpers set anonymous CORS before assigning their loopback source. The server returns the exact allowed origin. The real WebKit check also loads the same image without CORS and verifies that export raises SecurityError, proving the successful canvas case actually depends on correct origin handling.

## Evidence and limits

The independent service/planner/preparation tests are in [local assets](workers/local-assets.md), [typed candidates](workers/asset-plans.md) and [thumbnail preparation](workers/thumbnails.md). The latter is integrated as preparation only: retained canvas capture, final cache effects and thumbnail-producing folder listings are pending.

The full local suite passed 173 Rust tests (including two compile-fail boundary tests), 181 pinned-Node source/bridge tests and 20 frontend tests, plus 38 native host tests. The full 114-step desktop workflow, four GTK menu checks and actual drops at display scales 1 and 2 pass. Five ignored Rust entries are child-process fixture entrypoints invoked by the passing supervisor tests. Host tests cover publication after commit, backup/reset distinctions, asset overload without blocking saves, typed fields, literal-percent filenames, import destinations, accepted drops/listings, stale contexts, revocation and operation while the library owner is blocked.

`tests/desktop/assets_probe.js` runs in the actual retained WebView. Its receipt includes exact PNG bytes/hash, GET/HEAD/range responses, decoded opaque black pixels, successful canvas export with CORS, the negative CORS check, one-second PCM WAV seek/playback, and VP8 WebM seek/playback with decoded blue pixels. The native import callback observes its URL before retained conversion. A newly created output checks the same exact URL and context after its actual retained output role mounts. These are specific Linux software-rendered fixtures, not exhaustive codec, device, frame-timing or cross-platform GUI certification.

Explicit gaps include protected providers, templates/overlays and other typed authorization boundaries, universal external file-change/symlink-retarget behavior, registry reconciliation after failed renewal, MIME/codec breadth, long-lived playback/recovery, blocked OS-call cancellation, native Windows/macOS browser behavior, physical outputs and full renderer capture/thumbnail lifecycle. The original packaged file loader returns 200 for requested range bytes in this environment; the native HTTP interface intentionally uses the application's protected-protocol 206/range contract. Bounds and HTTP transport are deliberate differences from unrestricted Electron file loading. No application speedup is inferred from successful byte delivery.
