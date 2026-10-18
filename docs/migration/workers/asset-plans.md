# Trusted asset grant preparation

`crates/freeshow-asset-plans` prepares `freeshow_assets::NativeGrant` candidates synchronously from an explicit trusted source. It performs no filesystem calls, spawns no threads, starts no runtime, mutates no source JSON, and issues no URLs. A candidate does not establish file existence, permission, decryption, or playback. The existing `AssetServer::authorize_batch` performs bounded filesystem authorization afterward. Existing assets service/registry, shared manifests, IPC, bridge, Svelte helpers, and native entrypoints are unchanged in this slice.

## Host API

```rust
use freeshow_asset_plans::{AcceptedNative, AcceptedPath, Committed, Planner,
    Protection};

let classify = |source: &str| captured_protection_lookup(source);
let plan = Planner::default().plan(
    Committed::show_reply(&asset_context, &validated_show_reply, &classify)
)?;
let authorized = assets.authorize_batch(&plan.context, plan.grants, control).await?;
// Revalidate the host/library context; publish authorized grants before the
// original payload reaches retained renderer callbacks.
```

`Planner::new(Limits)` configures bounds. `plan(...) -> Result<Plan, Error>` accepts either `Committed` or `AcceptedNative`, with an explicitly supplied `freeshow_assets::Context`. Context conversion, owner commit, role checks, stale-result rejection, and publication ordering belong to the host. The planner only validates bounded context shape and preserves it. Native preparation from already validated readonly SHOW/browse results does not require another queued library-owner mutation behind ZIP work. Mutating SAVE/import/locator side effects must commit before grants activate.

Committed constructors:

- `Committed::show(&context, &full_show, classifier)` accepts an already normalized full Show object.
- `Committed::show_reply(&context, &reply, classifier)` accepts successful `{id,content:[id,Show]}`. It also handles the retained `loadSingleShow` legacy form where `content[1]` is one serialized `[id,Show]` string with a truthy Show name. Only that documented field is parsed, with a hard 1 MiB limit and the remaining container/operation budget. Corrupt/unsupported shapes fail explicitly.
- `Committed::save_snapshot(&context, &snapshot, classifier)` walks only `snapshot.showsCache`. `SAVE.SHOWS`, `MAIN.SHOWS`, and `REFRESH_SHOWS2` contain trimmed metadata and are not full media-bearing show sources. A snapshot with only `SHOWS` prepares no candidates.

The mandatory classifier is `&dyn Fn(&str) -> Option<Protection>`. `Some(Plain)` is an explicit host assertion that the source is ordinary plaintext media; `Some(Protected)` and `None` produce bounded diagnostics and no plaintext candidate. The callback receives the **exact original source key**, including any `file://`/percent spelling. Its protection lookup must account for the host's normalized-path/protected-provider metadata when relevant. It must be bounded, nonblocking, and use the captured authoritative snapshot; it must not perform filesystem or provider/network calls. The planner bounds its invocations and records `classifier_calls`. A `freeshow-protected://` prefix overrides a plaintext classifier without invoking it. The planner does not invent an `encrypted` JSON field, infer ciphertext from extensions, or assume that a cached provider hash is plaintext. The host's canonical protected-root exclusion during actual authorization remains necessary for symlinks/races and explicit native targets.

Accepted-native paths use `AcceptedPath::new(source_key: &str, native_path: &Path, classification: Option<Protection>)`. These borrowed wrappers, `Committed`, `AcceptedNative`, and `Source` have no `Deserialize` implementation. Their private internals force the host to choose the source constructor explicitly. Compile-fail tests guard this boundary. There is no public renderer authorize-path command. The JSON-lines CLI is a separate trusted test boundary only.

| Constructor                                                                  | Native trust boundary and provenance                                                                                                                                                                                                                                                                                                                  |
| ---------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `AcceptedNative::selection(ctx, &[AcceptedPath])`                            | Actual accepted `OPEN_FILE2.files` selections; `NativeSelection`. Request filters and arbitrary renderer path strings are not selections.                                                                                                                                                                                                             |
| `AcceptedNative::drop_files(ctx, &[DroppedFile { path, is_directory }])`     | Completed native Drop after metadata, gesture and library validation; `NativeDrop`. Directories are skipped. Enter/Over/Leave/cancelled/stale gestures are not this source.                                                                                                                                                                           |
| `AcceptedNative::located(ctx, Option<AcceptedPath>)`                         | Successful committed `LOCATE_MEDIA_FILE.path`; `LocatedMedia`. Null/error replies use `None`. Original and located spelling aliases can be provided separately by the host where needed.                                                                                                                                                              |
| `AcceptedNative::browse(ctx, &[BrowsedFile { path, is_folder, thumbnail }])` | Accepted native browse records only. Nonfolder original files use `NativeSelection`; explicitly validated existing thumbnails use `GeneratedThumbnail`. Folder records and their hypothetical thumbnails are skipped. No recursive folder-member strings are walked. Arbitrary `FILE_INFO(path)` request fields do not establish this trust boundary. |
| `AcceptedNative::committed_imports(ctx, &[AcceptedPath])`                    | Final native install/existing destinations after ordered import effects commit; `CommittedLibrary`. Never pass private staging paths or uncommitted import plans.                                                                                                                                                                                     |
| `AcceptedNative::generated_thumbnail(ctx, AcceptedPath)`                     | An actual generated/validated output; `GeneratedThumbnail`. A thumbnail intent is not an existing output.                                                                                                                                                                                                                                             |

The host must bound/validate its existing outcome collections before wrapping them, filter intended media records as appropriate, and retain a bounded lease across planning, authorization, and publication. The planner then bounds its own traversal/output. Do not turn the test CLI's JSON format into application IPC.

