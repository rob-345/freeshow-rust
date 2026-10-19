# Remote now-playing cover: source contract and implementation seam

Source investigation and implementation plan. The local folder-failure repair
is now integrated and verified independently; see
[the native now-playing evidence](../native-now-playing.md). The HTTP route still
returns501. No LAN/cover support or HTTP parity is established by this note.

## Immutable source and present gap

Oracle: `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`.
CRLF-normalized SHA256:

| File                             | SHA256                                                           |
| -------------------------------- | ---------------------------------------------------------------- |
| src/electron/servers.ts          | 2b8c81b56e77b754dc617e31777e4b3e1d9d15b368c369efc9d58030c1af4b41 |
| src/electron/audio/nowPlaying.ts | 0a203138d9c283b3e5fa37f0aa46cd33df596d8ee7183ac6cd73d3c59cf3bc7c |
| src/electron/utils/files.ts      | 98e563c55026d2dbbdd217ee9b94962ed7fc8b82d893795355bd7dc0f54fc5de |

`createServerInstance` registers GET `/nowplayingcover` before the index/static
routes, on every original app: REMOTE, STAGE, CONTROLLER and OUTPUT_STREAM. The
handler calls `getNowPlayingMediaPath()` on every matched request; when
`doesPathExist(filePath)` is true it calls `res.sendFile(filePath)` with no
options/callback. Otherwise it calls `res.status(404).end()` with no body. The
path is always `join(getDataFolderPath('audio'), 'NowPlayingCover.png')`.
`getDataFolderRoot()` selects current config.dataPath or the default data root.
No request path/query value, media ID, Socket.IO session, password or filename
selects the file. HTTP is not gated by the renderer's remote password.

The retained STAGE `MediaOutput.svelte` recognizes paths containing
`NowPlayingCover`, uses `/nowplayingcover?${updater}`, and changes updater from
Date.now once per second. The query is a browser cache-key buster; the source
handler does not parse it, change the file, or disable its ordinary cache
headers. This is independent of local media URL grants and PNG generation.

Current native `static_files::serve` intercepts only the exact raw path and
returns501 **before method filtering**, even for POST. `RemoteService` currently
implements three ServerKind values; OUTPUT_STREAM remains a separate gap.
Existing remotes parity tests inject `getNowPlayingMediaPath:()=>''` and
`doesPathExist:()=>false`, and only assert native501 for this route. Their
ordinary static-asset tests do not establish cover-route parity.

## HTTP behavior derived from the locked implementation

The pinned lock and installed modules agree on Express4.22.3, send0.19.0,
serve-static1.16.2, etag1.8.1, fresh0.5.2, range-parser1.2.1,
path-to-regexp0.1.13 and finalhandler1.3.1. send resolves its own mime1.6.0
(the top-level mime2.6.0 is not the send lookup). These are source-derived
requirements for future real loopback tests, not observed HTTP receipts here.

- Default Express routing is case-insensitive and non-strict: case variants and
  a single trailing slash match; the query is ignored. HEAD uses the GET route.
  Literal percent-encoded spelling, doubled slashes, suffixes and traversal-like
  paths must be checked against the actual router; do not normalize all URLs
  into this privileged fixed-file route. OPTIONS is Express's automatic
  GET/HEAD response; unsupported methods fall through static/finalhandler.
- Existing regular files are sent as raw bytes, including empty/corrupt data.
  There is no PNG decode, signature check, resize, nativeImage call, or cache
  deletion on this route. The fixed `.png` extension gives `image/png`.
- send defaults include Accept-Ranges:bytes, Cache-Control:public,max-age=0,
  Last-Modified from `stat.mtime.toUTCString()`, Content-Length for the selected
  representation, and weak ETag `W/"{sizeHex}-{mtimeDateMillisecondsHex}"`.
  That ETag uses neither a content hash nor ctime/birth time. Pinned Node22
  `dateFromMs` uses `new Date(MathRound(Number(ms)))`; preserve JS Math.round,
  signed/pre-epoch behavior and f64 stat conversion rather than Rust negative
  rounding or epoch-zero substitution. Last-Modified has whole-second HTTP
  date resolution. Same-length data with restored mtime can retain the source
  ETag even when contents differ.
- HEAD has GET/range/cache status and headers, with no body. Matching fresh
  conditionals yield304, stripping Content-Type, Content-Length, Content-Range,
  Content-Encoding and Content-Language. Request Cache-Control:no-cache defeats
  freshness. If-None-Match and If-Modified-Since together are both tested by
  fresh; If-Match precedes If-Unmodified-Since in the precondition branch.
  A failed precondition yields412 through Express error handling.
- Range uses range-parser with combine:true. A single resulting range yields
  206 and Content-Range; suffix/open ranges are supported. Unsatisfiable ranges
  yield416 with `bytes */size`. Invalid/multiple disjoint ranges normally fall
  back to full200, but adjacent/overlapping multiple input ranges can combine
  into one206 range. If-Range controls whether the range is honored. Merely
  ignoring every comma, as current native static code does, is insufficient.
