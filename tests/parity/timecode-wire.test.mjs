import assert from "node:assert/strict"
import { test } from "node:test"
import fs from "node:fs"
import ts from "typescript"

const source = fs.readFileSync(new URL("../../src/frontend/rust/timecodeWire.ts", import.meta.url), "utf8")
const code = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText
const { encodeTimecodeCommand: command, encodeTimecodeLtc: ltc } = await import(`data:text/javascript;base64,${Buffer.from(code).toString("base64")}`)
const context = () => ({ runtimeId: "native-時間", libraryGeneration: 7, dataPath: "/private/日本語/🎹" })
const parse = (bytes) => JSON.parse(new TextDecoder().decode(bytes))
const start = (framerate, data) => ({ channel: "TIMECODE_START", data: { type: "receive", mode: "MTC", framerate, data } })

test("exact private command variants distinguish source undefined/null and preserve primitive fps Number identities", () => {
    assert.deepEqual(parse(command({ channel: "TIMECODE_STOP" }, context())), { version: 1, context: context(), command: { kind: "stop" } })
    assert.deepEqual(parse(command({ channel: "TIMECODE_STOP", data: { ignored: "source stop ignores data" } }, context())).command, { kind: "stop" })
    for (const message of [{ channel: "TIMECODE_STATUS" }, { channel: "TIMECODE_STATUS", data: undefined }]) assert.deepEqual(parse(command(message, context())).command, { kind: "status", data: { kind: "undefined" } })
    assert.deepEqual(parse(command({ channel: "TIMECODE_STATUS", data: null }, context())).command.data, { kind: "json", value: null })
    const scalars = [
        [undefined, { kind: "undefined" }],
        [null, { kind: "null" }],
        [false, { kind: "bool", value: false }],
        [true, { kind: "bool", value: true }],
        ["29.97", { kind: "text", value: "29.97" }],
        [25, { kind: "number", value: { kind: "Finite", value: 25 } }],
        [-0, { kind: "number", value: { kind: "NegativeZero" } }],
        [NaN, { kind: "number", value: { kind: "NaN" } }],
        [Infinity, { kind: "number", value: { kind: "PositiveInfinity" } }],
        [-Infinity, { kind: "number", value: { kind: "NegativeInfinity" } }]
    ]
    for (const [fps, expected] of scalars) {
        const result = parse(command(start(fps, undefined), context())).command
        assert.deepEqual(result.framerate, expected)
        assert.deepEqual(result.data, { kind: "undefined" })
        if (expected.kind === "number") assert.deepEqual(parse(command({ channel: "TIMECODE_VALUE", data: fps }, context())).command.value, expected.value)
    }
})

test("data emits actual raw negative zero and exact finite number tokens with ordered unknown/prototype-named keys", () => {
    const value = Object.create(null)
    Object.assign(value, { z: -0, "a/b": 8.3163996793004788882e-275, "~": 5e-324, constructor: { prototype: true }, array: [false, null, "🎹"] })
    Object.defineProperty(value, "__proto__", { value: "data only", enumerable: true })
    const raw = command({ channel: "TIMECODE_STATUS", data: value }, context())
    const result = parse(raw).command.data.value
    assert.deepEqual(Object.keys(result), ["z", "a/b", "~", "constructor", "array", "__proto__"])
    assert.ok(Object.is(result.z, -0))
    assert.match(new TextDecoder().decode(raw), /"z":-0\.0/)
    assert.equal(result["a/b"], 8.3163996793004788882e-275)
    assert.equal(result["~"], Number.MIN_VALUE)
    assert.equal(result.__proto__, "data only")
    assert.equal({}.polluted, undefined)
})

