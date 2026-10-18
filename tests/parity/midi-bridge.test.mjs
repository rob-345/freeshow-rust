import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { createRequire } from "node:module"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import ts from "typescript"
import { root, upstreamFunctions } from "./oracle.mjs"

const require = createRequire(import.meta.url)
const JZZ = require("jzz")
assert.equal(require("jzz/package.json").version, "1.9.5")
const javascript = ts.transpileModule(fs.readFileSync(path.join(root, "src/frontend/rust/midi.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } }).outputText
const { hydrateMidi } = await import(`data:text/javascript;base64,${Buffer.from(javascript).toString("base64")}`)
const binary = process.env.FREESHOW_MIDI_PARITY || path.join(root, "target/debug/freeshow-midi-parity" + (process.platform === "win32" ? ".exe" : ""))
function native(cases) {
    const result = spawnSync(binary, [], { input: cases.map(JSON.stringify).join("\n") + "\n", encoding: "utf8", timeout: 10000, maxBuffer: 4 * 1024 * 1024 })
    assert.equal(result.status, 0, result.stderr || result.error?.message)
    return result.stdout
        .trim()
        .split("\n")
        .map((line) => {
            const reply = JSON.parse(line)
            assert.equal(reply.ok, true, line)
            return reply.value
        })
}

test("MIDI restoration compares actual source/JZZ callbacks including missing id/bytes and original own-property order", async () => {
    const messages = [[0x90], [0x80, 60], [0x90, 60, 0], [0xb0], [0xb0, 123], [0xbf, 1, 255], [0x9f, 127, 255], [0xa0, 60, 1]].map((bytes) => JZZ.MIDI(bytes))
    const cases = []
    const expected = []
    for (const registration of [{}, { id: null }, { id: "" }, { id: 0 }, { id: false }, { id: "日本語 🎹" }, { id: { unknown: [1, "x"] } }]) {
        let callback
        const events = []
        const port = {
            name: () => "controlled",
            or() {
                return this
            },
            connect(fn) {
                callback = fn
            }
        }
        const source = upstreamFunctions("src/electron/utils/midi.ts", ["receiveMidi"], { JZZ: () => ({ openMidiIn: () => port }), sendToMain: (channel, data) => events.push({ channel, data }), ToMain: { RECEIVE_MIDI2: "RECEIVE_MIDI2" } }, "let openedPorts = {};")
        await source.receiveMidi({ input: "controlled", ...registration })
        for (const message of messages) {
            const before = events.length
            callback(message)
            cases.push({ op: "receive", ...registration, bytes: [...message], description: message.toString() })
            // Electron's structured clone crosses realms while retaining own
            // undefined and key order. JSON normalization would lose this case.
            expected.push(events.length > before ? structuredClone(events.at(-1)) : null)
        }
    }
    native(cases).forEach((wire, index) => {
        if (!expected[index]) {
            assert.equal(wire, null, "ignored source messages introduce no callback")
            return
        }
        const value = hydrateMidi("RECEIVE_MIDI2", wire.data, wire.hydration)
        assert.equal(expected[index].channel, "RECEIVE_MIDI2")
        assert.deepEqual(value, expected[index].data)
        assert.deepEqual(Object.keys(value), Object.keys(expected[index].data))
        assert.deepEqual(Object.keys(value.values), Object.keys(expected[index].data.values))
        assert.equal(Object.hasOwn(value, "id"), true)
    })
})

test("MIDI device restoration preserves actual source missing/scalar names, duplicates, Unicode and own keys", () => {
    const devices = [{ extra: 1 }, { name: null }, { name: "日本語 🎹", manufacturer: "ignored" }, { name: "日本語 🎹" }, { name: false }, { name: 0 }, { name: "" }]
    const source = upstreamFunctions("src/electron/utils/midi.ts", ["getMidiInputs", "getMidiOutputs"], { JZZ: () => ({ info: () => ({ inputs: devices, outputs: devices }) }) })
    const [wire] = native([{ op: "devices", data: devices }])
    for (const [command, original] of [
        ["GET_MIDI_INPUTS", source.getMidiInputs()],
        ["GET_MIDI_OUTPUTS", source.getMidiOutputs()]
    ]) {
        const result = hydrateMidi(command, wire.data, wire.hydration)
        assert.deepEqual(result, structuredClone(original))
        assert.deepEqual(result.map(Object.keys), Array.from(original, Object.keys))
        assert.equal(Object.hasOwn(result[0], "name"), true)
        assert.equal(result[0].name, undefined)
    }
})

test("invalid MIDI restoration rejects the whole envelope without mutating callback data", () => {
    const data = { values: { channel: 1 }, type: "noteon" }
    const valid = { undefinedPaths: ["/id", "/values/note", "/values/velocity"], numbers: [] }
    for (const metadata of [
        { ...valid, undefinedPaths: [...valid.undefinedPaths, "/id"] },
        { ...valid, undefinedPaths: [...valid.undefinedPaths, "/values/__proto__"] },
        { ...valid, undefinedPaths: valid.undefinedPaths.slice(1) },
        { ...valid, numbers: [{ path: "/id", number: { kind: "NaN" } }] },
        { ...valid, undefinedPaths: ["/id", "/values/note", "/values/velocity", "/values/channel"] }
    ]) {
        const before = structuredClone(data)
        assert.throws(() => hydrateMidi("RECEIVE_MIDI2", data, metadata))
        assert.deepEqual(data, before)
        assert.equal(Object.hasOwn(data, "id"), false)
    }
    assert.throws(() => hydrateMidi("RECEIVE_MIDI2", { ...data, values: { channel: 17 } }, valid))
    assert.throws(() => hydrateMidi("RECEIVE_MIDI2", { ...data, values: { channel: 1, extra: 2 } }, valid))
    assert.throws(() => hydrateMidi("OTHER", data, valid))
})
