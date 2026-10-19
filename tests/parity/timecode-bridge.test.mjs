import assert from "node:assert/strict"
import { test } from "node:test"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { createHash } from "node:crypto"
import { execFileSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import ts from "typescript"

const root = fileURLToPath(new URL("../../", import.meta.url)),
    revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const compile = (source, module = ts.ModuleKind.ES2022) => ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module } }).outputText
const { createTimecodeBridge } = await import(`data:text/javascript;base64,${Buffer.from(compile(fs.readFileSync(path.join(root, "src/frontend/rust/timecodeBridge.ts"), "utf8"))).toString("base64")}`)
const hashes = {
    "src/frontend/IPC/main.ts": "26492a5d973e31afb1ed699b32a4f477c584725f90c9069379b4c36cf7febc9d",
    "src/frontend/components/timeline/timecode.ts": "dfed90528a164f69189ac60d4bc8c1054879a311ae9fd6fe5214943e2b1ef11a",
    "src/electron/timecode/timecode.ts": "18506a959deaf9053f5bae84235963e2dbf3779688d5a3d646a4995a65820a79",
    "src/electron/IPC/responsesMain.ts": "70372b48b698f74f239b82f92e48dde69880a7c4d78af1a9f0e1723313fb57a3",
    "public/assets/ltc-processor.js": "a221a037fadc21643c78329ad9faf09c1df55f408876edf8e69dc572e97bd332"
}
const sources = Object.fromEntries(
    Object.entries(hashes).map(([file, hash]) => {
        const source = execFileSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", maxBuffer: 1024 * 1024, timeout: 10000 }).replace(/\r\n/g, "\n")
        assert.equal(createHash("sha256").update(source).digest("hex"), hash, `${file}: immutable source after CRLF normalization`)
        return [file, source]
    })
)
function load(file, injected = {}, names) {
    const ast = ts.createSourceFile(file, sources[file], ts.ScriptTarget.Latest, true)
    const declarations = ast.statements
        .filter((node) => (names ? ts.isFunctionDeclaration(node) && names.includes(node.name?.text) : !ts.isImportDeclaration(node)))
        .map((node) => node.getText(ast))
        .join("\n")
    const exports = {}
    new vm.Script(compile(declarations, ts.ModuleKind.CommonJS), { filename: file }).runInContext(vm.createContext({ exports, Buffer, Uint8Array, ArrayBuffer, console: { error() {}, info() {}, warn() {}, log() {} }, ...injected }), { timeout: 1000 })
    return exports
}
const channels = ["TIMECODE_START", "TIMECODE_STOP", "TIMECODE_VALUE", "TIMECODE_STATUS", "TIMECODE_AUDIO_DATA", "ALERT"]
const Main = Object.fromEntries(channels.map((key) => [key, key]))
const ctx = (generation = 1) => ({ runtimeId: "native-timecode", libraryGeneration: generation, dataPath: `/private/時間-${generation}` })
const tick = () => new Promise((resolve) => setImmediate(resolve))
async function until(predicate) {
    const end = Date.now() + 2000
    while (!predicate()) {
        assert.ok(Date.now() < end, "bounded actual callback wait")
        await tick()
    }
}
function deferred() {
    let resolve, reject
    const promise = new Promise((yes, no) => {
        resolve = yes
        reject = no
    })
    return { promise, resolve, reject }
}
function fixture(overrides = {}) {
    let current = ctx()
    const commands = [],
        audio = [],
        errors = [],
        deliveries = [],
        completions = [],
        retirements = []
    const bridge = createTimecodeBridge({
        getContext: () => current,
        command: async (message, context) => {
            commands.push({ message, context })
        },
        audio: async (bytes, mode, context, announcedLength) => {
            audio.push({ bytes, mode, context, announcedLength })
        },
        complete: async (context, id, accepted) => {
            completions.push({ context, id, accepted })
        },
        retire: async (context) => {
            retirements.push(context)
        },
        deliver: (message) => {
            deliveries.push(message)
        },
        report: (error) => errors.push(error),
        ...overrides
    })
    return {
        bridge,
        commands,
        audio,
        errors,
        deliveries,
        completions,
        retirements,
        setContext: (value) => {
            current = value
        }
    }
}
function publication(sequence, channel = "TIMECODE_VALUE", data = 1000, context = ctx()) {
    return { event: { channel: "MAIN", message: { channel, data }, context: { ...context } }, hydration: { undefinedPaths: [], numbers: [] }, delivery: { id: `timecode-00000000-0000-0000-0000-${(BigInt(sequence) & ((1n << 48n) - 1n)).toString(16).padStart(12, "0")}`, context: { ...context }, run: 1, sequence: String(sequence) } }
}
function sender(bridge) {
    return load(
        "src/frontend/IPC/main.ts",
        {
            Main,
            MAIN: "MAIN",
            window: {
                api: {
                    send(channel, message) {
                        assert.equal(channel, "MAIN")
                        assert.equal(bridge.handle(structuredClone(message)), true)
                    }
                }
            }
        },
        ["sendMain"]
    ).sendMain
}