- The absent-file guard produces an **empty404**, distinct from a later
  sendFile stat/read error after exists returned true. sendFile errors forward
  to finalhandler except abort/write cases; directories call next(), so can
  fall through to the packaged static tree. Exact HTML/error bodies, permission
  mapping, disappearing-file races and environment-dependent stack messages
  require controlled tests. Do not replace all failures with empty404.
- Source defaults also emit X-Powered-By:Express and transport Date/connection
  headers; there is no explicit CORS, Content-Disposition or no-store header.
  Current native static responses omit Express's compatibility header. Record
  any deliberate header/framing gap and normalize only genuinely volatile
  Date/connection fields in parity evidence; do not ignore every header.

## Exact directory side effect and caught ALERT gap

Path lookup is not read-only: `getDataFolderPath` calls `createFolder(Audio)`.
When Audio is absent or is not an lstat directory, createFolder calls makeDir,
which attempts `fs.mkdirSync(Audio,{recursive:true})`. On failure it logs and
queues MAIN ALERT **exactly**
`Error: Could not create folder at: ${audioFolderPath}!`, then returns normally.
createFolder still returns the folder path; it does not return false. A later
fixed cover/text existence or write check therefore continues. Existing empty
Audio directories produce no such ALERT; an inaccessible directory does not
necessarily attempt mkdir. Use an existing regular file named Audio as the
stable failure fixture. Source symlinks are followed by exists/sendFile; native
root/link confinement must explicitly delimit this source variant.

A no-network Node22 source execution extracted the actual immutable declarations
and constants, in one realm, with mocked fs boundaries: Audio exists as a regular
file, mkdir throws controlled EEXIST, cover/text existence is false, writes throw
ENOTDIR, and parseFile returns common:{} without a picture. It observed:

| Source operation               | Ordered observations                                                                            |
| ------------------------------ | ----------------------------------------------------------------------------------------------- |
| Valid SET, even format:'plain' | AUDIO_METADATA; Icecast title; folder ALERT1; folder ALERT2; folder ALERT3; folder ALERT4; void |
| UNSET                          | Icecast empty title; folder ALERT1; failed text write; folder ALERT2; void                      |
| OPEN                           | folder ALERT1; missing-file ALERT `'This does not exist!'`; void                                |
| Direct matched HTTP handler    | folder ALERT1; status404; end()                                                                 |

The four SET lookup positions are: cover path after title, artwork_path dynamic
replacement, artwork_base64 dynamic replacement, then text path after the actual
`await convertDynamicValues`. All eight replacements are evaluated eagerly even
when the placeholder is absent. The first three folder ALERTs are in PostMetadata;
the fourth is in Text. A formatter error can stop earlier: missing duration can
stop after the third lookup, and invalid unknownLang can fail before the first.
Counts above are for the stated valid fixture, not every exceptional payload.
UNSET has two lookups (text, then cover); OPEN has one lookup plus its independent
missing-file notification. HTTP GET/HEAD has one lookup, then existence/sendFile.
writeFile/deleteFile's ordinary actionComplete log path does not add this ALERT.

At the time of this investigation, integrated now_playing_files exposed
EnsureFolder false/diagnostics but omitted that source ALERT; NativeNowPlaying
retained one publication per operation and the bridge accepted only the
missing-OPEN alert. Source path,
false-helper and text continuation tests do not certify caught folder ALERTs.
The separate folder-alert slice now repairs this gap on the documented corpus;
the following design records its source-derived requirements.

Minimum separate fix: add at most one typed `FolderCreationFailed` observation
per actual EnsureFolder job, carrying the fixed trusted Audio path and exact
message. Host must enqueue that original ALERT **before advancing that effect**,
continue source helper/path behavior, and retain every outstanding private
publication. A bounded per-operation list replaces Option<publication>: SET
requires at most5 (metadata+four folder alerts), UNSET2, OPEN2. Four whole source
operations give at most20 outstanding publications. Metadata ACK still must not
block post/text effects, and pending metadata authority cannot be overwritten by
an ALERT. Use unique process sequence/private IDs for each, full-context fencing,
whole-operation leases, bounded rejection/timeout/retirement cleanup, and an
explicit bridge validator for the exact trusted folder-failure variant. Expand
bridge completion-promise budget consistently across context replacements or
explicitly report bounded overload; do not emit callbacks outside owned quotas.
No new source command or source success ACK is required.

## Smallest proposed remote IO/authority seam

Keep this networking feature separate from the now-playing host repair. Reuse
the current remotes file pool (two default workers, eight queued requests),
TCP/run leases,32KiB chunks, two-chunk body channel, body deadline and run cancel.
Do not add one task/thread/runtime per HTTP request or another file server.

Add an optional native-only **per-start** fixed-cover source, rather than a
renderer START field or mutable process-global path. A concrete API direction:
`RemoteService::start_with_cover(payload,generation,Option<CoverSource>)`, with
old start using None. CoverSource is opaque/no Deserialize and exposes a trusted
fixed-file preparation callback with no URL/path/name argument. The callback
captures the complete RuntimeContext and LibraryService. It returns either
Missing or an owned regular File with real stat/version observations, plus at
most one typed folder-failure side effect. It never grants directory listing,
arbitrary file paths, or user-media registry access. Root selects only its
captured `data_path/Audio/NowPlayingCover.png`; the remote crate need not depend
on core or deserialize a RuntimeContext.

