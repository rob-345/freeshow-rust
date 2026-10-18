import assert from "node:assert/strict"
import fs from "node:fs"
import { createHash } from "node:crypto"
import { createRequire } from "node:module"
import path from "node:path"
import { spawnSync } from "node:child_process"
import test from "node:test"
import { clean, root, upstreamFunctions } from "./oracle.mjs"
const require = createRequire(import.meta.url)
const JZZ = require("jzz")
assert.equal(require("jzz/package.json").version, "1.9.5")
assert.equal(
    createHash("sha256")
        .update(fs.readFileSync(require.resolve("jzz")))
        .digest("hex"),
    "2cd148af6319c3106dd5a56ebb4eaa9a6ea161ca5740884d57cb1a5037ca01a0"
)
const binary = process.env.FREESHOW_MIDI_PARITY || path.join(root, "target/debug/freeshow-midi-parity" + (process.platform === "win32" ? ".exe" : ""))
const coverage = {}
test.after(() => console.log("MIDI parity coverage:", JSON.stringify(coverage)))
function batch(cases) {
    const result = spawnSync(binary, [], { input: cases.map(JSON.stringify).join("\n") + "\n", encoding: "utf8", timeout: 10000, maxBuffer: 32 * 1024 * 1024 })
    assert.equal(result.status, 0, result.stderr || result.error?.message)
    return result.stdout.trim().split("\n").map(JSON.parse)
}
function value(input) {
    const result = batch([input])[0]
    assert.equal(result.ok, true, JSON.stringify(result))
    return result.value
}
const number = (n) => (Number.isNaN(n) ? { kind: "NaN" } : n === Infinity ? { kind: "PositiveInfinity" } : n === -Infinity ? { kind: "NegativeInfinity" } : Object.is(n, -0) ? { kind: "NegativeZero" } : { kind: "Finite", value: n })
const numeric = (n) => (n === "NaN" ? NaN : n === "Infinity" ? Infinity : n === "-Infinity" ? -Infinity : n === "-0" ? -0 : n)
function wire(data) {
    const hydration = { undefinedPaths: [], numbers: [] }
    const visit = (item, at, array = false) => {
        if (item === undefined) {
            hydration.undefinedPaths.push(at)
            return array ? null : undefined
        }
        if (typeof item === "number") {
            const n = number(item)
            if (n.kind !== "Finite") hydration.numbers.push({ path: at, number: n })
            return Number.isFinite(item) ? (Object.is(item, -0) ? 0 : item) : null
        }
        if (Array.isArray(item)) return item.map((entry, i) => visit(entry, at + "/" + i, true))
        if (item && typeof item === "object") {
            const object = {}
            for (const key of Object.keys(item)) {
                const result = visit(item[key], at + "/" + key.replaceAll("~", "~0").replaceAll("/", "~1"))
                if (result !== undefined) object[key] = result
            }
            return object
        }
        return item
    }
    return { data: visit(data, ""), hydration }
}
function midiOracle(inputs = [], outputs = []) {
    const packets = [],
        events = [],
        errors = [],
        trace = [],
        callbacks = new Map(),
        failClose = new Set()
    let sequence = 0
    const port = (name, direction) => {
        const id = ++sequence
        return {
            name: () => name,
            or() {
                return this
            },
            close() {
                trace.push(["close", direction, name, id])
                if (failClose.has(id)) throw new Error("close failed")
                callbacks.delete(id)
            },
            connect(fn) {
                callbacks.set(id, fn)
                return this
            },
            control(...args) {
                const bytes = [...JZZ.MIDI.control(...args)]
                packets.push(bytes)
                trace.push(["packet", bytes])
            },
            noteOn(...args) {
                const bytes = [...JZZ.MIDI.noteOn(...args)]
                packets.push(bytes)
                trace.push(["packet", bytes])
            },
            noteOff(...args) {
                const bytes = [...JZZ.MIDI.noteOff(...args)]
                packets.push(bytes)
                trace.push(["packet", bytes])
            }
        }
    }
    const engine = {
        info: () => ({ inputs, outputs }),
        openMidiOut(name) {
            trace.push(["open", "out", name])
            return port(name, "out")
        },
        openMidiIn(name) {
            trace.push(["open", "in", name])
            return port(name, "in")
        }
    }
    const source = upstreamFunctions("src/electron/utils/midi.ts", ["getMidiInputs", "getMidiOutputs", "sendMidi", "receiveMidi", "closeMidiInPorts", "closeMidiOutPorts", "stopMidi"], { JZZ: () => engine, sendToMain: (channel, data) => events.push({ channel, data: wire(data) }), ToMain: { RECEIVE_MIDI2: "RECEIVE_MIDI2" } }, "let openedOutputPorts = {}; let openedPorts = {};")
    // The source helper injects a quiet console. Catch JZZ at the controlled port
    // boundary too so its actual error name/message can be compared explicitly.
    for (const direction of ["openMidiOut", "openMidiIn"]) {
        const original = engine[direction]
        engine[direction] = (...args) => {
            const result = original(...args)
            for (const method of ["control", "noteOn", "noteOff"]) {
                const send = result[method]
                result[method] = (...values) => {
                    try {
                        return send(...values)
                    } catch (error) {
                        errors.push({ name: error.name, message: error.message })
                        trace.push(["error", error.name, error.message])
                        throw error
                    }
                }
            }
            return result
        }
    }
    return { source, packets, events, errors, trace, callbacks, failClose, engine }
}

