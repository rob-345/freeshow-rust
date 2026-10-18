import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { spawnSync } from "node:child_process"
import { createRequire } from "node:module"
import { createHash } from "node:crypto"
import { fileURLToPath } from "node:url"
import test from "node:test"
import ts from "typescript"
const root = fileURLToPath(new URL("../../", import.meta.url)),
    require = createRequire(import.meta.url),
    JZZ = require("jzz")
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
assert.equal(require("jzz/package.json").version, "1.9.5")
assert.equal(
    createHash("sha256")
        .update(fs.readFileSync(require.resolve("jzz")))
        .digest("hex"),
    "2cd148af6319c3106dd5a56ebb4eaa9a6ea161ca5740884d57cb1a5037ca01a0"
)
// Every send/open below is a controlled virtual backend, never an OS port.
await JZZ({ engine: "none" })
const binary = process.env.FREESHOW_MIDI_PORTS_PARITY || path.join(root, "target/debug/freeshow-midi-ports-parity" + (process.platform === "win32" ? ".exe" : ""))
const coverage = { selectors: 0, scripts: 0, packets: 0, received: 0, deferred: 0, nativeProbes: 0, deviceLists: 0 }
test.after(() => {
    JZZ().close()
    console.log("Native MIDI port parity coverage:", JSON.stringify(coverage))
})
function batch(cases) {
    const result = spawnSync(binary, [], { cwd: root, input: cases.map(JSON.stringify).join("\n") + "\n", encoding: "utf8", timeout: 15000, maxBuffer: 32 * 1024 * 1024 })
    assert.equal(result.status, 0, result.stderr || result.error?.message)
    return result.stdout.trim().split("\n").map(JSON.parse)
}
const astCache = new Map()
function source(filename, names, injected = {}, prelude = "") {
    let ast = astCache.get(filename)
    if (!ast) {
        const result = spawnSync("git", ["show", `${revision}:${filename}`], { cwd: root, encoding: "utf8", timeout: 10000 })
        assert.equal(result.status, 0, result.stderr)
        ast = ts.createSourceFile(filename, result.stdout, ts.ScriptTarget.Latest, true)
        astCache.set(filename, ast)
    }
    const declarations = names.map((name) => {
        const node = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === name)
        assert.ok(node, name)
        return node.getText(ast).replace(/^export\s+/, "")
    })
    const errors = [],
        context = vm.createContext({
            ...injected,
            console: {
                error(error) {
                    errors.push({ name: error?.name, message: error?.message })
                },
                warn() {},
                info() {}
            }
        })
    new vm.Script(ts.transpileModule(prelude + "\n" + declarations.join("\n") + `\nglobalThis.result={${names.join(",")}}`, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText).runInContext(context, { timeout: 1000 })
    return { ...context.result, errors }
}
const clean = (value) => JSON.parse(JSON.stringify(value))
function wire(value) {
    const hydration = { undefinedPaths: [], numbers: [] }
    function visit(v, at) {
        if (v === undefined) {
            hydration.undefinedPaths.push(at)
            return undefined
        }
        if (Array.isArray(v)) return v.map((v, i) => visit(v, at + "/" + i))
        if (v && typeof v === "object") {
            const out = {}
            for (const [k, x] of Object.entries(v)) {
                const y = visit(x, at + "/" + k)
                if (y !== undefined) out[k] = y
            }
            return out
        }
        return v
    }
    return { data: visit(value, ""), hydration }
}
let sequence = 0
function engine(inputs, outputs) {
    const prefix = "native-parity-" + ++sequence + "-",
        trace = [],
        incoming = new Map(),
        physical = new Map(),
        registrations = [],
        pending = []
    let defer = false
    function register(direction, name) {
        const registered = prefix + direction + "-" + name
        const api = {
            _info: () => ({ name: registered }),
            [direction === "in" ? "_openIn" : "_openOut"](port) {
                port._pause()
                const open = () => {
                    if (name.startsWith("fail")) {
                        trace.push(["open", direction, name])
                        port._crash("Controlled open failure")
                        return
                    }
                    const key = direction + ":" + name
                    let clients = physical.get(key)
                    if (!clients) {
                        clients = new Set()
                        physical.set(key, clients)
                        trace.push(["open", direction, name])
                    }
                    clients.add(port)
                    port._info = { name: registered }
                    port._close = () => {
                        clients.delete(port)
                        if (!clients.size) {
                            trace.push(["close", direction, name])
                            physical.delete(key)
                        }
                    }
                    if (direction === "in") {
                        let list = incoming.get(name)
                        if (!list) {
                            list = new Set()
                            incoming.set(name, list)
                        }
                        list.add(port)
                        const close = port._close
                        port._close = () => {
                            list.delete(port)
                            close()
                        }
                    } else port._receive = (message) => trace.push(["send", name, [...message]])
                    port._resume()
                }
                if (defer) {
                    defer = false
                    pending.push(open)
                } else open()
            }
        }
        assert.equal(JZZ.lib[direction === "in" ? "registerMidiIn" : "registerMidiOut"](registered, api), true)
        registrations.push([direction, registered])
        return registered
    }
    const inputNames = inputs.map((n) => register("in", n)),
        outputNames = outputs.map((n) => register("out", n))
    const mapSelector = (value, names) => {
        if (typeof value === "string") {
            const index = (names === inputNames ? inputs : outputs).indexOf(value)
            return index < 0 ? value : names[index]
        }
        if (Array.isArray(value)) return value.map((v) => mapSelector(v, names))
        if (value && typeof value === "object" && typeof value.name === "string") return { ...value, name: mapSelector(value.name, names) }
        return value
    }
    const wrap = (port) => ({
        or(...args) {
            return Promise.resolve(port.or(...args)).then((actual) => ({ name: () => actual.name(), close: () => actual.close(), connect: (fn) => actual.connect(fn), control: (...args) => actual.control(...args), noteOn: (...args) => actual.noteOn(...args), noteOff: (...args) => actual.noteOff(...args), send: (bytes) => actual.send(Array.from(bytes)) }))
        }
    })
    const facade = () => ({ info: () => ({ inputs: inputs.map((name) => ({ name, extra: "ignored" })), outputs: outputs.map((name) => ({ name, extra: "ignored" })) }), openMidiIn: (selector) => wrap(JZZ().openMidiIn(mapSelector(selector, inputNames))), openMidiOut: (selector) => wrap(JZZ().openMidiOut(mapSelector(selector, outputNames))) })
    return {
        JZZ: facade,
        trace,
        pending,
        defer() {
            defer = true
        },
        emit(name, bytes) {
            for (const port of incoming.get(name) || []) port._emit(JZZ.MIDI(bytes))
        },
        cleanup() {
            for (const [direction, name] of registrations) JZZ.lib[direction === "in" ? "unregisterMidiIn" : "unregisterMidiOut"](name)
        }
    }
}
async function compareScript(inputs, outputs, actions) {
    const backend = engine(inputs, outputs),
        messages = [],
        sourceCallbacks = []
    const midi = source(
        "src/electron/utils/midi.ts",
        ["getMidiInputs", "getMidiOutputs", "sendMidi", "receiveMidi", "closeMidiInPorts", "closeMidiOutPorts", "stopMidi"],
        {
            JZZ: backend.JZZ,
            sendToMain: (_channel, data) => {
                sourceCallbacks.push(structuredClone(data))
                messages.push(wire(data))
            },
            ToMain: { RECEIVE_MIDI2: "RECEIVE_MIDI2" }
        },
        "let openedPorts={};let openedOutputPorts={};"
    )
    const mtc = source("src/electron/timecode/MTC.ts", ["sendMTC", "stopSendMTC"], { JZZ: backend.JZZ }, "let sendPort=null;")
    try {
        for (const action of actions) {
            switch (action.op) {
                case "send":
                    if (action.deferred) {
                        backend.defer()
                        const pending = midi.sendMidi(structuredClone(action.data))
                        await new Promise((resolve) => setImmediate(resolve))
                        assert.equal(backend.pending.length, 1)
                        assert.equal(backend.trace.filter((v) => v[0] === "send").length, 0)
                        backend.pending.shift()()
                        await pending
                        coverage.deferred++
                    } else await midi.sendMidi(structuredClone(action.data))
                    break
                case "listen":
                    await midi.receiveMidi(structuredClone(action.data))
                    break
                case "emit":
                    backend.emit(inputs[Number((action.device || "i0").slice(1))], action.bytes)
                    break
                case "close":
                    midi.closeMidiInPorts(action.id)
                    break
                case "stop":
                    midi.stopMidi()
                    break
                case "mtcSend":
                    await mtc.sendMTC(action.time, action.fps, { midiOutput: action.selector })
                    break
                case "mtcStop":
                    mtc.stopSendMTC()
                    break
                default:
                    throw new Error("unsupported source action " + action.op)
            }
        }
        const [actual] = batch([{ inputs, outputs, actions }])
        assert.equal(actual.shutdown, null, JSON.stringify(actual))
        assert.deepEqual(actual.trace, backend.trace)
        assert.deepEqual(
            actual.messages.map((v) => v.data),
            messages
        )
        for (const result of actual.results)
            if (!result.ok) {
                assert.ok(
                    midi.errors.some((e) => e.message === result.error.message),
                    JSON.stringify({ result, errors: midi.errors })
                )
            }
        coverage.scripts++
        coverage.packets += backend.trace.filter((v) => v[0] === "send").length
        coverage.received += messages.length
        return { ...actual, sourceCallbacks }
    } finally {
        midi.stopMidi()
        mtc.stopSendMTC()
        backend.cleanup()
    }
}

