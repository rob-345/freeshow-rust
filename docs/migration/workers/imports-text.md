# Retained text-format import transport

This extends the existing import preparation crate from root
`06501c09e6f8f924cd67abd6022292634a6b1c17`. The source oracle is immutable
FreeShow 1.6.7-beta.2, `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, with
declaration text normalized from CRLF to LF before SHA256 verification.

The new supported IDs are `openlp`, `opensong`, `mediashout`, `quelea`,
`songbeamer`, `easyslides`, `verseview`, and `videopsalm`. These are the actual
IDs in retained `Import.svelte` and `responsesMain.ts::IMPORT2`. No converter,
history/save logic, or native host/bridge code is replaced. The previous ten
native/FreeShow/TXT/CSV/ChordPro IDs remain supported. Ordinary standalone
ChordPro `.pro` and `.PRO` files now use the original text path rather than the
previous blanket special-format rejection.

## Source boundary and integration

The API stays `ImportService::prepare_with_cancel("IMPORT_FILES", &payload,
trusted_roots, cancel) -> PreparedImport`. `FORMATS` gains the eight IDs. The
coordinator retains full-context admission, ordered final effects, publication,
and actual GUI integration. There are no dependencies, manifest/lock changes,
new threads, new native IO authority, or source success acknowledgements.

Actual `responsesMain::importFiles` calls `importShow` without returning its
promise. `receiveMain` therefore sends no direct response; the asynchronous
source publishes `MAIN` / `IMPORT2` with `{channel: id, data: records}` and no
listener. Records preserve raw replacement-decoded UTF-8 content, BOM and NUL,
original path, basename/suffix quirks, and lowercase extension. The backend does
not validate XML or assert that malformed text creates a useful show. Missing,
unreadable or directory inputs retain source empty-content records. Null/empty
selections produce no event.

The source checks these suffixes **before** selecting archives:

- OpenLP: any exact lowercase `.sqlite` selection filters to those files and
  uses the SQLite handler.
- MediaShout: any exact lowercase `.mdb` selection filters to those files and
  uses the MDB handler.

The native slice explicitly rejects such whole selections, including mixed
valid XML/archive plus binary input, before reading an archive or publishing a
successful result. It does not turn an unimplemented binary path into text.
Uppercase/mixed-case SQLite/MDB suffixes do not trigger source filtering, but
the existing conservative native special-extension guard still rejects them.
This source/native difference is tested and remains explicit.

Any ordinary selected `.zip` ignores selected nonarchives, as in the source.
The existing validated archive/staging path admits XML for the XML formats,
extensionless/XML entries for OpenSong, `.ssc` for MediaShout, `.sng` for
Songbeamer, and the already supported JSON for VideoPsalm. Existing text/native
ZIP entry support remains. Entry order/names and absence of the plain-file
`path` field follow the source. Unsupported entries reject the preparation;
missing/malformed archives retain original ALERT/empty-result distinctions.

## Bounds and ownership

The unchanged defaults use two fixed preparation workers, one fixed cleanup
thread, two retained prepared leases, 64 selected files, 16 archives, 2,048
entries, depth 16, 16 MiB per text, 128 MiB aggregate input/expanded bytes,
32 MiB output, and bounded operations/deadline checks. The existing guarded ZIP
parser rejects traversal, duplicate entries, encrypted/unsupported archives,
declared-size/decompression/resource violations. Cancellation, caller drop,
retained plans and staging cleanup continue through the existing service.
Shutdown joins the preparation workers and waits for a cleanup fence. The existing
cleanup thread discards its join handle and may remain alive while cleanup
senders are retained; the fence does not certify its exit or actual join.
OS/filesystem calls remain cooperatively checked between chunks; this does not
make a blocking filesystem syscall preemptible.

## Evidence

`tests/parity/imports-text-oracle.mjs` loads SHA256-verified pinned declarations,
the actual `importShow`/`readFile`/ZIP code, actual `importFiles`, MAIN handler,
`receiveMain`, and `sendToMain`. It executes real controlled files/archives;
special converters are explicit traps. Tests prove that SQLite/MDB filtering
really reaches those traps, that ZIP `.pro` really reaches protobuf, and that
ordinary paths do not. `imports-text-source.json` records all source hashes.

The new tests compare exact single-file callback payloads and own-key order,
all eight IDs, Unicode, BOM, malformed UTF-8, NUL, missing/directory/empty inputs,
case variants, extensionless names, duplicates, mixed selections, malformed and
empty archives, and explicit unsupported paths. Multi-file ordinary source
`asyncPool` completion order is nondeterministic; those tests compare complete
multisets, including duplicate count, and do not certify universal order parity.

All eight actual retained converters are also executed through the original
frontend `IMPORT2` handler, actual `ShowObj`, `createCategory`, and
`setTempShows`/history. Deterministic UID/time, stores, scheduling, styling,
name/quick-access helpers are controlled dependencies. XML uses installed
`@xmldom/xmldom` 0.8.15 with only `children` and attribute-iteration adaptations.
These are source-converter/input compatibility checks, not WebView DOM parity.
Every fixture creates a nonempty show with actual Unicode lyric signal and
history; changing the input changes the converter output.

Focused verification: 21 Rust tests (17 existing, four new), 14 Node22.21.1 source
suites (nine existing covering the previous ten IDs, five new), strict Clippy,
Rust formatting, configured Prettier and diff checks. An independently built
exact `06501c09` crate rejects all eight new IDs and standalone ChordPro `.pro`;
the same nine real-file requests produce actual `IMPORT2` data after this patch.
Only private `/mnt/data/cache/freeshow-rust-build/import-text-formats/target` and
copied Node dependencies are used. No shared target, GUI, app, hardware, network,
or performance measurement was run.

## Remaining boundaries

SQLite, MDB, Word, PPT, PDF, ProPresenter protobuf, BIBLE detection and the
Lessons.church download workflow remain unsupported. Third-party `.vpc`, `.qsp`
and `.probundle` containers remain explicit errors even when the original ZIP
reader could handle them. Every archived `.pro` invokes original protobuf
handling regardless of import ID, so archived ChordPro `.pro` stays unsupported.
OpenSong HTML/browser and Bible variants are not certified by the XML fixture.
An extensionless name ending in an astral character loses one UTF-16 code unit
in the original and creates a lone surrogate; Rust rejects that representability
case explicitly. Native Windows/macOS runtime and actual retained WebView import
behavior remain coordinator/CI evidence, separate from these Linux/source checks.