test("device list values execute actual source projection, retaining order, duplicate Unicode names and undefined", () => {
    const inputs = [{ name: "日本語 é", id: 4 }, { name: "A", manufacturer: "discarded" }, { name: "日本語 é" }, {}, { name: null }, { name: 0 }]
    const outputs = [{ name: "🎹" }, { name: "e\u0301" }, { name: "é" }]
    coverage.deviceEntries = inputs.length + outputs.length
    const source = midiOracle(inputs, outputs).source
    assert.deepEqual(value({ op: "devices", data: inputs }), wire(source.getMidiInputs()))
    assert.deepEqual(value({ op: "devices", data: outputs }), wire(source.getMidiOutputs()))
})

test("actual sendMidi defaults/mutations and real JZZ note/control validation agree across numeric/coercion/error boundaries", async () => {
    const cases = []
    for (const type of [undefined, "", "control", "noteon", "noteoff", "unknown", true]) {
        cases.push({ output: "port", ...(type === undefined ? {} : { type }) })
        for (const channel of [undefined, null, false, true, 0, 1, 16, 17, -1, 1.5, "", "0", "1", "0x10", "1e1", "Infinity", "garbage", [], [2], {}]) {
            for (const key of [0, 60, 127, 128, -1, 1.5, "60", "060", "C5", "D♭5", "H4", "C𝄪0", "C♭0", "A𝄫10", null, [60], [], {}, true, false, 1e21, 1e-7]) {
                const values = type === "control" ? { controller: key, value: 64 } : { note: key, velocity: 64 }
                if (channel !== undefined) values.channel = channel
                cases.push({ output: "port", type, values, future: "retained" })
            }
        }
    }
    coverage.sendCases = cases.length
    const native = batch(cases.map((data) => ({ op: "send", data })))
    const source = midiOracle()
    for (const [i, original] of cases.entries()) {
        const data = structuredClone(original),
            errorsBefore = source.errors.length
        await source.source.sendMidi(data)
        assert.equal(native[i].ok, true, JSON.stringify(original))
        const result = native[i].value
        assert.deepEqual({ data: result.data, hydration: result.hydration }, wire(data), JSON.stringify(original))
        if (source.errors.length > errorsBefore) assert.deepEqual({ name: result.packet.error.name, message: result.packet.error.message }, source.errors.at(-1), JSON.stringify(original))
        else assert.deepEqual(result.packet.bytes, source.packets.at(-1), JSON.stringify(original))
    }
    assert.equal(source.trace.filter((entry) => entry[0] === "open").length, 1)
})

