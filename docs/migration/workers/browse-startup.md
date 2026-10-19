# Ordinary Audio first-mount browse acceptance

This Linux WebKit check runs the coordinator's normal production binary built
from `eb751551a8b8604940d1732b782ea5d6dc7db81c`. It builds no frontend or native
code and replaces no module, store, API, native response, or filesystem result.
Binary SHA-256 before and after both cases:
`f397b2ec5c72c75c5dc72d60611ee66c2f8d96ae22bb5d6a0a23a919587d9cbb`.

## Actual startup boundary

Production `stores.ts` initializes `activeDrawerTab` to `"shows"`; that store is
not saved by `save.ts` or accepted by `updateSettings.ts`. There is no production
preference to start on top-level Audio. `drawerTabsData.audio.activeSubTab` is a
real saved setting, so each fresh profile starts normally on Shows, then one
genuine WebDriver click of the existing Audio tab mounts Audio for the first
time. This is first Audio mount, not app automount of Audio.

The two independent profiles preseed actual `settings.json` with three audio
folders (2/3/1 valid one-second mono WAVs, Unicode names), two media folders,
and the saved audio subtab. Extra text files exercise the source's audio filter.
One profile selects `startup-0`; the other legitimately saves `all`. Neither
clicks All or any folder button. There is no All-to-folder workaround,
navigation retry, store mutation, or manual READ_FOLDER request.

`probe.js` attaches bounded ordinary DOM/click/error observers. It reads the
original public runtime context and actual rendered button paths, labels,
durations, counts and active state. It does not intercept IPC. The original
AudioTabs count requests and Audio component requests create their own native
browse burst. The test polls DOM only, so a successful result cannot be produced
by a test-triggered browse retry. Public `MAIN.CLOSE` is the only test-issued
application command; there is no private invoke/status hook or source success
acknowledgement.

## Completed actual cases

The first actual run passed both fresh-profile cases:

| Saved subtab | First rendered files                                   | Counts               | Navigation                  | Observations / WebDriver requests |
| ------------ | ------------------------------------------------------ | -------------------- | --------------------------- | --------------------------------- |
| `startup-0`  | exact two selected-folder paths/names, each `00:01`    | folders 2/3/1, All 6 | one trusted top Audio click | 9 / 20                            |
| `all`        | exact six paths/names in source UI order, each `00:01` | folders 2/3/1, All 6 | one trusted top Audio click | 10 / 22                           |

Both initially showed Shows with no Audio rows. The first complete result
remained correct through three subsequent observations without navigation.
The saved active folder/All state was preserved. No guide dismissal was needed.
Full runtime contexts were stable and distinct across the two profiles.

No `browse_busy`, browser error or unhandled rejection was observed. Each case
records the same three independent exact startup unsupported-command errors:
`MAIN.DEVICE_ID`, `MAIN.PROVIDER_CONNECTIONS`, `MAIN.SPOTIFY_GET_STATE`. These are
retained in full, not counted as successful capabilities or silently removed.
Every recorded error's request context matches its actual profile context.
Any other observed runtime/browser error fails the probe.

Each real app exited on normal native CLOSE before fixture termination. Its
saved settings retained all seeded audio/media folders and the selected audio
subtab. Driver and virtual PulseAudio children were actually joined/reaped;
the controller's final `ownedChildren` string was empty for both cases. No
forced app kill is counted as successful CLOSE.

## Durable evidence and limits

The full ordinary [receipt](../../../tests/desktop/browse_startup/receipt.json)
is 150,830 bytes (below the 256KiB evidence bound), SHA-256
`452cac070c5e9a2258a0e06067fd39eb4d75d885076b99c9807b043a9b3583d6`.
It retains settings/fixture hashes, all actual observations, raw diagnostics,
normal-CLOSE fields and actual process cleanup. Its values exactly match the
ignored original `test-output-rust/browse-startup/receipt.json` (112,083 bytes,
SHA-256 `0c6cc1c59da553f8d3bc8965edb899b2409ba0b8a7fafa7020caa4398fa10990`).
Python `json.dumps(value, ensure_ascii=False, indent=2)` reconstructs those
original bytes. Per-case receipts and driver/Pulse logs remain in the ignored
directory. The first actual run generated no failure receipt; future failures
are written per case in `finally`, alongside their real cleanup observation.

The observer is installed immediately after the WebDriver session is created
and before the first Audio click. Error absence is limited to that observed
interval; it is not proof that nothing happened before WebDriver attached.
Every case limits observations to 128, errors to 64, navigation observations
to 16, rendered rows to 32, and WebDriver requests to 512 with a 90s controller
deadline. Responses are capped at 1MiB and normal CLOSE has a 15s deadline.

This proves ordinary first Audio rendering/counts and normal-close cleanup for
the supplied Linux production build and this corpus. It does not claim an
Audio app-start preference, exhaustive queue/admission/cancellation behavior,
Windows/macOS runtime, playback/capture/physical audio, or performance. A real
private virtual Pulse sink makes the browser environment valid; no playback
command or performance sample is taken. Queued-owner and cancellation tests
remain separate native evidence owned by the coordinator.

## Reproduction

Use an already-built normal custom-protocol binary and the existing automation
tools. No build is performed by this runner:

```sh
FREESHOW_DESKTOP_BIN=/path/to/normal/target/debug/freeshow-desktop \
WEBKIT_WEBDRIVER=/path/to/WebKitWebDriver \
xvfb-run -a -s '-screen 0 1280x900x24' dbus-run-session -- \
python3 tests/desktop/browse_startup/run.py
```

Python compile, JavaScript function-body syntax, Prettier, receipt roundtrip,
context/row/count/navigation/cleanup checks and `git diff --check` pass. No
existing source, shared fixture, manifest, lockfile, binary or cache is changed.
