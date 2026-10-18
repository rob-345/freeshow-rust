import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { createRequire } from "node:module"
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import ts from "typescript"
const root = fileURLToPath(new URL("../../", import.meta.url)),
    require = createRequire(import.meta.url)
const binary = process.env.FREESHOW_LTC_PARITY || path.join(root, "target/debug/freeshow-ltc-parity" + (process.platform === "win32" ? ".exe" : ""))
const addonPath = path.resolve(process.env.FREESHOW_LTC_ORACLE_ADDON || path.join(root, "target/ltc-original/ltc-native.node"))
const addon = require(addonPath)
const module = { exports: {} }
new vm.Script(`(function(require,exports,module){${fs.readFileSync(require.resolve("libltc-wrapper"), "utf8")}})`).runInNewContext()(
    (id) => {
        assert.equal(id, "../build/Release/ltc-native")
        return addon
    },
    module.exports,
    module
)
const { LTCEncoder, LTCDecoder } = module.exports
const coverage = { fps: 0, encoded: 0, samples: 0, decoded: 0, emptyReads: 0, cases: 0, sourceSend: 0, sourceReceive: 0 }
test.after(() => console.log("Real LTC source coverage:", JSON.stringify(coverage)))
function native(cases) {
    const result = spawnSync(binary, [], { cwd: root, input: JSON.stringify(cases), encoding: "utf8", timeout: 20000, maxBuffer: 20 * 1024 * 1024 })
    assert.equal(result.status, 0, result.stderr)
    return JSON.parse(result.stdout)
}
function source(actions) {
    let encoder, decoder
    return actions.map((a) => {
        switch (a.op) {
            case "encoder":
                encoder = new LTCEncoder(48000, a.fps)
                return { kind: "done" }
            case "decoder":
                decoder = new LTCDecoder(48000, a.fps, "u8")
                return { kind: "done" }
            case "encode": {
                encoder.setTimecode({ hours: a.hours, minutes: a.minutes, seconds: a.seconds, frames: a.frames })
                encoder.encodeFrame()
                const bytes = encoder.getBuffer()
                coverage.encoded++
                coverage.samples += bytes.length
                return { kind: "audio", value: [...bytes] }
            }
            case "decode": {
                decoder.write(Buffer.from(a.bytes))
                const frame = decoder.read()
                frame === undefined ? coverage.emptyReads++ : coverage.decoded++
                return { kind: "frame", value: frame === undefined ? null : JSON.parse(JSON.stringify(frame)) }
            }
            case "stopEncoder":
                encoder = undefined
                return { kind: "done" }
            case "stopDecoder":
                decoder = undefined
                return { kind: "done" }
            default:
                throw Error("unsupported source fixture")
        }
    })
}
function compare(cases) {
    const expected = cases.map(source),
        actual = native(cases)
    for (const [i, results] of actual.entries()) {
        coverage.cases++
        assert.deepEqual(results.replies, expected[i], "real source case" + i)
        assert.equal(results.closed.encoderCreated, results.closed.encoderFreed)
        assert.equal(results.closed.decoderCreated, results.closed.decoderFreed)
        assert.equal(results.closed.loaded, false)
        assert.equal(results.closed.workerFinished, true)
        assert.equal(results.closed.admitted, 0)
    }
    return actual
}
const encode = (h, m, s, f) => ({ op: "encode", hours: h, minutes: m, seconds: s, frames: f })
function encoded(fps, count = 8) {
    const e = new LTCEncoder(48000, fps),
        bytes = []
    for (let i = 0; i < count; i++) {
        e.setTimecode({ hours: 1, minutes: 2, seconds: i, frames: i })
        e.encodeFrame()
        bytes.push(...e.getBuffer())
    }
    return bytes
}

