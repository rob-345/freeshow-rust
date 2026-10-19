# Exact Spotify command Request input

This component prepares a typed request for the native Spotify command host. It performs no IO, starts no service/worker/runtime, grants no authority and creates no reply or source acknowledgement. Shared Tauri routing, frontend selection/snapshotting and main-window/full-context validation remain coordinator integration work.

## API and frame

`spotify_input::parse(bytes: &[u8]) -> Result<Input, WireError>` returns `Input { pub request: freeshow_core::protocol::Request, pub number: Option<f64> }`. `WireError` has static `code` and `message` fields. `FRAME_LIMIT` is 65,536 bytes. Dependencies are the native manifest's existing `serde`, `serde_json` with `raw_value` and `preserve_order`, `freeshow-core`, and `freeshow-spotify`; there is no manifest/global serde feature change.

The body is **one** original Request snapshot encoded as UTF-8 JSON:

```json
{
    "channel": "MAIN",
    "data": {
        "channel": "SPOTIFY_COMMAND",
        "data": { "command": "seek", "value": 0.0000005 }
    },
    "listenerId": "optional-original-listener",
    "context": { "runtimeId": "native", "libraryGeneration": 0, "dataPath": "/selected" }
}
```

Outer channel/data/context and both middle fields are required. These private objects and full context reject extra, duplicate or malformed fields. Listener may be missing, null or a string, matching `Request::listener_id`; strings are bounded to 8,192 UTF-8 bytes. Payload must be an object with a literal supported command. Source payload extras remain ordered/ignored within bounds, but duplicate keys anywhere in that payload reject, including equivalent escaped keys. Context runtime ID is nonempty, at most 256 bytes and contains no NUL; data path is nonempty, at most 32,768 bytes and contains no NUL; generation is an integer in `0..=9007199254740991`. These are framing checks, not proof that a supplied path/context is authorized or current.

`seek` and `setVolume` require a present JSON Number. The selected `RawValue` and command derive from the **same** frame; `str::parse::<f64>` supplies exact finite binary64. Returned `Request.data.data.value` remains a numeric `Value` with the same bits, and `number` is `Some`. The pure `PreparedCommand::new` performs the foundation's actual supported seek-range validation (scaled microseconds finite and within the exact safe-integer range). Volume is not clamped. `playpause`, `next`, `prev` and `pause` return `None` and ignore any bounded JSON value shape, including missing. Unknown commands and unsupported numeric shapes reject before any native owner admission.

The main adapter should pass the returned owned Request to `NativeSpotify::dispatch_with_number(..., number)` for `Some`, and ordinary `dispatch` for `None`. It must not manufacture an override from a different frame or separately parsed generic `Value`. The existing native host still validates main role, full current context, listener, admission and deadline. No parser output is executable/session-bus authority.

## Bounds and unsupported inputs

An allocation-free iterative UTF-8/structural pass runs **before** recursive serde parsing. The whole frame is limited to 64 KiB, 16 nested containers and 4,096 structural tokens (keys count too). Brackets in escaped strings do not consume nesting. A second payload visitor rejects duplicate keys, preserves insertion order and exact number tokens, and charges the same host-style 32 bytes per `Value` plus decoded string/key bytes against 64 KiB, with at most 4,096 values. Repeated nested parsing has an explicit 17-frame-byte work budget. Initial envelope/route/header parsing adds only a fixed number of passes over the bounded frame. These are finite allocation/work charges, not a claim that the entire process heap is 64 KiB. Whole-frame depth/token bounds are deliberately narrower than the host's payload-only bounds.

This is the supported JSON subset, not all JavaScript object semantics. Lone Unicode surrogates, nonfinite number literals, duplicates, excessive depth/work and custom/boxed payload objects that remain objects reject explicitly. **JSON.stringify already destroys some distinctions:** `-0` becomes `0`, NaN/Infinity become null, undefined may disappear, and a boxed Number can become an indistinguishable primitive Number. The parser rejects collapsed null/missing numeric values, but cannot retrospectively reject a boxed Number serialized as a finite number. The frontend must validate primitive finite numbers before serialization and snapshot once. Raw valid `-0`/`-0.0` tokens preserve their sign; this does not claim JSON.stringify preserves it.

