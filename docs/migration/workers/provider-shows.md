# ChurchApps show-builder foundation

This is bounded pure preparation for the immutable
`src/electron/contentProviders/churchApps/ChurchAppsShowBuilder.ts`, not an
account connection, provider import, media grant or persisted show. Its source
SHA-256 is `866fc0a1645627b7489a805943d9917b0ec81b5471f79ee4c83912cb88d9232a`
at `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`. The original class warns that it
should only be used through ChurchAppsImport. Future native integration must
retain that provider/import boundary; no renderer command is introduced here.

## Native API

`SourceValue` represents `Undefined`, `Null`, booleans, all binary64 numbers,
UTF-8 strings, dense arrays, and ordered ordinary own properties. It has no
Deserialize implementation. `Environment { uids: &[&str], now_millis: &[f64] }`
is a borrowed trusted sequence at the original `uid()`/`Date.now()` boundaries.
The functions are:

- `parse_lyrics(&SourceValue) -> Result<Vec<Section>>`;
- `create_song_show(key, arrangement, song, details, Environment)`;
- `create_empty_show(id, title, description, seconds, Environment)`;
- `create_media_show(item, &[SourceValue], Environment)`.

Builders return `Built { value, uid_calls, now_calls, date_calls, work_bytes,
operations }`. `value` remains typed, including own undefined and nonfinite or
negative-zero numbers. `SourceValue::json_projection() -> Result<Value>` is a
separate bounded, lossy compatibility projection: own undefined is omitted in
objects and null in arrays; nonfinite numbers become null, and -0 becomes +0.
Do not use that projection to infer the original own values or key inventory.
Root undefined projects to null as a placeholder; actual builder roots are
always objects. No frontend hydration protocol is claimed by this crate.

For example, a trusted importer can convert already parsed ordinary records
using `SourceValue::from_json(&value)?`, provide enough ordinary source UIDs and
clock samples, call `create_song_show`, and retain the typed result before its
separate accepted import/persistence phase. `from_json` preserves the caller's
already parsed binary64 value; it cannot recover decimal precision lost before
entry. The provider's future network JSON parser is not certified by these
tests. No global serde feature change silently alters workspace parsing.

UIDs must have the real default `uid()` shape: eleven lowercase hexadecimal
characters. Collisions are allowed. Unused sequence entries are not interpreted;
entered calls reject exhaustion or unsupported UID shapes. A successful song
reports two Date constructor calls even though its ordinary immutable primitive
is parsed once, plus exactly the source's conditional Date.now call. Empty and
media builders call Date.now once. Trusted clock samples remain binary64,
including the artificial special values exercised in deterministic tests; a
real clock owner should supply actual Date.now values. No clocks, random source,
threads, workers, filesystem or network are created in this crate.

## Preserved source behavior

The implementation keeps paragraph/header branches, ECMAScript whitespace
(including FEFF and excluding NEL), ASCII trailing section-number removal,
CRLF/header oddities, empty lyric slides and retained line spacing. Dedup keys
use the original label and filtered lines. Repeated sections retain duplicate
layout references; a later nonempty child list overwrites the deduplicated
parent's children, while an empty later list leaves the old children intact.
Actual UID collisions overwrite dictionary values without moving the original
property position, including the source's resulting unusual references.

Fixed property insertion order, original absent fields and own undefined are
retained. Song item records do not acquire invented `type` fields; media slides
do not acquire invented global groups. Title/id interpolation and `||` use
source primitive/ordinary-array/object rules, rather than converting every
metadata field to a string or every seconds field to a number. Unknown root
fields are only validated for budgets; they are not scanned for content or
media. Values intentionally assigned by the source, such as an object-valued
artist or loop flag, keep their nested own properties and JS integer-key order.

Media inference is case-sensitive: exactly `fileType === 'video'`, `.mp4`
suffix, `/externalVideos/download/`, or truthy streamUrl, with original
short-circuit ordering. URLs/source paths remain source data. There is no file
opening, URL validation, decryption, playback or local asset authority here.

Song dates support numeric TimeClip, missing/null/boolean inputs, empty or the
explicit invalid strings `invalid`/`not a date`, and full explicit ISO dates:
`YYYY-MM-DD` or signed-six-digit extended years, optionally with `T` and an
explicit `Z`/`+HH:MM`/`-HH:MM` zone. Times support minutes, optional seconds and
fraction truncation. Real source comparisons cover epoch fallback, pre-epoch,
24:00 rollover, leap/day normalization and both inclusive TimeClip endpoints.
Zero and invalid Date results use Date.now for created and null for modified.
Fractional negative numeric values truncate to positive zero as actual Date
TimeClip requires, before the source's truthiness fallback.
ISO `24:00` additionally requires every fractional digit to be zero before
millisecond truncation. Actual whole-source comparisons cover invalid
`.0001Z`/`.0000000001Z` tails (one Date.now fallback, two Date constructions)
and valid `.0000Z` (no fallback). Away from `24:00`, sub-millisecond digits
continue to truncate normally.

## Explicit unsupported scope

Local-zone timestamps, legacy grammars, abbreviated year/month forms,
noncanonical zone spellings and object/array Date coercion are unsupported.
The real source accepts some of these: the negative controls demonstrate
finite source dates for `01/02/2024`, a timezone-naive ISO timestamp, an empty
array, and even `-000000-01-01` (a V8 legacy fallback). They are not relabeled
invalid dates by the native builder. Unsupported inputs return
`provider_shows_unsupported`, rather than a fabricated source result.