test("packet validators preserve actual JZZ defaults, signed zero and non-finite errors", () => {
    const cases = []
    const decode = (x) => (x?.special === "undefined" ? undefined : x?.special ? numeric(x.special) : x)
    for (const type of ["noteon", "noteoff", "control"]) for (const channel of [0, 15, -1, 16, { special: "NaN" }, { special: "Infinity" }, { special: "-0" }]) for (const val of [{ special: "undefined" }, null, false, 0, 127, 128, "0x10", "1e1", { special: "NaN" }, { special: "Infinity" }]) cases.push({ op: "packet", type, channel, key: type === "control" ? 1 : "C5", value: val })
    coverage.packetCases = cases.length
    const native = batch(cases)
    cases.forEach((input, i) => {
        try {
            const method = input.type === "noteon" ? "noteOn" : input.type === "control" ? "control" : "noteOff"
            const bytes = [...JZZ.MIDI[method](decode(input.channel), input.key, decode(input.value))]
            assert.deepEqual(native[i], { ok: true, value: bytes })
        } catch (error) {
            assert.equal(native[i].ok, false, JSON.stringify(input))
            assert.deepEqual({ name: native[i].error.name, message: native[i].error.message }, { name: error.name, message: error.message }, JSON.stringify(input))
        }
    })
})

test("receiveMidi callback uses actual JZZ toString, labels, zero velocity, short messages and source event fields", async () => {
    const source = midiOracle()
    await source.source.receiveMidi({ id: "action 日本語", input: "port" })
    const callback = [...source.callbacks.values()][0]
    const messages = []
    for (let channel = 0; channel < 16; channel++) for (const status of [0x80, 0x90, 0xa0, 0xb0, 0xc0]) for (const velocity of [0, 1, 127, 128, 255]) messages.push(JZZ.MIDI([status + channel, 60, velocity]))
    for (const bytes of [[], [0x90], [0x80, 60], [0xb0, 2], [0xf8], [0xf0, 0x7f, 1, 4, 1, 0, 0, 0xf7]]) messages.push(JZZ.MIDI(bytes))
    messages.push(JZZ.MIDI([0xa0, 60, 20]).label("Note Off"))
    coverage.receiveMessages = messages.length + 1
    const actual = batch(messages.map((msg) => ({ op: "receive", id: "action 日本語", bytes: [...msg], description: msg.toString() })))
    messages.forEach((msg, i) => {
        const before = source.events.length
        callback(msg)
        assert.equal(actual[i].ok, true)
        assert.deepEqual(actual[i].value, source.events.length > before ? source.events.at(-1).data : null, msg.toString())
    })
    assert.deepEqual(source.events.find((event) => event.data.data.type === "noteoff").channel, "RECEIVE_MIDI2")
    const missing = midiOracle()
    await missing.source.receiveMidi({ input: "port" })
    ;[...missing.callbacks.values()][0](JZZ.MIDI([0x90]))
    assert.deepEqual(value({ op: "receive", bytes: [0x90], description: JZZ.MIDI([0x90]).toString() }), missing.events[0].data)
})

function mtcOracle() {
    const effects = [],
        trace = []
    let callback,
        now = 0
    const port = {
        name: () => "port",
        connect: (fn) => {
            callback = fn
        },
        close: () => trace.push("close"),
        send: (bytes) => trace.push([...JZZ.MIDI([...bytes])])
    }
    port.or = () => port
    const engine = {
        openMidiIn: (name) => {
            trace.push(["in", name])
            return port
        },
        openMidiOut: (name) => {
            trace.push(["out", name])
            return port
        }
    }
    const source = upstreamFunctions("src/electron/timecode/MTC.ts", ["setupMTCListener", "sendMTC", "stopSendMTC"], { JZZ: () => engine, Date: { now: () => now }, processFrame: (time) => effects.push({ kind: "frame", value: number(time) }), updateTimecodeStatus: (status) => effects.push({ kind: "status", value: status }) }, "let sendPort = null;")
    return {
        source,
        effects,
        trace,
        feed: (bytes, time) => {
            now = numeric(time)
            callback(JZZ.MIDI(bytes))
        }
    }
}

