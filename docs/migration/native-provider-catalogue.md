# Native provider catalogue

`MAIN.GET_CONTENT_PROVIDERS` now uses the existing provider dispatcher to return
the pinned source's five declared providers. The retained Svelte caller receives
the original `MAIN` reply, exact listener and only `channel,data`; its send still
returns undefined. All fifteen fields and row/property order match the actual
immutable registry, constructors, handler and `receiveMain`.

This fixed declaration performs no filesystem, credential or network IO. It
does not enter the library queue or create an async task, runtime, thread or
timer. The dispatcher validates the main-window role, full real runtime/library
context and listener bound before admission, then rechecks context and sealing
before returning the event. The existing four whole-operation permits are
shared with stored-provider queries. A private, serde-skipped event guard holds
the actual permit through event cloning and native encoding; releasing an
observer, sealing admission or completing a library fence cannot release a
still-owned result. This guard does not certify WebView delivery or disposal of
an encoded transport buffer.

Four new native tests cover synchronous execution without Tokio, unchanged
corrupt ACCESS bytes, four retained/cloned results and shared busy admission,
serialization/private-field omission, role/context/listener failures, sealing,
actual library replacement and completion during a genuinely held library
commit. The six existing stored-provider host cases still pass. The foundation
adds two Rust cases and six source Node suites; a retained bridge suite checks
source replies and command snapshots before transport and while replies wait.
Independent review found no concrete blocker in binding `ac973731` or desktop
fixture `60143a2c`.

The isolated normal Linux binary uses only `custom-protocol` and no browse test
harness. Its immutable copy and twenty byte-checked adjacent remote resources
are separate from subsequent Cargo writes. Actual retained callbacks pass before
restart, after actual restart and after a real selected-library change. Each
phase compares complete rows, strict boolean types, WebView-captured own-key
arrays, listener, two callback arguments, void send and full native context with
source observations before and after the callback. Device identity remains
omitted from the exact receipt schema. Callback filters reject relevant
duplicates/private fields and ignore unrelated messages; duplicate observation
ends when the probe removes its listener.

The full local milestone passes 565 workspace Rust checks, 652 source Node
checks and twenty frontend checks, plus 382 native entry executions. Five Rust
supervisor fixtures and five source platform/capability skips remain explicit.
Strict workspace/native all-target Clippy, formatting, source snapshot, frontend
and normal binary builds pass. The desktop receipt records 194 steps, including
one additional readiness observation compared with the previous 193-step run;
this is not an extra feature claim. All four native menu cases, both drop scales
and final normal Quit/process exit pass. Independent checks preserve 141 cover
HTTP responses, both source JPEG pixel hashes, folder alerts/sentinel bytes and
the previous AI/Spotify callbacks. Forty-nine other editor diagnostics remain.
[Hashed local receipt](baselines/provider-catalogue-linux-local.json).

The fixed declarations are not authenticated account state or native content
backend health. Authentication, refresh, OAuth, provider content retrieval,
imports, sync and licensing remain incomplete. Foreign GUI behavior and current
cross-platform CI are not certified by this local run. The complete parity gate
continues to fail intentionally, and no new application performance gain is
claimed. See the [pure source contract](workers/provider-catalogue.md) and
[desktop probe](workers/provider-catalogue-desktop.md) for reproduction and
source-boundary details.