Live object identity/aliasing after return is outside this ordinary JSON value
snapshot contract. Custom prototypes/getters/functions, own toString/valueOf coercion overrides,
sparse arrays, BigInt/Symbol and unpaired UTF-16 surrogates are not represented
as ordinary native source values. The private fixture driver's JSON parser also
cannot transport lone surrogates. Truthy nonstring lyrics/URLs that require
source string methods are explicit unsupported cases, where the source can
throw. UID/prototype manipulation is excluded by the ordinary source UID shape.
Source exceptions are not claimed to have native JavaScript exception classes.

## Limits and intermediate ownership

Fixed limits apply before growing variable data structures:

| Resource                                           | Limit                                    |
| -------------------------------------------------- | ---------------------------------------- |
| Input raw UTF-8 key/string bytes, across arguments | 8 MiB                                    |
| Input nodes / depth                                | 65,536 / 16                              |
| Copied raw field-subtree depth                     | 8 (keeps the fixed output below depth16) |
| Lyrics UTF-8 bytes                                 | 1 MiB                                    |
| Global lyric slides and sections                   | 4,096 each                               |
| Media files                                        | 1,024                                    |
| Trusted UID / clock sequence entries               | 8,193 / 16                               |
| Conservatively charged output JSON bytes / nodes   | 8 MiB / 65,536                           |
| Cumulative string/container work charges           | 64 MiB                                   |
| Scanned/constructed logical operations             | 2,000,000                                |

These are independent conservative admission ceilings, not a promise that any
one maximum-sized input always fits all other limits. Output charges reserve
six bytes per UTF-8 byte plus punctuation, scalar/container overhead and nodes;
this can reject a value whose actual JSON would be smaller. Copied raw fields,
line splits/joins, dedup keys, lowercase context storage, dictionary entries and
global paragraph pushes have explicit bounded work/count checks. Ordered indexed
dictionaries avoid a growing linear slide lookup on every insertion. There is
no task or retained IO ownership to retire: an error drops all bounded local
preparation, and inputs are borrowed immutably. These logical/node/container
limits are not an allocation-profiler measurement or an exact RSS claim.

The private driver reads at most 16 MiB, accepts at most 128 fixture cases and
limits the retained aggregate's exact serialized output to 32 MiB. A counting
writer charges each completed row, including JSON escaping and array framing,
before retaining it beside earlier rows. At most one additional individually
bounded expanded row exists during that check. Rejection stops the lazy row
producer, writes no partial JSON, and preserves the original nonzero exit/error
message. Accepted aggregate output streams to stdout without another whole-array
serialization allocation. Small-budget Rust controls exercise real 32-file media
expansion at the exact two-row boundary and one byte below it; the latter must
not build the third row. This aggregate fixture budget is separate from the
unchanged production per-call ceilings. Its tagged/IEEE fixtures are test transport, not a
production deserializer or provider authority. The crate is now a root workspace member; integration removed its temporary
nested `[workspace]` and standalone lock without changing existing registry versions.

## Source evidence and checks

`tests/parity/provider-shows.test.mjs` obtains the whole original class using
`git show` at the immutable commit, normalizes CRLF transport, checks its strict
SHA, and executes actual TypeScript-transpiled methods in independent VM realms.
Only uid and the original Date nondeterministic boundary are controlled; Date
construction still uses the genuine runtime Date parser. Comparisons include
complete ordered typed own properties and all original IEEE bits, separate JSON
projection, and actual UID/Date call counts. Cross-realm arrays are observed via
Array.from, without discarding own undefined or numeric information.

The current corpus has nine Node suites: 159 whole-source comparisons
(70 parse, 64 song, 19 empty, 6 media), seven explicit unsupported source controls
and five public resource controls. Nine library Rust tests additionally cover input
aggregation, global lyric admission, copied/output bounds, UID consumption,
object ordering, TimeClip, own undefined, ISO fractional-tail validation and
long-label operation bounds. Three private-driver Rust tests cover aggregate
expansion, exact byte/framing limits and lazy producer termination.

Lowercase is pinned to the existing actual packaged Electron37.10.3 /
ICU74.2 / Unicode15.1 evidence in
`crates/freeshow-files/tests/unicode-oracle.json`. The table is copied byte for
byte from that crate, with its provenance comment, without regeneration. This
builder deliberately does **not** NFC-normalize labels. All 1,433 saved actual
Electron scalar mappings and 7,886 compatible already-NFC/header-stable context
vectors are checked separately, including final sigma. Node22's newer Unicode
behavior is not silently substituted: U+1C89 proves the actual Electron/Node
lowercase difference. The source VM corpus uses stable characters; the saved
actual Electron vectors establish the older Unicode behavior independently.

Foundation validation used the private target under
`/mnt/data/cache/freeshow-rust-build/provider-shows/target`, one Cargo job and
incremental/debug data disabled. Rust tests, Node22 source suites, strict
all-target Clippy for Linux, Windows GNU and macOS ARM, rustfmt and configured
Prettier passed before the foundation was sealed. The Date/aggregate-driver
corrections were checked separately under
`/mnt/data/cache/freeshow-rust-build/provider-shows-fixes/target`: nine library
plus three driver Rust tests, all nine Node22 source suites with the counts above,
strict Linux all-target Clippy, rustfmt, configured Prettier and `git diff --check`
passed. That follow-up did not rerun foreign cross checks. Cross compilation does not establish
native Windows/macOS runtime, accounts, provider integration, persisted imports,
or graphical/media behavior. No GUI or live provider was started.
