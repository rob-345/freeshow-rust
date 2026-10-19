# Retained provider catalogue desktop probe

This patch extends the existing `device_provider_probe.js` calls in the normal,
actual application restart and actual selected-library-switch portions of
`tests/desktop/smoke.py`. It adds one ordinary
`window.api.send("MAIN", {channel:"GET_CONTENT_PROVIDERS",data:ignored}, id)`
alongside the existing Device ID and empty stored-provider queries. It does not
add a diagnostic production route, bypass the retained bridge, or simulate a
provider backend.

The new listener ID is exactly
`device-provider-probe-${phase}-catalogue`. Existing identity/provider IDs and
their checks remain. The receiver requires an explicit own membership match in
the three-command ID table before comparing correlation: an unrelated callback
with undefined correlation cannot match a missing table entry. Unrelated/null
callbacks and wrong correlations are ignored; duplicates for any matched ID
reject. A retained failure flag also catches a duplicate emitted synchronously
after the final expected callback has resolved the wait. This is the bounded
probe's registration interval, not a promise to observe callbacks after removal.

For all three queries the WebView captures the exact original message-key array,
callback argument count, listener and full callback context. It requires two
arguments, only `channel,data`, the selected full context, and an immediate
undefined send result. Identity data itself is never copied into the receipt:
only existing boolean equality/nonempty/fallback/type evidence is retained.
Catalogue rows and each row's `Object.keys` array are captured before WebDriver
JSON serialization can alter object insertion order. The probe requires five
rows, the three exact ordered fields and string/string/boolean types. Callback
and runtime-error listeners/timers are removed on success and failure.

The new source-only CLI runs the actual immutable `catalogueOracle`, including
the registration/factory, all five constructors, original `GET_CONTENT_PROVIDERS`
initializer and original `receiveMain`:

```sh
node tests/desktop/provider_catalogue_oracle.mjs --expected normal
node tests/desktop/provider_catalogue_oracle.mjs --verify normal < observation.json
```

Node22.21.1 is required. Phases are `normal`, `restarted`, `switched` and the
source-only `source-check` control. The helper reuses the foundation's strict
source hashes and `assertNoCatalogueActivity`: only harmless constructor app,
route and fixture public-key setup is permitted; no credential read/write,
authentication/listen, network, browser opening, timer or publication occurs.

`--verify` accepts at most 64 KiB of JSON containing exactly `phase,context,
current,catalogue,sendUndefined`. It never accepts a raw identity field or whole
global config. `catalogue` has exactly `messageKeys,args,listener,callbackContext,
data,rowKeys`. Complete native data is compared strictly with all original five
rows/fifteen fields and original registration/key order. The separate captured
row-key arrays preserve the WebView's own-property evidence; strict JavaScript
comparison also rejects number-for-boolean substitutions Python equality would
accept. The listener must match the phase exactly. The helper additionally
checks full-context equality and void-send evidence; Python compares that
context independently with the actual native `runtime_context` command.

Each real smoke phase obtains a source expectation before native callbacks,
verifies the captured catalogue via the CLI, then repeats the source query.
`device-providers.json` keeps its existing per-phase identity/provider fields
and adds the captured catalogue, source-before/after/comparison evidence, and
void-send/context/command-error checks. The top-level `rawIdentityArchived:false`
and global identity continuity checks remain. `catalogueDeclarationOnly:true`
and `providerBackendHealthVerified:false` make the scope explicit: returned
library flags do not certify authenticated accounts or working native provider
content retrieval.

Local source-only validation passed three expected/verify phase fixtures, nine
negative CLI controls (row order, boolean type, row-key order, missing field,
private message field, listener, context, nonvoid send and input budget), plus
five controlled executions of the actual probe script. The positive script
control includes unrelated/undefined-correlation and null callbacks; negatives
cover an early duplicate, a final synchronous duplicate, a private message field
and a context change. These controlled callback executions are not native
WebView evidence. The summary is in the private temporary profile
`/tmp/freeshow-catalogue-probe-check-iF5uhu/summary.json`, with no raw identity.
Python AST syntax, Node syntax, configured Prettier and diff checks were also
run. No GUI, Cargo, app binary, shared build cache or performance measurement was
run or changed by this worker; the coordinator owns the production GUI run.
