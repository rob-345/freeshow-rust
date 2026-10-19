# Native audio metadata wire preparation

`freeshow-metadata-wire` prepares the original `AUDIO_METADATA` data `{filePath,metadata}` and a separate, typed hydration envelope. It is pure synchronous Rust with no filesystem access, parser, worker, runtime, publication, context validation or source ACK. Inputs are native-owned typed receipts, with no Deserialize implementation or renderer command granting file authority. The host retains its accepted operation/publication lease through preparation and delivery, validates the full context before delivery, and invokes the existing TypeScript `hydrateAudioMetadata` only after that acceptance and before the retained source callback.

```rust
use freeshow_audio::CommonValue;
use freeshow_metadata_wire::{build, PictureRef};

// These borrowed values come from accepted Audio/Cover receipts. Their host
// owner, descriptor authority and publication quota remain the caller's concern.
fn prepare(
    source_path: &str,
    common_without_picture: &CommonValue,
    ordered_picture_fields: &CommonValue,
    actual_cover_bytes: &[u8],
) -> Result<freeshow_metadata_wire::MetadataWire, freeshow_metadata_wire::WireError> {
    build(source_path, common_without_picture, &[PictureRef {
        fields: ordered_picture_fields,
        bytes: actual_cover_bytes,
    }])
}
```

The API is `build(file_path: &str, common: &CommonValue, pictures: &[PictureRef<'_>]) -> Result<MetadataWire, WireError>`. Both `MetadataWire.data: serde_json::Value` and `MetadataWire.hydration: Hydration` are public and Serialize. `PictureRef` has public borrowed `fields: &CommonValue` and `bytes: &[u8]`. Nothing is consumed or changed. There is no hidden asynchronous completion or resource owner in the result; the host must bound native inputs, serialized transport and completed/uncollected publications separately. Errors have bounded static details, implement Error/Display, and expose `code()` as `metadata_wire_invalid` or `metadata_wire_limit`; no input text/path/bytes are interpolated into diagnostics.

## Typed input and resulting envelope

`CommonValue` preserves ordered objects, dense arrays, own Undefined, Null, booleans, strings and f64 numbers. `common` must be an Object with no `picture` field. Picture fields must each be an Object excluding `data`; bytes are supplied separately. Each picture's `data` is appended last. A nonempty picture list appends root `picture`; an empty list leaves it absent. This does not infer whether covers are absent or merely unknown. The host must supply only cover results with the appropriate complete-read evidence; skipCovers metadata is not proof of absence.

Original APIC field order is `format,type,description,data`; FLAC order is `type,format,description,width,height,colour_depth,indexed_color,data`. The cover backend supplies these orders and an own Undefined type where appropriate. The producer preserves all supplied field orders, including ordinary unknown nested fields. The original common object's top-level picture insertion position cannot be reconstructed from a skipCovers result plus a separate cover receipt; root picture is explicitly appended. This is a documented placement gap, not complete top-level source insertion-order parity.

```ts
type Hydration = {
    undefinedPaths: string[]
    specialNumbers: { path: string; kind: "NaN" | "PositiveInfinity" | "NegativeInfinity" | "NegativeZero" }[]
    byteBuffers: string[]
    objectKeys: { path: string; keys: string[] }[]
}
```

The Rust fields use snake_case and serialize to the camelCase names above. `NumberKind::{Nan, PositiveInfinity, NegativeInfinity, NegativeZero}` serializes to those exact strings. Undefined/nonfinite fields get own null placeholders. NegativeZero gets a numeric zero placeholder. Finite numbers remain f64 JSON numbers. Every typed object, including root metadata and every picture/nested object, gets an `objectKeys` row. Arrays retain order and own undefined indices. Every picture gets exactly one byte-buffer pointer at `/metadata/picture/{0..7}/data`; STANDARD base64 is canonical, including empty/padded values. The hydrator restores genuine Uint8Array, own undefined and special numbers. JSON projection alone is not evidence of these raw values.

Pointers preserve exact Unicode and escape only `~` to `~0` and `/` to `~1`. Scalar/buffer targets are children below `/metadata`; object ordering includes `/metadata` itself. No pointer addresses `filePath`. Duplicate/dangerous keys (`__proto__`, `constructor`, `prototype`) reject. Integer keys must already follow JavaScript's representable enumeration order: canonical u32 indices below4294967295, ascending before non-index string keys. Unsupported native ordering is diagnosed rather than silently changed. Rust strings cannot represent lone JavaScript UTF16 surrogates; this interface does not certify that unsupported input class. Arbitrary source byte buffers outside the explicit picture slots have no representation in CommonValue and must not be guessed from lossy JSON.

## Complete preflight and shared limits

The first pass borrows the entire immutable input, validates shapes/keys/depth, and charges generated metadata plus the complete hydration envelope. No output Value tree, field-name copies, base64 strings or byte copies are built before all input passes. A bounded BTreeSet of borrowed keys detects duplicates; container counts are checked against remaining nodes before allocating that set. Pointer traversal uses bounded scratch. Keys and strings are checked for obvious length excess before detailed scanning. Byte slices are borrowed and aggregate size checked before base64 encoding. Only a successful full preflight starts the second output-building pass. A late invalid picture or field produces only an error, with no partial publication/result or input mutation.

These limits match `src/frontend/rust/metadata.ts`, including its generated-envelope overhead. They are representation/work bounds rather than measured heap or performance claims.

