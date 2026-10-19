# Audio metadata hydration

This is a standalone frontend restoration function, not production bridge/native-producer integration. It exports `hydrateAudioMetadata(data, hydration): void` from `src/frontend/rust/metadata.ts`. The coordinator must call it only after validating the native event is original `MAIN.AUDIO_METADATA` with its exact current full RuntimeContext, before delivering `{filePath, metadata}` to the retained handler. The function does not establish context, authorize files, publish events or create a source ACK.

```ts
type AudioMetadataHydration = {
    undefinedPaths: string[]
    specialNumbers: { path: string; kind: "NaN" | "PositiveInfinity" | "NegativeInfinity" | "NegativeZero" }[]
    byteBuffers: string[]
    objectKeys?: { path: string; keys: string[] }[]
}

// After channel/context acceptance and before the original source callback:
hydrateAudioMetadata(event.message.data, event.nativeAudioMetadata)
```

`nativeAudioMetadata` above is a proposed private envelope location, not a shared Event field added by this slice. The envelope structure is agreed with the coordinator; naming/wiring remains coordinator-owned. A rejection throws a bounded static Error. The bridge must reject the event rather than calling the retained callback with partial or unhydrated values. An empty valid envelope is a validated no-op.

## Values and paths

Only `/metadata` JSON-pointer paths are accepted. Scalar restorations require a child below that root; object ordering may target `/metadata` itself. `filePath` is validated but never changed. Payload root keys are exactly `filePath` and `metadata`; unknown ordinary fields inside metadata are preserved. Unknown envelope/record fields reject. Pointer escapes are only canonical `~0` and `~1`; Unicode and case remain exact. Array indices must be canonical nonnegative decimal indices in the existing dense array; `01`, `-0`, `length` and out-of-range additions reject. New own undefined keys may be added to objects, never by extending an array.

Undefined restoration requires an absent own property or an own null placeholder. An already undefined input is not a JSON placeholder. Special numbers require own null placeholders, except NegativeZero requires numeric zero. This produces actual NaN/positive Infinity/negative Infinity/signed zero, not strings. Duplicate or ancestor/descendant scalar/buffer mutation paths reject across all mutation lists. Dangerous `__proto__`, `constructor` and `prototype` names reject in paths and throughout metadata/envelope object keys.

Picture bytes are canonical standard base64 strings only at `/metadata/picture/{0..7}/data`. Every entry in a declared root `picture` array must be a plain object with its own data string and exactly one corresponding byte-buffer path. Missing data, incomplete restoration, other byte locations or more than eight pictures reject. Padding, alphabet, length and unused padding bits are checked without decoding first; whitespace, URL-safe characters, data-URL prefixes and alternate encodings reject. Empty canonical base64 restores a genuine empty `Uint8Array`; it does not certify an empty picture was source-produced. Output is `Uint8Array`, not a Node Buffer, number array or placeholder object.

`objectKeys` lists must contain exactly the final own keys, including newly restored undefined keys, once each. Duplicate object-order paths reject. Ordered targets must be objects, not arrays/scalars. Key order must be representable by JavaScript enumeration: canonical integer keys ascend before all other string keys. Source APIC and FLAC insertion orders are restored from actual source lists, including an own undefined picture type in its original position. Optional omission of an order list makes no promise to recover an unknown original insertion position.

## Validation, staging and atomic failure

Inputs are native JSON delivery trees: ordinary or null-prototype records, dense ordinary arrays, finite numeric primitives, strings, booleans and null. Symbols, accessors, nonstandard property descriptors, custom prototypes/classes, functions, bigint, existing typed arrays, nonfinite pre-hydration numbers, sparse/extra-property arrays, cycles and shared object aliases reject. Accessor getters are not invoked. Arbitrary active JavaScript Proxies are outside this JSON-input contract; this function is not an evaluator or a security boundary against executing renderer code.

The entire data/envelope trees, schema, budgets, target paths, placeholders, complete byte coverage and final object orders validate before any decoded-byte allocation or original-data mutation. Only then is metadata cloned into bounded staging objects, bytes decoded and restorations/orders applied there. The final commit replaces only `data.metadata` once. Success therefore replaces nested metadata object identities; the callback has not yet received those native JSON objects. `filePath` and the payload object remain unchanged. Validation failure, a malformed late record and a later decoder allocation failure leave the original root/nested references and values untouched. No generic JSON-pointer write is applied directly to callback data.

The staging tree has no external native resources. Temporary JavaScript allocations become reclaimable normally on failure; this is not a claim of synchronous GC or OS resource drain. No tasks, threads, apps, native subprocesses or asynchronous completion owners are created.

