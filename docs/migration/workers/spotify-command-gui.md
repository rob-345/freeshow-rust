# Native Spotify command desktop fixture

This is test-only preparation for the normal production Linux editor. No native app or GUI was launched while developing this fixture. The coordinator must run the frozen normal binary and inspect `test-output-rust/desktop/spotify-commands.json` before claiming actual bridge coverage.

## Actual boundaries

`spotify_command_oracle.mjs ordinary|held-switch` requires Node 22.21.1. It hashes and executes the pinned complete `spotify.ts`, original MAIN handler, and `receiveMain`, on an independently owned real private D-Bus/GIO peer. The same source corpus executes before and after each native phase. Outputs include exact source commands, callback/listener/key order, real peer effects, source errors and actual child/peer/bus cleanup. The held case also executes the pinned UPDATE_DATA_PATH handler with labelled controlled config/store boundaries while the real Get is pending. It proves source overlap semantics, not a second actual filesystem library selection.

The browser probe uses only original `window.api.send/receive` for the six commands, plus the two direct negative native routes. It requires original void send and the exact two-argument callback `{channel:'SPOTIFY_COMMAND',data:true}` with the supplied listener, checks its captured full context and private-field isolation, then mutates the original input after send. That early true reply is scheduling evidence, never peer success. Smoke separately waits for the actual peer's complete ordered effects and native `spotify_status` command completion, empty PID list, spawned/reaped equality and zero retained results. Source peer effects are compared exactly, including Volume `0.5000000000000001` (IEEE `3fe0000000000001`) and seek `3.0000005` (`40080000431bde83`) producing integer 3000001. No source success ACK is introduced.

The two route controls send the same original pause through generic `ipc_dispatch`, then nonraw JSON directly to `spotify_command`; each must reject `spotify_raw_body`, with no original callback, new native command or peer effect. They do not bypass the production raw route for positive cases.

## Smoke placement and ownership

The ordinary peer starts only after the existing restarted AI/ServiceUnknown phase. All six commands settle before its actual stop/wait. The held peer is prepared before the existing GTK library chooser. After GTK realization and literal path typing, a callback submits the held seek and observes its early original reply plus real Get; Smoke refocuses the same dialog and uses the existing actual Return/Open interaction. It releases the real pending metadata reply immediately after actual persisted/native/renderer library context changes, requiring the same runtime and a greater generation. Actual SetPosition and native settlement must follow. The original callback keeps its old context/listener. The native two-second deadline remains unchanged: a slow chooser/switch is an explicit failure, with no retry or fabricated continuation.

Both peers stop/wait before the existing switched AI/ServiceUnknown source check. Normal, restarted and switched absence proofs, existing 32-byte inventory, Nemotron missing status and catalogue/import checks are unchanged. Two new editor observations are appended. No accounts, actual Spotify application, audio playback or non-Linux control delivery are certified.

`SpotifyCommandPeer` uses the existing inert original GIO fixture on Smoke's actual private session. It refuses an already-owned name, retains one real child and its pipe handles, bounds cumulative output at 64 KiB, individual buffer at 16 KiB and events at 64. Actual stop/wait precedes bounded EOF drain and name absence verification. Emergency kill is waited and still fails the fixture. Cleanup is attempted before existing driver/server cleanup and does not skip those owners if the peer fails. Partial phase receipts are written before effects; failure cleanup retains the exit receipt when available. No helper thread, extra runtime or production API is added.

## Development checks

Run from the fixture worktree with its normal dependencies:

```
FREESHOW_TEST_NODE=/path/to/node22.21.1 dbus-run-session -- /usr/bin/python3 tests/desktop/spotify_command_fixture_test.py -v
/path/to/node22.21.1 --test tests/desktop/spotify_command_fixture.test.mjs
/path/to/node22.21.1 tests/desktop/spotify_command_oracle.mjs ordinary
/path/to/node22.21.1 tests/desktop/spotify_command_oracle.mjs held-switch
```

Four Python tests passed: actual six-effect peer with held metadata and real wait/name cleanup/reopen; malformed receipt controls; status settlement controls; actual extracted GTK helper timing with and without the optional hook. Four JS suites passed against the actual browser script: all six send-time snapshots; both raw-body rejection routes; duplicate/private/malformed/context/error/lost/nonvoid controls; invalid fixture actions/numbers. Python compile and configured Prettier checks passed. Each source phase was executed twice with byte-identical complete receipts; these are source-only controls, not native GUI results. Logs and source receipts are under `/mnt/data/cache/freeshow-rust-build/spotify-command-gui-fixture`.

The GUI requires coordinator `a37f555e`'s normal raw Spotify routes and readonly status. No raw route, main, bridge, manifests, native implementation, deadlines or source fixture was edited in this slice. Actual normal editor/Quit/process cleanup remains coordinator evidence.