| Resource                   | Bound                                                                                                          |
| -------------------------- | -------------------------------------------------------------------------------------------------------------- |
| Shared JSON nodes          | 65,536 across payload root/filePath, metadata, hydration root/arrays/records/path/key strings                  |
| Metadata depth             | 16 edges below root; envelope's fixed generated shape fits depth16                                             |
| Metadata JSON charge       | 8MiB including base64, keys, values and punctuation                                                            |
| Hydration JSON charge      | 2MiB                                                                                                           |
| Individual metadata string | 6MiB JSON-string charge                                                                                        |
| Envelope string            | Generated paths/keys also fit the frontend's12,290-byte string charge                                          |
| Pointer                    | 2,048 UTF8 bytes each; 1MiB aggregate across undefined/special/buffer/order paths                              |
| Key                        | 1,024 UTF8 bytes each; 1MiB aggregate including metadata keys, envelope property names and ordered key strings |
| filePath                   | 32,768 UTF8 bytes, preserved exactly                                                                           |
| Pictures / decoded bytes   | 8 / 4MiB aggregate                                                                                             |

JSON charge counts UTF8 plus JSON escapes/quotes/punctuation, conservatively32 bytes per finite number (including the signed-zero placeholder),5 per boolean,4 per null. A single4MiB byte slice produces5,592,408 base64 characters; eight512KiB slices produce5,592,416 together. Both pass the real hydrator with ordinary small common metadata. Combining maximum cover and large common strings can exceed the shared8MiB budget and rejects. Empty picture bytes are representable but are not certified valid images. The producer neither decodes nor modifies them. Logical aggregate/node/depth/key/pointer limits bound all loops and copied output; caller-owned input size and Rust allocator/serialization overhead remain separate.

The host must not mutate the public output or substitute untyped JSON before hydration. It must bound raw transport before parsing; this producer budgets compact parsed representation, not arbitrary whitespace or alternate JSON spellings. The output is not an authorization token, parser-completeness receipt, immutable filesystem snapshot or context acceptance. Existing audio/cover parser and authorized-descriptor evidence remain independent requirements.

## Source and regression evidence

Eight Node suites execute immutable `2d94defcf98175e4f5c9504e7f4fd1d33c35c975` `nowPlaying.ts::getAudioMetadata` with actual installed `music-metadata@7.14.0`, feed its typed raw fields to the Rust producer, and invoke the actual exported TypeScript hydrator. Six generated real WAV/MP3/FLAC files include invalid YEAR NaN, signed zero, overflowing Infinity, own undefined genre index, and APIC/FLAC unknown picture types. Three actual source pictures include two genuine PNG payloads above64KiB. Tests assert raw Object.is values, own properties, key order and exact bytes, distinct from JSON projections. Only source publication is captured and native typed fixture input is controlled. This proves the wire transformation on these source receipts, not an independent Rust metadata/cover parsing proof, real Electron structured clone or bridge/context integration.

The corpus also covers64 generated typed trees, one4MiB/eight-picture success, decoded overflow, exact65,536 shared-node/depth16 limits, aggregate key/pointer/envelope/metadata limits, duplicate/dangerous/nonrepresentable keys, malformed late picture fields, filePath bounds, and atomic late hydrator failure with root/nested identities preserved. The measured producer corpus has76 accepted and15 rejected builds. Seven Rust tests additionally check direct borrowed input preservation, canonical base64, exact generated-envelope accounting and explicit native shape/budget errors. Existing hydrator tests remain unchanged and independently exercise its malformed-envelope validation.

The private `freeshow-metadata-wire-parity` fixture CLI is not production authority. It accepts at most16 cases/16MiB stdin, bounds typed conversion to65,536 nodes/depth17, allows at most9 pictures for rejection fixtures, bounds each base64 fixture string to5,594,000 characters and streams at most32MiB output. Typed tuple fixtures preserve own undefined/specials/object pair order/duplicate keys; they are not a general parser or arbitrary-path IPC. The production function receives borrowed native values and never invokes this driver.

```sh
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=target/assets
cargo test --manifest-path crates/freeshow-metadata-wire/Cargo.toml --offline --all-targets
cargo build --manifest-path crates/freeshow-metadata-wire/Cargo.toml --offline --bin freeshow-metadata-wire-parity
FREESHOW_METADATA_WIRE_BIN="$PWD/target/assets/debug/freeshow-metadata-wire-parity" "$ORACLE_NODE" --test tests/parity/metadata-wire.test.mjs
cargo clippy --manifest-path crates/freeshow-metadata-wire/Cargo.toml --offline --all-targets -- -D warnings
cargo fmt --manifest-path crates/freeshow-metadata-wire/Cargo.toml -- --check
```

The source test defaults to `target/debug/freeshow-metadata-wire-parity` (`.exe` on Windows), with `FREESHOW_METADATA_WIRE_BIN` override for isolated caches. Pinned standalone Node22.21.1 and installed TypeScript4.9.5 are used. Linux Rust tests/strict Clippy and Windows GNU/macOS aarch64 `cargo check --all-targets` pass; cross checks are compile evidence, not runtime execution on those systems. No app, GUI, profiling, native image codec or source binary download is needed. Shared main/bridge/runtime/manifests/CI are untouched. The standalone manifest/lock are temporary integration artifacts; coordinator removes the nested workspace/local lock when adding the workspace member.