## Evidence

Six focused Rust tests pass using Node 22.21.1 and Rust 1.99.1. The cross-language test creates 29 canonical/raw numerical frames and executes the actual pinned complete Spotify module, `SPOTIFY_COMMAND` handler and `receiveMain` via the existing source oracle. That oracle verifies CRLF-normalized immutable source hashes:

| Source at `2d94defcf98175e4f5c9504e7f4fd1d33c35c975` | SHA-256                                                            |
| ---------------------------------------------------- | ------------------------------------------------------------------ |
| `src/electron/utils/spotify.ts`                      | `75d1ceab68e46c2a21dfc5a61f4e667f1d7d130e7b34b377938b7a97082918fd` |
| `src/electron/IPC/responsesMain.ts`                  | `70372b48b698f74f239b82f92e48dde69880a7c4d78af1a9f0e1723313fb57a3` |
| `src/electron/IPC/main.ts`                           | `11afb38cdc1ca36134b48b7457da2cd27078dedd8b6d7a54cf24e0ecc0608609` |

Actual source `exec` is controlled to capture the generated command and supply a fixed metadata track ID; no Spotify/service/network is invoked. Every case observes the source early `true` callback with its original listener and an undefined receive return. Actual Node IEEE hex bits are compared directly with both the native selected number and retained payload; re-parsed expected decimal numbers are not the numeric oracle. Cases include subnormals, exponent forms, adjacent binary64 values, decimal halfway ties, signed zero, maximal finite volume and positive/negative seek bounds. Another 24 actual source cases verify the four commands with ignored value shapes. Separate tests cover same-frame selection/order, context/listener limits, malformed UTF-8/Unicode/JSON/schema/duplicates, numeric rejection, bounded charged work and an actual 20,000-level nested input rejected by preflight.

A genuine failing control temporarily replaced exact token parsing with generic `serde_json::Value::as_f64`. The actual source-bit test first failed on a genuine canonical JSON.stringify frame containing `-9.084938291167941e+48`: generic bits `14562632777540591288`, actual Node bits `14562632777540591289`. An earlier raw long-decimal control also failed on `8.3163996793004788882e-275`: generic bits `506383232743652210`, actual Node bits `506383232743652209`. Restoring the exact parser passes. The tests also demonstrate boxed Number acceptance **after** irreversible JSON serialization and explicit rejection of NaN/Infinity/null/string/object numeric inputs. No global `float_roundtrip` feature is enabled to make this test pass.

Focused native tests and strict all-target Clippy run through a private headless manifest/target under `/mnt/data/cache/freeshow-rust-build/spotify-input`, with one Cargo job, incremental disabled and debug info zero. The integration test uses ordinary `node`/installed TypeScript by default; local isolated checks select `FREESHOW_ORACLE_NODE` and an optional `FREESHOW_ORACLE_NODE_LOADER` for existing private TypeScript dependencies. There is no missing-source/dependency skip. Linux runs execute the six tests; Windows GNU/macOS ARM checks compile/lint but do not establish foreign runtime evidence.

## Source-global command lifetime distinction

At the immutable source, `responsesMain.ts:98–101` changes stores for `UPDATE_DATA_PATH` without cancelling Spotify. Its command handler at 253–255 awaits `executeSpotifyCommand` and returns true; the Linux seek callback in `spotify.ts:228–240` can later schedule `SetPosition` independently of the selected library. There is no context/root cancellation in that module. The current native host's explicit old-context command cancellation is a **native retirement policy**, not an observed differential source match. This wire component adds no cancellation policy. Caller/event Drop preservation and the early callback are separate source-backed contracts. No hardware, real Spotify account, playback, main/bridge wiring or production raw transport claim is made by these framing tests.
