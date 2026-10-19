import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import fs from "node:fs"
import path from "node:path"
import test from "node:test"
import { assertNoCatalogueActivity, catalogueOracle } from "./provider-catalogue-oracle.mjs"
import { assertNoProviderActivity, root } from "./provider-connections-oracle.mjs"

const binary = process.env.FREESHOW_PROVIDER_CONNECTIONS_BIN || path.join(root, "target/debug/examples/provider-connections-parity" + (process.platform === "win32" ? ".exe" : ""))
assert.ok(fs.existsSync(binary), "Build freeshow-system --example provider-connections-parity or set FREESHOW_PROVIDER_CONNECTIONS_BIN")
const clean = (value) => JSON.parse(JSON.stringify(value))
function native(data, { catalogue = true, raw = false } = {}) {
    const result = spawnSync(binary, catalogue ? ["--catalogue"] : [], { input: raw ? data : data === undefined ? "" : JSON.stringify(data), encoding: "utf8", timeout: 10000, maxBuffer: 2 * 1024 * 1024 })
    assert.equal(result.status, 0, result.stderr || result.error?.message)
    return JSON.parse(result.stdout)
}
function compare(oracle, data) {
    const expected = oracle.invoke(data),
        actual = native(data)
    assert.equal(actual.ok, true, actual.error?.code)
    assert.deepEqual(actual.value, clean(expected))
    assert.equal(actual.value.length, 5)
    for (let index = 0; index < expected.length; index++) {
        assert.deepEqual(Object.keys(actual.value[index]), Object.keys(expected[index]))
        for (const key of Object.keys(expected[index])) assert.equal(typeof actual.value[index][key], typeof expected[index][key])
    }
    assertNoCatalogueActivity(oracle.calls)
    return actual.value
}

test("actual immutable registration, factory, constructors and MAIN catalogue agree with the native fixed rows and key order", () => {
    const oracle = catalogueOracle()
    assert.equal(oracle.calls.apps, 3, "three actual Connect class apps at module load")
    assert.equal(oracle.calls.routes, 0)
    const keysBefore = oracle.calls.keys,
        rows = compare(oracle)
    assert.equal(oracle.calls.apps, 8)
    assert.equal(oracle.calls.routes, 5, "provider constructors register routes without listening")
    assert.equal(oracle.calls.keys, keysBefore + 5)
    assert.deepEqual(
        rows.map((row) => row.providerId),
        clean(oracle.registry.getAvailableProviders())
    )
    for (const row of rows) {
        const instance = oracle.registry.getProvider(row.providerId)
        assert.equal(row.displayName, instance.displayName)
        assert.equal(row.hasContentLibrary, instance.hasContentLibrary)
        assert.deepEqual(Object.keys(row), ["providerId", "displayName", "hasContentLibrary"])
    }
    assert.ok(Buffer.byteLength(JSON.stringify(rows)) < 1024)
    const before = { ...oracle.calls }
    compare(oracle)
    assert.deepEqual(oracle.calls, before, "repeat calls reuse source instances without new backend work")
})

test("ordinary MAIN catalogue ignores payload and preserves the exact original reply route and listener", async () => {
    const oracle = catalogueOracle(),
        baseline = compare(oracle)
    for (const [index, payload] of [undefined, null, false, 0, "canva", [], { providerId: "unknown", enabled: false, contentProviders: { canva: true }, secret: "fixture-not-a-provider-capability" }].entries()) {
        assert.deepEqual(compare(oracle, payload), baseline)
        const listener = index % 2 ? undefined : `catalogue-${index}-敬拜`,
            replies = await oracle.dispatch(payload, listener)
        assert.equal(replies.length, 1)
        assert.equal(replies[0].length, 3)
        assert.equal(replies[0][0], "MAIN")
        assert.deepEqual(Object.keys(replies[0][1]), ["channel", "data"])
        assert.equal(replies[0][1].channel, "GET_CONTENT_PROVIDERS")
        assert.deepEqual(clean(replies[0][1].data), baseline)
        assert.ok(Object.hasOwn(replies[0], 2))
        assert.equal(replies[0][2], listener)
        assertNoCatalogueActivity(oracle.calls)
    }
})

test("catalogue declarations neither read authentication nor establish a working content backend", () => {
    const oracle = catalogueOracle({ contentProviders: { churchApps: { plans: { access_token: "fixture" } } } }),
        baseline = compare(oracle)
    oracle.failRead(new Error("ACCESS must remain untouched"))
    assert.deepEqual(compare(oracle, { requireConnected: true }), baseline)
    oracle.available(false)
    assert.deepEqual(compare(oracle), baseline)
    for (const provider of baseline) {
        const instance = oracle.registry.getProvider(provider.providerId)
        assert.equal(instance.isConnected(instance.supportedScopes[0]), false, "source instance has no authenticated access")
    }
    assert.equal(
        baseline.some((row) => row.hasContentLibrary),
        true
    )
    assertNoCatalogueActivity(oracle.calls)
})

test("source and native catalogue results are fresh snapshots despite consumer mutation", () => {
    const oracle = catalogueOracle(),
        expected = compare(oracle),
        source = oracle.invoke(),
        actual = native().value
    source[0].displayName = "Changed"
    source.reverse()
    actual[0].hasContentLibrary = false
    actual.pop()
    assert.deepEqual(compare(oracle), expected)
})

test("the reused strict original graph really traps refresh, authentication listeners and backend activity", async () => {
    const refresh = catalogueOracle({ contentProviders: { planningcenter: { services: { created_at: 1, expires_in: 1, refresh_token: "fixture" } } } })
    compare(refresh)
    assert.equal(await refresh.registry.connect("planningcenter", "services"), false)
    assert.equal(refresh.calls.backend, 1)
    assert.throws(() => assertNoProviderActivity(refresh.calls), /backend/)
    const auth = catalogueOracle()
    compare(auth)
    assert.equal(await auth.registry.connect("canva", "folder:read design:content:read design:meta:read"), false)
    assert.equal(auth.calls.listens, 1)
    assert.equal(auth.calls.opens, 0)
    assert.throws(() => assertNoProviderActivity(auth.calls), /listens/)
})

test("private catalogue selector preserves the existing transport bound and default stored-access mode", () => {
    assert.deepEqual(native("invalid-json-is-still-ignored", { raw: true }).value, native().value)
    const excessive = native("x".repeat(1024 * 1024 + 1), { raw: true })
    assert.equal(excessive.ok, false)
    assert.equal(excessive.error.code, "provider_connections_transport")
    assert.deepEqual(native({ contentProviders: { onstage: { presenter: true } } }, { catalogue: false }), { ok: true, value: { onstage: true } })
    assert.equal(native("{invalid-json", { raw: true, catalogue: false }).error.code, "provider_connections_parse")
})
