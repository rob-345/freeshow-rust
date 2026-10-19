# Native FFmpeg download host

Coordinator integration now routes main-window `FFMPEG_DOWNLOAD` through this host, uses the actual media resolver cache API, hooks library-context changes, and drains downloads/media before terminal library write admission closes. All 15 Linux host tests and strict native Clippy pass. The **148-step Linux editor** verifies two retained requests, before/after library selection, using an inert gzip served by one owned loopback fixture. Both install identical bytes with mode `0755` into the same private userData/bin, emit the original completion/progress, ignore renderer URL/path overrides, and leave no staging entries or live request leases. The fixture executable is never run. Real upstream TLS/redirects, downloaded binary usability, macOS signing and foreign platform execution remain unverified; the new native test target is enabled for the next complete CI run.

This module completes the host-side installation boundary of `freeshow-ffmpeg-download`, against immutable Electron `2d94defcf98175e4f5c9504e7f4fd1d33c35c975` (`src/electron/streaming/ffmpegManager.ts::downloadFfmpeg` and `responsesMain.ts::FFMPEG_DOWNLOAD`). It is not wired into production in this commit. It preserves the retained Svelte request/reply/progress contracts. No upstream executable was downloaded or executed during development.

## Host API and authority

```rust
let downloads = NativeFfmpegDownloads::new(
    library.clone(),
    media.clone(),
    app_user_data.clone(), // fixed native application userData, never selected dataPath
    ffmpeg_download::Config::default(),
    Arc::new(move |event| publish(event)), // bounded, nonblocking, context-aware host delivery
)?;
let ticket = downloads.begin(request, "main")?;
let result = ticket.finish().await?;
// Retain/log result.diagnostics before consuming it.
let events = result.into_events()?;
```

The host owns one instance and routes only `MAIN/FFMPEG_DOWNLOAD` from the main window. The source handler ignores its payload; this implementation discards it before queuing. No request field chooses a URL, executable, filesystem root, chmod mode, command or test override. `RuntimeContext` is mandatory and is checked as a complete runtime ID, generation and data path at admission, the library's ordered commit, progress delivery and result consumption. `context_changed(&current)` promptly cancels stale preparation/children and revokes ready results. Ordered commits and final context validation independently reject stale installation even without that notification.

Supply the same application user-data path as `Runtime.stores.app_data`, and configure the actual `MediaService` resolver with its `bin` folder. `prepare_root` executes on the existing ordered library filesystem owner: it verifies that application-root identity, creates `bin`, rejects redirected directories, and captures root/bin identities. Installation revalidates those identities and the library context on that same owner, verifies the sealed accepted artifact, applies source `0755` on Unix, then renames to `ffmpeg`/`ffmpeg.exe`. Selecting a different library never selects another binary destination. Unix identity uses device/inode; the non-Unix fallback uses directory creation time and has not received Windows filesystem execution evidence.

Platform/architecture aliases select the foundation's pinned table: b6.1.1 for Linux x64/arm64/arm, macOS x64/arm64 and Windows x64, b6.0 for Windows ia32. Unsupported combinations fail before mkdir/network work. `ControlledTest::loopback(platform, arch, numeric_loopback_http_url)` and optional absolute `mac_tools(xattr, codesign)` are native-only constructors without `Deserialize`; no production IPC should expose them. Production uses the source HTTPS URLs, bounded HTTPS redirects and the foundation's rustls/ring transport.

## Effects, results and shutdown

Two fixed async host pumps and two whole-host leases cover queued root preparation, network/gzip work, accepted install, macOS effects, resolver invalidation, cleanup, and ready results. The foundation has two fixed network pumps and two fixed filesystem/DNS/gzip workers; no per-request blocking task is spawned. A third request gets `download_busy`, including when both completed responses remain uncollected. `DownloadTicket::finish()` does not release that lease. Only `DownloadResult::into_events()`, dropping its last owner, or revocation of a fully ready result releases it.

Dropping/canceling a ticket before acceptance cancels preparation and waits within its fixed pump for the real cleanup receipt. Dropping its waiting future after acceptance leaves the accepted continuation on its pump. The accepted artifact guard survives ordered-owner admission/waits, filesystem effects, child waits, the actual resolver mutex wait, and final cleanup. A stale ordered commit still returns its guard to the pump for receipted cleanup. The separate foundation patch adds `PreparedDownload::accept_or_discard(...).await` while preserving `accept`: rejected acceptance retains its original diagnosis and awaits cleanup before returning. Cleanup faults are also sticky service failures; unknown private-directory contents are preserved rather than recursively deleted.

On macOS, source `xattr -cr <destination>` failures are ignored and `codesign -s - --force <destination>` failures are retained as warnings. Child stdout/stderr/stdin go to null. Native cancellation, wait failure or deadline is an explicit failed/canceled operation; each started child is killed and actually reaped before its owner advances. There are at most two child processes, one per host pump. After any successful rename, **the actual `MediaService::clear_ffmpeg_path_cache().await` runs even if mac work, context or CLOSE subsequently rejects source success**. It serializes on the real resolver lock, preventing an older in-flight resolver from restoring the old cache after invalidation. Accepted cleanup completes before the success/error progress event and reply.