## Protocol budgets

| Resource                      | Bound                                                                                                                         |
| ----------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| Parsed data/envelope values   | 65,536 nodes combined, including root/header values, envelope arrays/records/key strings and newly added undefined properties |
| Metadata depth                | 16 edges below the metadata root; scalar pointer depth agrees; generic envelope depth also capped16                           |
| Metadata JSON charge          | 8MiB aggregate, including base64 strings, keys, punctuation and values                                                        |
| Envelope JSON charge          | 2MiB aggregate                                                                                                                |
| Individual metadata string    | 6MiB JSON-string charge                                                                                                       |
| Envelope strings              | Generic charge at most12,290 bytes; typed paths/keys have the stricter bounds below                                           |
| Pointer                       | 2,048 UTF8 bytes; all scalar/buffer/order paths together at most1MiB                                                          |
| Key                           | 1,024 UTF8 bytes; metadata/envelope property names, restored new keys and ordered key names together at most1MiB              |
| Root filePath                 | String at most32,768 UTF8 bytes                                                                                               |
| Picture count / decoded bytes | 8 pictures / 4MiB aggregate                                                                                                   |

JSON charge is computed without allocating an escaped string or TextEncoder copy: UTF8 bytes plus JSON escaping/quotes, conservative32 bytes per finite number and5 per boolean, ordinary punctuation/null. It is a conservative representation budget for the parsed compact native payload; original transport spelling/whitespace is already gone, so the host/bridge must bound raw transport before parsing. Individual code-unit length checks reject obviously oversized strings before scanning. Array/container counts and shared nodes bound validation/staging work. Order validation indexes newly added keys by parent, avoiding a mutation-count × order-count scan. Paths have at most16 child segments, bounding prefix recording and target walks.

Base64 is charged as ASCII bytes, not doubled UTF16 storage. A single4MiB cover has5,592,408 base64 characters; eight512KiB covers have5,592,416 together. Both fit with ordinary common metadata under the8MiB metadata charge and the6MiB individual-string bound. The4MiB decoded quota is validated for all pictures before creating any output Uint8Array. Simultaneously maximizing a large4MiB common payload and4MiB of covers can exceed the shared8MiB encoded budget and rejects explicitly; the producer must preflight the complete envelope/payload rather than silently remove metadata. Logical limits are not a measured browser heap or performance claim; engine object/enumeration overhead and host-held original JSON are separate.

## Source evidence and checks

Eleven Node suites load the actual exported TypeScript function through installed TypeScript in the same realm. Six generated real WAV/MP3/FLAC files execute immutable FreeShow `2d94defcf98175e4f5c9504e7f4fd1d33c35c975` `nowPlaying.ts::getAudioMetadata` with actual installed `music-metadata@7.14.0`. The source publishes the exact common object. Only the future native JSON envelope encoder is controlled. Native wire insertion order is deliberately disturbed before hydration, then raw values, own keys/order and bytes are compared with actual source output.

The source corpus includes6 special-number occurrences (invalid YEAR NaN, signed zero, overflowing positive Infinity and real common special fields),3 own undefined values (genre index plus APIC/FLAC unknown picture type),3 actual parser pictures and2 genuine PNG picture payloads larger than64KiB. These raw ownership/number checks remain distinct from JSON projections. No Rust parser, Electron app, browser structured-clone route, real bridge publication or native image decoding is claimed by this frontend slice.

Other checks cover one full4MiB buffer and eight-picture aggregate success, more-than-quota rejection with zero decoder allocations, canonical base64 padding/bytes, incomplete pictures, source key orders, numeric-key enumeration, nested arrays/escaped keys,64 counted malformed atomic-failure cases plus strict malformed-tree cases, late allocation failure, exact depth/node boundaries, wire/string/key/path/envelope limits, getters not invoked and prototype isolation.

```sh
"$ORACLE_NODE" --test tests/parity/metadata-hydration.test.mjs
npx tsc --noEmit --strict --skipLibCheck --target ES2022 --module ESNext --moduleResolution Node --lib ES2022,DOM src/frontend/rust/metadata.ts
npx prettier --config config/formatting/.prettierrc.yaml --check src/frontend/rust/metadata.ts tests/parity/metadata-hydration.test.mjs docs/migration/workers/metadata-hydration.md
git diff --check
```

Verified with pinned standalone Node22.21.1 and the installed TypeScript4.9.5 compiler. Source parser decisions are actual source execution; packaged Electron clone behavior, platform/browser execution and full-context producer/bridge integration remain coordinator work. Shared main/bridge/manifests/IPC/CI and existing frontend helpers are untouched.
