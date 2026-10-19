# Native import and filesystem host integration

The native host retains the Svelte import converters and their history/save
behavior. Rust prepares transport and bundled media; it does not reinterpret
show, template, overlay, action, stage, theme, TXT, ChordPro or CSV contents.
The immutable source comparison and preparation limits are described in
[the import worker contract](workers/imports.md).

## Admission, ownership and publication

Two native host leases cover the entire import pipeline: parsing, retained private
staging, waiting for the library owner and final installation. Two fixed parser
threads and a fixed bounded cleanup reaper belong to `ImportService`. Parsing
does not occupy the library owner. The service also retains a separate prepared
lease through private staging removal, preventing completed jobs from creating an
unbounded disk backlog. Overload is an explicit error.

Prepared media installation runs through `LibraryService::commit`. The existing
write fence and filesystem barrier validate the request's frozen full context
before invoking the trusted closure. Its destination is the current library's
`Imports`, never a root taken from the renderer payload. The preparation and host
lease move together into the closure. Canceling an IPC reply wait after admission
does not abandon an accepted install. Dropping a stale closure queues private
cleanup before releasing its host lease. A changed library cancels old preparation
tokens and rejects old queued commits.

Successful source `IMPORT2`/`ALERT` events receive the verified context, with no
invented acknowledgment or listener ID. Preparation failures retain source prior
alerts and an explicit error; commit failures retain only failure alerts. A stale
context suppresses those events. Alert timing differs: source importing alerts
can appear before parsing, while native publication waits for preparation/context
validation and successful conversion events wait for the commit.

Normal Quit first drains admitted library writes. Import shutdown then closes
admission, cancels parsing/retained tokens, joins parser work and waits for all
host leases. A second reaper fence covers plans dropped after the first service
shutdown fence. Cleanup failure is surfaced instead of declaring a clean drain.

The host exposes imports only to the main window. Unsupported formats, unsafe or
unverified archive variants, excess input/output and cleanup failures remain
explicit errors. A selected filesystem call cannot be interrupted inside the OS;
cooperative preparation/install bounds are separate from time waiting for the
ordered commit. Imported media installation does not establish playback parity.

## Filesystem browsing

`READ_FOLDER`, `FILE_INFO` and `GET_SIMILAR` run on two independent fixed filesystem
workers with two host leases. Context validation precedes work and reply
publication. Library changes cancel old tokens; Quit closes admission and joins
the worker service. Filesystem queries do not enter the library write queue.

The [browse source/packaged IPC contract](workers/file-browse.md) preserves all
fourteen numeric Stats fields, Linux statx timestamps, folder depth/capture
precedence and case-sensitive UTF-16 similarity. Main-window delivery restores
only the explicit native `READ_FOLDER` folder `noMedia:null` placeholder to an
own undefined property. The conversion occurs after context acceptance; missing
capture-stub keys, other nulls and other commands stay unchanged.

Native filesystem Stats are currently verified only on Linux with statx. Other
platforms and an unavailable statx backend return explicit unsupported errors;
portable source similarity tests do not certify their filesystem behavior.
Listings that actually require thumbnail generation return
`thumbnail_unavailable` until capture integration is implemented. They do not
publish guessed thumbnails or claim successful empty listings. Parallel source
traversal can expose different outer entry ordering/duplicate winners; native
ordering is deterministic. Raw filesystem calls and non-Unicode/lone-surrogate
names remain compatibility limits.

## Verification surfaces

- `tests/parity/imports.test.mjs` compares actual source import/export helpers and
  feeds transport into the retained frontend converters.
- `src-tauri/tests/imports.rs` exercises accepted-caller cancellation, stale
  queued imports, retained-plan saturation/shutdown, tampered staging, raw text
  transport and source failure alerts.
- `tests/desktop/imports_probe.js` runs through the real retained Svelte handlers;
  the desktop driver saves using its actual Ctrl+S shortcut and reads saved
  Unicode lyrics/unknown fields back through Rust. The bundle probe checks the
  installed media hash and persisted remapped reference.
- Browse source/packaged IPC tests cover Stats, own undefined properties and
  traversal; `src-tauri/tests/browse.rs` checks native role/context/platform
  errors and actual queries while the library owner is deliberately blocked.
- The Linux desktop regression compares the three readonly browse commands with
  original handlers during the same real 64 MiB ZIP, including bridge hydration.

These surfaces cover implemented slices. The complete migration gate remains
failing; wider import/export, native capture/playback and cross-platform
application workflows remain unfinished.
