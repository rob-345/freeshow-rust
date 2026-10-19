import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import path from "node:path"
import fs from "node:fs"
import { createRequire } from "node:module"
import vm from "node:vm"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import ts from "typescript"
const root = fileURLToPath(new URL("../../", import.meta.url))
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const binary = process.env.FREESHOW_TIMECODE_PARITY || path.join(root, "target/debug/freeshow-timecode-parity" + (process.platform === "win32" ? ".exe" : ""))
function original(name) {
    const r = spawnSync("git", ["show", `${revision}:src/electron/timecode/${name}.ts`], { cwd: root, encoding: "utf8", timeout: 10000 })
    assert.equal(r.status, 0, r.stderr)
    return r.stdout
}
const sources = { timecode: original("timecode"), LTC: original("LTC"), MTC: original("MTC") }
const responsesResult = spawnSync("git", ["show", `${revision}:src/electron/IPC/responsesMain.ts`], { cwd: root, encoding: "utf8", timeout: 10000 })
assert.equal(responsesResult.status, 0, responsesResult.stderr)
const responses = ts.createSourceFile("responsesMain.ts", responsesResult.stdout, ts.ScriptTarget.Latest, true)
const responsesObject = responses.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((d) => d.name.getText(responses) === "mainResponses")).declarationList.declarations.find((d) => d.name.getText(responses) === "mainResponses").initializer
const statusHandler = responsesObject.properties.find((p) => p.name?.getText(responses) === "[Main.TIMECODE_STATUS]").initializer.getText(responses)

const U = { $undefined: true },
    N = (n) => ({ $number: n })
