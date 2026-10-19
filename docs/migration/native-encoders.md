# Native encoder detection

`MAIN.ENCODER_DETECT` now uses the unchanged Svelte listener protocol and the
source-compared `freeshow-encoders` preparation service. Only the detection object
is returned, with the original listener ID and frozen runtime/library context.
`SET_RTMP_ENCODER` remains unsupported until the capture owner can apply its
required RTMP reconfiguration; storing a setting alone would not implement it.

Four whole host requests cover media executable resolution, filesystem
preparation, supervised detection, ordered cache effects and reply completion.
An admitted request remains owned when its IPC caller disappears. Two fixed
filesystem workers prepare inputs, with eight preparation slots. Each current
library binding has one owned encoder runtime/thread, two active probes and
bounded callers. Non-forced callers share a matching in-flight probe; forced
calls request fresh bounded work. Listing an encoder never establishes hardware
availability: the source's actual test encode must succeed.

The independently bounded media resolver shares its two-job budget with metadata
queries and can report `media_busy` before all four encoder host slots are used.
The trusted media resolver supplies the executable spelling. The filesystem
owner freezes the actual absolute executable authority separately from the
source cache key's literal path and fractional Node `fs.Stats.mtimeMs`. A PATH
lookup can therefore execute an absolute path while retaining `ffmpeg|0` in the
source key. PATH and working directory are captured from the native process;
renderer payloads cannot choose executables or cache roots. Prepared inputs and
cache intents expose no deserialization boundary or mutable executable input.

Cache reads accept at most 4 MiB, verify the frozen directory identity before and
after reading, and reject special files without waiting on cache FIFOs. Forced
detection bypasses the cache. A cache hit preserves the source's raw truthy JSON
value, including unknown fields, rather than rebuilding a typed detection object.
Cache persistence runs on the ordered library owner and revalidates full context,
private run, app-data root, directory identity, current executable resolution and
identity, exact source key, and intent generation. Retargeting an executable
symlink to an equal-mtime different file is rejected. Coalesced and late older
probe completions cannot overwrite the newest handled probe's cache. The exact
sealed intent bytes are atomically installed. Persistence failures warn and retain
a valid detection reply, matching the source behavior.

Library retirement removes the old binding, cancels and reaps its encoder
children, and creates a fresh private run for the new context. Final checks reject
obsolete cache effects and replies. Terminal shutdown closes admission, cancels
encoder work and filesystem preparation, and drains all four whole host leases,
including accepted cache closures whose callers disappeared. The first teardown
failure remains recorded: retrying Quit or refreshing a library cannot turn a
failed worker/runtime/child cleanup into successful teardown or a replacement
owner. An actual filesystem-worker panic regression exercises repeated shutdown
and context refresh. A cache closure
handled after terminal retirement rejects its derived effect. These changes use
bounded ownership rather than spawning a blocking thread per request.

## Verification and limits

The controlled encoder child is a portable Rust fixture that acknowledges only
`-version` and deliberately fails listing. It never claims a working encoder or
GPU. Native tests exercise missing tools, listener/context preservation, payload
limits, actual fractional mtime bits from Node, exact intent installation,
executable mutation, equal-mtime symlink retargets, directory replacement,
oversized/special caches, four overlapping host leases, caller cancellation,
blocked ordered cache commits, cache failure, context retirement and terminal
child kill/reap. Unix tests additionally check that the child PIDs no longer
exist; other platforms retain the portable lifecycle cases.

Linux desktop checks use the real system FFmpeg, the retained `window.api` bridge
and the pinned original detector. They compare availability, recommendations,
error text, cache keys/values, unknown cache fields, cache hits without new child
processes, forced new probes and cached replies during actual ZIP compression.
Only process memory addresses inside bracketed FFmpeg log prefixes are normalized
for independent live-process comparisons; raw replies, source calls and native
cache bytes remain in `test-output-rust/desktop/encoders.json`. Controlled negative
fixtures retain exact error-text comparisons. Cache JSON values are compared
across runtimes; source byte ordering is not an acceptance claim.

This does not certify capture, RTMP relay or output lifecycle, binary downloads,
all hardware/driver combinations, or real Windows/macOS media/GUI workflows.
Filesystem path checks are not an anchored-handle transaction against concurrent
external directory/executable replacement. Windows directory identity currently
uses creation time rather than volume/file identity. PATH executable suffix
selection retains the documented `.exe` assumption. Per-request filesystem
cancellation and a hard whole-host/OS shutdown deadline remain unimplemented;
cooperative worker cancellation cannot interrupt a kernel filesystem call. The
encoder probe's 5-second child and 35-second operation limits exclude prior media
resolution, filesystem preparation and ordered cache waits. Existing media
resolution work may finish independently after a library change. No application
performance gain or full migration parity is claimed.
