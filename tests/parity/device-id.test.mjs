import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import Conf from "conf"
import { test } from "node:test"
import { execute, fixtureUuid, sourcePlan, sourceExpose } from "./lib/device-id-oracle.mjs"

test("actual immutable getMachineId reuses raw truthy JSON and performs no legacy/random/persist effects", () => {
    for (const value of [true, 1, -1, "retained 🐟", [], {}, JSON.parse('{"z":0,"2":"two","__proto__":{"own":true}}')]) {
        assert.deepEqual(sourcePlan({ value, legacyFailed: true, randomFailed: true, writeFailed: true }), { reply: value, writes: [], calls: ["get"] })
    }
})

test("actual source persists generated legacy/UUID values and outer write/entropy/read failures return the literal", () => {
    for (const value of [undefined, null, false, 0, -0, ""]) {
        assert.deepEqual(sourcePlan({ value }), { reply: "fixture-legacy-id", writes: [{ key: "machineId", value: "fixture-legacy-id" }], calls: ["get", "legacy", "set"] })
    }
    assert.equal(sourcePlan({ legacyFailed: true }).reply, fixtureUuid)
    for (const input of [{ readFailed: true }, { loadFailed: true }, { writeFailed: true }, { legacyFailed: true, randomFailed: true }, { legacyFailed: true, writeFailed: true }]) assert.equal(sourcePlan(input).reply, "fallback-machine-id")
    assert.deepEqual(sourcePlan({ configAbsent: true }).calls, ["legacy"])
    assert.deepEqual(sourcePlan({ setterAbsent: true }).writes, [])
})

test("actual pinned node-machine-id handles empty provider output and exact marker/whitespace/case rules", () => {
    const empty = sourceExpose("linux", Buffer.alloc(0))
    assert.equal(empty.hash, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    assert.deepEqual(sourceExpose("win32", "no REG marker"), { ok: false })
    assert.deepEqual(sourceExpose("darwin", "no IO property"), { ok: false })
    const linux = sourceExpose("linux", "ABcd\r\n\t\uFEFF")
    assert.deepEqual(sourceExpose("win32", "header REG_SZ ABcd\r\n\t\uFEFF"), linux)
    assert.deepEqual(sourceExpose("darwin", 'header IOPlatformUUID = "ABcd"\nignored'), linux)
    assert.equal(execute({ mode: "plans", cases: [{}] }).length, 1)
})

test("actual Conf accepts a valid deep unrelated configuration and source retains its existing identity", () => {
    const folder = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-device-id-depth-"))
    try {
        const store = new Conf({ cwd: folder, configName: "config", projectName: "freeshow-device-id-parity" })
        const raw = '{"machineId":"synthetic-retained-id","nested":' + "[".repeat(130) + "0" + "]".repeat(130) + "}"
        fs.writeFileSync(store.path, raw)
        const value = store.get("machineId")
        assert.equal(value, "synthetic-retained-id")
        assert.deepEqual(sourcePlan({ value }), { reply: value, writes: [], calls: ["get"] })
        assert.equal(fs.readFileSync(store.path, "utf8"), raw)
    } finally {
        fs.rmSync(folder, { recursive: true, force: true })
    }
})
