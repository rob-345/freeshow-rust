# Configured fixed-cover HTTP transport

This slice adds an optional native cover source to `freeshow-remotes`. It does not connect the selected-library cover, native folder ALERT publications, or application IPC. Those remain host integration work. Tests use inert native fixtures and loopback TCP; they establish no LAN, playback, image-decoder, or performance claim.

## Native API and authority

`RemoteService::start_with_cover(payload, context, Option<Arc<dyn CoverSource>>)` freezes a source for the new remote run. Ordinary `start` delegates with `None`, preserving the existing exact `/nowplayingcover` 501 response. `CoverSource`, `CoverControl`, `CoverFuture`, and `PreparedCover` are public native Rust interfaces, without deserialization or renderer-selected paths. Reexports `CoverState`, `CoverFileStat`, and `CoverErrorRendering` come from `freeshow-cover-http`.

```rust,ignore
pub trait CoverSource: Send + Sync + 'static {
    fn error_rendering(&self) -> CoverErrorRendering; // default Deferred
    fn prepare(&self, control: CoverControl)
        -> CoverFuture<'_, Box<dyn PreparedCover>>;
}
pub trait PreparedCover: Send + 'static {
    fn state(&self) -> CoverState;
    fn read_at(&self, offset: u64, buffer: &mut [u8])
        -> std::io::Result<usize>;
    fn before_headers(&mut self) -> CoverFuture<'_, ()>;
    fn finish(self: Box<Self>) -> CoverFuture<'static, ()>;
}
```

`CoverControl` contains a native `CancellationToken` and `std::time::Instant` deadline. No URL, path, query, headers, or filename reaches `prepare`. The host must capture the frozen full runtime context and selected-library authority, use its bounded ordered filesystem phase and guarded held-cover implementation, and revalidate that authority in `before_headers`. The source's fixed `Audio/NowPlayingCover.png` path and actual metadata must be resolved by that host. The fixture example's CLI-selected file and caller-supplied fixture mtime are test authority only.

The host must queue the actual source folder ALERT before returning its prepared observation, while retaining its separate publication receipt in the holder. That private renderer ACK must not gate empty 404 or response headers. `finish` must settle every accepted notification and native resource even when the HTTP request, body, or run has been canceled. HTTP `CoverControl.cancel` is **not** authoritative library-context/publication retirement: it is also canceled by normal response-body disposal. Native publication retirement needs its separate trusted context/terminal authority.

## HTTP behavior and error policy

Configured routing uses the pure source planner on all REMOTE, STAGE, and CONTROLLER ports. GET and HEAD matches prepare the fixed source; automatic OPTIONS returns `GET,HEAD` without lookup. Raw nonmatches retain ordinary static fallthrough, including encoded route spellings. Query parameters never select a file. Source Missing produces an empty 404; a failed preparation or `CoverState::Failed` is an explicit 503, never a fabricated Missing observation. A zero-length or opaque non-PNG file is served as the original fixed PNG-named route would serve it; no PNG validity claim is made.

The host must choose its actual error-rendering policy. Default `CoverErrorRendering::Deferred` returns explicit 501 `cover_error_rendering_unsupported` for the planner's 412/416 development finalhandler bodies. To enable source production 412/416 HTML, a native source overrides `error_rendering` with `CoverErrorRendering::Production`. This is a native startup decision, not a request header. Development stack-bearing finalhandler HTML remains unsupported. Successful/range/cache/missing responses otherwise use the planner's headers directly. A sealed missing-HEAD marker repairs only Axum/Hyper's inferred `Content-Length: 0`, preserving the source's absent length on that case.

Request projection follows the relevant Node 22 HTTP/1 semantics: Latin-1 field values, trimmed HTTP whitespace, first singleton duplicate, `cookie` joining with `; `, other scalar duplicates joining with `, `. Array-valued request `set-cookie` is explicitly unsupported (400). Raw fields are bounded before projection; the pure planner also checks the projected UTF-8 budget. Arbitrary native response-header maps are never accepted.

## Ownership, limits, and failures

