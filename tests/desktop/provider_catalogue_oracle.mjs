// Source-only catalogue expectations/comparison; no native identity is accepted.
import assert from "node:assert/strict"
import { catalogueOracle, assertNoCatalogueActivity } from "../parity/provider-catalogue-oracle.mjs"
import { provenance } from "../parity/provider-connections-oracle.mjs"

assert.equal(process.versions.node, "22.21.1", "Pinned catalogue oracle requires Node22.21.1")
const [mode, phase, ...extra] = process.argv.slice(2)
assert.ok(["--expected", "--verify"].includes(mode) && extra.length === 0, "Expected --expected/--verify PHASE")
assert.ok(["normal", "restarted", "switched", "source-check"].includes(phase), "Unknown catalogue fixture phase")
const oracle = catalogueOracle(),
    listener = "device-provider-probe-" + phase + "-catalogue",
    replies = await oracle.dispatch({ ignored: "payload cannot select a global store", machineId: "untrusted", contentProviders: { fake: true } }, listener)
assert.equal(replies.length, 1)
assert.equal(replies[0].length, 3)
assert.equal(replies[0][0], "MAIN")
assert.equal(replies[0][1].channel, "GET_CONTENT_PROVIDERS")
assert.equal(replies[0][2], listener)
const data = replies[0][1].data,
    rowKeys = Array.from(data, (row) => Object.keys(row)),
    messageKeys = Object.keys(replies[0][1])
assert.equal(data.length, 5)
assert.equal(
    rowKeys.reduce((count, keys) => count + keys.length, 0),
    15
)
assert.deepEqual(messageKeys, ["channel", "data"])
for (const keys of rowKeys) assert.deepEqual(keys, ["providerId", "displayName", "hasContentLibrary"])
assertNoCatalogueActivity(oracle.calls)
const expected = {
    phase,
    catalogue: { outer: replies[0][0], command: replies[0][1].channel, data, rowKeys, messageKeys, listener, sourceReplyArgs: replies[0].length },
    sourceActivity: { ...oracle.calls },
    provenance: { sourceCommit: provenance.oracle, node: process.versions.node, handlerSha256: "3e5bceffed2ea7418b66395a1c4d134b309dc1dbf0058ccf8fd15fff9d862eb9", registrySha256: provenance.sources["src/electron/contentProviders/ContentProviderRegistry.ts"] },
    declarationOnly: true,
    authenticationOrBackendHealthVerified: false,
    rawIdentityArchived: false
}
let comparison
if (mode === "--verify") {
    const chunks = []
    let length = 0
    for await (const chunk of process.stdin) {
        length += chunk.length
        assert.ok(length <= 64 * 1024, "Catalogue observation exceeds the private transport budget")
        chunks.push(chunk)
    }
    const observed = JSON.parse(Buffer.concat(chunks).toString("utf8"))
    assert.deepEqual(Object.keys(observed).sort(), ["catalogue", "context", "current", "phase", "sendUndefined"])
    assert.equal(observed.phase, phase)
    const context = observed.context
    assert.deepEqual(Object.keys(context).sort(), ["dataPath", "libraryGeneration", "runtimeId"])
    assert.ok(typeof context.runtimeId === "string" && !!context.runtimeId && Number.isSafeInteger(context.libraryGeneration) && context.libraryGeneration >= 0 && typeof context.dataPath === "string")
    assert.deepEqual(observed.current, context)
    assert.deepEqual(Object.keys(observed.catalogue).sort(), ["args", "callbackContext", "data", "listener", "messageKeys", "rowKeys"])
    assert.deepEqual(observed.catalogue.callbackContext, context)
    assert.equal(observed.catalogue.args, 2, "Retained preload callback carries only message and listener")
    assert.equal(observed.catalogue.listener, listener)
    assert.deepEqual(observed.catalogue.messageKeys, messageKeys)
    assert.deepEqual(observed.catalogue.rowKeys, rowKeys)
    // JSON transport preserves all catalogue values; strict comparison also
    // rejects number-for-boolean substitutions hidden by Python equality.
    assert.deepEqual(observed.catalogue.data, JSON.parse(JSON.stringify(data)))
    assert.equal(observed.sendUndefined, true, "Source send returns undefined")
    comparison = { matches: true, completeRows: 5, completeFields: 15, keyOrderMatched: true, exactListenerMatched: true, fullCallbackContextMatched: true, voidSendMatched: true }
}
process.stdout.write(JSON.stringify({ ...expected, ...(comparison ? { comparison } : {}) }) + "\n")
