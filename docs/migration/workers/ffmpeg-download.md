# FFmpeg download preparation

This slice prepares a private, bounded gzip download. It does not install or execute FFmpeg and does not establish that an encoder works. Shared IPC, the native host, renderer wiring and final installation remain coordinator work.

The source oracle is `src/electron/streaming/ffmpegManager.ts::downloadFfmpeg/getDownloadUrl` and the `Main.FFMPEG_DOWNLOAD` handler in `src/electron/IPC/responsesMain.ts`, read directly from immutable commit `2d94defcf98175e4f5c9504e7f4fd1d33c35c975` (FreeShow 1.6.7-beta.2). Tests transpile and run the actual functions with real Node fetch, streams, gunzip and filesystem operations. Only Electron roots, the final install boundary and macOS command execution are controlled. Archives contain generated inert bytes. No external FFmpeg download, binary execution or production installation was performed.

## Trusted host contract

`DownloadOwner`, `HostDownloadPlan` and `ControlledTestConfig` have native constructors and no `Deserialize` implementation. Do not expose a renderer command accepting their paths or URLs. The original public download request selects neither a path nor a URL.

```rust
let service = DownloadService::start(Config::default())?; // existing Tokio runtime
let owner = DownloadOwner::new(runtime_id, generation, selected_data_path)?;
// Host IO owner first creates the authoritative app.getPath("userData")/bin.
let plan = plan_for_host(platform, arch, trusted_existing_bin_root, owner.clone())?;
let ticket = service.begin(plan, context_cancel, deadline)?;
let mut progress = ticket.progress(); // latest-value watch; clone borrow before await
let prepared = ticket.finish().await?;
// Validate the captured library/runtime context before accepting.
let accepted = prepared.accept(&current_owner)?;
// Move `accepted` onto the actual ordered host IO owner before filesystem work.
let view = accepted.verify_for_install(&current_owner)?;
// Host performs chmod, rename, macOS steps, and cache reset below.
accepted.finish().await?; // private cleanup only; not a renderer success reply
service.shutdown().await?;
```

`verify_for_install` is synchronous read-only filesystem work for that host IO owner, never GTK or a Tokio IO thread. It rechecks captured root/private-directory identity and the completed regular artifact's identity, size, modification and change metadata. Unix identity includes device/inode. A successful check does not replace the host's context/identity checks immediately before its ordered mutation; this preparation API is not a filesystem transaction against a malicious process racing path components.

`PreparedDownload::discard().await` explicitly cleans unused results. Dropping a ticket cancels the job. Pending prepared results, including successful replies not yet collected by a caller, are reaped on cancellation, deadline or shutdown. Once accepted, the actual host owner retains the guard through its final write even if the requester drops, cancels or closes. `AcceptedInstall::finish` or its last-owner drop queues private cleanup; duplicate cleanup waits share the same real worker acknowledgement. Do not detach a rename task while dropping its guard.

The host must preserve the original final effect order:

1. Use the authoritative `userData/bin` root, created recursively on the host IO owner before download. The service neither creates arbitrary parent directories nor touches an existing final binary during preparation.
2. Revalidate the captured context and stage identity on the ordered install owner. On non-Windows platforms chmod the staged file to `0755`, then rename it to `InstallView.destination` (`ffmpeg.exe` on Windows, otherwise `ffmpeg`). Retain the accepted guard through these actual operations.
3. On macOS run `xattr -cr <destination>` and ignore its failure, then `codesign -s - --force <destination>` and warn on its failure. `post_install_commands()` returns exactly these data-only steps and failure policies. No command is run by this crate.
4. Clear the FFmpeg resolver cache after those steps. Source code clears `resolvedPath`; existing resolution coordination is the host's responsibility.
5. Clean private staging on both success and failure. Emit `source_completion(true, None)` only after actual installation and required source effects. A prepared file alone is insufficient. On failure use `source_completion(false, Some(message))`.

