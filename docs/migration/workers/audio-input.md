# Native raw AUDIO frame parser

`src-tauri/src/audio_input.rs` is a pure parser. It returns:

```rust,ignore
pub fn parse(body: &[u8]) -> Result<AudioInput<'_>, AudioInputError>;
pub struct AudioInput<'a> {
    pub context: RuntimeContext,
    pub origin: AudioOrigin, // Port / Pcm / Capture, exact wire spellings below
    pub metadata: freeshow_audio_pipeline::AudioMetadata,
    pub pcm: &'a [u8],
}
// AudioInputError { pub code: &'static str, pub message: String }
```

The PCM slice borrows its exact suffix in the caller's body, including when the
whole body starts at a nonzero offset in another allocation. No PCM allocation,
clone, conversion, I/O, task, thread, runtime state, sink snapshot, device or
network effect occurs. There is no source AUDIO success ACK. Parser success
establishes framing validity only; the host still validates the actual invoking
window, current full library context, native admission, sink authority and
lifecycle before using those borrowed bytes.

## Wire contract

The raw Tauri body is four little-endian u32 bytes containing the JSON prefix
length, the UTF-8 prefix, then PCM bytes. The prefix contains exactly:

```json
{
    "version": 1,
    "context": { "runtimeId": "native", "libraryGeneration": 1, "dataPath": "/private/library" },
    "origin": "port",
    "metadata": { "sampleRate": null, "id": null, "icecast": null },
    "negativeZeroPaths": []
}
```

Version is integer 1. Origin is exactly `port`, `PCM` or `CAPTURE`. Context fields
are exact and required; runtime ID is nonempty, at most 256 UTF-8 bytes, data path
nonempty and at most 32768 UTF-8 bytes, both without NUL; generation is an integer
in 0..9007199254740991. UTF-8 spelling is retained in the native PathBuf without
filesystem access or canonicalization. This accepts context syntax rather than
granting authority over a supplied path.

Metadata is an object with only optional `sampleRate`, `id`, `icecast` fields.
Unknown nested config keys, insertion order, arrays, strings, booleans, null and
finite numbers are preserved. Missing fields, including own undefined omitted
by the frontend encoder, become `JsInput::Undefined`. Present null remains
`JsInput::Json(Value::Null)`. Absent Icecast remains None, explicit null is
Some(Null). No rate default, numeric/ID coercion or latest-config latch occurs.
NaN/Infinity literals, overflow exponents and additional sentinel formats are
not accepted. Unknown config data named `__proto__`, `constructor` or `prototype`
is ordinary safe Rust map data, including during restoration.

All metadata numbers convert locally from their exact raw token through Rust's
binary64 parser, preserving JS 53-bit rounding, the observed one-ULP decimal case,
subnormals, underflow and raw negative zero. `serde_json` global
`float_roundtrip`/`arbitrary_precision` are not enabled or changed. The parser
uses `raw_value` only to retain exact lexical tokens, without changing shared
serde parsing behavior. Shared/root source files and manifests are untouched.

`negativeZeroPaths` is optional; when present it must be a string array, never
null. Paths are RFC6901 strings relative to metadata, with only `~0` and `~1`
escapes, at most eight segments. No URI fragment/percent decoding, prototype
traversal, implicit array coercion or executable property access occurs. Arrays
require canonical decimal indices (`0`, `1`, etc., without leading zeros/signs).
Object keys such as `01` and empty keys remain valid exact string keys. Every
target must already be an actual numeric zero; false, null, `"0"`, absent keys,
nonzero values, duplicates and ancestor/descendant overlaps are rejected. Every
path is validated against the original tree before any negative-zero restoration.
Restored values use `serde_json::Number::from_f64(-0.0)` in native Json metadata.

## Bounds and errors

Prefix length is 1..49152 bytes, PCM is 0..32768 bytes, total frame at most
81924 bytes. Empty PCM is structurally valid; whether to ignore it remains the
source/native pipeline's responsibility. Prefix truncation, invalid UTF-8/JSON,
trailing JSON input, unknown/missing/wrong-typed envelope/context fields and
duplicate JSON object keys are errors. Metadata's raw encoded JSON value is at
most 8192 bytes, depth eight including its root, and 256 nodes. Recursive token
conversion stops at that budget; serde's ordinary syntax scan is separately
bounded by the prefix size and its normal recursion limit. Restoration accepts
at most 256 unique paths, each at most 1024 UTF-8 bytes. Path work/allocations are
also bounded by the total prefix. No supplied key/value/path is echoed in errors.

Codes distinguish `audio_input_framing`, `audio_input_limit`, `audio_input_json`,
`audio_input_version`, `audio_input_context`, `audio_input_metadata`,
`audio_input_number` and `audio_input_path`; messages are fixed bounded strings.
The caller maps these into its native error protocol, retains the Tauri body
through any borrow use, and copies/accepts bytes only inside its bounded trusted
owner. Do not retain this borrowed result after the body expires or interpret
origin/config data as renderer-selected sink authority.

## Tests and coordinator additions

The twelve focused native cases cover zero-copy offsets, Unicode context,
absent/null preservation, all provenance spellings, exact number parsing,
escaped/Unicode/prototype-named paths, malformed/duplicate schema, prefix/PCM/
total/metadata/context boundaries, path/node/depth budgets and atomic restoration
validation. One cross-language case runs the coordinator's actual
`src/frontend/rust/audioRaw.ts::encodeAudioFrame` using pinned Node 22.21.1 and
the installed TypeScript compiler, then feeds its real base64-decoded output to
the parser. It verifies offset bytes, Unicode, omitted undefined versus null and
actual negative zero restored through escaped nested and own `__proto__` keys.
That test supervises its child with a ten-second kill/reap deadline. It defaults
to the integrated repo root above `CARGO_MANIFEST_DIR`; the local temporary
manifest alone sets `FREESHOW_AUDIO_INPUT_ORACLE_ROOT`. Node is test-only, never
a product sidecar or dependency of the parser.

Root adds `mod audio_input`, uses `parse` for Tauri Raw AUDIO bodies and maps the
returned metadata into its current native host; existing core/audio-pipeline/
serde dependencies suffice. Ensure `serde_json` features include `raw_value`
and `preserve_order` (the API foundation already uses both). No extra base64
dependency is needed; decoding is a test-only bounded helper. The coordinator's
separate frontend raw encoder must be present for the cross-language test.
Window/context/admission/accepted-work/terminal ownership remain coordinator work.
No GUI, audio hardware, playback, encoder or network delivery is certified here.

Local checks use only `/tmp/freeshow-audio-input-check/Cargo.toml` and reuse the
worker target with one job and incremental disabled:

```sh
FREESHOW_ORACLE_NODE=/home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node FREESHOW_AUDIO_INPUT_ORACLE_ROOT=/home/rob-admin/GitHub/freeshow-rust CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --manifest-path /tmp/freeshow-audio-input-check/Cargo.toml --offline --test audio_input
CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --manifest-path /tmp/freeshow-audio-input-check/Cargo.toml --offline --lib --test audio_input -- -D warnings
rustfmt --edition 2021 --check src-tauri/src/audio_input.rs src-tauri/tests/audio_input.rs
npx prettier --config config/formatting/.prettierrc.yaml --check docs/migration/workers/audio-input.md
```