test("actual sendMain wrappers retain own undefined, IEEE numbers, unknown field order and no source ACK", async () => {
    const f = fixture(),
        sendMain = sender(f.bridge)
    const data = { type: "send", mode: "MTC", framerate: -0, data: { midiOutput: undefined, second: "🎹", extra: NaN } }
    sendMain("TIMECODE_START", data)
    data.data.second = "mutated"
    sendMain("TIMECODE_STOP")
    sendMain("TIMECODE_STATUS", undefined)
    for (const number of [NaN, Infinity, -Infinity, -0, 1.5]) {
        sendMain("TIMECODE_VALUE", number)
        await tick()
    }
    await until(() => f.commands.length === 8)
    assert.deepEqual(Object.keys(f.commands[0].message.data.data), ["midiOutput", "second", "extra"])
    assert.equal(f.commands[0].message.data.data.second, "🎹")
    assert.ok(Object.hasOwn(f.commands[0].message.data.data, "midiOutput"))
    assert.ok(Object.is(f.commands[0].message.data.framerate, -0))
    assert.ok(Number.isNaN(f.commands[0].message.data.data.extra))
    assert.ok(Object.hasOwn(f.commands[1].message, "data"))
    assert.equal(f.commands[1].message.data, undefined)
    assert.ok(Object.isFrozen(f.commands[0].message.data.data))
    assert.ok(Object.isFrozen(f.commands[0].context))
    assert.deepEqual(
        f.commands.slice(3).map(({ message }) => message.data),
        [NaN, Infinity, -Infinity, -0, 1.5]
    )
    assert.equal(f.deliveries.length, 0)
    assert.equal(f.completions.length, 0)
    assert.equal(f.errors.length, 0)
    f.bridge.handle({ channel: "TIMECODE_STATUS" })
    await until(() => f.commands.length === 9)
    assert.equal(Object.hasOwn(f.commands[8].message, "data"), false)
    await f.bridge.close()
})

test("actual Electron handlers receive the same four source arguments and keep void return behavior", async () => {
    const ast = ts.createSourceFile("responsesMain.ts", sources["src/electron/IPC/responsesMain.ts"], ts.ScriptTarget.Latest, true)
    const declaration = ast.statements.flatMap((node) => (ts.isVariableStatement(node) ? [...node.declarationList.declarations] : [])).find((node) => node.name.getText(ast) === "mainResponses")
    assert.ok(declaration && ts.isObjectLiteralExpression(declaration.initializer))
    const properties = declaration.initializer.properties.filter((node) => channels.slice(0, 4).some((channel) => node.name?.getText(ast) === `[Main.${channel}]`)).map((node) => node.getText(ast))
    assert.equal(properties.length, 4)
    const trace = []
    const handlers = new vm.Script(compile(`const handlers={${properties.join(",")}};globalThis.result=handlers`, ts.ModuleKind.CommonJS)).runInNewContext({
        Main,
        timecodeStart: (data) => {
            trace.push(["start", data])
        },
        timecodeStop: () => {
            trace.push(["stop"])
        },
        updateTimecodeValue: (data) => {
            trace.push(["value", data])
        },
        console: {
            log: (data) => {
                trace.push(["status", data])
            }
        }
    })
    const f = fixture({
        command: async (message) => {
            assert.equal(handlers[message.channel](message.data), undefined)
        }
    })
    const sendMain = sender(f.bridge)
    sendMain("TIMECODE_START", { type: "receive", mode: "ableton", framerate: 0, data: null })
    sendMain("TIMECODE_STOP", { ignored: "source stop ignores data" })
    sendMain("TIMECODE_VALUE", NaN)
    sendMain("TIMECODE_STATUS", undefined)
    await until(() => trace.length === 4)
    assert.deepEqual(trace, [["start", { type: "receive", mode: "ableton", framerate: 0, data: null }], ["stop"], ["value", NaN], ["status", undefined]])
    assert.equal(f.deliveries.length, 0)
    await f.bridge.close()
})

