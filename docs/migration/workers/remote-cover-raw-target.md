# Cover request-target transport boundary

This follow-up tests the actual HTTP boundary around the pure cover route matcher. It changes no listener, planner, filesystem authority, or application integration. The native adapter currently passes `request.uri().to_string()` to the matcher in `crates/freeshow-remotes/src/cover.rs:125`. That is a parsed URI, not the untouched request target.

Hyper 1.12 constructs `http::Uri` from the HTTP/1 request target in `src/proto/h1/role.rs:225–230`. `http` 1.5 then truncates a literal fragment in `src/uri/path.rs:21–30`. There is no retained raw-target request extension in this path. Axum's `OriginalUri(pub Uri)` preserves a URI across router nesting; it cannot recover bytes already discarded by the HTTP parser. No second HTTP parser or dependency patch is added here.

Consequently, the planner's 8,192-byte target limit applies to the parsed URI supplied to it. The separate Hyper transport configuration still limits the complete HTTP head to 16,384 bytes. A long discarded fragment can exceed the planner's nominal raw-target limit while remaining under that transport limit. Serving that request is also observed source behavior, not an intentional native rejection.

`tests/parity/remote-cover-raw-target.test.mjs` sends literal request bytes with `net.Socket`, avoiding client URL normalization. It extracts the original `/nowplayingcover` callback from immutable `servers.ts` at `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, CRLF-normalizes it, and verifies SHA-256 `2b8c81b56e77b754dc617e31777e4b3e1d9d15b368c369efc9d58030c1af4b41`. The actual callback runs with Express, send, a genuine temporary PNG, real `fs.existsSync`, and a following static middleware rooted at an empty private directory. The native example freezes that same file/stat before binding its three controlled loopback listeners. This is the callback/static-router boundary; it does not claim the entire original server or Engine.IO router has been retested.

| Literal target                                   | Source                   | Native, all three ports       |
| ------------------------------------------------ | ------------------------ | ----------------------------- |
| `/nowplayingcover#fragment`                      | 200, PNG                 | Exact matching representation |
| `/NOWPLAYINGCOVER/?ignored=1#fragment` with HEAD | 200, no body, PNG length | Exact matching representation |
| `/nowplayingcover#\after-fragment`               | 200, PNG                 | Exact matching representation |
| `/nowplayingcover#` followed by 9,000 `f` bytes  | 200, PNG                 | Exact matching representation |
| `/nowplayingcover\#fragment`                     | 200, PNG                 | 400, `Invalid asset path`     |

The large target is exactly 9,017 bytes; its complete HTTP head is 9,070 bytes. The first four controls compare status, every response header except volatile `Date` and `Connection`, and exact body bytes. The source fixed-file lookup counter must increment exactly once per request. The last control records a capability difference: parseurl falls back to Node's legacy URL parser on `#`, which normalizes the preceding backslash into the single trailing slash accepted by the Express route. Hyper instead retains the backslash before discarding the fragment; native routing falls through and the existing static handler rejects that backslash. A backslash after the fragment does not produce this difference. The test records both actual representations rather than fabricating raw rejection parity.

The fixture has one native child, one source listener, sequential requests, five-second operation/cleanup timeouts, a 64 KiB response limit, and only loopback clients/listeners. Successful cleanup requires the native `joined:true` receipt and actual process exit; failure cleanup kills/reaps the child and closes all source sockets. No network registration, renderer-selected source path, native application, image-decoder, GUI, or performance claim follows from these controls. The example's CLI file selection is test-only native authority.

Verified on Linux with Node 22.21.1, Express 4.22.3, send 0.19.0, parseurl 1.3.3, and finalhandler 1.3.1: four Node tests passed, covering 20 real TCP exchanges, including 12 complete native/source representation comparisons and three explicit difference assertions. The already-built root example had SHA-256 `2631426f9c69910ff4a541bee2662825cdc51ddefecb1fc57bff389aacac7fe5`; this slice performed no Cargo build. Windows/macOS behavior needs the same native test in root CI and is not established by this Linux result.

```sh
FREESHOW_REMOTES_COVER_BIN=/path/to/target/debug/examples/cover-serving-parity \
  /path/to/node-v22.21.1/bin/node --test tests/parity/remote-cover-raw-target.test.mjs
```

The accompanying read-only lifecycle review found no additional concrete blocker in the configured HTTP pool: entered native preparation/validation/finish futures remain owned by fixed file workers; body and worker share the request permit; transport cancellation disposes bodies before workers finish; transport failure/panic drains those file futures while the runtime remains alive; explicit stop awaits completion and joins the file threads. These guarantees depend on the documented trusted, nonpanicking source callback contract and cooperative actual native cleanup. They do not prove descriptor authorization, private publication completion, peer receipt of bytes, or an immutable file snapshot; those remain the native source adapter's obligations.
