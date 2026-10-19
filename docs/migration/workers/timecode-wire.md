# Private timecode wire input

This slice adds a lossless bounded command encoder and native parser for the
owned timecode host. It does not change shared bridge/main/manifests, register a
public IPC command, issue a source success ACK, open devices or run codecs. The
coordinator owns native-only endpoint/window authority, inbound body allocation
limits, request ordering and full-context validation at real effects.

## API and host wiring

Frontend:

```ts
encodeTimecodeCommand(message: TimecodeMessage, context: RuntimeContext): Uint8Array
encodeTimecodeLtc(bytes: Uint8Array, mode: "LTC", context: RuntimeContext): Uint8Array
```

Native:

```rust
parse_command(body: &[u8]) -> Result<CommandInput>
// CommandInput { context: RuntimeContext, command: freeshow_timecode::Command }
parse_ltc(body: &[u8]) -> Result<LtcInput<'_>>
// LtcInput { context: RuntimeContext, mode: &'static str, bytes: &[u8] }
```

Pass command results directly to
`NativeTimecode.command(&input.context, input.command, cancellation)`; Value
already contains the exact `freeshow_midi::Number` variant. Pass raw results to
`NativeTimecode.audio(&input.context,input.mode,input.bytes,input.bytes.len(),cancel)`.
The suffix is borrowed from the original body without a second audio allocation;
the host makes its existing bounded accepted snapshot. Parsed inputs are not
serializable replies or trusted context grants. A successful parse proves syntax
and supported resource shape, not current context, native capability or delivery.

Inject these functions into the `timecodeBridge` command/audio callbacks. The
adapter still owns snapshotting/queue/cleanup; the native host still owns source
state, IO, codecs, completions and actual joins. Commands use a distinct raw-byte
endpoint from LTC frames. Do not auto-detect a command from untrusted content,
reuse the32KiB AUDIO PCM parser, JSON-stringify these bytes into numeric arrays or
add a frontend codec/PCM engine. Actual Tauri raw invocation remains coordinator
integration work.

## Exact private schema

Command body is UTF-8 JSON:

```json
{ "version": 1, "context": { "runtimeId": "native", "libraryGeneration": 1, "dataPath": "/private" }, "command": { "kind": "stop" } }
```

Exactly four command variants exist:

| kind     | Required fields                                               |
| -------- | ------------------------------------------------------------- |
| `start`  | `type`:string, `mode`:string, `framerate`:Scalar, `data`:Data |
| `stop`   | none                                                          |
| `value`  | `value`:Number                                                |
| `status` | `data`:Data                                                   |

Scalar tags are `{kind:"undefined"}`, `{kind:"null"}`,
`{kind:"bool",value:boolean}`, `{kind:"text",value:string}` or
`{kind:"number",value:Number}`. Number tags exactly match the native Number
representation: `{kind:"Finite",value:number}`, `{kind:"NaN"}`,
`{kind:"PositiveInfinity"}`, `{kind:"NegativeInfinity"}` or
`{kind:"NegativeZero"}`. Finite tags cannot carry negative zero or overflow.
Unused value fields, including explicit null, reject rather than disappearing.
Data is `{kind:"undefined"}` or `{kind:"json",value:finitePlainJson}`.

Missing and own undefined source framerate/data both become the same exact native
Undefined variant because the original functions read their value, not property
presence. Null stays distinct. Source STOP ignores its argument; ignored START
outer fields remain ignored, while unknown fields inside supported Data::Json
retain insertion order and values. The encoder visits only command-relevant
arguments; its caller supplies the original send-time snapshot.

Data::Json supports null, booleans, finite numbers, Unicode strings, dense arrays
and plain objects, including prototype-named keys as inert data. The encoder
emits raw `-0.0` for nested negative zero. The parser borrows each raw JSON token
and uses Rust's local binary64 parser rather than a generic serde number
roundtrip; negative zero, subnormals, long round-to-nearest decimal cases and
numbers beyond integer precision retain JavaScript Number semantics. No shared
serde_json feature/parser is changed. Top-level source Value/framerate nonfinite
values use explicit Number tags. Nested undefined/nonfinite data, array holes,
BigInt/functions/symbol values, accessors/custom objects/cycles and lone UTF-16
surrogates reject explicitly: route::Data::Json cannot faithfully carry them.
NaN identity is a Number variant, not a promise to preserve arbitrary NaN payload
bits. This format does not execute custom coercions or toJSON.