test("genuine MessagePort and actual worklet preserve raw capture no-op; explicit wrapper uses original Buffer.from semantics", async (t) => {
    const f = fixture(),
        channel = new MessageChannel()
    t.after(() => {
        channel.port1.close()
        channel.port2.close()
    })
    let Processor
    const processorContext = vm.createContext({
        Uint8Array,
        AudioWorkletProcessor: class {
            constructor() {
                this.port = channel.port2
            }
        },
        registerProcessor(name, value) {
            assert.equal(name, "ltc-processor")
            Processor = value
        }
    })
    new vm.Script(sources["public/assets/ltc-processor.js"]).runInContext(processorContext, { timeout: 1000 })
    const processor = new Processor(),
        seen = []
    const sendMain = sender(f.bridge)
    channel.port1.onmessage = (event) => {
        seen.push(event.data)
        sendMain("TIMECODE_AUDIO_DATA", event.data)
    }
    const input = Float32Array.from({ length: 4096 }, (_, i) => ((i % 257) - 128) / 128)
    assert.equal(processor.process([[input]]), true)
    await until(() => seen.length === 1)
    assert.ok(seen[0] instanceof Uint8Array)
    assert.equal(seen[0].length, 4096)
    const fed = []
    const original = load("src/electron/timecode/timecode.ts", { feedLTCFrame: (bytes) => fed.push([...bytes]) })
    original.processAudioData(seen[0])
    assert.equal(fed.length, 0)
    assert.equal(f.audio.length, 0)
    // Explicit callers obey the declared {mode,buffer} payload. Views are copied
    // by their elements, matching the real original Buffer.from call.
    const backing = Uint8Array.from([9, 0, 255, 128, 7]),
        views = [backing.subarray(1, 4), new Uint16Array([256, 511, 1025]), new Float32Array([NaN, -1.5, 256.5]), new DataView(backing.buffer, 1, 3), backing.buffer, { type: "Buffer", data: [257, -1, NaN, null, "2"] }, [1, , undefined, Infinity]]
    for (const buffer of views) {
        original.processAudioData({ mode: "LTC", buffer })
        sendMain("TIMECODE_AUDIO_DATA", { mode: "LTC", buffer })
        await tick()
    }
    await until(() => f.audio.length === views.length)
    assert.deepEqual(
        f.audio.map(({ bytes }) => [...bytes]),
        fed
    )
    assert.deepEqual(
        f.audio.map(({ announcedLength }) => announcedLength),
        fed.map((bytes) => bytes.length)
    )
    backing.fill(33)
    assert.deepEqual([...f.audio[0].bytes], [0, 255, 128])
    assert.equal(f.errors.length, 0)
    await f.bridge.close()
})