## Exact extraction and spelling

The pinned oracle is source commit `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, version `1.6.7-beta.2`.

`electron/utils/files.ts::bundleMediaFiles` reads `Show.media` using `media.path || media.id`, then slide items only when `item.type === "media"`. This fallback, including JavaScript truthiness, and exact item-type condition are preserved. A truthy malformed non-string path does not fall through to `id`; it receives a shape diagnostic. Object iteration visits canonical numeric index keys first, then other insertion keys, matching JavaScript `Object.values`. Serde's `preserve_order` feature is already used by the integrated core and is requested by this standalone crate as well.

The planner deliberately covers additional real rendering consumers omitted by that exporter: `Show.slides[*].settings.backgroundImage`, audio/video `Show.slides[*].timeline.actions[*].data.path`, and audio/video `Show.layouts[*].timeline.actions[*].data.path`. The tests execute the actual `showActions.ts` background-selection block and `TimelinePlayback.resolveMediaPaths` method, with only their store/locator boundaries mocked. Other action types, layout background/audio IDs, text item `.src`, unknown `.path`/`.src` fields, custom nested objects, and arbitrary strings are never traversed. Templates/overlays/project paths outside these explicit Show fields need their own trusted typed boundary; this collector does not recursively scan a SAVE payload.

Exact lowercase `http://`, `https://`, `data:`, and `blob:` sources are skipped without copying/decoding their payload; the retained helper handles them unchanged. Relative bundled resources remain with the original encoder. They are not resolved against the host process cwd. A protected URI produces an explicit `asset_plan_protected` diagnostic. Unknown local classification produces `asset_plan_unclassified`.

Committed local targets reuse the already source-verified pure `freeshow_files::paths::normalize_local_path`: file-URL conversion/fallback, then JavaScript-compatible `decodeURI` behavior. `NativeGrant.source_path` retains the exact source string, including URI query/hash/percent spelling and Unicode normalization form. Native accepted paths are separately authoritative and are **not decoded**: a selected filename literally containing `%20` stays that exact native filename. Exact duplicate source keys targeting the same native path deduplicate; different source keys targeting one file remain distinct aliases. No case/NFC folding occurs. Conflicting target/protection facts for one source key abort with `asset_plan_conflict`, returning no partial plan that could expose a denied path.

## Work and output bounds

Defaults: 128 shows; 4,096 entries per typed container; 8,192 typed containers; 100,000 charged traversal/sort operations; 4,096 string references; 32 KiB per local source/native path; 4 MiB aggregate local input-path bytes; 1,024 candidates; 8 MiB accounted candidate bytes. Candidates account for copied source/native UTF-8 bytes plus fixed per-entry overhead. Seen keys borrow input strings, so they add no unbounded string copies. Counts, path sizes, aggregate bytes, and operation charges are checked before candidate insertion. Known oversized show maps fail before traversing shows or invoking classifiers. Ordered-object scratch allocations are bounded by container size and charged a comparison budget before allocation. Traversal depth is fixed by typed fields, rather than JSON nesting depth. The one legacy JSON parse has its separate hard byte/depth bounds.

Diagnostics default to 64 entries/64 KiB aggregate UTF-8 bytes. Each source-key excerpt is at most 512 bytes and ends on a valid UTF-8 boundary. Extra diagnostics increment `stats.diagnostics_omitted`; even a zero-sized diagnostics budget retains these omission/protected/unclassified counters. Error code/field/message are static bounded strings. Configuration has hard ceilings; invalid or exhausted limits return `asset_plan_limit` with no partial plan. Malformed typed containers return `asset_plan_shape`. Non-Unicode explicit native paths and invalid native targets produce bounded diagnostics, without filesystem probing. Large unknown subtrees are ignored without walking them. Already owned input JSON/outcome allocations belong to their original host/service admission bounds.

## Verification

Eight synchronous native unit tests cover exact aliases/selected percent signs, numeric-key ordering, early container/work rejection, classifier invocation bounds, diagnostic count/UTF-8 bytes/omissions, conflicting facts, all candidate/path/work limits, unknown-field isolation, and mandatory protection classification. Two compile-fail doc tests ensure committed/native source wrappers cannot deserialize. Eight source-derived Node tests cover 72 generated source-shaped shows against the actual original bundler, actual media helpers/path normalization, background and audio/video timeline consumers, legacy SHOW replies, committed `showsCache` versus trimmed `SHOWS`, typed native provenance, exact Unicode/path spellings, protected/unclassified outcomes, and resource bounds. No GUI or release build is required for this pure slice.

```sh
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR="$PWD/target/assets" cargo test --manifest-path crates/freeshow-asset-plans/Cargo.toml --locked
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR="$PWD/target/assets" cargo build --manifest-path crates/freeshow-asset-plans/Cargo.toml --bin freeshow-asset-plans-parity --locked
FREESHOW_ASSET_PLANS_PARITY="$PWD/target/assets/debug/freeshow-asset-plans-parity" node --test tests/parity/asset-plans.test.mjs
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR="$PWD/target/assets" cargo clippy --manifest-path crates/freeshow-asset-plans/Cargo.toml --all-targets --locked -- -D warnings
```

The source test defaults to workspace `target/debug/freeshow-asset-plans-parity` (`.exe` on Windows). The isolated override is `FREESHOW_ASSET_PLANS_PARITY`. The coordinator removes the standalone `[workspace]`/lockfile when adding the crate to its shared workspace. Planner-only checks establish candidate preparation; host integration, actual bytes/ranges, stale publication rejection, protected decryption/licensing, browser codecs, and Svelte/WebKit behavior remain their respective service/integration gates.