test("MTC send preserves exact source eight-packet order, 24fps code bug, 29.97 and special math", async () => {
    const cases = []
    for (const time of [-1e30, -86400001, -1, "-0", 0, 1, 999, 1000, 3599999, 3600000, 86399999, 86400000, 1e30, "NaN", "Infinity", "-Infinity"]) for (const fps of [0, 23.976, 24, 25, 29.97, 30, 60, -25, "NaN", "Infinity"]) cases.push({ op: "mtcSend", time, fps })
    coverage.mtcSendCases = cases.length
    const actual = batch(cases)
    for (const [i, input] of cases.entries()) {
        const source = mtcOracle()
        await source.source.sendMTC(numeric(input.time), numeric(input.fps), { midiOutput: "port" })
        assert.deepEqual(actual[i], { ok: true, value: source.trace.slice(1) }, JSON.stringify(input))
    }
    const source = mtcOracle()
    await source.source.sendMTC(0, 24, { midiOutput: "first" })
    await source.source.sendMTC(0, 30, { midiOutput: "second" })
    assert.equal(source.trace.filter((entry) => entry[0] === "out").length, 1)
    source.source.stopSendMTC()
    await source.source.sendMTC(0, 25, { midiOutput: "second" })
    coverage.mtcSenderLifecycleCalls = 3
    assert.deepEqual(
        source.trace.filter((entry) => entry[0] === "out"),
        [
            ["out", "first"],
            ["out", "second"]
        ]
    )
})

test("MTC receiver ordered transitions execute source partial quarter/full/SPP transport and clock edge cases", async () => {
    const sequences = []
    for (const fps of [24, 25, 29.97, 30]) {
        const frames = value({ op: "mtcSend", time: 3723456, fps })
        sequences.push([...frames.map((bytes, i) => ({ bytes, now: i })), { bytes: [0xf1, 0x70], now: 10 }, { bytes: [0xfa], now: 1000 }, { bytes: [0xf8], now: 1499 }, { bytes: [0xf8], now: 1999 }, { bytes: [0xf8], now: 1900 }, { bytes: [0xfc], now: 2000 }, { bytes: [0xf8], now: 5000 }])
    }
    sequences.push([
        { bytes: [0xf1], now: 0 },
        { bytes: [0xf1, 0x7f], now: 1 },
        { bytes: [0xf0, 0x7f, 0, 1, 1], now: 2 },
        { bytes: [0xfa], now: 3 },
        { bytes: [0xf8], now: 4 },
        { bytes: [0xf2], now: 5 },
        { bytes: [0xfa], now: 0 },
        { bytes: [0xf8], now: 1 }
    ])
    sequences.push([
        { bytes: [0xf0, 0x7f, 99, 1, 1, 0x61, 2, 3, 29, 0], now: 1 },
        { bytes: [0xfb], now: 100 },
        { bytes: [0xf8], now: "Infinity" },
        { bytes: [0xf8], now: "NaN" },
        { bytes: [0xf8], now: 200 },
        { bytes: [0xf2, 255, 255], now: 300 },
        { bytes: [0xfa], now: -1 },
        { bytes: [0xf8], now: 400 }
    ])
    for (let code = 0; code < 4; code++)
        sequences.push([
            { bytes: [0xf0, 0x7f, 1, 1, 1, (code << 5) | 27, 59, 58, 255, 0], now: 0 },
            { bytes: [0xf1, 0x70 | (code << 1) | 1], now: 1 }
        ])
    let seed = 0x4d5443
    const random = () => (seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0)
    for (let sequence = 0; sequence < 30; sequence++) {
        const messages = []
        let now = 1000
        for (let i = 0; i < 80; i++) {
            const status = [0xf1, 0xf2, 0xf8, 0xfa, 0xfb, 0xfc, 0x90][random() % 7]
            const bytes = [status, random() % (status === 0x90 ? 128 : 256), random() % 256]
            now += (random() % 800) - 100
            messages.push({ bytes, now })
        }
        sequences.push(messages)
    }
    coverage.mtcReceiverSequences = sequences.length
    coverage.mtcReceiverMessages = sequences.reduce((count, messages) => count + messages.length, 0)
    const actual = batch(sequences.map((messages) => ({ op: "mtcReceive", messages })))
    for (const [i, messages] of sequences.entries()) {
        const source = mtcOracle()
        const listener = await source.source.setupMTCListener(99, { midiInput: "port" })
        const effects = messages.map(({ bytes, now }) => {
            const start = source.effects.length
            source.feed(bytes, now)
            return source.effects.slice(start)
        })
        assert.deepEqual(actual[i].value.effects, effects, "sequence " + i)
        listener.stop()
        assert.equal(source.trace.at(-1), "close")
    }
})