test("actual retained listener forwards unwrapped worklet data and source frontend consumes hydrated Uint8Array samples", async (t) => {
    const f = fixture(),
        channels = [],
        samples = [],
        effects = [],
        tracks = [],
        forwarded = []
    t.after(() =>
        channels.forEach(({ port1, port2 }) => {
            port1.close()
            port2.close()
        })
    )
    class AudioContext {
        state = "running"
        sinkId = "speaker"
        audioWorklet = {
            addModule: async (url) => {
                effects.push(["module", url])
            }
        }
        createMediaStreamSource() {
            return { connect() {} }
        }
        createBuffer(count, length, rate) {
            assert.deepEqual([count, rate], [1, 48000])
            const data = new Float32Array(length)
            samples.push(data)
            return { getChannelData: () => data }
        }
        createBufferSource() {
            return {
                connect() {},
                start() {
                    effects.push(["start"])
                }
            }
        }
        close() {
            effects.push(["close"])
            return Promise.resolve()
        }
    }
    const frontend = load("src/frontend/components/timeline/timecode.ts", {
        get: () => ({ mode: "LTC", audioInput: "microphone", audioOutput: "speaker" }),
        timecode: {},
        Main,
        sendMain: (...args) => {
            forwarded.push(structuredClone(args))
            sender(f.bridge)(...args)
        },
        AudioContext,
        getActiveTimelinePlayback: () => ({ setTime: (n) => effects.push(["time", n]), play: (n) => effects.push(["play", n]), pause: (n) => effects.push(["pause", n]) }),
        navigator: { mediaDevices: { getUserMedia: async () => ({ getTracks: () => [{ stop: () => tracks.push("stop") }] }) } },
        AudioWorkletNode: class {
            constructor() {
                const value = new MessageChannel()
                channels.push(value)
                this.port = value.port1
            }
            connect() {}
        }
    })
    await frontend.startListeningLTC()
    channels[0].port2.postMessage(new Uint8Array([2, 3, 4]))
    await until(() => forwarded.length === 1)
    assert.equal(forwarded[0][0], "TIMECODE_AUDIO_DATA")
    assert.ok(forwarded[0][1] instanceof Uint8Array)
    assert.equal(Object.hasOwn(forwarded[0][1], "mode"), false)
    assert.equal(f.audio.length, 0)
    assert.equal(f.commands.length, 0)
    const output = fixture({
        deliver: (message) => {
            if (message.channel === "TIMECODE_VALUE") frontend.updateTimelineTime(message.data)
            if (message.channel === "TIMECODE_STATUS") frontend.updateTimelineStatus(message.data)
            if (message.channel === "TIMECODE_AUDIO_DATA") frontend.processTimecodeFrame(message.data)
        }
    })
    output.bridge.publish(publication(1, "TIMECODE_VALUE", 1234))
    await tick()
    output.bridge.publish(publication(2, "TIMECODE_STATUS", "play"))
    await tick()
    const audio = publication(3, "TIMECODE_AUDIO_DATA", null)
    audio.bytes = [0, 128, 255]
    output.bridge.publish(audio)
    await tick()
    assert.deepEqual([...samples[0]], [-1, 0, 127 / 128])
    assert.ok(effects.some((event) => event[0] === "time" && event[1] === 1234))
    assert.ok(effects.some((event) => event[0] === "play" && event[1] === true))
    await frontend.stopListeningLTC()
    assert.deepEqual(tracks, ["stop"])
    assert.equal(output.completions.length, 3)
    await f.bridge.close()
    await output.bridge.close()
})

test("private hydration restores IEEE identity, exact bytes and alert strings before one-use completion", async () => {
    const f = fixture()
    for (const [index, kind] of ["NaN", "PositiveInfinity", "NegativeInfinity", "NegativeZero"].entries()) {
        const event = publication(index + 1, "TIMECODE_VALUE", null)
        event.hydration.numbers.push({ path: "", number: { kind } })
        f.bridge.publish(event)
        await tick()
    }
    assert.deepEqual(
        f.deliveries.map(({ data }) => data),
        [NaN, Infinity, -Infinity, -0]
    )
    const event = publication(5, "TIMECODE_AUDIO_DATA", null),
        raw = new Uint8Array([99, 0, 255, 7])
    event.bytes = raw.subarray(1, 3)
    f.bridge.publish(event)
    raw.fill(44)
    await tick()
    assert.ok(f.deliveries[4].data instanceof Uint8Array)
    assert.deepEqual([...f.deliveries[4].data], [0, 255])
    f.bridge.publish(publication(6, "ALERT", "Could not initialize LTC Encoder!"))
    await tick()
    assert.equal(f.deliveries[5].data, "Could not initialize LTC Encoder!")
    assert.equal(f.completions.length, 6)
    assert.ok(f.completions.every(({ accepted }) => accepted))
    assert.ok(f.deliveries.every((message) => Object.keys(message).join() === "channel,data"))
    await f.bridge.close()
})

