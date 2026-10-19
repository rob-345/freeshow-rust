# Fixed-cover HTTP representation foundation

This slice is a pure response planner. It opens no file, starts no listener, and
creates no threads, tasks, timers, registrations, renderer grants or source ACKs.
It does not establish native `/nowplayingcover` endpoint or full route parity.
Native file preparation, folder ALERTs, authority and descriptor/body ownership
remain separate from this crate.

## Source and API

Oracle: immutable Electron commit
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, `src/electron/servers.ts`.
The test verifies its LF-normalized SHA256
`2b8c81b56e77b754dc617e31777e4b3e1d9d15b368c369efc9d58030c1af4b41`
and extracts the actual `/nowplayingcover` callback with TypeScript's AST.
Installed pinned Express4.22.3, send0.19.0, etag1.8.1, fresh0.5.2,
range-parser1.2.1 and finalhandler1.3.1 are used and version-asserted. Algorithm
provenance and upstream MIT notices are retained in the crate's NOTICES.md.

```rust
use freeshow_cover_http::{plan, Body, ErrorRendering, Headers, Method,
    SourceFileStat, SourceState};

// Trusted native observations of the already-authorized held fixed file:
let stat = SourceFileStat::new(observed_length, node_mtime_ms)?;
let headers = Headers::new(normalized_node_request_headers)?;
let representation = plan(SourceState::File(stat), Method::Get, &headers,
    ErrorRendering::Deferred)?;
match representation.body {
    Body::File { offset, length } => { /* stream ONLY held file's bounded slice */ }
    Body::Empty => { /* HEAD / 304 / zero body */ }
    Body::DeferredError { .. } => { /* actual host finalhandler rendering needed */ }
    Body::ProductionError { .. } => unreachable!("only explicit production policy"),
}
```

The constructor receives a binary64 `fs.Stats.mtimeMs` projection, not a Rust
mtime duration rounded toward zero. It performs pinned Node22's Date
`Math.round(Number(ms))`, including negative half ties toward positive infinity,
then emits the stat weak ETag and whole-second UTC Last-Modified. Native IO must
preserve the actual sec/nanos-to-binary64 projection and validate held identity,
context and observed file version before committing headers. Neither this typed
stat nor `Body::File` grants file authority or guarantees immutable bytes.
`SourceState::Missing` means only the original existence guard was false.
`SourceState::Failed` returns `cover_source_failure`; a later stat/read failure,
nonregular file or authorization failure must never be relabeled empty404.

Inputs are native only; stat, source, headers and method have no Deserialize.
The example JSON binary is explicitly a test adapter, not IPC or an HTTP app.
Outputs contain every stable source header; transport Date and connection are
the host's responsibility. HEAD retains the selected GET headers without a body.
Node22's source missing guard yields GET404 Content-Length:0, but HEAD404 has no
Content-Length. Query/path selection is absent from this API.

## Preserved decisions and bounded differences

- Freshness uses both If-None-Match and If-Modified-Since when both are present.
  Source weak comparisons, comma/SP token behavior, case-sensitive no-cache and
  If-Match precedence over If-Unmodified-Since are preserved. Unused lower
  priority dates are not parsed or rejected.
- Source range parsing uses decimal parseInt prefixes and combines overlapping
  or adjacent ranges. Disjoint valid ranges fall back to full200. Oversized
  suffixes, suffix zero and all-invalid byte members yield416; a different unit
  or uppercase `Bytes=` is ignored. Stale If-Range overrides even an
  unsatisfiable range and returns full200. A quoted If-Range uses the source's
  substring search for the whole current weak ETag, including its quirks.
- Default 412/416 outcomes are **incomplete** `DeferredError` representations:
  known status/security/entity headers are retained; Content-Length and actual
  development stack HTML must come from a real host renderer. No release-build
  assumption silently selects production error bodies. Explicit
  `ErrorRendering::Production` is separately verified against actual Express
  production finalhandler, including HTML bytes, HEAD Content-Length and416
  Content-Range. Source build-time NODE_ENV is not proof of runtime HTTP policy.
- Conditional dates support tested IMF-fixdate with explicit GMT/UTC,
  case-insensitive English names, year1601..9999 and source day overflow
  normalization. Known digit-free invalid spellings yield invalid-date behavior.
  Other V8 spellings, including ISO8601, RFC850 and zone-less asctime, return
  `cover_date_unsupported` when that condition is actually consulted. The tests
  explicitly demonstrate a source ISO date succeeding where this capability
  rejects it. This is a protocol gap, not full Date.parse parity.
