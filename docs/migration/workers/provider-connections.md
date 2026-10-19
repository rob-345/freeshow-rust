# Stored provider connection projection

This foundation implements the original `PROVIDER_CONNECTIONS` stored-access
indication. It does not implement provider authentication, token refresh, OAuth
servers, service imports, or provider availability. Those capabilities remain
Partial/Pending until their real backends and lifecycles are implemented.

## Native API and integration

```rust
use freeshow_system::provider_connections::{project, ProjectionError};

// A successful whole ACCESS snapshot from the trusted app-data Stores owner.
let connections: serde_json::Value = project(&access_snapshot)?;
```

`project(&Value) -> Result<Value, ProjectionError>` borrows the snapshot, does no
IO, creates no workers or timers, and returns at most five `true` properties.
`ProjectionError::code()` and `message()` are static bounded diagnostics;
they contain no source keys, tokens, secrets, or values. An invalid root has
`provider_connections_invalid`; byte/node/depth rejection has
`provider_connections_limit`. There is no renderer-deserializable authority.

The coordinator must read actual `ACCESS` through the existing ordered
`LibraryService`/`Stores` owner and validate full request context before reply.
`ACCESS` is **nonportable app-data**, not the selected library's `Config/ACCESS`.
No renderer payload selects the store, root, provider catalogue, or scopes.
The original handler ignores its payload and replies on MAIN with channel
`PROVIDER_CONNECTIONS`, the original listener ID, and the projected object,
including an empty object when that is the actual source result. No
`PROVIDER_CONNECT` event is synthesized.

Host read/parse/admission/context failures must remain failures; passing `{}`
after such a failure would fabricate a source result. The original optional
uninitialized `_store.ACCESS` yields `{}`, while an actual throwing `get()`
propagates through the handler/`receiveMain` without a reply. A successful default
store snapshot is distinct from either failure. This pure API cannot establish
path authority or perform that distinction on behalf of the host.

`freeshow-system` must explicitly enable **`serde_json/preserve_order`** so its
`Value` result retains registry insertion order in standalone builds as well as
workspace builds. This worker leaves the existing manifest untouched. The
coordinator's separate DeviceID foundation already provides that dependency
setting; merge it when integrating this change. Local commands below enable the
same feature explicitly.

## Source behavior retained

Oracle: `2d94defcf98175e4f5c9504e7f4fd1d33c35c975` (1.6.7-beta.2).
The committed source receipt records 22 normalized file hashes and the exact
handler AST-text hash. CRLF is normalized to LF before hashing. Tests read the
immutable Git source, so migrated working files cannot silently replace it.

The original `ContentProviderRegistry` tests exact stored keys in this order:

| Provider         | Supported stored scope keys                                                     |
| ---------------- | ------------------------------------------------------------------------------- |
| `churchApps`     | `plans`                                                                         |
| `planningcenter` | `calendar`, `check_ins`, `giving`, `groups`, `people`, `publishing`, `services` |
| `amazinglife`    | `openid profile email` (one combined key)                                       |
| `onstage`        | `presenter`                                                                     |
| `canva`          | `folder:read design:content:read design:meta:read` (one combined key)           |

JavaScript truthiness determines connection: `{}`, `[]`, nonempty strings and
nonzero numbers are true; missing/null/false/zero/empty string are false. Expiry,
`access_token`, the active OnStage team, and the provider instance's `access`
are not inspected. Truthy malformed token objects therefore still count. Wrong
shapes for `contentProviders` or a provider container produce no matching keys.
Unknown providers, case variants, reordered combined scopes and unknown scopes
do not contribute. Only `true` keys are returned, in registration order.

Input must be ordinary serialized ACCESS JSON. JavaScript getter/prototype
objects, own `undefined`, non-finite native numbers, binary secrets and arbitrary
non-JSON values are outside this API. It does not infer decrypted/plaintext
credential meaning. The existing source store setup's repair/migration behavior
is not implemented by this projection.

