import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import test from "node:test"
import Conf from "conf"
import { assertNoProviderActivity, original, provenance, providerOracle, root } from "./provider-connections-oracle.mjs"

const binary = process.env.FREESHOW_PROVIDER_CONNECTIONS_BIN || path.join(root, "target/debug/examples/provider-connections-parity" + (process.platform === "win32" ? ".exe" : ""))
assert.ok(fs.existsSync(binary), "Build freeshow-system --example provider-connections-parity with serde_json/preserve_order or set FREESHOW_PROVIDER_CONNECTIONS_BIN")
const clean = (value) => JSON.parse(JSON.stringify(value))
function native(snapshot, raw = false) {
    const result = spawnSync(binary, [], { input: raw ? snapshot : JSON.stringify(snapshot), encoding: "utf8", timeout: 10000, maxBuffer: 2 * 1024 * 1024 })
    assert.equal(result.status, 0, result.stderr || result.error?.message)
    return JSON.parse(result.stdout)
}
const source = providerOracle()
function compare(snapshot) {
    const before = structuredClone(snapshot)
    source.set(snapshot)
    const expected = clean(source.invoke())
    const result = native(snapshot)
    assert.equal(result.ok, true, result.error?.code)
    assert.deepEqual(result.value, expected)
    assert.deepEqual(Object.keys(result.value), Object.keys(expected), "provider registration order")
    assert.deepEqual(snapshot, before, "projection does not mutate credentials")
    assertNoProviderActivity(source.calls)
    return result.value
}

test("all immutable source hashes and actual constructor initialization remain accounted for", () => {
    for (const filename of Object.keys(provenance.sources)) original(filename)
    const oracle = providerOracle()
    // Three actual Connect class apps at module load; five actual provider apps
    // and five base setupAuthRoutes calls only on the first registry query.
    assert.equal(oracle.calls.apps, 3)
    assert.equal(oracle.calls.routes, 0)
    const keysBefore = oracle.calls.keys
    assert.equal(keysBefore, 4)
    assert.deepEqual(clean(oracle.invoke()), {})
    assert.equal(oracle.calls.apps, 8)
    assert.equal(oracle.calls.routes, 5)
    assert.equal(oracle.calls.keys, keysBefore + 5)
    const before = { ...oracle.calls }
    oracle.invoke()
    for (const key of ["apps", "routes", "keys"]) assert.equal(oracle.calls[key], before[key])
    assertNoProviderActivity(oracle.calls)
})

test("source exact registry order, scopes, empty objects and expired access values", () => {
    assert.deepEqual(compare({}), {})
    assert.deepEqual(
        compare({
            secrets: { fixture: "not-published" },
            contentProviders: {
                canva: { "folder:read design:content:read design:meta:read": [] },
                onstage: { presenter: { activeTeamId: null, byTeam: {} } },
                amazinglife: { "openid profile email": { access_token: "", expires_in: 0, created_at: 1 } },
                planningcenter: { services: "expired fixture" },
                churchApps: { plans: {} }
            }
        }),
        { churchApps: true, planningcenter: true, amazinglife: true, onstage: true, canva: true }
    )
    for (const scope of ["calendar", "check_ins", "giving", "groups", "people", "publishing", "services"]) assert.deepEqual(compare({ contentProviders: { planningcenter: { [scope]: true } } }), { planningcenter: true })
    assert.deepEqual(compare({ contentProviders: { unknown: { plans: true }, planningcenter: { unknown: {} }, amazinglife: { openid: {}, "profile email": {} }, canva: { "design:content:read folder:read design:meta:read": true }, churchapps: { plans: true } } }), {})
    assert.deepEqual(compare(JSON.parse('{"contentProviders":{"__proto__":{"onstage":{"presenter":true}},"constructor":{"plans":true}}}')), {})
})

test("source JS truthiness and wrong-shaped nested containers agree for every provider", () => {
    const scopes = { churchApps: "plans", planningcenter: "services", amazinglife: "openid profile email", onstage: "presenter", canva: "folder:read design:content:read design:meta:read" }
    for (const [provider, scope] of Object.entries(scopes)) {
        for (const value of [null, false, true, 0, -0, 1, -1, 1e-200, Number.MIN_VALUE, Number.MAX_VALUE, "", " ", "\0", "日本語", [], [null], {}, { access_token: null }]) compare({ contentProviders: { [provider]: { [scope]: value } } })
        for (const value of [null, false, true, 0, "wrong shape", [], [null]]) assert.deepEqual(compare({ contentProviders: { [provider]: value } }), {})
    }
    for (const value of [null, false, true, 0, "wrong shape", [], [null]]) assert.deepEqual(compare({ contentProviders: value }), {})
})