test("actual pinned JZZ selector includes splice-index quirks, duplicate names, Unicode and ordered fallback", () => {
    const text = fs.readFileSync(require.resolve("jzz"), "utf8"),
        ast = ts.createSourceFile("JZZ.js", text, ts.ScriptTarget.Latest, true)
    let node
    function visit(n) {
        if (ts.isFunctionDeclaration(n) && n.name?.text === "_filterList") node = n
        ts.forEachChild(n, visit)
    }
    visit(ast)
    assert.ok(node)
    const context = vm.createContext({ _func: (v) => typeof v === "function" })
    new vm.Script(node.getText(ast) + ";globalThis.filter=_filterList").runInContext(context)
    const names = ["A", "B", "A", "敬拜 🙌", "0", "1"],
        selectors = [undefined, null, false, true, 0, 1, 2, -1, 1.5, 6, 10, "0", "1", "01", "A", "B", "敬拜 🙌", "missing", {}, { name: "A" }, { name: "敬拜 🙌" }, [], ["A"], ["missing", "B"], [1, "A"], ["A", 1], [null, "A"], [["A"]], [{ name: "B" }], { name: null }]
    const cases = []
    for (const selector of selectors) {
        cases.push({ op: "select", names, selector })
        for (let end = 0; end < names.length; end++) cases.push({ op: "select", names: names.slice(0, end), selector })
    }
    cases.push({ op: "select", names, parts: [{ value: "敬拜 🙌" }, { undefined: true }, { value: "A" }] }, { op: "select", names, parts: [{ value: "A" }, { undefined: true }, { value: 1 }] })
    const actual = batch(cases)
    cases.forEach((item, i) => {
        context.names = JSON.stringify(item.names)
        context.selector = JSON.stringify(item.selector)
        context.parts = item.parts ? JSON.stringify(item.parts) : null
        const expected = new vm.Script(`var arr=JSON.parse(names).map((name,i)=>({name,i})); var q=parts?JSON.parse(parts).map(v=>v.undefined?undefined:v.value):selector===undefined?undefined:JSON.parse(selector);filter(q,arr).map(v=>v.i)`).runInContext(context)
        assert.equal(actual[i].ok, true, JSON.stringify(actual[i]))
        assert.deepEqual(actual[i].indices, Array.from(expected))
        coverage.selectors++
    })
})
test("actual JZZ deferred output opens, caches after open and validates messages after opening", async () => {
    await compareScript([], ["X"], [{ op: "send", data: { output: "X", values: { channel: 17, note: 60, velocity: 100 } }, deferred: true }, { op: "send", data: { output: "X", type: "control", values: { channel: 1, controller: 7, value: 100 } } }, { op: "send", data: { output: "X" } }, { op: "stop" }, { op: "send", data: { output: "X", values: { channel: 16, note: "C4", velocity: 127 } } }, { op: "stop" }])
})
test("shared native input routes exact source payloads and closes only after its last listener", async () => {
    await compareScript(
        ["A"],
        [],
        [{ op: "listen", data: { id: "2", input: "A" } }, { op: "listen", data: { id: "1", input: "A" } }, { op: "listen", data: { id: "1", input: "missing" } }, { op: "listen", data: { id: "sentinel", input: "input" } }, { op: "emit", bytes: [0x90, 60, 0], expect: 2 }, { op: "close", id: "2" }, { op: "emit", bytes: [0xb2, 7, 101], expect: 1 }, { op: "close", id: "missing" }, { op: "listen", data: { id: "next", input: "A" } }, { op: "emit", bytes: [0x80, 60, 1], expect: 1 }, { op: "stop" }]
    )
})
test("integer listener close order, unknown-ID close-all and original valid note/control transformations", async () => {
    const actions = [
        { op: "listen", data: { id: "10", input: "A" } },
        { op: "listen", data: { id: "2", input: "B" } },
        { op: "listen", data: { id: "z", input: "敬拜" } }
    ]
    for (const status of [0x80, 0x90, 0x9f, 0xb0, 0xbf]) for (const value of [0, 1, 127]) actions.push({ op: "emit", device: "i0", bytes: [status, 60, value], expect: 1 })
    actions.push({ op: "close", id: "unknown" }, { op: "stop" })
    await compareScript(["A", "B", "敬拜"], [], actions)
})
test("actual source MTC output remains independent of ordinary MIDI stop and retains first selector", async () => {
    await compareScript([], ["X", "Y"], [{ op: "send", data: { output: "X" } }, { op: "mtcSend", selector: "X", time: 12345, fps: 24 }, { op: "mtcSend", selector: "Y", time: 12346, fps: 29.97 }, { op: "stop" }, { op: "mtcSend", selector: "Y", time: 9999, fps: 30 }, { op: "mtcStop" }])
})
test("real native enumeration reports actual OS results or explicit unavailable, without a fake port", () => {
    const [actual] = batch([{ op: "native" }])
    for (const direction of ["inputs", "outputs"]) {
        const result = actual[direction]
        if (result.ok) {
            assert.equal(result.value.kind, "devices")
            for (const device of result.value.value.data) {
                assert.deepEqual(Object.keys(device), ["name"])
                assert.equal(typeof device.name, "string")
            }
        } else assert.equal(result.error.code, "midi_backend_unavailable", JSON.stringify(result))
    }
    assert.equal(actual.shutdown, null)
    assert.equal(actual.status.workerFinished, true)
    assert.equal(actual.status.admitted, 0)
    coverage.nativeProbes++
})

