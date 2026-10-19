# Private remote-cover folder ALERT bridge

`src/frontend/rust/coverBridge.ts` restores only the original MAIN folder-creation ALERT from a private native fixed-cover publication. It adds no remote route, path grant, filesystem operation, source command, source success ACK, metadata handling or native producer retirement. Root's shared bridge/main wiring and actual native producer ownership remain separate.

## Contract and integration

```ts
const covers = createCoverBridge({
    getContext: () => currentFullContext,
    deliver: (message) => originalMainReceiver(message), // listenerId is undefined
    report: reportPrivateError,
    complete: (context, id, accepted) => invokeNativeCoverComplete(context, id, accepted)
})
// Only native publication envelopes from the trusted main-window subscription:
covers.publish({
    event: {
        channel: "MAIN",
        message: { channel: "ALERT", data: "Error: Could not create folder at: /native/Audio!" },
        context: capturedFullContext
    },
    id: "native-cover-" + native32LowerHex,
    sequence: nativePositiveDecimalU64,
    folder: "/native/Audio"
}) // returns original void

await covers.retire(previousFullContext) // only a bounded JS completion observation
await covers.close() // terminal admission seal + observation of all JS completions
```

The producer must construct the publication only from an actual ordered folder-mkdir failure with the captured full context and native fixed Audio path. This helper validates that private receipt; it does not prove the native path's provenance. There is no JS path join, canonicalization, normalization, parent reconstruction or filesystem authority derived from the diagnostic. Literal Linux backslashes and native Windows separators remain unchanged.

The envelope has exactly `event`, `id`, `sequence`, `folder`; event has exactly `channel`, `message`, `context` and optional own `listenerId: undefined`; message has exactly `channel`, `data`. Other private fields must remain outside this projected envelope. Context has exactly `runtimeId`, `libraryGeneration`, `dataPath`. Plain/null-prototype data objects are accepted; arrays, foreign prototypes, symbols, accessors, hidden fields and unexpected keys reject. Native input must be bounded serialized data, not arbitrary JS Proxies; reflection cannot certify a hostile Proxy's behavior. `getContext`, `deliver` and `report` are trusted bounded synchronous callbacks.

The current complete context is captured as a frozen three-field object. Event context must match runtime ID, generation and path exactly; there is no same-generation shortcut or foreign-context adoption. Runtime ID is nonempty and at most256 UTF8 bytes, selected path nonempty and at most32768 bytes, and generation a nonnegative safe integer. Strings reject NUL and lone UTF16 surrogates. No text normalization is applied.

Folder is at most32774 UTF8 bytes and ends in exactly `/Audio` or `\Audio`. Data is at most32814 UTF8 bytes and must equal `Error: Could not create folder at: ` + folder + `!`. Native code must derive this folder from its actual ordered observation, never renderer fields. The longer bound covers a32768-byte context path plus the fixed Audio suffix. Strings are length-checked before scanning and counted without allocating UTF8 buffers.

Only a fresh `{channel:"ALERT",data:originalString}` reaches the source callback. No event/context/nonce/sequence/folder/private completion fields or listener ID enter that object. The private completion gets the frozen context, captured ID and boolean acceptance; mutation of source inputs/callback data cannot change those values. This completion is not an original HTTP/source acknowledgement and must not gate the source404 continuation.

## Ownership, replay and retirement

Four completion records are shared for this WebView module's lifetime, across contexts, reentrancy and accidental helper replacement. Reservation and the owned promise are installed before any original callback or validation report that can reenter. `complete` enters in a microtask after synchronous source delivery. The slot remains held until its actual returned promise settles, including lost responses, caller-observer drop, context change and terminal close. A fifth publication reports `cover_completion_busy` and performs neither source delivery nor a new native completion call. The same unconsumed sequence can retry after real settlement frees capacity.

Canonical sequence is a positive decimal u64, at most20 digits and at most18446744073709551615. A single BigInt monotonic counter is shared across contexts. It commits after all envelope/message checks and immediately before the original callback, preventing reentrant replay. A throwing callback may have partial effects: it reports the error and completes negatively while retaining the consumed sequence. Known current-context malformed payloads negatively complete if capacity exists, without committing sequence. Invalid identity/context, an already consumed/reordered sequence, and an ID already among retained completions never cause another native invocation. Native completion rejection reports once and releases its slot only at actual settlement.

`retire(context)` synchronously seals admission only when that exact context is currently authoritative. It waits only for already reserved/entered completions with that full context. A genuine current-context change clears/replaces the one shared seal marker. Retiring an old/foreign context may wait its matching records without adopting it or adding an unbounded tombstone set. One scoped observer is allowed; another scope returns `cover_wait_busy`. Repeated observation of the same scope reuses its promise. The supplied `getContext` must remain the one authoritative current context for replacement helpers.

`close()` globally and terminally seals admission and observes all four possible records, even if `getContext` is absent or close occurs reentrantly before a reserved native invocation has entered. There is one terminal observer, independent of the scoped observer. Default timeout is20000ms; explicit native/test configuration must be an integer1..60000ms. `cover_retirement_timeout` never releases, cancels, replaces or declares disposal of a retained completion. The observer can be retried. A discarded timed-out observer has an internal rejection handler while the returned promise still rejects for observing callers. No per-publication timer or observer queue is created.

Settlement observations resolve after actual promise settlement, including a reported completion failure. They do not establish successful native acknowledgement, disposal of the native producer or IO-thread join. Root must independently retain, retire and drain the native producer even if a frontend observation fails or disappears. Browser reload creates a new JS context; this module lifetime is not native process lifetime. Native nonce/sequence authority and process resources must remain protected by the native owner across reloads.

`status()` exposes private `completionSlots`, `enteredCompletions`, `sequence`, `closed`, `sealedCurrent`, `scopedWait`, `terminalWait`. It adds no original source events. Now-playing's separate twenty-completion budget and all metadata behavior remain untouched.

## Evidence and remaining integration

Eighteen Node22 suites execute the actual exported TypeScript helper. A strict TypeScript program checks it against the current real `RuntimeContext` declaration, with no shared implementation imports. The immutable source `files.ts` is SHA256-checked; its actual `makeDir` runs against a real Unicode regular-file blocker and supplies the exact ALERT/void observation. Only its publication/console boundaries are controlled. Bytes in the blocker stay unchanged.

Tests cover Unicode/UTF8 maxima, both path separator spellings, private-field isolation, full-context runtime/path/generation mismatches, invalid envelopes/accessors/prototypes/hidden fields/symbols, NUL/lone surrogates, canonical u64 limits/replay, callback/native failure, frozen context, four lost responses across context change, fifth rejection/retry, reentrant source/native completion calls, synchronous seals, scoped versus terminal waits, timeout/retry without quota release, discarded observers, missing current context at close, and replacement-helper budget/sequence/seal/terminal retention. No real native IPC, HTTP body, GUI, OS route or producer-disposal parity is claimed.

```sh
/home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node \
  --test tests/parity/cover-bridge.test.mjs
npx prettier --config config/formatting/.prettierrc.yaml --check \
  src/frontend/rust/coverBridge.ts tests/parity/cover-bridge.test.mjs \
  docs/migration/workers/cover-bridge.md
```

No Cargo, GUI, shared bridge/main, manifests, native owner or original Electron source is changed by this slice. Production registration, native-only provenance, full-context acceptance, main-window isolation, real completion invocation and independent native shutdown remain Root integration obligations.
