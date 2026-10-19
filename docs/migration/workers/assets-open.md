# Native registered-media descriptors

This implements the descriptor/anchor seam in [the now-playing host plan](now-playing-host-plan.md). It adds no renderer command, grant issuer, metadata/image decoder, now-playing output write, default-path fallback or source success ACK. Shared main/bridge/manifests remain coordinator-owned. Existing asset URL/HTTP authorization and reads retain their policies.

## Native API and ownership

```rust
// No arbitrary path authorization: each lookup must already exist in this registry.
AssetServer::open_registered_media(&Context, RegisteredMedia::SourceKey(key), Control)
AssetServer::open_registered_media(&Context, RegisteredMedia::GrantId(id), Control)
// Host wrapper checks LibraryService's exact RuntimeContext before/after admission.
NativeAssets::open_registered_media(&LibraryService, &RuntimeContext, source_key, Control)
```

Each async call returns `AuthorizedMediaRead`, a held read-only `File` duplicate plus private `MediaAuthority`. The preferred whole wrapper exposes `file_mut()` and `authority()`. For existing native parser boundaries, consuming `into_parts()` returns `(File, MediaAuthority)`. Neither authority, descriptor nor observation implements Deserialize or has a public path/File constructor. Lookups do not activate contexts, authorize unregistered paths, normalize another spelling, interpret a URL or create grants. An exact genuinely registered file-URI key remains valid; a URL or renderer filePath alone is not authority. Grant IDs from another AssetServer do not select that server's registry.

Example for a trusted native continuation (parser/image calls are other bounded services, not request-thread filesystem reads):

```rust
let read = assets
    .open_registered_media(&library, &captured, &source_key, Control::default())
    .await?;
let (file, authority) = read.into_parts();
let before = authority.revalidate(Control::default()).await?;
// Retain `authority` + the host's whole-operation lease throughout this sequence:
// audio.reserve_opened()?.submit(file, source_name, FormatHint::FromSourceName,
//     cancel)?.finish().await -> OpenedRead
// OpenedRead::into_parts() returns common result, File and parser observations.
// Revalidate authority; move the SAME File to the existing cover worker next.
// Explicit rewind/sequential reads are mandatory; try_clone may share seek state.
// Revalidate again after cover completion and immediately before publication.
// Publish the ORIGINAL authority.source_path(), not a reopened renderer path.
drop(before);
// Drop File (including all trusted native copies) before dropping authority.
drop(file);
drop(authority);
```

The audio handoff is the independently owned `84928e56` API, documented in `audio-opened.md`; this slice does not add an audio dependency or rewrite its parser. Native `source_name` can be derived from the authorized canonical basename for a format hint without changing original callback source spelling. Actual host composition still needs full-current-context checks at publication, ordered continuation ownership, all parser/image budgets and cleanup.

**A transferred std::fs::File is trusted native authority.** Rust callers can clone it. After splitting, they MUST retain `MediaAuthority` until every descriptor use/copy has actually finished. The API cannot prove disposal of a File retained after its guard was deliberately dropped. No concurrent shared-offset seeks are certified. Main-only/window/command authority remains the future host's responsibility.

## Checks and observations

All open, canonicalization, identity, native-time and anchor revalidation operations use the existing fixed asset IO workers. Native lookup accepts only a current registry entry and keeps its exact source spelling, canonical path and original file identity. It checks full context/live grant before and after actual IO. Replaced/revoked entries and obsolete server/context ownership reject explicitly.

Before/after open and each revalidation, it checks canonical protected-root exclusion, unchanged canonical spelling, a regular file, expected file identity and configured maximum size. Unix opens use O_NOFOLLOW|O_NONBLOCK; Windows uses FILE_FLAG_OPEN_REPARSE_POINT. Revalidation compares the held anchor and a separately guarded current-path observation to the initial identity, length and native modification/change timestamps. Unix uses dev/inode + mtime/ctime seconds/nanoseconds; Windows uses volume/file index + actual FILE_BASIC_INFO LastWriteTime/ChangeTime. Existing Windows dependency features already include these calls; no manifest additions are required.

`MediaObservation` is sealed and retains the same resource lease; it exposes length, same_file and same_version comparisons. It is an observation, not read authorization by itself. `authority.revalidate(Control)` returns an owned observation; retain/drop it deliberately. A new read can begin after a same-inode edit that happened before its own baseline capture, matching existing identity-based grant policy. Changes after that capture reject when observed.