test("source device-list projection preserves native names/order/duplicates while omitting backend metadata", () => {
    const inputs = ["A", "敬拜 🙌", "A"],
        outputs = ["X", "Y", "X"]
    const original = source("src/electron/utils/midi.ts", ["getMidiInputs", "getMidiOutputs"], { JZZ: () => ({ info: () => ({ inputs: inputs.map((name) => ({ name, ignored: 1 })), outputs: outputs.map((name) => ({ name, id: "ignored" })) }) }) })
    const [actual] = batch([{ inputs, outputs, actions: [{ op: "inputs" }, { op: "outputs" }] }])
    assert.deepEqual(actual.results[0].value.value, wire(original.getMidiInputs()))
    assert.deepEqual(actual.results[1].value.value, wire(original.getMidiOutputs()))
    coverage.deviceLists += 2
})

test("actual JZZ tries ordered candidate opens and failed opens never cache fake native availability", async () => {
    await compareScript([], ["fail-X", "X"], [{ op: "send", data: {} }, { op: "send", data: {} }, { op: "stop" }])
    await compareScript([], ["fail-X"], [{ op: "send", data: { output: "fail-X" } }, { op: "send", data: { output: "fail-X" } }, { op: "stop" }])
})

test("numeric close IDs preserve source property coercion and falsy close-all behavior", async () => {
    await compareScript(["A", "B"], [], [{ op: "listen", data: { id: 12, input: "A" } }, { op: "listen", data: { id: "other", input: "B" } }, { op: "close", id: 12 }, { op: "close", id: 0 }, { op: "stop" }])
})