test("full contexts and monotonic u64 strings fence historic replay, reorder and retired delivery without UUID history", async () => {
    const f = fixture(),
        first = publication(9007199254740993n)
    f.bridge.publish(first)
    await tick()
    f.bridge.publish(first)
    f.bridge.publish(publication(9007199254740992n))
    assert.equal(f.deliveries.length, 1)
    assert.equal(f.completions.length, 1)
    f.setContext(ctx(2))
    f.bridge.publish(publication(9007199254740994n))
    await tick()
    assert.equal(f.completions[1].accepted, false)
    assert.equal(f.deliveries.length, 1)
    await f.bridge.contextChanged()
    f.bridge.publish(publication(9007199254740995n, "TIMECODE_STATUS", "stop", ctx(2)))
    await tick()
    assert.equal(f.deliveries.length, 2)
    assert.equal(f.bridge.status().lastSequence, "9007199254740995")
    assert.deepEqual(f.retirements, [ctx()])
    f.bridge.publish(publication(9007199254740994n, "TIMECODE_VALUE", 0, ctx(2)))
    assert.equal(f.deliveries.length, 2)
    assert.equal(f.errors.filter(({ code }) => code === "timecode_replay").length, 3)
    await f.bridge.close()
})

test("seven ordinary leases saturate while accepted caller-drop work stays owned; reserved cleanup runs independently", async () => {
    const accepted = deferred(),
        cleanup = deferred(),
        trace = []
    const f = fixture({
        command: async (message) => {
            trace.push(message.data)
            await accepted.promise
        },
        retire: async () => {
            trace.push("retire")
            await cleanup.promise
        },
        drainTimeoutMs: 10
    })
    for (let i = 0; i < 8; i++) f.bridge.handle({ channel: "TIMECODE_VALUE", data: i })
    await until(() => trace.length === 1)
    assert.equal(f.bridge.status().accepted, 7)
    assert.equal(f.errors.at(-1).code, "timecode_busy")
    const close = f.bridge.close()
    await until(() => trace.includes("retire"))
    assert.equal(f.bridge.status().queued, 0)
    assert.equal(f.bridge.status().accepted, 2)
    await assert.rejects(close, { code: "timecode_drain_timeout" })
    assert.equal(f.bridge.status().active, true)
    const retry = f.bridge.close()
    accepted.resolve()
    cleanup.resolve()
    await retry
    assert.deepEqual(trace, [0, "retire"])
    assert.equal(f.bridge.status().accepted, 0)
    assert.equal(f.errors.filter(({ code }) => code === "timecode_context_closed").length, 6)
})

test("independent cleanup failure retries same context and close waiter cannot multiply", async () => {
    const wait = deferred()
    let attempts = 0
    const f = fixture({
        retire: async () => {
            attempts++
            if (attempts === 1) throw new Error("real owner still open")
            await wait.promise
        }
    })
    f.setContext(ctx(2))
    await assert.rejects(f.bridge.contextChanged(), { code: "timecode_cleanup" })
    f.bridge.handle({ channel: "TIMECODE_STOP" })
    assert.equal(f.errors.at(-1).code, "timecode_closed")
    const retry = f.bridge.contextChanged()
    await assert.rejects(f.bridge.contextChanged(), { code: "timecode_control_busy" })
    assert.equal(attempts, 2)
    wait.resolve()
    await retry
    f.bridge.handle({ channel: "TIMECODE_STOP" })
    await tick()
    assert.deepEqual(f.commands[0].context, ctx(2))
    await f.bridge.close()
})