- Headers must already have Node incoming HTTP/1 lowercase naming, OWS trimming
  and duplicate joining/discarding semantics. Duplicate input names are rejected
  rather than guessing Node's field-specific rules. Header values must be the
  Node Latin-1 string projection; CR/LF, other controls and non-Latin-1 input are
  rejected. Limits are32 headers,16KiB aggregate UTF8 bytes,8KiB per value;
  lengths are checked before scanning/copying. This UTF8 budget can reject an
  obs-text request earlier than a Latin-1 wire-byte budget.
- At most64 range members,4MiB body length and finite native mtime in
  year1601..9999 are accepted. Limits produce explicit resource errors. Every
  collection/scan/sort/output is bounded; no queue, worker or cancellation owner
  is needed for this synchronous computation. Caller retains all real IO work.

Optional route matching was deliberately left to the host. Tests verify actual
case/trailing-slash/query variants that reach this callback, but do not supply a
Rust route matcher, OPTIONS/default routing, static fallback, disappearing-file
HTML, symlink policy, directory behavior or post-header IO recovery. These must
remain explicit integration gaps.

## Focused evidence and integration

Linux source runtime: standalone Node22.21.1 (the pinned Electron release also
embeds Node22.21.1, but has a different V8/ICU build). Canonical HTTP dates and
ASCII numeric stat spellings avoid relying on unverified locale parsing. No
Windows/macOS native filesystem evidence is claimed from this Linux run. The
suite runs real files on each host OS; only actual pre-epoch filesystem mtime
capability failures are diagnosed/skipped individually.

Six Rust tests cover source rounding, pre-epoch dates, type/header/aggregate/
range/body budgets, ordering, deferred error distinction and conditional
precedence. Six Node tests execute the original callback using actual Express,
send and finalhandler on sequential controlled127.0.0.1 listeners. The only
injected source boundaries are fixed path lookup/existence (and finalhandler
stderr logging); temporary bytes, fs.Stats, HTTP parser, response renderer and
transport are real. This does not test path helper mkdir/ALERT behavior.

The corpus includes GET/HEAD200/206/304/412/416, empty/opaque files, true missing
guard404 versus actual disappearance after a successful existence check,
Unicode-independent fractional/pre-epoch stat behavior, all stable headers,
exact body slices/production HTML, generated ranges/dates and source capability
gaps. Half-millisecond supplemental stat cases execute the actual running Node
`internal/fs/utils` dateFromMs declaration and actual etag package; they do not
pretend the filesystem retained a precision it cannot represent. Same-size
changed bytes with restored mtime also demonstrate source stat-only cache identity.
Only Date and connection response headers are removed in exact comparisons;
development error Content-Length is explicitly unknown in the deferred outcome.
The fixture waits for actual socket close events, not just server.close callback.
All listeners/files are bounded and closed; no external endpoints are used.

Final local receipt:6 Rust tests and6 Node tests passed, with304 completed real
loopback HTTP exchanges and all fixture socket joins. No filesystem pre-epoch
case was skipped on this Linux run. Strict Clippy for all targets, rustfmt,
Prettier and diff whitespace checks passed.

```sh
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
  CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/path/to/shared/target \
  cargo test --manifest-path crates/freeshow-cover-http/Cargo.toml --offline
# Same environment for build and strict Clippy:
cargo build --manifest-path crates/freeshow-cover-http/Cargo.toml \
  --example cover-http-parity --offline
cargo clippy --manifest-path crates/freeshow-cover-http/Cargo.toml \
  --all-targets --offline -- -D warnings
FREESHOW_COVER_HTTP_BIN=/path/to/shared/target/debug/examples/cover-http-parity \
  /path/to/node22/bin/node --test tests/parity/cover-http.test.mjs
```

For workspace integration, remove the temporary crate-local `[workspace]` and
lock, add workspace membership/dependency and example build/source test in root
CI. Dependencies are chrono with only std and serde derive; dev-only serde_json
raw_value preserves test-adapter number spelling (no float_roundtrip or
arbitrary_precision feature). No shared root manifest/lock/route was changed.
The production host must separately own fixed-path preparation, folder alerts,
bounded descriptor/body/result leases, cancellation/retry/join proof and runtime
error rendering policy. This foundation makes no throughput, GUI, image decode,
compression, LAN, full HTTP endpoint or performance claim.