Preparation must enqueue the actual EnsureAudioFolder and confined file open on
the existing ordered library IO owner under full-context validation/FS barrier.
This preserves the source mkdir side effect without writes on a Tokio/HTTP
thread. Transfer the opened descriptor and observation to the existing remotes
file worker for header/range/stream reads. No image parser/generator is involved.
A held root/directory identity and no-follow/anchored relative open should avoid
ancestor substitution; fail explicit unsupported/conflict on unverified links,
nonregular files and OS backends rather than expose arbitrary symlink targets.
Do not assume current static safe_parents check is an anchored-open proof.

The actual preparation future/result must be retained by a fixed bounded owner
through accepted library work, even if HTTP disconnects. A bounded preparation
channel driven by the existing NativeRemotes owner is one option; fixed file
workers may instead await a retained native preparation receipt on an existing
runtime handle. Choose one after a small cancellation/drain prototype. Merely
awaiting library.commit inside an HTTP handler and dropping that future on client
abort is insufficient proof that remotes stop settled its accepted mkdir/open.
No per-request task workaround is needed. Whole request permits remain charged
through real preparation, stream disposal, and any independent private ALERT
publication. Source ALERT queue acceptance precedes HTTP continuation; renderer
ACK must not gate the404 body. Remote folder failures need their own bounded
main-only event ownership (at most one per request); do not reuse/overwrite a
pending now-playing metadata publication or forge a Socket.IO client event.

When the cover source is configured, workers must start even if static_roots is
empty: FilePool::new currently exits early for that case. Unsupported/unconfigured
cover remains501; missing file is source empty404; capacity/deadline/stale/OS
unsupported failures remain explicit native errors, not false missing-image
success. A proposed4MiB cover bound matches the image producer; ordinary static
max is16MiB. External larger covers would be an explicit native resource boundary.
Both valid PNG and arbitrary bounded/zero-byte fixed-file contents are served.

Capture the complete selected-library root at listener start and retire the run
on context change; never redirect an old request to the new selected library.
Revalidate authority before headers, then stream only the held file/version and
check run cancellation between bounded reads/sends. Source itself allows
in-place-write/stat/read races; the initial scope should reject detected identity
changes before headers and document post-header truncation/change limits.
Already transmitted bytes cannot be recalled. stop/context change must drain
accepted preparation, body/request leases, source notifications and real file
workers before shared library retirement/final write fence. Retry retains the
same actual owners after timeout. Library.finish_writes remains a fence, not an
IO-thread join. Do not claim strong atomic snapshots or a performance gain.

## Proposed acceptance corpus before implementation claims

Use a new immutable-source oracle (or extend the original wrapper deliberately)
that loads actual servers.ts **and** actual path/files functions, with controlled
selected roots; current always-missing injection cannot establish this feature.
Run actual Express and native listeners only on controlled loopback during the
future authorized implementation phase. No production/LAN endpoints are needed.

1. Real selected Audio cover: GET/HEAD/raw bytes, zero bytes, invalid PNG bytes,
   all three native ports, query changes, case/trailing slash, encoded/double
   slash/nonmatching paths, OPTIONS and unsupported methods. Assert selected
   fixed file only; query containing a path never selects a different file.
2. Exact stable headers/status for200/206/304/412/416; volatile transport fields
   separately. Positive/fractional/pre-epoch mtime, no ctime substitution,
   same-size restored-mtime content, valid/invalid conditions, no-cache, If-Range,
   suffix/open/adjacent/overlapping/disjoint/invalid ranges and empty files.
3. Missing Audio (real creation intent), regular-file Audio (one source ALERT
   then empty404), absent cover, directories, unreadable files, symlinks and
   controlled disappearance between existence and sendFile. Preserve original
   error/body distinctions or label deliberately unsupported variants.
4. Actual native SET WAV/MP3 PNG followed by route GET, then UNSET and route404;
   exact installed bytes, source pixel/byte distinction kept separate. Two
   selected roots with different bytes; same generation but differing full
   runtime/data path; cancellation at open and stream boundaries; no stale
   cross-library selection, no external arbitrary path or media grant bypass.
5. Saturation, slow/aborted clients, bounded headers/bytes/chunks/queue, guarded
   parent/source replacement, cancelled preparation after accepted library job,
   simultaneous now-playing LibraryPhase, blocked IO, timed-out stop retry,
   retained notification leases and actual worker/runtime joins. Verify no lost
   owned file/accepted mkdir and no response masquerading as a source ACK.

No implementation estimate, exhaustive Express error parity, OS timestamp
portability, LAN behavior, hardware/playback, cover generation, or throughput
claim is made here. The next decision is approval of this bounded IO/source-alert
seam, followed by its focused source/native prototype and tests.