test("blocked private completion remains bounded through drain timeout and rejects overlap/reentrant publications", async () => {
    const wait = deferred(),
        f = fixture({ complete: () => wait.promise, drainTimeoutMs: 10 })
    f.bridge.publish(publication(1))
    f.bridge.publish(publication(2))
    assert.equal(f.deliveries.length, 1)
    assert.equal(f.errors.at(-1).code, "timecode_completion_busy")
    await assert.rejects(f.bridge.close(), { code: "timecode_drain_timeout" })
    assert.equal(f.bridge.status().completion, true)
    const retry = f.bridge.close()
    wait.resolve()
    await retry
    assert.equal(f.bridge.status().completion, false)
    let bridge
    const reentrant = fixture({ deliver: () => bridge.publish(publication(2)) })
    bridge = reentrant.bridge
    bridge.publish(publication(1))
    await tick()
    assert.equal(reentrant.errors.at(-1).code, "timecode_completion_busy")
    assert.equal(reentrant.completions.length, 1)
    await bridge.close()
})

test("real delivery exceptions complete false; completion transport failures stay sticky after terminal cleanup", async () => {
    const f = fixture({
        deliver: () => {
            throw new Error("retained handler threw")
        }
    })
    f.bridge.publish(publication(1))
    await tick()
    assert.equal(f.completions[0].accepted, false)
    assert.equal(f.errors[0].code, "timecode_transport")
    await f.bridge.close()
    const failed = fixture({
        complete: async () => {
            throw new Error("native completion transport failed")
        }
    })
    failed.bridge.publish(publication(1))
    await until(() => failed.bridge.status().terminal)
    await assert.rejects(failed.bridge.close(), { code: "timecode_completion" })
    await assert.rejects(failed.bridge.close(), { code: "timecode_completion" })
    assert.equal(failed.retirements.length, 1)
})

test("malformed publications are bounded hard rejections and valid private leases receive false", async () => {
    const f = fixture()
    const variants = [
        (e) => {
            e.hydration.undefinedPaths = [""]
        },
        (e) => {
            e.hydration.numbers = [{ path: "/unexpected", number: { kind: "NaN" } }]
        },
        (e) => {
            e.event.context.dataPath = "foreign"
        },
        (e) => {
            e.event.listenerId = "source-success-ack"
        },
        (e) => {
            e.event.message.channel = "UNKNOWN"
        },
        (e) => {
            e.bytes = [0]
        },
        (e) => {
            e.event.message.channel = "TIMECODE_AUDIO_DATA"
            e.event.message.data = null
            e.bytes = [0, -1]
        },
        (e) => {
            e.event.message.channel = "TIMECODE_AUDIO_DATA"
            e.event.message.data = null
            e.bytes = new Uint8Array(65537)
        },
        (e) => {
            e.event.message.channel = "ALERT"
            e.event.message.data = "a".repeat(65537)
        }
    ]
    for (const [index, mutate] of variants.entries()) {
        const event = publication(index + 1)
        mutate(event)
        f.bridge.publish(event)
        await tick()
    }
    assert.equal(f.deliveries.length, 0)
    assert.equal(f.completions.length, variants.length)
    assert.ok(f.completions.every(({ accepted }) => !accepted))
    for (const sequence of ["0", "01", "-1", "18446744073709551616", "9".repeat(10000)]) {
        const event = publication(20)
        event.delivery.sequence = sequence
        f.bridge.publish(event)
    }
    assert.equal(f.completions.length, variants.length)
    await f.bridge.close()
})

test("path, metadata, raw storage, accessor, depth and source shape limits do not invoke native sinks", async () => {
    const f = fixture()
    let accessed = 0
    const accessor = {
        channel: "TIMECODE_STATUS",
        get data() {
            accessed++
            return 1
        }
    }
    const cycle = {}
    cycle.self = cycle
    const deep = {}
    let parent = deep
    for (let n = 0; n < 10; n++) parent = parent.next = {}
    for (const message of [accessor, { channel: "TIMECODE_STATUS", data: cycle }, { channel: "TIMECODE_STATUS", data: deep }, { channel: "TIMECODE_STATUS", data: "🙌".repeat(20000) }, { channel: "TIMECODE_START", data: null }, { channel: "TIMECODE_VALUE", data: "1" }, { channel: "TIMECODE_AUDIO_DATA", data: { mode: "LTC", buffer: new Uint8Array(65537) } }, { channel: "TIMECODE_AUDIO_DATA", data: { mode: "LTC", buffer: new Uint8Array(new SharedArrayBuffer(2)) } }])
        assert.equal(f.bridge.handle(message), true)
    assert.equal(f.bridge.handle({ channel: "OTHER" }), false)
    assert.equal(accessed, 0)
    assert.equal(f.commands.length + f.audio.length, 0)
    assert.equal(f.errors.length, 8)
    f.setContext({ ...ctx(), dataPath: "🙌".repeat(10000) })
    f.bridge.handle({ channel: "TIMECODE_STOP" })
    assert.equal(f.errors.at(-1).code, "timecode_context")
    await f.bridge.close()
})

