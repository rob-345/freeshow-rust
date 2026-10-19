import assert from "node:assert/strict"
import fs from "node:fs"
import { test } from "node:test"
import ts from "typescript"
const source = fs.readFileSync(new URL("../../src/frontend/rust/nowPlayingWire.ts", import.meta.url), "utf8")
const code = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText
const { encodeNowPlayingCommand: encode } = await import(`data:text/javascript;base64,${Buffer.from(code).toString("base64")}`)
const context = () => ({ runtimeId: "native-音", libraryGeneration: 3, dataPath: "/selected/日本語/🎹" })
const data = () => ({ filePath: "/audio/日本語.wav", name: "Name", unknownLang: ["Artist", "Title", "Album"], format: "{year}|{duration}|{duration_s}", duration: 61.25 })
const parse = (bytes) => JSON.parse(new TextDecoder().decode(bytes))
test("three original commands preserve source void framing and all numeric duration identities", () => {
    assert.deepEqual(parse(encode({ channel: "NOW_PLAYING_UNSET", data: { ignored: true } }, context())), { version: 1, context: context(), command: { kind: "unset" } })
    assert.deepEqual(parse(encode({ channel: "OPEN_NOW_PLAYING" }, context())).command, { kind: "open" })
    for (const [value, tag] of [
        [61.25, { kind: "Finite", value: 61.25 }],
        [0, { kind: "Finite", value: 0 }],
        [Number.MIN_VALUE, { kind: "Finite", value: Number.MIN_VALUE }],
        [-0, { kind: "NegativeZero" }],
        [NaN, { kind: "NaN" }],
        [Infinity, { kind: "PositiveInfinity" }],
        [-Infinity, { kind: "NegativeInfinity" }]
    ]) {
        const payload = { ...data(), duration: value }
        const result = parse(encode({ channel: "NOW_PLAYING", data: payload }, context()))
        assert.equal(result.command.data.duration, null)
        assert.deepEqual(result.command.duration, tag)
        assert.ok(Object.is(payload.duration, value), "input duration is never replaced by its private placeholder")
    }
})
test("missing, own undefined, null and nonnumeric duration retain distinct supported source scalars", () => {
    const absent = data()
    delete absent.duration
    for (const payload of [absent, { ...data(), duration: undefined }]) {
        const result = parse(encode({ channel: "NOW_PLAYING", data: payload }, context())).command
        assert.equal(Object.hasOwn(result, "duration"), false)
        assert.equal(Object.hasOwn(result.data, "duration"), false)
    }
    for (const value of [null, "61", false, [1, 2]]) {
        const result = parse(encode({ channel: "NOW_PLAYING", data: { ...data(), duration: value } }, context())).command
        assert.deepEqual(result.data.duration, value)
        assert.equal(Object.hasOwn(result, "duration"), false)
    }
})
test("send-time bytes snapshot reused input/full context and preserve finite unknown fields and negative zero", () => {
    const payload = data(),
        frozen = context()
    Object.assign(payload, { extra: { z: -0, tiny: 8.3163996793004788882e-275, "a/b": [false, null, "🎹"] } })
    Object.defineProperty(payload.extra, "__proto__", { value: "ordinary data", enumerable: true })
    const frame = encode({ channel: "NOW_PLAYING", data: payload }, frozen)
    payload.name = "Changed"
    payload.unknownLang.fill("Changed")
    payload.extra.z = 5
    frozen.dataPath = "/different"
    const result = parse(frame)
    assert.equal(result.context.dataPath, context().dataPath)
    assert.equal(result.command.data.name, "Name")
    assert.equal(result.command.data.unknownLang[0], "Artist")
    assert.ok(Object.is(result.command.data.extra.z, -0))
    assert.equal(result.command.data.extra.__proto__, "ordinary data")
    assert.deepEqual(Object.keys(result.command.data.extra), ["z", "tiny", "a/b", "__proto__"])
    assert.equal({}.polluted, undefined)
})
test("unsupported nested values/accessors/coercions are rejected without executing user code", () => {
    let called = 0
    const getter = {
            get x() {
                called++
                return 1
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
    for (const value of [getter, custom, cycle, { a: undefined }, [undefined], [NaN], { a: Infinity }, [, 1], new Date(), { fn() {} }, { symbol: Symbol("x") }]) {
        assert.throws(() => encode({ channel: "NOW_PLAYING", data: { ...data(), extra: value } }, context()), { code: "now_playing_wire_shape" })
    }
    assert.throws(
        () =>
            encode(
                {
                    channel: "NOW_PLAYING",
                    data: {
                        ...data(),
                        get duration() {
                            called++
                            return 1
                        }
                    }
                },
                context()
            ),
        { code: "now_playing_wire_shape" }
    )
    assert.equal(called, 0)
    for (const channel of ["AUDIO", "ACK", "NOW_PLAYING_SUCCESS", "MAIN"]) assert.throws(() => encode({ channel }, context()), { code: "now_playing_wire_unsupported" })
})
test("data/context work, encoded-size and UTF16 limits reject before a native request", () => {
    for (const value of ["a".repeat(65537), "\u0001".repeat(12000), "🙌".repeat(16385), Array.from({ length: 1024 }, () => null)]) assert.throws(() => encode({ channel: "NOW_PLAYING", data: { ...data(), extra: value } }, context()), { code: "now_playing_wire_limit" })
    let deep = null
    for (let index = 0; index < 9; index++) deep = [deep]
    assert.throws(() => encode({ channel: "NOW_PLAYING", data: { ...data(), extra: deep } }, context()), { code: "now_playing_wire_limit" })
    for (const value of ["\ud800", "\udc00", "a\ud800b"]) assert.throws(() => encode({ channel: "NOW_PLAYING", data: { ...data(), name: value } }, context()), { code: "now_playing_wire_shape" })
    for (const invalid of [
        { ...context(), runtimeId: "" },
        { ...context(), dataPath: "a\0b" },
        { ...context(), libraryGeneration: 9007199254740992 }
    ])
        assert.throws(() => encode({ channel: "OPEN_NOW_PLAYING" }, invalid), { code: "now_playing_wire_context" })
})
