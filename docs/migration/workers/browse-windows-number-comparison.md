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

Each numeric value must agree with the source JSON Number and its exact
captured binary64 bytes. JSON's lost `-0` sign is established only by that
pre-JSON bit capture. There is no rounding tolerance or stringified-record
comparison. Every file requires its own bit observation; folder/foreign-path
observations cannot silently pass. Existing own-undefined and thumbnail-intent
checks remain. The stat workflow's size assertion likewise checks the actual
Number bytes against the existing source capture and the known 21-byte fixture.

Three synthetic comparator tests cover integer/f64 equivalence, a changed bit
in each of the 14 fields, signed-zero distinction, missing/reordered native and
source Stats/bit keys, missing/wrong-type bit evidence, strict record types and
keys, nested object order, array order/length/type, and unexpected fields. The
same self-contained comparator module can be copied verbatim from the end of
`engine/windows_tests.rs` into a temporary Linux test crate using
`serde_json` with `preserve_order`; these controls prove comparison behavior,
not Windows API behavior. No original Windows test is removed or skipped.

Verified locally: nine Linux crate tests, nine existing Linux source suites,
three portable comparator tests, the existing LF/forced-CRLF stat source gate
controls, and the existing portable listing source control. The temporary
portable harness used the exact appended module (SHA-256
`adde12061d3fd62e4e539120c05d79c6f725324832a4e82032102c36ed383e82`)
and the same workspace serde_json `1.0.154`/preserve_order dependency. Strict
`cargo clippy -p freeshow-browse --target x86_64-pc-windows-gnu --all-targets
--offline --locked -- -D warnings`, rustfmt, and doc formatting pass. Builds
used the existing private target with jobs 1 and incremental/debug information
disabled; workspace manifests and lockfiles are unchanged.

Actual Windows execution of the correction remains pending the coordinator's
next CI. All 11 original engine tests and 11 original stat tests are retained;
the other original Windows tests are untouched, with three comparator tests
added. Cross compilation does not turn the earlier four failures into passed
runtime evidence.
