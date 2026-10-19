# Remote cover desktop evidence helper

`tests/desktop/remote_cover_http.py` exports:

```python
receipt = probe_cover_http(
    script, invoke, ROOT, phase, full_context,
    expected_path,  # actual Audio/NowPlayingCover.png Path, or None for 404
    blocker=False,
)
```

Root owns `smoke.py` integration, actual GUI execution, fixture changes and
independent regular-file blocker byte checks. It also verifies/dismisses the
original visible ALERT popup. This helper neither writes artifacts nor sends
source commands. `script(source, fixture)` uses the existing WebView receipt
wrapper; `invoke(name)` uses the existing status invocation wrapper.

The JavaScript helper registers its MAIN listener before Python HTTP traffic.
Only `remotes_status` and `now_playing_status` are invoked. It checks the full
current context, captured listener run/endpoints, exact original ALERT string,
own public message keys `channel,data`, undefined listener ID, zero runtime and
browser errors, and unchanged scoped now-playing `revealCalls`. It records
bounded unrelated MAIN channel names separately; cover/ACK events are failures.
Initial and final actual `cover.filesRetained` and `publicationsRetained` must
be zero. A nonzero final value is polled, rather than treated as completion.

Python connects the three actual native bound ports using `http.client`, mapping
unspecified listen addresses to loopback and rejecting non-loopback destinations.
It sends OPTIONS first on every port and checks `200`, `GET,HEAD`, matching
Content-Length and Allow. A separate observer checkpoint requires zero ALERTs
and zero retained owners; `Audio` lstat evidence must remain unchanged. This
proves no observed folder mutation/publication, not the absence of every syscall.
Then each port receives:

| Fixture                    | Requests after OPTIONS                | Expected evidence                                                        |
| -------------------------- | ------------------------------------- | ------------------------------------------------------------------------ |
| Missing cover              | GET, HEAD                             | Empty404; HEAD omits Content-Length                                      |
| Existing cover             | GET, HEAD, Range GET, conditional GET | Exact independent bytes; image/png and full length; one-byte206; ETag304 |
| Regular-file Audio blocker | One GET only                          | Empty404 and exactly one original folder ALERT per port                  |

There are 9, 15, or 6 requests per phase respectively. Blocker HEAD is intentionally
excluded because it would produce three additional genuine source ALERTs. The
Linux fixture message is exactly `Error: Could not create folder at: ` plus
captured `dataPath + '/Audio'` plus `!`; this test string creates no native path
authority. Actual file bytes are read independently with a bounded regular-file
descriptor; matching observed identity/size/times around that read does not prove
an immutable snapshot. Existing fixtures must contain 1 byte through 4 MiB so the
one-byte range case has an unambiguous 206 expectation. Opaque and zero-length production
files remain covered by the independent file/planner tests.

The JS slot is singular, auto-cleans after 15s, and removes its receiver/error
listeners on finish, abort, context failure or failed checks. Python invokes
abort on intermediate failures without replacing the original diagnostic. The
phase has a 15s monotonic observation/body budget, socket operations use at most
one-second timeouts, and each response permits 4096 reads of at most 32 KiB, 4 MiB
body, 32 headers and 16 KiB aggregate header text. JSON receipts are capped at 512 KiB
and contain lengths/hashes/headers instead of full image payloads. Driver and OS
call lifetimes remain governed by Root's outer desktop harness. HEAD evidence
uses HTTP-framed body semantics, not raw bytes beyond that frame.

Focused helper validation used Node 22 syntax/Prettier, Python compilation, 18
controlled JS cases and 15 controlled Python request-flow cases. The controls
include private-field/listener leakage, full-context changes, retained-owner
waiting, reveal changes, callback/runtime failures, cleanup, malformed HTTP
headers/body/ranges, OPTIONS mutation, 4 MiB payload and bounded-read failures.
These controls substitute status/HTTP boundaries and are not native GUI/TCP
evidence. Root's actual GUI run supplies that evidence. Neither the receipt nor
zero retention is presented as an independent native IO-thread join proof.
