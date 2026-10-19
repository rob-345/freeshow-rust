# Native Spotify playback controls

The retained Svelte `MAIN.SPOTIFY_COMMAND` path supports the six pinned Linux
selectors: `playpause`, `next`, `prev`, `pause`, `seek` and `setVolume`. The
foundation uses at most four concurrent commands on its single fixed runtime
thread, sharing four whole-job slots with the sequential state-query lane.
Actual direct children remain supervised through deadlines, kill/wait and the
retained native owner join. There is no new per-request runtime or OS thread.

The bridge snapshots once at send time. Numeric commands require a primitive
finite JavaScript Number before JSON coercion. One canonical full Request is sent
as at most 64 KiB of raw bytes. The native iterative preflight bounds depth and
work before recursive decoding; duplicate/ambiguous fields are rejected. Numeric
tokens are parsed directly from the same raw frame, rather than rounded through
generic `serde_json::Value`. Foundation validation and source-compatible rounding
remain shared. Other selectors retain bounded JSON-compatible, source-ignored values.
Ordinary typed `ipc_dispatch` rejects playback controls so it cannot silently
bypass this exact-number boundary.

The host validates main-window role, original full runtime/generation/path
context, known command and bounded listener before admission. Its original
`{channel: "SPOTIFY_COMMAND", data: true}` callback means scheduling acceptance.
It is not a playback, authentication or child-completion acknowledgment. Native
events retain real receipt lifetimes through clones and encoding; there is no
renderer ACK. A private readonly `spotify_status` command exposes actual owner
state for diagnostics and tests without changing the product callback.

Accepted controls are OS-global in the pinned source. Caller/event disposal and
later library selection must not cancel their effects. Delayed scheduling replies
retain their original full context/listener; the bridge accepts that exact origin
within the current runtime without retagging the selected library. New admissions
still require the current full context. State-query observers retain their
separate library-retirement policy. Terminal CLOSE cancels controls and retains
their actual child waits and native joins; failures and later retries remain
observable. Four bounded strong command controls do not retain the whole-result
leases merely to observe settlement.

Source evidence uses the actual complete Spotify module, original response handler
and `receiveMain`, pinned to `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`.
Five host test entries exercise seven existing state scenarios and eight command
scenarios, including 41 actual private-bus effects and 36 source comparisons.
Four selection cases combine real `LibraryService` updates with the original
UPDATE handler and a held source seek. Restoring the canceled-command or final
library-fence behavior causes observable failures. Six raw-parser tests include
53 actual source cases and a genuine one-ULP generic-parser negative control.
Four additional bridge cases cover all six selectors, single-clone snapshots,
primitive guards, raw framing and old-context publication. Three real bridge
mutations fail; the restored complete 51-case bridge suite passes.

These focused checks and cross-target lint are separate from a normal desktop
workflow and native foreign-platform execution. Linux is the implemented command
backend. Actual accounts, playback/audio devices, macOS/Windows command backends,
full transport-buffer disposal and whole-application performance are not certified.

See the preserved [initial host scope](workers/spotify-command-host.md),
[source-global correction](workers/spotify-global-context.md) and
[same-frame numeric parser](workers/spotify-input.md).