Raw LTC frame is4-byte little-endian JSON-prefix length, exact UTF-8 prefix, then
borrowed audio bytes. Prefix schema is exactly
`{version:1,context,mode:"LTC",byteLength:<actual suffix length>}`. Empty audio is
valid; lengths through65536 are supported independently of AUDIO PCM's32768 cap.
The exact source worklet's unwrapped raw Uint8Array no-op remains handled by the
adapter before this encoder. Only an explicit `{mode:"LTC",buffer}` route reaches
this frame format; no hidden capture repair is added.

## Strictness and finite resources

Every native envelope, context, variant, scalar/Number/Data tag and LTC prefix
rejects unknown and duplicate fields. Nested JSON objects reject duplicates too,
including escaped aliases such as key versus \u006bey. Missing required fields,
extra fields from another command variant, unexpected tag casing, trailing JSON,
invalid UTF-8, nonfinite raw tokens and malformed framing cannot produce a
partial command. No ACK/success/audio command variant exists in command framing.

Host-aligned limits use the smaller relevant bound: type32 UTF-8 bytes,
mode/framerate text4096; Data1024 nodes/depth8/65536 aggregate UTF-8 key/string
bytes, with each primitive charging16 as in NativeTimecode. Runtime ID is a
nonempty NUL-free256-byte Unicode string, data path nonempty NUL-free32768 bytes,
and generation a nonnegative safe JS integer. Filesystem path is provenance only;
the parser does not dereference it or authorize a library. All supported valid
frames are within these host bounds; raw JSON data restoration does not reparse
through a lossy intermediate Value serializer.

Command encoded body caps262144 bytes. LTC prefix caps49152, suffix65536 and total
body4+49152+65536. Escaping can make otherwise bounded raw strings exceed an
encoded-body/prefix cap; the encoder rejects that resource variant explicitly.
Native byte limits are checked before deserialization. The upstream ingress must
also bound its initial allocation before handing a body to this parser.

The implementation is synchronous/pure, with no jobs, threads, timers, filesystem,
network, library lookup or renderer path command. Input caps bound scanning and
allocation; custom data parsing adds at most9 native value-recursion levels.
The cached serde_json1.0.154 `de.rs::ignore_value` used for RawValue scans nested
containers iteratively with a byte scratch stack, rather than recursively
allocating a declared value tree. At most the bounded body contributes to that
scratch; repeated borrowed subtree scans are bounded by the supported depth.
The regression includes20000 declared nested arrays and rejects at depth8. String
decoding and malformed token scanning can allocate/work up to bounded body size
before a semantic rejection. CPU/allocator/VM stalls are cooperative; there is no
claim of a preemptive syscall deadline. Concurrent callers require the existing
bounded native ingress/host, not this parser as a new scheduler.

## Checks and portable prerequisites

Eight focused native tests are `src-tauri/tests/timecode_input.rs`; six frontend
tests are `tests/parity/timecode-wire.test.mjs`. A standalone headless check manifest used
path core/midi/timecode dependencies, serde derive, serde_json(raw_value and
preserve_order), and test-only tempfile. Root already uses those serde features;
the coordinator owns manifest registration. No Tauri/GTK application build is
needed. Node22.21.1, installed TypeScript and git source are existing test tools;
there are no downloads or codec/hardware prerequisites.

Native tests launch the actual frontend TypeScript encoder through Node and feed
its emitted bytes into the real Rust parser, including all scalar/Number tags,
precise binary64 cases, undefined/null distinctions, negative-zero data and a
65536-byte subarray changed after encode. This is mandatory cross-language
coverage, not a skipped optional oracle. `FREESHOW_TIMECODE_NODE` selects pinned
Node; otherwise PATH node is required. The test owns one bounded child with15s
deadline, kill/reap on timeout and temporary output files; it is test-only, never
a production sidecar. It separately checks every schema layer's duplicates,
wrong variants, context/resource/depth limits and exact borrowed suffix identity.

```sh
"$ORACLE_NODE" --test tests/parity/timecode-wire.test.mjs
FREESHOW_TIMECODE_NODE="$ORACLE_NODE" CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test --manifest-path src-tauri/Cargo.toml --test timecode_input
npx tsc --noEmit --target ES2022 --module ES2022 --moduleResolution node --strict --skipLibCheck --lib ES2022,DOM src/frontend/rust/timecodeWire.ts
```

Pure wire tests prove encoding/parsing and source command representations. Actual
immutable source routing/codec evidence remains in the earlier host/adapter tests.
Production raw Tauri invocation, endpoint/window authority, current-context effects,
private renderer completions and application shutdown are coordinator obligations;
none are reported as completed by a parser result.