function hydrate(v) {
    if (v?.$number) return { NaN, Infinity, "-Infinity": -Infinity, "-0": -0 }[v.$number]
    if (Array.isArray(v)) return v.map(hydrate)
    if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, v]) => [k, hydrate(v)]))
    return v
}
function normal(v) {
    if (v === undefined) return U
    if (typeof v === "number") return Number.isNaN(v) ? N("NaN") : !Number.isFinite(v) ? N(String(v)) : Object.is(v, -0) ? N("-0") : v
    if (Array.isArray(v) || ArrayBuffer.isView(v)) return Array.from(v, normal)
    if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, v]) => [k, normal(v)]))
    return v
}
async function reference(config) {
    const trace = [],
        errors = [],
        thrown = [],
        caught = [],
        opens = [],
        modules = {}
    let read,
        decoderCount = 0,
        encoderCount = 0,
        clock = 0
    const callbacks = []
    const JZZ = () => ({
        openMidiIn: () => ({
            or: () =>
                new Promise((resolve, reject) => {
                    const index = opens.length
                    const port = {
                        name: () => "controlled",
                        connect: async (callback) => {
                            callbacks[index] = callback
                        },
                        close: () => {
                            add("mtc.close", index)
                            if (config.failClose?.includes(index)) throw new Error("close failure")
                        }
                    }
                    opens.push({ resolve: () => resolve(port), reject: () => reject(new Error("open failure")) })
                })
        })
    })
    const add = (...args) => trace.push(normal(args))
    const main = new Proxy({}, { get: (_, key) => key })
    const mtc = {
        setupMTCListener: (fps, data) => {
            add("mtc.open", fps, data)
            const index = opens.length
            let resolve, reject
            const promise = new Promise((yes, no) => {
                resolve = yes
                reject = no
            })
            opens.push({
                resolve: () =>
                    resolve({
                        stop() {
                            add("mtc.close", index)
                            if (config.failClose?.includes(index)) throw new Error("close failure")
                        }
                    }),
                reject: () => reject(new Error("open failure"))
            })
            return promise
        },
        sendMTC: (time, fps, data) => add("mtc.send", time, fps, data),
        stopSendMTC: () => {
            add("mtc.stop")
            if (config.failStopMtc) throw new Error("stop failure")
        }
    }
    class LTCDecoder {
        constructor(...args) {
            add("decoder.new", ...args)
            decoderCount++
            if (config.failDecoder === decoderCount) throw new Error("decoder failure")
        }
        write(bytes) {
            add("decoder.write", Array.from(bytes))
            if (config.failWrite) throw new Error("write failure")
        }
        read() {
            add("decoder.read")
            if (config.failRead) throw new Error("read failure")
            const frame = read
            read = undefined
            return frame
        }
    }
    class LTCEncoder {
        constructor(...args) {
            add("encoder.new", ...args)
            encoderCount++
            if (config.failEncoder === encoderCount) throw new Error("encoder failure")
        }
        setTimecode(frame) {
            add("encoder.set", frame)
            if (config.failSet) throw new Error("set failure")
        }
        encodeFrame() {
            add("encoder.encode")
            if (config.failEncode) throw new Error("encode failure")
        }
        getBuffer() {
            add("encoder.buffer")
            if (config.failBuffer) throw new Error("buffer failure")
            return Buffer.from(config.bytes || [0, 128, 255])
        }
    }
    const context = vm.createContext({
        Buffer,
        Date: { now: () => clock },
        console: {
            info() {},
            error: (...args) => {
                errors.push(args.at(-1).message)
                caught.push(args.at(-1).message)
            },
            log: (...args) => add("log", ...args)
        },
        observe: (promise) =>
            promise.catch((e) => {
                errors.push(e.message)
                thrown.push(e.message)
            })
    })
    new vm.Script(ts.transpileModule(`globalThis.sourceStatusHandler=(${statusHandler})`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).runInContext(context)
    function load(name) {
        if (modules[name]) return modules[name].exports
        const module = { exports: {} }
        modules[name] = module
        const extra =
            name === "MTC"
                ? ""
                : name === "timecode"
                  ? `
exports.snapshot=()=>({framerate,currentMode,sendData,receivers:Object.keys(timecodeReceivers)});
const sourceReceive=receiveTimecode;receiveTimecode=(...args)=>{const promise=sourceReceive(...args);globalThis.observe(promise);return promise};`
                  : `
exports.snapshot=()=>({decoder:!!decoder,decoderFailed,decoderFramerate:framerate,encoder:!!encoder,encoderFailed,encoderFramerate:currentFramerate,lastTime:lastTimeMs});`
        const compiled = ts.transpileModule(sources[name], { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText
        const require = (id) => {
            if (id === "./LTC") return load("LTC")
            if (id === "./timecode") return load("timecode")
            if (id === "./MTC")
                return config.actualMtc
                    ? {
                          setupMTCListener: (fps, data) => {
                              add("mtc.open", fps, data)
                              return load("MTC").setupMTCListener(fps, data)
                          },
                          sendMTC: mtc.sendMTC,
                          stopSendMTC: mtc.stopSendMTC
                      }
                    : mtc
            if (id === "jzz") return { default: JZZ }
            if (id === "libltc-wrapper") return { LTCDecoder, LTCEncoder }
            if (id.endsWith("/IPC/main")) return { sendMain: (channel, data) => add("main", channel, data), sendToMain: (channel, data) => add("toMain", channel, data) }
            if (id.endsWith("/Main")) return { Main: main }
            if (id.endsWith("/ToMain")) return { ToMain: main }
            if (id === "..") return { isWindows: !!config.windows }
            throw new Error("Unexpected source boundary " + id)
        }
        new vm.Script(`(function(require,exports,module){${compiled}\n${extra}\n})`).runInContext(context)(require, module.exports, module)
        return module.exports
    }
    const router = load("timecode"),
        ltc = load("LTC"),
        states = []
    for (const raw of config.actions) {
        const a = hydrate(raw)
        try {
            if (a.op === "start") router.timecodeStart({ type: a.type, mode: a.mode, framerate: a.fps, data: a.data })
            else if (a.op === "value") router.updateTimecodeValue(a.value)
            else if (a.op === "stop") router.timecodeStop()
            else if (a.op === "audio") {
                read = a.frame
                router.processAudioData({ mode: a.mode, buffer: Uint8Array.from(a.bytes) })
            } else if (a.op === "status")
                add("log", a.value) // actual responsesMain TIMECODE_STATUS body, asserted below
            else if (a.op === "mtc") {
                clock = a.now
                callbacks[a.index](a.bytes)
            } else if (a.op === "frame") router.processFrame(a.value)
            else if (a.op === "updateStatus") router.updateTimecodeStatus(a.value)
            else if (a.op === "resolve") opens[a.index].resolve()
            else if (a.op === "reject") opens[a.index].reject()
            else if (a.op !== "accept") throw new Error("Unknown reference action " + a.op)
        } catch (e) {
            errors.push(e.message)
            thrown.push(e.message)
        }
        // Complete the controlled native open/connect promise chain, including
        // cross-realm thenable jobs, before observing the matching host receipt.
        await new Promise((resolve) => setImmediate(resolve))
        states.push(normal({ ...router.snapshot(), ...ltc.snapshot() }))
    }
    return { trace, errors, states, thrown, caught }
}
const coverage = { scripts: 0, actions: 0, constructorCalls: 0, audioBuffers: 0 }
test.after(() => console.log("Timecode source coverage:", JSON.stringify(coverage)))
async function compare(cases) {
    const r = spawnSync(binary, [], { cwd: root, input: JSON.stringify(cases), encoding: "utf8", timeout: 20000, maxBuffer: 16 * 1024 * 1024 })
    assert.equal(r.status, 0, r.stderr)
    const native = JSON.parse(r.stdout),
        originals = []
    for (const [index, fixture] of cases.entries()) {
        const expected = await reference(fixture)
        originals.push(expected)
        coverage.scripts++
        coverage.actions += fixture.actions.length
        coverage.constructorCalls += expected.trace.filter((v) => v[0].endsWith(".new")).length
        coverage.audioBuffers += expected.trace.filter((v) => v[1] === "TIMECODE_AUDIO_DATA").length
        assert.deepEqual(native[index], { trace: expected.trace, errors: expected.errors, states: expected.states }, `source fixture ${index}: ${JSON.stringify(fixture)}`)
    }
    return originals
}
const start = (type, mode, fps, data) => ({ op: "start", type, mode, ...(fps === undefined ? {} : { fps }), ...(data === undefined ? {} : { data }) })
const value = (value) => ({ op: "value", value }),
    stop = { op: "stop" },
    resolve = (index) => ({ op: "resolve", index })

test("actual source routing keeps framerate truthiness and send mode/data after STOP without ACK", async () => {
    const falseValues = [undefined, null, false, 0, N("-0"), "", N("NaN")]
    const cases = falseValues.map((fps) => ({ actions: [start("send", "MTC", 29.97, { midiOutput: "Unicode 敬拜" }), value(1000), start("other", "LTC", fps), stop, value(1500), start("send", "ableton", true), value(2000), stop] }))
    cases.push({ actions: [value(0), stop, start("send", "unknown", 48, null), value(100), stop, { op: "status", value: "pause" }] })
    await compare(cases)
})
test("actual source LTC constructor/frame/buffer decisions include negative time, IEEE values, strict fps and backwards reset", async () => {
    const cases = []
    for (const fps of [24, 25, 29.97, 30, -25, "25", true, " 0x19 ", "0", N("Infinity")]) cases.push({ bytes: [0, 1, 127, 128, 254, 255], actions: [start("send", "LTC", fps), ...[0, 1, 39.9, 1000, 999, -0.1, -3600001, 86400123, N("NaN"), N("Infinity"), N("-Infinity"), N("-0")].map(value), stop, value(2000)] })
    cases.push({ actions: [start("send", "LTC", 25), value(0), start("other", "other", "25"), value(0), start("other", "other", 25), value(0)] })
    await compare(cases)
})
test("source LTC receive reads one frame after write and keeps its own fps separate from shared send fps", async () => {
    await compare([
        {
            actions: [
                { op: "audio", mode: "LTC", bytes: [128] },
                start("receive", "LTC", 29.97),
                start("receive", "LTC", 60),
                { op: "audio", mode: "LTC", bytes: [0, 128, 255], frame: { hours: 23, minutes: 59, seconds: 59, frames: 29 } },
                { op: "audio", mode: "LTC", bytes: [] },
                { op: "audio", mode: "MTC", bytes: [0], frame: { hours: 1, minutes: 2, seconds: 3, frames: 4 } },
                stop,
                { op: "audio", mode: "LTC", bytes: [0] },
                start("receive", "LTC", "25"),
                { op: "audio", mode: "LTC", bytes: [128], frame: { hours: -1, minutes: 60, seconds: N("NaN"), frames: 2 } }
            ]
        }
    ])
})
test("actual source sticky failed constructors preserve old encoder and exact Windows ALERT suffix", async () => {
    const cases = []
    for (const windows of [false, true]) {
        cases.push({ windows, failDecoder: 1, actions: [start("receive", "LTC", 24), start("receive", "LTC", 60), stop, start("receive", "LTC", 30)] })
        cases.push({ windows, failEncoder: 2, actions: [start("send", "LTC", 25), value(1000), value(999), value(5000), stop, value(0), start("send", "MTC", 24), value(0)] })
    }
    const original = await compare(cases)
    assert.ok(original.every((v) => v.caught.length === 1 && v.thrown.length === 0))
})
test("source async MTC opens dedup only after completion, can overwrite and revive after STOP", async () => {
    await compare([{ actions: [start("receive", "MTC", 24, { midiInput: "A" }), start("receive", "MTC", 30, { midiInput: "B" }), stop, resolve(1), start("receive", "MTC", 60), resolve(0), start("send", "MTC", undefined, { midiOutput: "X" }), value(50), stop, value(100), start("receive", "MTC", 25, { midiInput: "C" }), resolve(2), stop] }, { actions: [start("receive", "MTC", 25, { midiInput: "A" }), { op: "reject", index: 0 }, start("receive", "MTC", 30, { midiInput: "B" }), resolve(1), stop] }])
})
test("source receive close ordering and synchronous throw retain receiver map and abort subsequent stops", async () => {
    await compare([
        { failClose: [1], actions: [start("receive", "MTC", 25), start("receive", "MTC", 25), resolve(0), resolve(1), start("receive", "LTC", 30), stop, start("receive", "LTC", 25), stop] },
        { failClose: [0], actions: [start("receive", "LTC", 25), start("receive", "MTC", 25), resolve(0), stop, start("receive", "LTC", 25), stop] },
        { failStopMtc: true, actions: [start("receive", "LTC", 25), start("send", "MTC", 25), stop, start("receive", "LTC", 25)] }
    ])
})
test("source status gate survives STOP, every frame emits, and native write/set errors preserve state", async () => {
    const actions = [{ op: "updateStatus", value: "play" }, { op: "updateStatus", value: "play" }, { op: "frame", value: 0 }, { op: "frame", value: 0 }, stop, { op: "updateStatus", value: "play" }, { op: "updateStatus", value: "pause" }, { op: "updateStatus", value: "pause" }, { op: "updateStatus", value: "stop" }, { op: "frame", value: N("NaN") }]
    await compare([{ actions }, { failWrite: true, actions: [start("receive", "LTC", 25), { op: "audio", mode: "LTC", bytes: [128] }, stop] }, { failSet: true, actions: [start("send", "LTC", 25), value(1000), value(1001), stop] }])
    const response = spawnSync("git", ["show", `${revision}:src/electron/IPC/responsesMain.ts`], { cwd: root, encoding: "utf8" }).stdout
    assert.match(response, /\[Main\.TIMECODE_STATUS\]: \(data\) => console\.log\(data\)/)
})

test("actual source MTC callbacks pass through the shared router status gate in receiver order", async () => {
    await compare([
        {
            actualMtc: true,
            actions: [
                start("receive", "MTC", 25, { midiInput: "A" }),
                start("receive", "MTC", 25, { midiInput: "B" }),
                resolve(0),
                resolve(1),
                { op: "mtc", index: 0, bytes: [250], now: 100 },
                { op: "mtc", index: 1, bytes: [250], now: 100 },
                { op: "mtc", index: 0, bytes: [252], now: 105 },
                { op: "mtc", index: 1, bytes: [248], now: 110 },
                { op: "mtc", index: 0, bytes: [251], now: 120 },
                { op: "mtc", index: 1, bytes: [242, 16, 0], now: 125 },
                stop,
                { op: "mtc", index: 0, bytes: [242, 32, 0], now: 130 }
            ]
        }
    ])
})

test("actual LTC source errors stop at read/encode/buffer boundary without changing capability flags", async () => {
    await compare([{ failRead: true, actions: [start("receive", "LTC", 25), { op: "audio", mode: "LTC", bytes: [128] }, stop] }, ...["failEncode", "failBuffer"].map((failure) => ({ [failure]: true, actions: [start("send", "LTC", 25), value(1000), value(1001), stop] }))])
})

test("installed source libltc JavaScript wrapper preserves native arguments and plural/singular frame quirk", () => {
    const require = createRequire(import.meta.url)
    const filename = require.resolve("libltc-wrapper")
    const code = fs.readFileSync(filename, "utf8")
    const calls = [],
        handle = { nativeBoundary: true },
        buffer = Buffer.from([0, 128, 255])
    const addon = {
        createLTCEncoder: (...args) => {
            calls.push(["encoder", ...args])
            return handle
        },
        encoderSetTimecode: (owner, time) => {
            assert.equal(owner, handle)
            calls.push(["timecode", normal(time)])
        },
        encoderEncodeFrame: (owner) => {
            assert.equal(owner, handle)
            calls.push(["encode"])
        },
        encoderGetBuffer: (owner) => {
            assert.equal(owner, handle)
            return buffer
        },
        createLTCDecoder: (...args) => {
            calls.push(["decoder", ...args])
            return handle
        },
        writeAudio: (owner, bytes) => {
            assert.equal(owner, handle)
            assert.equal(bytes, buffer)
            calls.push(["write"])
        },
        readFrame: (owner) => {
            assert.equal(owner, handle)
            calls.push(["read"])
            return undefined
        }
    }
    const module = { exports: {} }
    new vm.Script(`(function(require,exports,module){${code}})`).runInNewContext()(
        (id) => {
            assert.equal(id, "../build/Release/ltc-native")
            return addon
        },
        module.exports,
        module
    )
    const encoder = new module.exports.LTCEncoder(48000, 25)
    encoder.setTimecode({ hours: 1, minutes: 2, seconds: 3, frames: 24 })
    encoder.encodeFrame()
    assert.equal(encoder.getBuffer(), buffer)
    const decoder = new module.exports.LTCDecoder(48000, 29.97, "u8")
    decoder.write(buffer)
    assert.equal(decoder.read(), undefined)
    assert.deepEqual(calls, [["encoder", 48000, 25, 1], ["timecode", { hours: 1, minutes: 2, seconds: 3, days: 0, months: 0, frame: 0, timezone: "+0200", frames: 24, years: 0 }], ["encode"], ["decoder", 48000 / 29.97, 32, "u8"], ["write"], ["read"]])
    assert.throws(() => new module.exports.LTCDecoder(48000, 60, "u8"), /Framerate cannot be above 30/)
    const native = fs.readFileSync(path.join(path.dirname(filename), "../src/ltcnative.c"), "utf8")
    assert.match(native, /NAPI_GETOBJPARAM_NUMBER_ORRETURN\("frame", timecode\.frame\)/)
    assert.match(native, /napi_get_value_int32\(env, args\[0\], &obj->apv\)/)
})
