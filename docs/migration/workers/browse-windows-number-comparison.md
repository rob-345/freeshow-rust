# Windows browse test Number comparison

This test-only correction is based on `5a5ba2c5`. CI run `39258039091`
executed 33 Windows browse tests: 29 passed, and four stopped at strict
`serde_json::Value` integer-versus-float equality. The listing records printed
equal numeric values (`21` versus `21.0` in the workflow fixture), but these are
different serde Number variants. JavaScript has one Number representation.
No production parser, filesystem behavior, ordering, resources, or source
fixture is changed here.

The READ_FOLDER comparison retains the full source path set and strict record
keys/order. Every non-Stats value is checked recursively, including its type,
object keys/order and array shape/order. Stats require exactly these 14 fields,
in the same order in the native object, source JSON object, and original source
pre-JSON `statsBits` object:

`dev, mode, nlink, uid, gid, rdev, blksize, ino, size, blocks, atimeMs, mtimeMs,
ctimeMs, birthtimeMs`.

Each native numeric value must match the original captured binary64 bytes.
The source JSON value establishes Number type only: serde decimal reparsing can
differ by one ULP, and JSON loses the `-0` sign. JSON-projected numeric values
are not substituted for the original pre-JSON bit capture. There is no rounding
tolerance or stringified-record comparison. Every file requires its own bit observation; folder/foreign-path
observations cannot silently pass. Existing own-undefined and thumbnail-intent
checks remain. The stat workflow's size assertion likewise checks the actual
Number bytes against the existing source capture and the known 21-byte fixture.

Three synthetic comparator tests cover integer/f64 equivalence, a changed bit
in each of the 14 fields, signed-zero distinction, missing/reordered native and
source Stats/bit keys, missing/wrong-type bit evidence, a real fractional-date
decimal reparse difference, strict record types and keys, nested object order,
array order/length/type, and unexpected fields. The
same self-contained comparator module can be copied verbatim from the end of
`engine/windows_tests.rs` into a temporary Linux test crate using
`serde_json` with `preserve_order`; these controls prove comparison behavior,
not Windows API behavior. No original Windows test is removed or skipped.

The decimal control uses actual Node `22.21.1` output:
`JSON.stringify(1792415704000.0015)` is `1792415704000.0015`, with pre-JSON
little-endian bytes `06003cd244157a42`. Workspace serde_json `1.0.154` reparses
that numeral to binary64 `427a1544d23c0007`, one ULP above the source's
`427a1544d23c0006`. The control accepts the correct native source bits despite
that decimal transport difference and rejects substituting the reparsed value.
String/null Stats projections still fail their required Number type.

Verified locally: nine Linux crate tests, nine existing Linux source suites,
three portable comparator tests, the existing LF/forced-CRLF stat source gate
controls, and the existing portable listing source control. The temporary
portable harness used the exact appended module (SHA-256
`e9b4cc0b408ff41d1921d5df817d5350ac72d98a155b3eccf5f313b6bce67df5`)
and the same workspace serde_json `1.0.154`/preserve_order dependency. Strict
`cargo clippy -p freeshow-browse --target x86_64-pc-windows-gnu --all-targets
--offline --locked -- -D warnings`, rustfmt, and doc formatting pass. Builds
used the existing private target with jobs 1 and incremental/debug information
disabled; workspace manifests and lockfiles are unchanged.

Actual Windows execution at `19d68467` now passes all 36 browsing cases in
[CI run 39260766076](https://github.com/rob-345/freeshow-rust/actions/runs/39260766076).
All 11 original engine tests and 11 original stat tests are retained; the other
original Windows tests are untouched, with three comparator tests added.
None is ignored. All six CI jobs pass, including both complete native jobs and
the Linux desktop workflow. The coordinator independently checked the actual
logs and downloaded Linux artifacts.
[Durable receipt](../baselines/windows-listing-native-ci.json).