Completion plans preserve the source reply `{success:true}` or `{success:false,error}` and `MEDIA_DOWNLOAD_PROGRESS` payload `{url:"ffmpeg",name:"FFmpeg",progress:100,total:100,status:"complete"}` / `{url:"ffmpeg",name:"FFmpeg",progress:0,total:0,status:"error"}`. Missing/empty errors use `Unknown download error`. Diagnostics are capped at 1 KiB of UTF-8. Progress uses the original unclamped `Math.round(received / total * 100)` decision, with no downloading notification for unknown/zero length. The native watch coalesces identical rounded values and retains the latest value, with at most 256 publications per job. It does not reproduce Node's network chunk boundaries or deliver every original per-chunk callback.

## Fixed URL and transport

All URLs use `https://github.com/eugeneware/ffmpeg-static/releases/download/`. The table is identical to source:

| Platform | Architecture    | Release / archive                |
| -------- | --------------- | -------------------------------- |
| darwin   | arm64, x64      | `b6.1.1/ffmpeg-darwin-{arch}.gz` |
| win32    | x64             | `b6.1.1/ffmpeg-win32-x64.gz`     |
| win32    | ia32            | `b6.0/ffmpeg-win32-ia32.gz`      |
| linux    | x64, arm64, arm | `b6.1.1/ffmpeg-linux-{arch}.gz`  |

Other pairs yield the original `Unsupported platform: platform/arch` error (oversized native arguments are rejected before constructing that diagnostic). Production follows bounded HTTPS redirects and rejects downgrade/credentials. TLS uses rustls 0.23.46, ring, webpki roots, certificate verification and safe default TLS 1.2/1.3 versions. No global crypto provider is changed. Installed rustls source bounds individual handshake messages to 65,535 bytes (`msgs/deframer/handshake.rs::MAX_HANDSHAKE_SIZE`) and TLS record payloads to 18,432 bytes (`msgs/message/mod.rs::MAX_PAYLOAD`). The trust store, proxy behavior and TLS error text are not claimed equivalent to Electron/Node fetch. No real upstream TLS handshake or GitHub redirect chain was exercised here.

`ControlledTestConfig::loopback(url)` is an explicit native-only fixture mode. Every initial and redirected fixture URI must use HTTP with a numeric loopback address, no credentials and at most 8,192 bytes. Production has no URL override. The fixture CLI always requires this mode and never downloads the fixed production URL.

HTTP status/body checks match the tested source cases: 2xx with a body proceeds; 204/205 and non-2xx report `Download failed: <status>`. HTTP Content-Encoding other than `identity` is explicitly unsupported; Node fetch transparently decodes it. Common relative/absolute redirects are supported, but universal WHATWG URL normalization equivalence is not claimed.

## Ownership and limits

There are two fixed async network pumps, two fixed filesystem/DNS/gzip threads, a two-job queue and a four-command IO queue. A full-pipeline lease covers queued work, sockets, decompression, a pending/accepted artifact and its actual cleanup. The third live job is rejected immediately. Network connections are polled by their owning pump, without per-request spawned tasks or a Node/process substitute. Compressed data passes through two 32 KiB channel slots; decode output is a fixed 32 KiB buffer. There is no whole-video/archive allocation. Hyper HTTP/1 headers are limited to 100 with a 64 KiB read buffer.

| Bound                                                   | Default     | Hard configurable ceiling |
| ------------------------------------------------------- | ----------- | ------------------------- |
| Compressed payload                                      | 128 MiB     | 256 MiB                   |
| Decompressed bytes                                      | 512 MiB     | 1 GiB                     |
| Gzip header bytes per member, including optional fields | 8 KiB       | 64 KiB                    |
| Gzip members                                            | 16          | 64                        |
| Gzip decode/input operations                            | 4,000,000   | 16,000,000                |
| Redirects                                               | 20          | 20                        |
| Whole pending job age                                   | 120 seconds | 600 seconds               |
| Idle network operation                                  | 10 seconds  | 30 seconds                |
| Shutdown wait                                           | 5 seconds   | 30 seconds                |

Each DNS result is capped at 16 addresses and handled on a fixed worker. OS resolver calls, file writes and `sync_all` are not forcibly interruptible. Cancellation closes the socket/pipe, checks decode work between bounded operations, then drains the real archive/file worker before releasing its lease. A blocked OS operation or retained accepted guard can exceed a drain deadline; shutdown reports `download_shutdown_timeout` instead of claiming termination. The service remains closed, owns outstanding work and permits a later shutdown retry. No unbounded cleanup tasks are spawned. Callers must await shutdown before destroying the runtime.