test("timecode shared status suppression executes original persistent lastStatus behavior", () => {
    const events = []
    const source = upstreamFunctions("src/electron/timecode/timecode.ts", ["updateTimecodeStatus", "processFrame"], { sendMain: (channel, value) => events.push({ channel, value }), Main: { TIMECODE_STATUS: "TIMECODE_STATUS", TIMECODE_VALUE: "TIMECODE_VALUE" } }, "let lastStatus = null;")
    const values = ["play", "play", "pause", "pause", "stop", "stop", "play", "stop", "pause"]
    coverage.statusCalls = values.length
    const expected = values.map((status) => {
        const count = events.length
        source.updateTimecodeStatus(status)
        return events.length > count ? { kind: "status", value: status } : null
    })
    assert.deepEqual(value({ op: "status", values }), expected)
    source.processFrame(NaN)
    assert.equal(events.at(-1).channel, "TIMECODE_VALUE")
    assert.equal(Number.isNaN(events.at(-1).value), true)
})

test("native port bookkeeping follows actual source reuse, numeric-key close order, close failures and unknown-id close-all", async () => {
    const actions = [
        { kind: "listen", id: "z", input: "input" },
        ...["z", "10", "2", "a"].map((id) => ({ kind: "listen", id, input: "device " + id })),
        { kind: "listen", id: "z", input: "replacement" },
        { kind: "close", id: "z", failClose: [1] },
        { kind: "listen", id: "z", input: "replacement" },
        { kind: "close", id: "unknown", failClose: [3] },
        ...["z", "10", "2", "a"].map((id) => ({ kind: "send", data: { output: id, type: "noteon", values: { channel: 1, note: 60, velocity: 1 } } })),
        { kind: "send", data: { output: "z", values: { channel: 17, note: 60, velocity: 1 } } },
        { kind: "stop", failClose: [6] },
        { kind: "send", data: { output: "z", values: { channel: 1, note: 60 } } },
        { kind: "stop" }
    ]
    const source = midiOracle()
    for (const action of actions) {
        source.failClose.clear()
        for (const id of action.failClose || []) source.failClose.add(id)
        if (action.kind === "listen") await source.source.receiveMidi(action)
        else if (action.kind === "send") await source.source.sendMidi(structuredClone(action.data))
        else if (action.kind === "close") source.source.closeMidiInPorts(action.id)
        else source.source.stopMidi()
    }
    coverage.portActions = actions.length
    const actual = value({ op: "ports", actions })
    assert.deepEqual(actual.trace, clean(source.trace))
    assert.deepEqual(actual.counts, [0, 0, false])
})

test("failed real-boundary opens never populate native or original caches and are retried", async () => {
    const actions = [{ kind: "send", failOpen: true, data: { output: "missing", values: { channel: 1, note: 60 } } }, { kind: "send", failOpen: true, data: { output: "missing", values: { channel: 1, note: 60 } } }, { kind: "send", data: { output: "missing", values: { channel: 1, note: 60 } } }, { kind: "listen", failOpen: true, id: "a", input: "missing" }, { kind: "listen", id: "a", input: "missing" }, { kind: "stop" }]
    const source = midiOracle()
    let failed = false
    for (const method of ["openMidiIn", "openMidiOut"]) {
        const open = source.engine[method]
        source.engine[method] = (name) => {
            if (failed) {
                source.trace.push(["open", method === "openMidiIn" ? "in" : "out", name])
                throw new Error("backend open failed")
            }
            return open(name)
        }
    }
    for (const action of actions) {
        failed = action.failOpen === true
        if (action.kind === "send") await source.source.sendMidi(structuredClone(action.data))
        else if (action.kind === "listen") await source.source.receiveMidi(action)
        else source.source.stopMidi()
    }
    const native = value({ op: "ports", actions })
    coverage.portActions += actions.length
    assert.deepEqual(native.trace, clean(source.trace))
    assert.deepEqual(native.counts, [0, 0, false])
})