Progress is outer `MAIN`, inner `MEDIA_DOWNLOAD_PROGRESS`, no listener, with `{url:"ffmpeg",name:"FFmpeg",progress,total:100,status:"downloading"}`. Completion uses `status:"complete"`, progress/total 100; failure uses `status:"error"`, progress/total zero. The reply is outer `MAIN`, inner `FFMPEG_DOWNLOAD`, the original listener ID and `{success:true}` or `{success:false,error:<message>}`. Foundation progress is a bounded latest-value watch (at most 256 changes plus completion), not a claim of Node stream chunk cadence. Error text from native OS/resource diagnostics is explicit rather than a byte-for-byte match for JavaScript exception strings. Errors after cancellation/context loss do not publish source success.

CLOSE integration must keep Tokio, library write admission, publisher authority and the actual media resolver alive during this drain:

1. Stop renderer admission and call `downloads.shutdown()` concurrently with `media.shutdown()` so a resolver child holding its cache mutex can cancel/reap.
2. Collect every service's real drain result; do not return early on one error or substitute a timer for a drain.
3. Only after those owners drain call `library.finish_writes()` and the shared final drain. `finish_writes()` closes write admission permanently, so calling it before accepted installations finish would reject their ordered effects.

`shutdown()` closes host admission, cancels preparations/mac children, revokes completed/uncollected results, and awaits all host tasks plus real foundation owners. It collects task/cleanup errors rather than stopping at the first. A timeout returns `download_shutdown_timeout`, retaining task handles and live leases for retry; retry never polls an already-consumed join handle. Leases from an interrupted pump cannot be force-released before the actual foundation owners exit. Cleanup faults remain visible in `status().failures` / `status().foundation.failure` and subsequent shutdown results even after all owners have drained. Call explicit shutdown while the runtime is alive; ordinary `Drop` initiates cancellation and retained fixed-pump cleanup, and is not a synchronous final-drain receipt.

## Bounds and evidence

Native configuration limits shutdown/tool deadlines to 30 seconds each (defaults 5/10), listener IDs to 4096 bytes, application root to 32690 UTF-8 bytes, controlled platform/architecture to 32 bytes, diagnostics to 8 × 1024 UTF-8 bytes per result and 4 sticky host diagnostics. The foundation retains compressed/uncompressed/header/member/redirect/operation/network/idle/deadline limits documented in [workers/ffmpeg-download.md](workers/ffmpeg-download.md). Its default compressed/uncompressed budgets are 128/512 MiB with streaming 32 KiB buffers. Network/file queues, host queue and in-flight results remain bounded by the two admissions. Filesystem/kernel calls can outlast a deadline; the owned job stays admitted until its actual call/cleanup returns. No timeout authorizes releasing that work.

Focused Linux tests use actual loopback TCP, actual gzip and private files, actual `LibraryService` ordered commits and actual `MediaService` cache/executable probes. Generated harmless shell fixtures are the only executable test content. They establish:

- exact installed bytes, replacement, Unicode application path, Unix mode, source event/reply shape, fixed app root and ignored payload;
- two completed/uncollected leases, overload, explicit cancellation, caller Drop before/after acceptance, live CLOSE and terminal retry;
- blocked ordered root preparation and accepted installation, successful accepted replacement during CLOSE, generation changes during preparation and before an accepted commit, and ready-result revocation;
- actual cached-path invalidation, including an in-flight resolver publishing its old candidate before the host obtains its cache lock;
- macOS command warning semantics under controlled Unix executables, timeout/CLOSE killing and `/proc` evidence of reaping, and invalidation after a canceled post-rename child;
- cleanup-fault preservation, sticky diagnosis and complete draining of the other admitted owner.

The existing immutable-source suite `tests/parity/ffmpeg-download.test.mjs` executes the original functions/handler with controlled HTTP/real gzip/filesystem boundaries, compares 25 URL cases, 26 downloads, 20 progress samples, 7 resource cases and 2 chunk-dependent padding cases, and checks source effect ordering. The new host tests complement that source oracle with real install/cache/ownership behavior; they do not claim independent hardware/production-download evidence.

Actual GitHub TLS/download availability, installed FFmpeg functionality/codecs, native Windows replacement semantics and real macOS xattr/codesign execution remain unverified. Source TLS trust differs from rustls web PKI roots. Existing foundation Windows/macOS cross-builds are blocked by unavailable target C compilers for ring; this host has been compiled/tested on Linux only. Non-Unicode context/root paths, symlink/redirected download roots, excess resources and gzip padding followed by another member in a later source chunk are explicit bounded/unsupported cases. Native cleanup faults fail closed where source's final force-rm ignores an error. Byte installation never establishes encoding or playback parity.

Cross-platform source testing compares the original actual `chmod(0755)` invocation with the native install intent. When the test simulates a Unix target on Windows, Node still uses the real Windows filesystem and cannot expose Unix execute bits via stat; Unix hosts retain exact effective-mode assertions. Windows native strict checks also require the same cfg guard on the Value test import as on its actual Unix-only uses. No failure branch was skipped.