Private 128-bit random staging directories live under the trusted bin root, with Unix modes `0700` / `0600` and create-new/no-follow artifact opening. Cleanup checks recorded identity, deletes only the owned artifact, then removes the now-empty private directory. It never recursively deletes unknown contents. Directory replacement, a changed artifact or foreign files produce an explicit terminal cleanup fault, stop admission and remain visible through status/shutdown. Lost root authority can leave an orphan in the moved directory; that requires host recovery and is not reported as successful cleanup. Non-Unix identity currently falls back to creation metadata; it has not been certified as equivalent to Unix inode authority.

The bounded gzip parser supports real deflate, CRC32/ISIZE, concatenated members, optional extra/name/comment/header-CRC fields and all-zero padding. Header storage never grows with an unterminated name/comment. Known malformed header/CRC/length/truncation messages match the tested original parser. Other malformed deflate messages use an explicit generic diagnostic and are not all Node-zlib wording equivalents. Source limits are absent; exceeding native resource limits intentionally rejects work the original may attempt.

Actual Node streaming tests show a member after zero padding can be ignored when delivered in one chunk, but a later chunk fails (`stream.push() after EOF` or `incorrect header check`, depending on stream timing). Native code explicitly rejects nonzero data after padding as `download_gzip_padding`, rather than selecting an invented universal result.

## Focused evidence

Ten Rust tests cover actual HTTP/gzip bytes, untouched existing destination during preparation, inert host chmod/rename followed by correct private cleanup, source malformed archives, unknown length/status failures, two held live gzip jobs and immediate overload, caller drop/cancellation, live body deadline/idle timeout, stale owner rejection, accepted guard ownership across a timed-out shutdown, private identity replacement, nonrecursive cleanup failure, dropped prepared results and successful-but-uncollected reply shutdown.

Seven immutable-source suites pass on standalone Node 22.21.1: 25 platform/architecture table cases, 26 source/native loopback download comparisons, 20 separately controlled per-body-chunk progress samples, seven native resource/unsupported cases and two chunk-sensitive padding-source cases. Generated archives include Unicode/inert binary bytes, concatenated/optional-header/zero-padded members, bad CRC/ISIZE, truncation, empty/bad header, redirect, unknown-length, bodyless/error status and macOS ignore/warn ordering. Source final rename is intercepted to inspect bytes/mode and record the install effect; macOS commands are recorded stubs. These are source-function tests, not an Electron release GUI or real platform installation proof.

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo test --manifest-path crates/freeshow-ffmpeg-download/Cargo.toml --locked
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo clippy --manifest-path crates/freeshow-ffmpeg-download/Cargo.toml --locked --all-targets -- -D warnings
cargo fmt --manifest-path crates/freeshow-ffmpeg-download/Cargo.toml -- --check
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets cargo build --manifest-path crates/freeshow-ffmpeg-download/Cargo.toml --locked --bin freeshow-ffmpeg-download-parity
FREESHOW_FFMPEG_DOWNLOAD_PARITY=target/assets/debug/freeshow-ffmpeg-download-parity /home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node --test tests/parity/ffmpeg-download.test.mjs
```

The test binary defaults to `target/debug/freeshow-ffmpeg-download-parity` (`.exe` on Windows) with `FREESHOW_FFMPEG_DOWNLOAD_PARITY` override. The CLI's bounded whole-file read is test-only, restricted to inert fixtures under 1 MiB. Production code streams file output.

Linux tests, build, strict Clippy and formatting pass. Windows GNU cross-check stopped in ring because `x86_64-w64-mingw32-gcc` is absent; macOS arm64 stopped because the available Linux C compiler rejects Apple target flags. No cross compiler was installed. Windows/macOS compilation, filesystem authority, TLS, actual downloads, installation, signing, encoder usability and UI integration remain unverified. The standalone manifest/lock are temporary integration inputs; no shared manifest or host file is changed by this slice.