test("65536-byte LTC framing snapshots exact subarray offsets and stays separate from PCM32KiB", () => {
    const storage = Uint8Array.from({ length: 65538 }, (_, index) => index % 256)
    for (const length of [0, 32769, 65536]) {
        const raw = ltc(storage.subarray(1, 1 + length), "LTC", context())
        const size = new DataView(raw.buffer).getUint32(0, true),
            prefix = parse(raw.subarray(4, 4 + size))
        assert.deepEqual(prefix, { version: 1, context: context(), mode: "LTC", byteLength: length })
        assert.deepEqual([...raw.subarray(4 + size)], [...storage.subarray(1, 1 + length)])
    }
    const raw = ltc(storage.subarray(1, 4), "LTC", context())
    storage.fill(9)
    assert.deepEqual([...raw.subarray(raw.length - 3)], [1, 2, 3])
    assert.throws(() => ltc(new Uint8Array(65537), "LTC", context()), { code: "timecode_wire_limit" })
    assert.throws(() => ltc(new Uint8Array(1), "MTC", context()), { code: "timecode_wire_unsupported" })
    assert.throws(() => ltc(new Uint8Array(new SharedArrayBuffer(1)), "LTC", context()), { code: "timecode_wire_shape" })
})

test("host data node/depth/byte and scalar bounds reject before producing a frame", () => {
    for (const value of ["🙌".repeat(16385), Array.from({ length: 1024 }, () => null), { a: "a".repeat(65536) }]) assert.throws(() => command({ channel: "TIMECODE_STATUS", data: value }, context()), { code: "timecode_wire_limit" })
    let deep = null
    for (let depth = 0; depth < 9; depth++) deep = [deep]
    assert.throws(() => command({ channel: "TIMECODE_STATUS", data: deep }, context()), { code: "timecode_wire_limit" })
    for (const message of [{ channel: "TIMECODE_START", data: { type: "a".repeat(33), mode: "MTC" } }, { channel: "TIMECODE_START", data: { type: "send", mode: "a".repeat(4097) } }, start("a".repeat(4097), undefined)]) assert.throws(() => command(message, context()), { code: "timecode_wire_limit" })
    assert.equal(parse(command({ channel: "TIMECODE_STATUS", data: "a".repeat(65536) }, context())).command.data.value.length, 65536)
    assert.throws(() => command({ channel: "TIMECODE_STATUS", data: "\u0001".repeat(65536) }, context()), { code: "timecode_wire_limit" })
    assert.throws(() => ltc(new Uint8Array(), "LTC", { ...context(), dataPath: "\u0001".repeat(20000) }), { code: "timecode_wire_limit" })
})

test("unsupported nested values and malformed shapes never flatten into JSON null or execute getters/coercions", () => {
    let called = 0
    const getter = {
            get unknown() {
                called++
                return "value"
            }
        },
        custom = {
            toJSON() {
                called++
                return null
            }
        },
        cycle = {}
    cycle.self = cycle
    for (const value of [{ own: undefined }, [undefined], [NaN], { value: Infinity }, [, 1], getter, custom, cycle, new Date(), { fn() {} }, { value: Symbol("source") }]) assert.throws(() => command({ channel: "TIMECODE_STATUS", data: value }, context()), { code: "timecode_wire_shape" })
    for (const message of [{ channel: "TIMECODE_VALUE", data: "1" }, { channel: "TIMECODE_START", data: null }, start({}, null)]) assert.throws(() => command(message, context()), { code: "timecode_wire_shape" })
    for (const channel of ["TIMECODE_AUDIO_DATA", "ACK", "TIMECODE_SUCCESS", "MAIN"]) assert.throws(() => command({ channel }, context()), { code: "timecode_wire_unsupported" })
    assert.equal(called, 0)
})

test("full context, lone-surrogate and runtime/path byte limits fail explicitly", () => {
    for (const value of [
        { ...context(), runtimeId: "" },
        { ...context(), dataPath: "" },
        { ...context(), runtimeId: "a\0b" },
        { ...context(), dataPath: "a\0b" },
        { ...context(), libraryGeneration: -1 },
        { ...context(), libraryGeneration: 9007199254740992 }
    ])
        assert.throws(() => command({ channel: "TIMECODE_STOP" }, value), { code: "timecode_wire_context" })
    for (const value of [
        { ...context(), runtimeId: "🙌".repeat(65) },
        { ...context(), dataPath: "a".repeat(32769) }
    ])
        assert.throws(() => command({ channel: "TIMECODE_STOP" }, value), { code: "timecode_wire_limit" })
    for (const value of ["\ud800", "\udc00", "x\ud800y"]) assert.throws(() => command({ channel: "TIMECODE_STATUS", data: value }, context()), { code: "timecode_wire_shape" })
    assert.equal(parse(command({ channel: "TIMECODE_STATUS", data: "🙌" }, context())).command.data.value, "🙌")
})