test("actual JZZ controller descriptions preserve the source All Notes Off classification quirk", async () => {
    const actions = [{ op: "listen", data: { id: "id", input: "A" } }]
    for (let controller = 0; controller < 128; controller++) actions.push({ op: "emit", bytes: [0xb0, controller, 0], expect: 1 })
    actions.push({ op: "emit", bytes: [0xb0, 123], expect: 1 }, { op: "stop" })
    const result = await compareScript(["A"], [], actions)
    assert.deepEqual(result.messages[123].data.data, { id: "id", values: { note: 123, velocity: 0, channel: 1 }, type: "noteoff" })
    // Raw structured-clone evidence is separate from the JSON/WireData projection.
    const complete = result.sourceCallbacks[123].values
    assert.equal(Object.hasOwn(complete, "note"), true)
    assert.equal(complete.note, 123)
    assert.equal(Object.hasOwn(complete, "velocity"), true)
    assert.equal(complete.velocity, 0)
    assert.equal(Object.hasOwn(complete, "controller"), false)
    const truncated = result.sourceCallbacks[128].values
    assert.equal(Object.hasOwn(truncated, "velocity"), true)
    assert.equal(truncated.velocity, undefined)
    assert.equal(Object.hasOwn(clean(truncated), "velocity"), false)
    assert.deepEqual(result.messages[128].data, {
        data: { id: "id", values: { note: 123, channel: 1 }, type: "noteoff" },
        hydration: { undefinedPaths: ["/values/velocity"], numbers: [] }
    })
})

test("malformed null listener preserves the source rejection instead of reporting an ignored successful listen", async () => {
    const original = source(
        "src/electron/utils/midi.ts",
        ["receiveMidi"],
        {
            JZZ: () => {
                throw new Error("must not open")
            }
        },
        "let openedPorts={};"
    )
    await assert.rejects(original.receiveMidi(null), { name: "TypeError", message: "Cannot read properties of null (reading 'input')" })
    const [actual] = batch([{ inputs: ["A"], outputs: [], actions: [{ op: "listen", data: null }] }])
    assert.equal(actual.results[0].ok, false)
    assert.equal(actual.results[0].error.code, "midi_invalid")
    assert.equal(actual.results[0].error.message, "Cannot read properties of null (reading 'input')")
    assert.deepEqual(actual.trace, [])
})
