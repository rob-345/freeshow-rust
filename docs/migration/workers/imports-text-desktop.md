# Ordinary text import desktop fixture

This test-only followup extends the existing production smoke import checkpoint.
It owns no native/runtime/frontend implementation. The coordinator must run the
actual normal app; the worker ran no GUI, Cargo, native binary or performance
measurement and does not claim native/browser success.

`imports_text_fixtures.mjs --fixtures ABSOLUTE_DIRECTORY` requires a fresh empty
directory and pinned Node22.21.1. It writes nine small inert ordinary files:
OpenLP XML, extensionless OpenSong XML, MediaShout XML, Quelea XML, BOM/CRLF
SongBeamer `.SNG`, EasySlides XML, VerseVIEW CDATA XML, VideoPsalm JSON, and
standalone ChordPro `.pro`. Each format has its own Unicode title and lyric.
MediaShout and VideoPsalm use different explicit source IDs to avoid overwriting
each other. EasySlides keeps the source corpus's two-Item shape: its retained
converter requires an array, so both resulting shows are expected. There are
nine new import requests, ten new shows, and fifteen expected shows with the
five existing text/ChordPro/CSV/native-show/project fixtures.

The helper reuses the immutable hash-verified transport and retained converters
from `imports-text-oracle.mjs`. Real source `IMPORT_FILES -> importShow -> IMPORT2`
runs on actual temporary file bytes; special/binary handlers trap and must remain
unused. The source IMPORT2 handler and actual ShowObj/converters run with bounded
controlled stores/UID/style/DOM boundaries. XML uses the existing xmldom
adaptation, not an actual browser parser. ChordPro is additionally extracted from
the immutable source (SHA256
`760d1e8966349505ad4215f484303b86ff45353073ec24abd0f7739830c571e7`)
and consumes through the original IMPORT2 handler. These are source expectations,
not replacement app converters. Changing each primary lyric must remove that
signal from actual source conversion output; all nine negative controls pass.
The pin remains `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`.

The JSON helper result has `requests`, `shows`, `sourceProof` and labelled
`source`. Each new request carries source `expectedRaw`, captured
`expectedFileKeys`, and expected converted names. Only `{id,paths}` is sent to
the original native IMPORT_FILES command. Expectations never enter a production
IPC payload or a renderer store. File key arrays are captured independently,
because WebDriver can reorder object members in fixture args/results. The probe
compares exact original field strings separately from key order.

Each actual native callback must have original `{channel,data}` message shape,
two callback arguments and undefined listener. Send must return undefined, with
no IMPORT_FILES success ACK. New raw FileData strings/path/name/extension and
their observable key order must match the original transport. After the deferred
converters settle, exactly one matching new IMPORT2 reply is required per file.
The event capture is bounded to64. Prior project alert/media remapping and
unknown-field checks remain in place.

Shows uses a real VirtualList, so the probe scans its ordinary DOM viewport to
observe names rather than requiring all fifteen rows mounted simultaneously.
Each scan has at most64 steps, waits for actual scrolling/render updates, and
restores scroll position. The existing per-condition deadline remains15seconds.
Each new converter's names are observed before starting the next request, and
the project remains last, preserving its original alert/converter order. No
Svelte store, converter, application lifecycle or diagnostic route is replaced.

The driver still issues real Ctrl+S and then permits the existing SHOW read
checks. The original five expectations/unknown fields remain, and the added
expectations require all fifteen saved IDs distinct. Python independently reads
all fifteen actual `.show` files and verifies exact names and lyric signals.
The project source `popup.importing` alert must exist, and installed PNG bytes,
remapped paths and future fields retain their original checks. This does not add
recorded smoke steps: the same existing waits/two import evidence steps remain.

`imports.json` adds `ordinaryTextSource` containing helper provenance/fixtures and
source-only conversion signals. Probe request rows now include actual
`fileKeys`, `sendUndefined` and `rawMatchesSource`. No existing artifact field is
removed. The earlier AI inventory/status/Spotify files and their32-byte fixture
remain untouched. Existing ten-format source controls and special/binary
capability boundaries are not changed. No external media, network, SQLite/MDB,
binary ProPresenter, ZIP `.pro`, or broader import-handler support is certified.

Local checks pass:

- Nine actual source transport/IMPORT2/converter cases produce ten distinct
  Unicode shows, plus nine changed-lyric source controls.
- The actual probe runs under a controlled JS/IPC/virtual-viewport boundary:
  one positive case accepts reordered fixture objects and observes all ten
  individually virtualized names, plus seven negative cases reject changed raw
  content, changed native field order, private message fields, null listener,
  duplicates, nonvoid send and wrong saved lyric. Its named listener is removed
  on each completion. These are helper controls, not actual native/UI/save proof.
- Node helper/probe syntax, Python smoke compilation, repository Prettier and
  diff checks pass. Local source/control receipts are
  `/tmp/freeshow-imports-text-desktop-source.json` and
  `/tmp/freeshow-imports-text-probe-controls.json`, with explicit scope.

The coordinator's actual GUI run is still required to verify Linux WebKit XML,
retained history/rendering and real Ctrl+S/native filesystem persistence for the
new formats. Failures must remain visible rather than substituting direct
renderer-store writes or source-only conversion results for that evidence.