test("real original addon and pinned ELF/header are used, with truthful capability", () => {
    assert.equal(process.versions.node, "22.21.1")
    const sha = (file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex")
    assert.equal(sha(path.join(root, "node_modules/libltc-wrapper/libltc/bin/libltc.so.11")), "f02c75f982fe01d16f345875d2b09404b8b326bb847f2b5456616f6d0227d09f")
    assert.equal(sha(path.join(root, "crates/freeshow-ltc/vendor/ltc.h")), "a2d5666fb300ffd05cbfada622029f2647dc26ba3ff03a834e3d5bd7ff77dff7")
    const result = native([[{ op: "capability" }]])[0]
    assert.equal(result.replies[0].value.available, true)
    assert.equal(result.replies[0].value.headerVersion, "1.3.1")
    assert.equal(result.closed.loaded, false)
    assert.equal(typeof addon.createLTCEncoder, "function")
    assert.equal(typeof addon.readFrame, "function")
})
test("actual waveform bytes equal original wrapper/defaultdate/BGF/frame0 across four fps and reset sequences", () => {
    const cases = []
    for (const fps of [24, 25, 29.97, 30]) {
        coverage.fps++
        cases.push([{ op: "encoder", fps }, ...[encode(0, 0, 0, 0), encode(1, 2, 3, 1), encode(1, 2, 3, 24), encode(23, 59, 59, 29), encode(1, 2, 3, 999)], { op: "encoder", fps }, encode(1, 2, 3, 23), { op: "stopEncoder" }, { op: "encoder", fps }, encode(0, 0, 0, 0)])
    }
    compare(cases)
    // The ignored plural field does not change source bytes when reset to the
    // same initial biphase state. This is measured using the real native addon.
    const same = compare([
        [{ op: "encoder", fps: 25 }, encode(1, 2, 3, 0)],
        [{ op: "encoder", fps: 25 }, encode(1, 2, 3, 24)]
    ])
    assert.deepEqual(same[0].replies[1], same[1].replies[1])
})
test("real decoder packet fields, offsets, date/timezone, volume and one-read backlog match arbitrary chunking", () => {
    const cases = []
    for (const fps of [24, 25, 29.97, 30]) {
        const bytes = encoded(fps, 10)
        for (const chunk of [1, 128, 1919, 65536]) {
            // Byte-at-a-time is limited to the first300samples, then the rest
            // arrives in one actual write; keep the fixture bounded.
            const actions = [{ op: "decoder", fps }]
            let offset = 0
            if (chunk === 1) {
                for (; offset < 300; offset++) actions.push({ op: "decode", bytes: bytes.slice(offset, offset + 1) })
            }
            while (offset < bytes.length) {
                actions.push({ op: "decode", bytes: bytes.slice(offset, offset + chunk) })
                offset += chunk === 1 ? bytes.length : chunk
            }
            for (let i = 0; i < 12; i++) actions.push({ op: "decode", bytes: [] })
            cases.push(actions)
        }
    }
    compare(cases)
})
test("real default queue32 overflow, reverse samples, silence/noise and stop/recreate semantics match source", () => {
    const bytes = encoded(30, 40)
    let seed = 1234567
    const noise = Array.from({ length: 16000 }, () => {
        seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
        return seed >>> 24
    })
    compare([
        [{ op: "decoder", fps: 30 }, { op: "decode", bytes }, ...Array.from({ length: 34 }, () => ({ op: "decode", bytes: [] })), { op: "stopDecoder" }, { op: "decoder", fps: 30 }, { op: "decode", bytes: bytes.slice(0, 6400) }, ...Array.from({ length: 5 }, () => ({ op: "decode", bytes: [] }))],
        [{ op: "decoder", fps: 25 }, { op: "decode", bytes: encoded(25, 8).reverse() }, ...Array.from({ length: 10 }, () => ({ op: "decode", bytes: [] }))],
        [
            { op: "decoder", fps: 25 },
            { op: "decode", bytes: Array(8000).fill(128) },
            { op: "decode", bytes: Array(8000).fill(0) },
            { op: "decode", bytes: noise },
            { op: "decode", bytes: [] }
        ]
    ])
})
test("decoder preserves real incoming nonzero frame/date/timezone/drop flags across encoder BGF modes", () => {
    const cases = []
    for (const fps of [24, 25, 29.97, 30])
        for (const flags of [0, 1, 2, 4, 8]) {
            const encoder = new LTCEncoder(48000, fps, flags),
                bytes = []
            for (let frame = 7; frame < 15; frame++) {
                encoder.setTimecode({ hours: 12, minutes: 34, seconds: 56, frame, days: 31, months: 12, years: 2077, timezone: "-0500" })
                encoder.encodeFrame()
                bytes.push(...encoder.getBuffer())
            }
            cases.push([{ op: "decoder", fps }, { op: "decode", bytes }, ...Array.from({ length: 10 }, () => ({ op: "decode", bytes: [] }))])
        }
    const actual = compare(cases)
    const packets = actual.flatMap((c) => c.replies.filter((r) => r.kind === "frame" && r.value !== null).map((r) => r.value))
    assert.ok(packets.some((p) => p.frames === 7))
    assert.ok(packets.some((p) => p.days === 31 && p.months === 12 && p.years === 77 && p.timezone === "-0500"))
    assert.ok(packets.some((p) => p.drop_frame_format))
})
test("immutable LTC.ts uses actual original addon for frame arithmetic, backward/fps resets, feed ordering and stop", () => {
    const original = spawnSync("git", ["show", "2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/timecode/LTC.ts"], { cwd: root, encoding: "utf8", timeout: 10000 })
    assert.equal(original.status, 0, original.stderr)
    const actions = [],
        expected = [],
        audio = [],
        frames = []
    class Encoder extends LTCEncoder {
        constructor(rate, fps) {
            assert.equal(rate, 48000)
            super(rate, fps)
            actions.push({ op: "encoder", fps })
            expected.push({ kind: "done" })
        }
        setTimecode(fields) {
            actions.push({ op: "encode", ...fields })
            return super.setTimecode(fields)
        }
        getBuffer() {
            const bytes = super.getBuffer()
            expected.push({ kind: "audio", value: [...bytes] })
            return bytes
        }
    }
    class Decoder extends LTCDecoder {
        constructor(rate, fps, type) {
            assert.equal(rate, 48000)
            assert.equal(type, "u8")
            super(rate, fps, type)
            actions.push({ op: "decoder", fps })
            expected.push({ kind: "done" })
        }
        write(bytes) {
            actions.push({ op: "decode", bytes: [...bytes] })
            return super.write(bytes)
        }
        read() {
            const frame = super.read()
            expected.push({ kind: "frame", value: frame === undefined ? null : JSON.parse(JSON.stringify(frame)) })
            return frame
        }
    }
    const exports = {}
    const main = new Proxy({}, { get: (_, key) => key })
    const boundaries = {
        "libltc-wrapper": { LTCEncoder: Encoder, LTCDecoder: Decoder },
        "../../types/IPC/Main": { Main: main },
        "../../types/IPC/ToMain": { ToMain: main },
        "../IPC/main": {
            sendMain: (key, bytes) => {
                assert.equal(key, "TIMECODE_AUDIO_DATA")
                coverage.sourceSend++
                audio.push(Buffer.from(bytes))
            },
            sendToMain: () => assert.fail("real codec must not report unavailable")
        },
        "./timecode": {
            processFrame: (ms) => {
                coverage.sourceReceive++
                frames.push(ms)
            }
        },
        "..": { isWindows: false }
    }
    const js = ts.transpileModule(original.stdout, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText
    new vm.Script(`(function(require,exports){${js}})`).runInNewContext({ console, Buffer })((id) => {
        assert.ok(Object.hasOwn(boundaries, id), id)
        return boundaries[id]
    }, exports)
    for (const [ms, fps] of [
        [0, 25],
        [40, 25],
        [3723123, 25],
        [3723163, 25],
        [100, 25],
        [100, 24],
        [200, 29.97],
        [300, 30]
    ])
        exports.sendLTC(ms, fps)
    exports.stopSendLTC()
    actions.push({ op: "stopEncoder" })
    expected.push({ kind: "done" })
    exports.sendLTC(0, 25)
    const listener = exports.setupLTCListener(25)
    assert.equal(typeof listener.stop, "function")
    const bytes = Buffer.from(encoded(25, 10))
    exports.feedLTCFrame(bytes)
    for (let i = 0; i < 12; i++) exports.feedLTCFrame(Buffer.alloc(0))
    listener.stop()
    actions.push({ op: "stopDecoder" })
    expected.push({ kind: "done" })
    const before = actions.length
    exports.feedLTCFrame(bytes)
    assert.equal(actions.length, before)
    const actual = native([actions])[0]
    assert.deepEqual(actual.replies, expected)
    assert.equal(audio.length, 9)
    const packets = expected.filter((r) => r.kind === "frame" && r.value !== null).map((r) => r.value)
    assert.deepEqual(
        frames,
        packets.map((f) => (f.hours * 3600 + f.minutes * 60 + f.seconds + f.frames / 25) * 1000)
    )
    assert.ok(frames.length > 0)
    assert.equal(actual.closed.encoderCreated, actual.closed.encoderFreed)
    assert.equal(actual.closed.decoderCreated, actual.closed.decoderFreed)
    assert.equal(actual.closed.loaded, false)
})
test("unsupported/uncapped source variants reject before native allocation rather than fake availability", () => {
    const result = native([[{ op: "encoder", fps: 0 }, { op: "decoder", fps: 60 }, { op: "encoder", fps: 25 }, encode(-1, 2, 3, 0), encode(1, 60, 3, 0), { op: "decode", bytes: [0] }]])[0]
    assert.deepEqual(
        result.replies.map((r) => r.error?.code || r.kind),
        ["ltc_unsupported", "ltc_unsupported", "done", "ltc_unsupported", "ltc_unsupported", "ltc_not_initialized"]
    )
    assert.equal(result.closed.encoderCreated, 1)
    assert.equal(result.closed.encoderFreed, 1)
    assert.equal(result.closed.decoderCreated, 0)
})