This is **not an immutable snapshot or race-proof pathname sandbox**. Before/after checks do not make ancestor renames atomic, prohibit hard-link aliases globally, or prevent a writer from modifying bytes while restoring observed metadata. The local filesystem demonstrated that multiple very fast same-size writes plus restored mtime can share the original coarse ctime tick. That is an explicit undetected-change possibility, not a content-hash guarantee. The ctime regression waits for an _observed actual filesystem clock tick_ before asserting a changed version. Linux/macOS/Windows filesystem timestamp precision/semantics differ; compilation alone cannot certify them. The registry has no time TTL: revoked/replaced/context-invalid grants are distinct from Control deadlines; no nonexistent grant expiry is claimed.

## Finite resources and shutdown

There are four whole native descriptor/result leases per AssetServer, shared by opens, held descriptors, ready-uncollected replies and retained observations. The accepted worker operation retains that lease/anchor even if its waiting caller drops, cancels or times out. The existing worker/queue/handle limits can reject earlier with assets_busy. Existing four NativeAssets publication leases and HTTP connection limits remain separate finite budgets. Status adds nativeReads and fileWorkersJoined; openFiles counts worker anchors/HTTP handles, not every exported native duplicate or arbitrary trusted clone.

Native open/revalidation Controls cap cooperative operation time at ten seconds. Cancellation/deadline/context change can return while actual OS work is still held; actual ownership remains charged until completion. No per-call thread, task or unbounded wait queue is added. Canonicalization/open/fstat/close and final native/TLS thread teardown cannot be preempted by an async timer.

Existing fixed worker JoinHandles are now retained and actually collected. Logical completion alone does not prove a thread exited. Shutdown closes admission, cancels work, joins those workers, and waits for native authority/observation leases; timeout retains real joins/guards for retry. A caught teardown panic remains sticky across shutdown retry. The host must retain the service owner until explicit shutdown finishes; dropping its final handle is emergency close, not a join receipt. Exported consumers must drain first: the coordinator plans to retire now-playing before assets/audio/images/pipeline/library dependencies. An external descriptor deliberately kept beyond its guard remains outside the service's proof.

## Focused evidence and integration

Linux execution uses one Cargo job, debug0/incremental0 and the existing private worker target. The package tests cover actual bytes, exact keys and foreign grants, full contexts, revocation/replacement, same-inode length/mtime/ctime changes, real protected leaf/parent symlink swaps, four whole ready-result/observation leases, caller Drop/cancel/deadline during held opens and revalidation, context change, honest retained-descriptor timeout, actual worker joins after a held post-completion thread tail, and sticky real worker panic collection. Windows symlink setup skips only actual error1314 (missing native symlink privilege), never the generic file/context suites.

The new `src-tauri/tests/assets_open.rs` tests actual NativeAssets/LibraryService composition: NOW_PLAYING text cannot mint a grant; trusted committed-import publication permits a later exact read; foreign full contexts reject; context retirement invalidates existing authority and requires fresh publication; protected cache selection and canceled reads create no descriptor. They run in a temporary headless manifest with the same existing core/assets/asset-plans/files/thumbnails dependencies. No Tauri/GTK application or GUI is built.

Local checks: **17 asset crate tests** (10 new, 7 existing), **9 native host tests** (3 new, 6 existing), and **9 existing Node HTTP/source suites**, all passing. Strict Clippy for asset all-targets and the headless native test manifest, scoped Rustfmt, Prettier and diff checks pass. Commands use CARGO_BUILD_JOBS=1, CARGO_INCREMENTAL=0, CARGO_PROFILE_DEV_DEBUG=0 and CARGO_PROFILE_TEST_DEBUG=0, and the worktree's existing crates/freeshow-remotes/target cache:

```sh
cargo test -p freeshow-assets --lib --offline
cargo clippy -p freeshow-assets --all-targets --offline -- -D warnings
cargo test --manifest-path /tmp/freeshow-osc-assets-open-check/Cargo.toml --tests --offline
cargo clippy --manifest-path /tmp/freeshow-osc-assets-open-check/Cargo.toml --tests --offline -- -D warnings
cargo build -p freeshow-assets --bin freeshow-assets-parity --offline
FREESHOW_ASSETS_PARITY="$CARGO_TARGET_DIR/debug/freeshow-assets-parity" node --test tests/parity/local-assets.test.mjs
```

The Node check used pinned standalone22.21.1; the temporary manifest is local-only, not a root dependency or committed tool. No dependency versions, Cargo manifests/locks, baseline artifacts or shared tests are changed.

Coordinator integration needs only the existing dependencies and discovery of the new native test. Production main has no descriptor command to register. The now-playing continuation must call this native API, retain authority through sequential Audio/Cover work, validate immediately before publication, and drain its owner before dependency shutdown. The new NativeAssets method is intentionally unused by existing production commands until that host is wired; do not expose it as a renderer grant route. Actual Windows/macOS filesystem execution remains mandatory root CI evidence; no hardware, metadata/cover coherence under an adversarial writer, now-playing UI or full now-playing parity is claimed here.
