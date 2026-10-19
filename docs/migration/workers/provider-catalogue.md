# Pinned provider catalogue preparation

`freeshow_system::provider_connections::catalogue() -> serde_json::Value`
returns a fresh fixed five-row `GET_CONTENT_PROVIDERS` declaration. It takes no
request data, ACCESS snapshot, context, native path, account or provider handle.
It does no IO and needs no worker, task, timer, additional dependency or shutdown
owner. The existing stored-access `project` operation keeps its previous scopes,
ordering and bounds; both projections share one fixed provider declaration table.

The exact source registration order and result are:

| providerId     | displayName     | hasContentLibrary |
| -------------- | --------------- | ----------------- |
| churchApps     | ChurchApps      | true              |
| planningcenter | Planning Center | false             |
| amazinglife    | APlay           | true              |
| onstage        | OnStage         | false             |
| canva          | Canva           | true              |

Every row's own keys are `providerId,displayName,hasContentLibrary`, in that
order. Values are ordinary strings and booleans; there are no numeric,
undefined or restoration fields. `serde_json/preserve_order` is already enabled
by the crate. The five rows, fifteen fields and constant strings put output
below one KiB; there is no variable-length input or intermediate growth.
Consumer mutation cannot change later results.

These are **source-declared capabilities**. A row or `hasContentLibrary:true`
does not establish authentication, connection, working Rust provider content
retrieval, authorization to start OAuth, or a usable network backend. In the
actual source oracle, every instance still reports disconnected while the
catalogue declares three content libraries. Source catalogue calls never read
ACCESS, even when that store is unavailable or its reader throws. The pure
native API cannot read it because it has no input or access capability.

The original handler at `src/electron/IPC/responsesMain.ts:210` ignores its
payload, maps the registry's insertion order, and uses
`provider?.displayName || providerId` and `provider?.hasContentLibrary || false`.
The pinned actual constructors produce the five declarations above. Arbitrary
runtime registry modifications or third-party provider registration are outside
this fixed immutable-source catalogue. The source factory's lazy instance cache
is not reproduced as fake native provider objects; it affects no returned
declaration in this scope.

Coordinator integration is a short existing-dispatcher operation: validate the
real caller/window, whole request bounds and frozen full RuntimeContext, call
`catalogue()`, then issue the original `MAIN` / `GET_CONTENT_PROVIDERS` reply with
the actual listener ID and accepted context. The original `receiveMain` replies
to its requesting sender. Any stricter fork window policy belongs to the host,
not this input-free projection. No credential read, new runtime owner, hydration
or provider authentication should be added merely to answer this catalogue.
Native host/main/bridge integration is intentionally absent from this patch.

`tests/parity/provider-catalogue-oracle.mjs` reuses the existing immutable module
graph in `provider-connections-oracle.mjs`: actual registry initialization,
factory insertion order, base class and all five real constructors execute with
strict trapped OS/auth/network boundaries. All source files are obtained with
`git show` from `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, normalized only for
CRLF, and checked against the existing source-hash inventory. The separate actual
`GET_CONTENT_PROVIDERS` initializer is AST-extracted and additionally checked as
SHA256 `3e5bceffed2ea7418b66395a1c4d134b309dc1dbf0058ccf8fd15fff9d862eb9`.
Actual immutable `receiveMain` supplies the route/listener evidence.

Six new Node suites compare that actual source with the real Rust projection,
including field types/order, repeated constructor caching, ignored payloads,
listener and original reply shape, unavailable/failed ACCESS readers, fresh
snapshots, and the private transport/default-mode controls. Real source negative
controls deliberately attempt an expired-token refresh and OAuth authentication;
the network/listen traps are observed, and the no-activity assertions reject
them. Ordinary catalogue calls assert zero reads/writes, backend/listen/open,
crypto, timers, publications and logs. Only harmless source Express app/route
construction and fixture public-key lookup are allowed at initialization; no
actual Express server, provider session or network request is started.

The existing private `provider-connections-parity` example gains only a
`--catalogue` mode. Its one-MiB input transport bound remains before selection;
bounded stdin content is ignored in catalogue mode, matching the input-free
projection. Default invocation preserves the stored-access projection and its
JSON parse/resource errors. This fixture selector is not a product IPC command
or renderer authority boundary.

Focused commands, using a private Cargo target, one job, debug/incremental off:

```sh
cargo test -p freeshow-system --test provider_connections --locked
cargo build -p freeshow-system --example provider-connections-parity --locked
cargo clippy -p freeshow-system --all-targets --locked -- -D warnings
FREESHOW_PROVIDER_CONNECTIONS_BIN=/private/target/debug/examples/provider-connections-parity \
  node --test tests/parity/provider-catalogue.test.mjs tests/parity/provider-connections.test.mjs
```

No live-provider, authentication, content-import, GUI or whole-platform parity
claim follows from this pure catalogue seam.

Local validation passed seven focused Rust tests (the five existing projection
cases plus two catalogue cases), all fourteen Node22.21.1 suites (six catalogue
and eight unchanged stored-access suites), strict Linux all-target Clippy,
rustfmt, configured Prettier and `git diff --check`. Cargo used only
`/mnt/data/cache/freeshow-rust-build/provider-catalogue/target` with one job and
debug/incremental disabled; no root/shared cache was built or modified. Foreign
native execution and the eventual dispatcher/GUI hookup were not tested here.
