# Native common values in the now-playing planner

This extends only `freeshow-now-playing`, its controlled fixture driver and source tests. It composes the actual opened-file `NativeCommon` receipt from audio commit `b6158ce2`; it adds no parser, worker, runtime, IPC command, frontend transport, image decoder or source success ACK.

## Public native API

```rust,ignore
let (common, opened_file, observations) = actual_opened_read.into_parts();
let metadata = Metadata::from_native_common(common?, Covers::Unknown)?;
// Keep opened_file/identity observations in the host through real cover reads
// and final publication validation. Common alone is not file authority.
let next = router.complete(&accepted_parse, Receipt::Metadata(Some(metadata)))?;
// ReadCovers remains mandatory. A skipped-cover parse is not empty-cover proof.
let pictures: Vec<NativePicture> = actual_complete_covers;
let publish = router.complete(&accepted_covers, Receipt::NativeCovers(pictures))?;
let native = publish.effect.native_metadata().expect("native metadata path");
// Validate the live host context and accepted effect before actual publication.
host_publish(native.file_path(), native.common(), native.pictures())?;
```

`Metadata::from_native_common(NativeCommon, Covers)` retains the **actual sealed parser receipt** in `Arc<NativeCommon>`. Unknown-to-complete cover transition changes only covers on the same Metadata; it no longer reconstructs metadata from compatibility JSON. Formatting reads native `CommonValue` directly. No nonfinite number or own undefined is inferred from JSON null.

`Metadata::from_common_value(CommonValue, Covers)` is a separate trusted native value constructor. It is useful for native exact values and source fixtures; it is not parser/file-identity evidence. `NativeMetadataPublication::parsed_common() -> Option<&NativeCommon>` is Some only for the real retained parser receipt.

The additive types are `NativePicture`, `NativeMetadataPublication`, `Covers::NativeComplete(Vec<NativePicture>)`, and `Receipt::NativeCovers(Vec<NativePicture>)`. `NativePicture::from_host(CommonValue, Vec<u8>)` accepts ordered source fields without `data` and separate compact bytes. Its getters are `fields() -> &CommonValue` and `bytes() -> &[u8]`. Root code should insert `Undefined` only using the actual cover backend's authoritative `undefined_fields` receipt and recorded original field order. Missing/null JSON fields do not authorize inventing own undefined.

`Effect::native_metadata() -> Option<&NativeMetadataPublication>` exposes `file_path() -> &str`, `common() -> &CommonValue`, `parsed_common()`, and `pictures() -> &[NativePicture]`. Common excludes picture; every actual picture and byte array is separate. Typed Object fields expose their original ordered `(String, CommonValue)` vector, preserving own undefined and IEEE values. The observation is immutable/shared, has private internal fields and no Serialize/Deserialize; it grants no independent context/publication authority.

The old `Metadata::from_host`, `from_audio_reply`, `Covers::Complete`, `Receipt::Covers` and JSON fixture driver remain supported. The existing PublishMetadata variant keeps `common`, `pictures`, and `json_projection_only`, plus a serde-skipped `native_receipt` field. Old serialized JSON shape is unchanged. Native publication's JSON is explicitly marked projection-only: nonfinite numbers can be null and undefined fields can disappear there. A native producer must use the accessor, not that projection. Old READ_AUDIO_METADATA JSON still rejects ambiguous null YEAR; its null cannot safely be upgraded to native NaN.

## Exact values and bounds

Source `metadata?.year?.toString() || "Unknown Year"` distinguishes NaN (`"NaN"`), infinities, zero/negative zero (both format `"0"`), false (`"false"`), empty string, null, missing and own undefined. Publication retains negative zero and own-property presence even when formatting produces the same string. Scientific numbers use the existing planner's JavaScript/ryu spelling, not `CommonValue::string`'s Rust number formatting. Native title/album truthiness and artist filtering/trimming preserve actual source behavior, including metadata publication before malformed truthy artists fail.