The existing one remote runtime/thread and bounded TCP tasks remain. Cover work reuses the existing fixed file pool, starting it even when only a cover source is configured. Defaults are two fixed file workers, eight queued jobs, and ten whole requests shared with static files; existing native configuration limits remain at most four workers and 64 queued jobs. No task, thread, or runtime is created per cover request. Bodies use at most 32 KiB reads and a two-chunk channel; the pure planner caps fixed cover bodies at 4 MiB, 32 request fields, 16 KiB aggregate headers, 8 KiB per header value, and its existing target/range bounds. HTTP timeout is the existing bounded `body_timeout` (default ten seconds).

A whole request permit is held jointly by the file worker and HTTP body until both actual native `finish` and actual body disposal occur. Enqueuing/yielding the last chunk does not prove body disposal or peer receipt. A body deadline cancels only its trusted TCP connection; run cancellation cancels all connections. Slow-channel backpressure uses the existing bounded file-worker retry pattern, with cancellation/deadline checks and 2 ms sleeps only while the channel is full, not an idle poll.

Entered `prepare`, `before_headers`, and consuming `finish` futures are driven on the already-owned runtime by the fixed file workers. They are never select-dropped. An `Err` from `prepare` therefore must mean any work it accepted has really settled; no holder exists for the HTTP worker to finish afterward. Any returned holder is finished on every ordinary response, cancellation, revalidation/error, and abandoned HTTP path. Native trait callbacks are trusted nonpanicking code; a panic inside a consuming native callback cannot certify that callback's cleanup. Ordinary preparation/revalidation errors become bounded diagnostics and explicit HTTP 503. A finish failure is recorded as a bounded diagnostic and returned by stop after actual file/thread drain, without rewriting an already-published HTTP response into a fake 404.

The file pool is owned outside fallible/panicking transport polling. Transport failure first cancels admission/connections, then drains network work and actual file futures while the runtime is still alive, then joins the file threads. Stop observer timeout/drop retains the same active owner and thread for retry. A held native finish receipt can intentionally keep stop pending; the host must independently retire publications during context/terminal shutdown. OS reads and native filesystem/callback work are cooperative and cannot be preempted by these deadlines. Final native thread/TLS destructors may still delay a synchronous join after its done signal. Crash recovery and the existing service-drop emergency behavior are not strengthened by this slice; production owners must await explicit stop/shutdown.

## Evidence and integration

`src/cover_tests.rs` adds eight actual loopback/lifecycle tests to the seven existing remotes tests. They cover all three ports; ranges/cache/HEAD/OPTIONS; distinct missing/failed/empty/deferred outcomes; held independent finish; aborted entered preparation under normal stop and injected transport error/panic; revalidation and finish errors; two entered workers plus eight queued jobs and excess rejection; real slow socket body abort/deadline/run cancellation; raw-header limits and a genuinely settled preparation failure. Held gates witness actual entered work, and final cleanup checks holder drops and returned permits. Timeout is only an observation; successful real stop/join and cleanup are required afterward.

`tests/fixed-cover.test.mjs` runs the immutable original `/nowplayingcover` callback with real Express/send/fs and compares it with the native `cover-serving-parity` example over TCP. Source `servers.ts` is read with `git show 2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/servers.ts`, CRLF-normalized, and SHA-256 checked as `2b8c81b56e77b754dc617e31777e4b3e1d9d15b368c369efc9d58030c1af4b41`. Node 22.21.1 matches the pinned Electron dependency. The corpus compares 84 complete status/header/body responses across three ports, excluding only Date and connection headers: ordinary/opaque/empty/missing files, true pre-epoch mtime, case/trailing/query handling, ranges, conditionals, duplicate headers, Latin-1 whitespace, and production errors. Encoded static fallthrough is a separate status/body assertion, not full static-header parity. Development errors and unconfigured 501 are explicit capability assertions, not parity counts. Fixture shutdown requires the native joined receipt and process exit; observer timeouts kill and reap failed fixture processes for test cleanup.

Build both examples, then run the new source suite with `FREESHOW_REMOTES_COVER_BIN` pointing to `target/debug/examples/cover-serving-parity` (add `.exe` on Windows). The existing `tests/parity/remotes.test.mjs` uses `FREESHOW_REMOTES_BIN` and the ordinary `remotes-parity` example. Neither suite skips absent native binaries. The coordinator must update shared workspace/native lockfiles for the new path dependency `freeshow-cover-http`; this slice changes only the remotes manifest. Linux has real transport evidence; Windows/macOS compile checks do not establish native filesystem or HTTP runtime parity there.