An actual Conf getter normalizes raw valid-JSON null/scalar/array roots using
`Object.assign` into an object. The isolated file fixture verifies these
normalized snapshots. Passing the raw non-object directly to this native API is
an explicit invalid-input error. Existing native `Stores::get` also rejects a raw
non-object file, so that store-normalization difference remains a host/storage
gap; it is not evidence of an original source parse error.

The source factory lazily initializes cached provider instances. The native
projection does not recreate their inert Express objects; an eventual real
provider service must separately own actual initialization/authentication and
cleanup. No provider backend is reported available by this foundation.
Comparison assumes one stable stored snapshot for a synchronous query. The
original helper performs repeated `get("contentProviders")` calls; this API does
not reproduce an external process changing that file between those reads.

## Bounds and ownership

- Whole snapshot: at most **1 MiB compact serde JSON bytes**, including escaped
  string bytes, keys, punctuation, unused secrets and unknown fields.
- At most **16,384 value nodes**, and depth **16**, counting the root as zero.
  Object keys consume bytes; their associated values consume nodes.
- Validation walks borrowed data before projection. Oversized strings fail by
  their UTF-8 lower bound before a whole-string scan; escaping is counted without
  allocating a serialized snapshot. Recursion stops at the fixed depth cap.
- Allocation of the input has already occurred. The host must bound file reads
  and JSON parsing independently; this function cannot retroactively limit a
  parser's allocation. It adds no hard filesystem deadline/cancellation API.
- The existing trusted IO owner's admission/lifecycle governs calls. This pure
  function adds no new queue, runtime, process, network socket or thread.

Native limits are explicit resource boundaries relative to the
unbounded source. Rejections never masquerade as successful empty connection
objects. Resource work is finite; no whole-application performance claim follows.

## Focused evidence

Five new Rust tests cover registration order, exact scope keys, truthiness,
wrong shapes, immutability, invalid roots, and exact byte/node/depth boundaries,
including unused secret fields, Unicode, escapes and oversized keys. Eight Node
22 source suites compare the actual original handler/registry/base/five provider
constructors/data helper and five Connect/OAuth/type modules against the native
projection, including:

- Actual module load creates three Connect Express apps; the first query adds
  five cached provider apps/routes and five public-configuration key reads.
  Subsequent queries reuse those instances. No actual private keys are read.
- Strict spies observe zero backend/listener/browser/event/timer/store-write
  activity during queries. Negative controls invoke actual PCO refresh and Canva
  authentication, proving the network and listener traps would detect them.
  External requests, helpers/importers/content libraries remain trapped; this is
  not a provider authentication success test.
- An isolated real `Conf` app-data file and conflicting selected-library file
  demonstrate snapshot selection and live stored transitions. Fixtures and
  directories are privately created and removed; no real user stores are opened.
- The original MAIN dispatcher preserves channel/listener/payload behavior and
  propagates injected source read errors. Resource errors and malformed transport
  are explicit in the native test driver.

The example is a bounded **test driver**, not a runtime Node/native sidecar:

```sh
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo build -p freeshow-system --features serde_json/preserve_order \
  --example provider-connections-parity --offline --locked
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test -p freeshow-system --features serde_json/preserve_order \
  --all-targets --offline --locked
node --test tests/parity/provider-connections.test.mjs
```

The Node driver defaults to root `target/debug/examples/provider-connections-parity`
(`.exe` on Windows); set `FREESHOW_PROVIDER_CONNECTIONS_BIN` for a private target.
Tests require Node22.21.1, installed project dependencies and the pinned Git
object. No official Electron executable, credentials, browser, Spotify process,
provider listener or external connection is required. This is portable pure
projection evidence, with no macOS/Windows native auth/runtime claim.

Verified locally: five new Rust tests plus all eight existing system tests;
eight Node suites with zero skips; strict all-target Clippy on Linux, Apple
`aarch64-apple-darwin`, and Windows `x86_64-pc-windows-gnu`; Rust/Prettier formatting.
Apple/Windows checks are compilation evidence, not native runtime executions.