Before any compatibility projection, native common is checked against a conservative **1MiB** budget (32 bytes per node, six times UTF-8 key/string bytes), maximum 1,024 nodes and depth8. This is intentionally narrower than Audio's native output limit4MiB. Native picture fields use the same node/depth policy and a conservative64KiB budget. Eight pictures and4MiB aggregate compact bytes remain the planner limit. Legacy JSON upgraded for mixed native-cover publication is also checked against these native bounds. Dense arrays, unique own object keys in JavaScript enumeration order and ordinary supported scalar/object coercion are supported. Duplicate keys, noncanonical numeric-key ordering, `toString`/`valueOf`/`__proto__` keys, sparse/custom/prototyped/cyclic/function/Symbol/BigInt values and lone UTF16 surrogates are outside this receipt model; representable unsupported key variants explicitly fail. Picture fields may not contain a `data` key, even own undefined. The host remains responsible for truthful provenance, accepted whole-operation/byte leases and renderer envelope limits; retained observations can share Arc data but do not replace that ownership.

Unknown covers always issue ReadCovers. Unsupported/missing complete-cover capability is an explicit failure with no metadata publication or destructive cover effects. Native typed bytes are never inferred from metadata JSON. Only first picture drives the existing image path; all pictures belong in publication. The planner's existing stale/token/cancellation/image cleanup behavior is unchanged. Controlled image receipts in this suite are not decoder or nativeImage parity.

**Common-picture insertion order remains unverified.** Native common parsing skips covers; later complete pictures are separate from its common object. The source's `picture` key may occur between other tags. Tests verify ordered common fields **excluding picture**, ordered picture fields, typed bytes and own undefined separately. They do not establish the original combined common-object picture-key position, nor own empty-picture-property variants. Appending picture later cannot be advertised as exact original common-object order until a combined real tagged-file source comparison establishes that position.

## Evidence and reproduction

The new same-realm source suite loads immutable `2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/audio/nowPlaying.ts` with `git show`, canonicalizes CRLF and transpiles the actual complete module. Canonical SHA256 is `0a203138d9c283b3e5fa37f0aa46cd33df596d8ee7183ac6cd73d3c59cf3bc7c`. Source functions, injected promises and actual music-metadata objects share one realm; no VM cross-realm ordering assumption is used. Native tags in the driver are controlled fixtures, not a renderer wire protocol.

Five new Node22.21.1 suites cover26 source/native comparisons, including5 deterministic real opened-WAV files (actual original music-metadata7.14.0 parse from the source helper),4 picture observations,3 matching partial failures, own undefined/NaN/infinities/negative zero/numeric key order and a separately explicit unavailable-cover case. Generated WAVs contain only fmt/INFO/data, and actual source common confirms no picture; that fixture-specific complete-empty cover receipt is not a claim that an arbitrary skipped-cover parse proves absence. Existing nine JSON/source suites remain green:95 comparisons,2 real WAVs and their prior explicit unsupported JSON YEAR case. Twelve Rust tests (four new) cover Arc receipt retention through cover transition and router collection, raw publication versus lossy JSON, ordered native picture fields/bytes, native input/capture caps and unsupported variants, and existing authority/token/cancellation/resource/image cleanup. This is source/typed receipt evidence, not playback, image-decoder or production host delivery evidence.

```sh
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/private/cached/target cargo test -p freeshow-now-playing --offline
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_TARGET_DIR=/private/cached/target cargo build -p freeshow-now-playing --bins --offline
FREESHOW_NOW_PLAYING_NATIVE_PARITY=/private/cached/target/debug/freeshow-now-playing-native-parity "$ORACLE_NODE" --test tests/parity/now-playing-native-common.test.mjs
FREESHOW_NOW_PLAYING_PARITY=/private/cached/target/debug/freeshow-now-playing-parity "$ORACLE_NODE" --test tests/parity/now-playing.test.mjs
```

The auto-discovered new binary defaults in source tests to root `target/debug/freeshow-now-playing-native-parity` (`.exe` on Windows); its environment override is test-only. It creates one existing AudioService, uses its real opened-file admission/receipts, and joins actual parser owners before emitting results. Input/cases/steps/output are bounded. Linux executed the corpus; cross checking is build evidence rather than execution on another OS. Strict Clippy/format and final post-ENOSPC checks are reported with the immutable commit. No shared manifest/lock, runtime, frontend, CI or source oracle files are changed.