test("queued stale contexts are canceled before native entry and path/runtime identity changes require retirement", async () => {
    const gate = deferred(),
        calls = []
    const f = fixture({
        command: async (message, context) => {
            calls.push({ message, context })
            if (calls.length === 1) await gate.promise
        }
    })
    f.bridge.handle({ channel: "TIMECODE_VALUE", data: 1 })
    f.bridge.handle({ channel: "TIMECODE_VALUE", data: 2 })
    await until(() => calls.length === 1)
    f.setContext({ ...ctx(), dataPath: "/same-generation-different-root" })
    gate.resolve()
    await until(() => !f.bridge.status().active)
    assert.equal(calls.length, 1)
    assert.equal(f.errors.at(-1).code, "timecode_stale_context")
    f.bridge.handle({ channel: "TIMECODE_STOP" })
    assert.equal(f.errors.at(-1).code, "timecode_stale_context")
    await f.bridge.contextChanged()
    f.bridge.handle({ channel: "TIMECODE_STOP" })
    await until(() => calls.length === 2)
    assert.equal(calls[1].context.dataPath, "/same-generation-different-root")
    f.setContext({ ...ctx(), runtimeId: "different-process" })
    f.bridge.handle({ channel: "TIMECODE_STOP" })
    assert.equal(f.errors.at(-1).code, "timecode_stale_context")
    await f.bridge.close()
})

test("terminal close upgrades an entered context drain and late publication cannot start new accepted work", async () => {
    const gate = deferred(),
        stages = []
    const f = fixture({
        retire: async (context, terminal) => {
            stages.push({ context, terminal })
            if (stages.length === 1) await gate.promise
        }
    })
    f.setContext(ctx(2))
    const changed = f.bridge.contextChanged()
    await until(() => stages.length === 1)
    await assert.rejects(f.bridge.close(), { code: "timecode_control_busy" })
    f.bridge.publish(publication(1))
    assert.equal(f.completions.length, 0)
    gate.resolve()
    await changed
    assert.deepEqual(stages, [
        { context: ctx(), terminal: false },
        { context: ctx(), terminal: true }
    ])
    await f.bridge.close()
    f.bridge.publish(publication(2))
    assert.equal(f.completions.length, 0)
    assert.equal(f.deliveries.length, 0)
    assert.equal(stages.length, 2)
})

test("actual source malformed audio errors are explicit while non-LTC payloads keep source no-op", async () => {
    const source = load("src/electron/timecode/timecode.ts", {
        feedLTCFrame() {
            assert.fail("no valid source LTC buffer in this corpus")
        }
    })
    const f = fixture()
    for (const data of [null, undefined, { mode: "LTC" }, { mode: "LTC", buffer: null }]) {
        assert.throws(() => source.processAudioData(data))
        assert.equal(f.bridge.handle({ channel: "TIMECODE_AUDIO_DATA", data }), true)
    }
    for (const data of [0, "raw", false, new Uint8Array([1, 2]), { mode: "MTC", buffer: null }, { unknown: "field" }]) {
        assert.equal(source.processAudioData(data), undefined)
        f.bridge.handle({ channel: "TIMECODE_AUDIO_DATA", data })
    }
    assert.equal(f.errors.length, 4)
    assert.equal(f.commands.length + f.audio.length, 0)
    await f.bridge.close()
})