test("original helper's live stored transitions, independent of provider cached access", () => {
    const oracle = providerOracle({ contentProviders: { onstage: { presenter: { access_token: "fixture" } } } })
    const provider = oracle.registry.getProvider("onstage")
    assert.equal(provider.isConnected("presenter"), false, "instance access remains null")
    assert.deepEqual(clean(oracle.invoke()), { onstage: true })
    oracle.helper.setContentProviderAccess("onstage", "presenter", null)
    assert.deepEqual(clean(oracle.invoke()), {})
    oracle.helper.setContentProviderAccess("onstage", "presenter", {})
    assert.deepEqual(clean(oracle.invoke()), { onstage: true })
    assert.equal(provider.isConnected("presenter"), false)
    assert.equal(oracle.calls.writes, 2)
    assertNoProviderActivity({ ...oracle.calls, writes: 0 })
    compare({ contentProviders: { onstage: { presenter: null } } })
    compare({ contentProviders: { onstage: { presenter: {} } } })
})

test("actual isolated Conf app-data access is used rather than selected library contents", () => {
    const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-provider-access-"))
    try {
        const appData = new Conf({ cwd: path.join(temporary, "app-data"), configName: "ACCESS", defaults: { contentProviders: {}, secrets: {} } })
        const selected = new Conf({ cwd: path.join(temporary, "selected-library", "Config"), configName: "ACCESS", defaults: {} })
        appData.set("contentProviders", { churchApps: { plans: {} } })
        selected.set("contentProviders", { onstage: { presenter: true } })
        const oracle = providerOracle({}, (key) => appData.get(key))
        assert.deepEqual(clean(oracle.invoke()), { churchApps: true })
        assert.deepEqual(native(appData.store).value, { churchApps: true })
        assert.deepEqual(native(selected.store).value, { onstage: true }, "pure API cannot infer root authority; host must supply actual ACCESS")
        appData.set("contentProviders", { planningcenter: { services: [] } })
        assert.deepEqual(clean(oracle.invoke()), { planningcenter: true })
        assert.deepEqual(native(appData.store).value, { planningcenter: true })
        for (const value of [null, [], true, 1, "raw scalar root"]) {
            appData.store = value
            const normalized = appData.store
            assert.equal(typeof normalized, "object", "actual Conf getter normalizes raw JSON roots")
            assert.equal(Array.isArray(normalized), false)
            assert.deepEqual(clean(oracle.invoke()), {})
            assert.deepEqual(native(normalized).value, {})
            assert.equal(native(value).error.code, "provider_connections_invalid", "raw non-object is not a successful normalized snapshot")
        }
        assertNoProviderActivity(oracle.calls)
    } finally {
        fs.rmSync(temporary, { recursive: true, force: true })
    }
})

test("actual MAIN reply preserves listener/channel, ignores payload, and does not swallow source read errors", async () => {
    const oracle = providerOracle({ contentProviders: { churchApps: { plans: {} } } })
    const replies = await oracle.dispatch({ contentProviders: { onstage: { presenter: true } } }, "listener-敬拜")
    assert.deepEqual(clean(replies), [["MAIN", { channel: "PROVIDER_CONNECTIONS", data: { churchApps: true } }, "listener-敬拜"]])
    oracle.failRead(new Error("fixture read failure"))
    assert.throws(() => oracle.invoke(), /fixture read failure/)
    const failedReplies = []
    await assert.rejects(
        oracle.dispatch(undefined, undefined, (...args) => failedReplies.push(args)),
        /fixture read failure/
    )
    assert.deepEqual(failedReplies, [], "a failed source read cannot publish an empty reply")
    oracle.failRead(null)
    oracle.available(false)
    assert.deepEqual(clean(await oracle.dispatch()), [["MAIN", { channel: "PROVIDER_CONNECTIONS", data: {} }, "provider-fixture"]], "source's uninitialized optional ACCESS differs from failed read")
    assertNoProviderActivity(oracle.calls)
    for (const root of [null, [], true, 1, "ACCESS is not an object"]) assert.equal(native(root).error.code, "provider_connections_invalid")
    assert.equal(native("{invalid-json", true).error.code, "provider_connections_parse")
})

test("strict oracle controls actually block refresh backend and OAuth listener lifecycles", async () => {
    const refresh = providerOracle({ contentProviders: { planningcenter: { services: { created_at: 1, expires_in: 1, refresh_token: "fixture" } } } })
    assert.equal(await refresh.registry.connect("planningcenter", "services"), false)
    assert.equal(refresh.calls.backend, 1, "actual source refresh reached trapped network boundary")
    const auth = providerOracle()
    assert.equal(await auth.registry.connect("canva", "folder:read design:content:read design:meta:read"), false)
    assert.equal(auth.calls.listens, 1, "actual source OAuth reached trapped Express listen")
    assert.equal(auth.calls.opens, 0, "listener trap stopped browser opening")
    assert.throws(() => assertNoProviderActivity(refresh.calls), /backend/)
    assert.throws(() => assertNoProviderActivity(auth.calls), /listens/)
})

test("native resource errors include unused secrets and never become successful empty projections", () => {
    assert.equal(native({ secrets: Array(16384).fill(null) }).error.code, "provider_connections_limit")
    let nested = null
    for (let index = 0; index < 17; index++) nested = [nested]
    assert.equal(native({ secrets: nested }).error.code, "provider_connections_limit")
    const transport = native({ secrets: "fixture-private-data-".repeat(65536) })
    assert.equal(transport.error.code, "provider_connections_transport")
    assert.equal(JSON.stringify(transport).includes("fixture-private-data"), false)
})
