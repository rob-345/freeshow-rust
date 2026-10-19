# Retained desktop AI inventory and Spotify-null probe

This test-only follow-up extends the normal production desktop smoke at its
existing normal, restarted and switched-library device/provider checkpoints.
Each phase adds one recorded step and preserves the existing actual application
restart, native library picker and selected-library checks. No native host,
bridge, protocol, production frontend, manifest or collector changes are made.
The coordinator must rebuild/run the actual desktop; this worker ran no GUI or
native Cargo build and claims no new GUI pass.

Before native startup, smoke creates two inert regular files under its actual
trusted `FREESHOW_RUST_APP_DATA` root, `temp/app/bin/nemotron`: a 27-byte README
and five-byte nested Unicode filename, 32 bytes total. Their SHA256/lengths are
archived. Different positive-sized renderer-root and selected-library decoys
make a wrong root observable. Actual fixture bytes are independently reread
before/after every phase, so restart/library switch must retain the global
inventory. Other binary directories and selected-library contents never become
AI source authority.
The existing inert FFmpeg-download check now requires exactly the two known
`bin` entries (`ffmpeg`, `nemotron`) and verifies the preexisting AI bytes are
unchanged, retaining its rejection of unexpected staging/output entries.

`ai_spotify_oracle.mjs` reuses the existing immutable AI-local oracle's actual
LocalModelManager, getFolderSize, MAIN handler and receiveMain dispatcher with
genuine Node fs. It asserts native userData capture exactly once, original
path/name/size insertion order, ignored renderer payload and exact original
outer MAIN/message/listener reply. No model, setup, inference or provider backend
is imported. This is apparent-size inventory, not executable/model validity.

For Spotify it executes the actual immutable helper, actual SPOTIFY_GET_STATE
handler property and actual receiveMain dispatcher against the **same inherited
private session bus as the native app**. Before/after each source query it calls
real D-Bus NameHasOwner and ListActivatableNames, requiring the fixed Spotify
name to be neither owned nor activatable. The source's real fixed `&&` shell
query must fail specifically with ServiceUnknown and return null through the
original reply shape; arbitrary process failure, unavailable capability or a
synthetic disconnected result is not accepted. The actual source shell's close
is awaited and recorded. The bus address/environment and raw identity are not
archived. The test never starts Spotify, an account session or an MPRIS fixture.

Each real WebView probe sends both commands through retained `window.api.send`
and receives their original named/correlated MAIN callbacks. It checks send
returns undefined, exactly two callback arguments, exact message keys and AI row
keys, positive actual inventory and Spotify's exact null. Private native metadata
must not enter the source callback. Callback-time and before/after contexts must
equal the actual native runtime context. Matching runtime errors fail the probe;
the smoke also checks its command-specific error log after source revalidation.
It does not require unrelated known desktop runtime errors to disappear.

The source expectation is rerun after each native phase, so bus/fixture state is
checked around the actual callbacks. `test-output-rust/desktop/ai-spotify.json`
contains both source observations, native callback receipts, fixture hashes and
three phase results. A pending source receipt is written before the WebView
send, preserving useful evidence on a later failure. The artifact is included in
normal smoke cleanup; all actual fixture files belong to its temporary profile.
No machine identity, provider credentials or raw D-Bus address is recorded.

Immutable source pin: `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`.
Normalized source hashes are verified by the reused loaders/new helper:

- manager: `8f015d524d7f14ac536033530a15407989033556be3ac1c21ca3f0c265b1f80b`
- folder function: `5d68a709d96083c42805dd8649af0ea30e03ddb377227224db852ab19db2109a`
- Spotify helper: `75d1ceab68e46c2a21dfc5a61f4e667f1d7d130e7b34b377938b7a97082918fd`
- Spotify handler: `8d655e635c64c162bbf128b08089ec4f48a0f9a13af8979c4874e5f5f6069944`
- receiveMain: `943411675b781c85f088eab5ddfb4bb9294cfc5a5171e07372115ac956bda91b`

Focused local checks: Node22.21.1 helper syntax, WebDriver probe function syntax,
Python smoke compilation, repository Prettier and diff checks. One genuine
source-only `dbus-run-session` execution passes with the same deterministic
32-byte fixture, original row/message key order and ServiceUnknown null; its
actual wrapper exits zero and the private temporary fixture is removed. The
local source-only receipt is `/tmp/freeshow-ai-spotify-desktop-source-check.json`.
That is source evidence, not production native/WebView execution evidence.

Coordinator run prerequisites stay the normal desktop harness requirements plus
the integrated AI/Spotify native foundations, `/usr/bin/dbus-send`, a fresh
`dbus-run-session` bus and pinned Node22.21.1 (`FREESHOW_TEST_NODE` may select it).
The inherited bus checks deliberately fail if the environment has an active or
activatable Spotify service. Actual non-null Spotify metadata and IEEE/undefined
restoration remain covered by the separate private-MPRIS source/native and
hydration suites; these three desktop phases certify only the missing-service
case when they are actually run. No hardware, playback, OAuth, LAN, performance
or broad AI/Spotify parity claim is added.
