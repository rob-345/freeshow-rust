import assert from "node:assert/strict"
import { test } from "node:test"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { createHash } from "node:crypto"
import ts from "typescript"
import { root } from "./oracle.mjs"

const compile = (source) => ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText
const { createAudioPortBridge } = await import(`data:text/javascript;base64,${Buffer.from(compile(fs.readFileSync(path.join(root, "src/frontend/rust/audioPortBridge.ts"), "utf8"))).toString("base64")}`)
const hashes = {
    "src/electron/preload.ts": "4908efef01e11bb9e8899233a2273a965538a00b969729b862b93aad890edb47",
    "src/electron/audio/receiveAudio.ts": "0b79baab29495da7acb5b4ffe32f064b98032781c3b30d4ccd5161831a44067c",
    "public/assets/pcm-worklet.js": "8317a6217e2b248f0b385c8f0d65393472b272fb6fe0d46037dc9bb62786a24a"
}
function immutable(file) {
    const source = fs.readFileSync(path.join(root, file), "utf8")
    assert.equal(createHash("sha256").update(source).digest("hex"), hashes[file], `${file}: actual immutable 2d94def source`)
    return source
}
const sourcePreload = immutable("src/electron/preload.ts")
const sourceReceiver = immutable("src/electron/audio/receiveAudio.ts")
const sourceWorklet = immutable("public/assets/pcm-worklet.js")
function load(source, filename, injected) {
    const ast = ts.createSourceFile(filename, source, ts.ScriptTarget.Latest, true)
    const declarations = ast.statements
        .filter((node) => !ts.isImportDeclaration(node))
        .map((node) => node.getText(ast))
        .join("\n")
    const exports = {}
    const script = ts.transpileModule(declarations, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
    new vm.Script(script, { filename }).runInContext(vm.createContext({ exports, console: { error() {}, info() {} }, ...injected }), { timeout: 1000 })
    return exports
}
const ctx = (n = 1) => ({ runtimeId: "native-audio", libraryGeneration: n, dataPath: `/private/音楽-${n}` })
const tick = () => new Promise((resolve) => setImmediate(resolve))
async function until(test) {
    const end = Date.now() + 3000
    while (!test()) {
        assert.ok(Date.now() < end, "bounded actual event/effect wait")
        await new Promise((resolve) => setTimeout(resolve, 2))
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
function page(t) {
    const { port1, port2 } = new MessageChannel()
    const events = []
    port2.onmessage = (event) => events.push(event)
    t.after(() => {
        for (const event of events) for (const port of event.ports) port.close()
        port1.close()
        port2.close()
    })
    return {
        post(message, origin, ports) {
            assert.equal(origin, "*")
            port1.postMessage(message, ports)
        },
        async next() {
            await until(() => events.length > 0)
            return events.shift()
        }
    }
}
function oracle(t) {
    const browser = page(t),
        frames = [],
        trace = [],
        rawPorts = []
    class MainPort {
        constructor(raw) {
            this.raw = raw
        }
        on(type, callback) {
            this.raw.on(type, type === "message" ? (data) => callback({ data }) : callback)
        }
        start() {
            this.raw.start()
        }
        close() {
            this.raw.close()
        }
    }
    class MessageChannelMain {
        constructor() {
            const channel = new MessageChannel()
            rawPorts.push(channel.port1, channel.port2)
            this.port1 = new MainPort(channel.port1)
            this.port2 = channel.port2
        }
    }
    const listeners = new Map()
    load(sourcePreload, "actual-preload.ts", {
        process: { env: { NODE_ENV: "production" } },
        setTimeout,
        contextBridge: { exposeInMainWorld() {} },
        webUtils: {},
        ipcRenderer: { on: (name, callback) => listeners.set(name, callback) },
        window: { postMessage: browser.post }
    })
    const { receiveAudio } = load(sourceReceiver, "actual-receiveAudio.ts", {
        Buffer,
        ArrayBuffer,
        Uint8Array,
        MessageChannelMain,
        IcecastSender: { disconnect: () => trace.push("disconnect") },
        processAudio: (buffer, sampleRate, id, icecast) => frames.push({ bytes: [...buffer], sampleRate, id, icecast })
    })
    const sender = { isDestroyed: () => false, postMessage: (name, data, ports) => listeners.get(name)({ ports }, data) }
    t.after(() => rawPorts.forEach((port) => port.close()))
    return { handle: (message) => receiveAudio({ sender }, message), frames, trace, page: browser }
}
function adapter(t, overrides = {}) {
    const browser = page(t),
        frames = [],
        errors = [],
        trace = []
    let current = ctx()
    const bridge = createAudioPortBridge({
        getContext: () => current,
        postPort: browser.post,
        transport: async (bytes, metadata, context, origin) => {
            frames.push({ bytes, metadata, context, origin })
            trace.push("packet")
        },
        disconnect: async () => trace.push("disconnect"),
        closeContext: async () => trace.push("closeContext"),
        report: (error) => errors.push(error),
        ...overrides
    })
    t.after(async () => {
        // Cases holding actual effects release their own gates before this drain.
        await bridge.close().catch(() => {})
    })
    return {
        bridge,
        frames,
        errors,
        trace,
        page: browser,
        switch: (value) => {
            current = value
        }
    }
}
async function opened(owner, id, missing = false) {
    owner.handle({ channel: "INIT_PORT", data: missing ? {} : { id } })
    const event = await owner.page.next()
    assert.deepEqual(event.data, { type: "AUDIO_PORT_RESPONSE", targetId: missing ? undefined : id })
    assert.equal(event.ports.length, 1)
    return event.ports[0]
}

test("actual preload response and real transferred ports retain raw target IDs and source target-key replacement", async (t) => {
    const source = oracle(t),
        native = adapter(t)
    const actual = { handle: native.bridge.handle, page: native.page }
    for (const message of [{ channel: "INIT_PORT" }, { channel: "INIT_PORT", data: null }, { channel: "INIT_PORT", data: false }, { channel: "INIT_PORT", data: "primitive" }]) {
        source.handle(message)
        native.bridge.handle(message)
        const a = await source.page.next(),
            b = await native.page.next()
        assert.deepEqual(b.data, a.data)
        assert.deepEqual(Object.keys(b.data), ["type", "targetId"])
        assert.equal(b.data.targetId, undefined)
        a.ports[0].close()
        b.ports[0].close()
    }
    for (const id of [undefined, null, false, 0, "", "日本語", [1, "a"], { future: true }]) {
        const a = await opened(source, id, id === undefined)
        const b = await opened(actual, id, id === undefined)
        const payload = { buffer: new Uint8Array([1, 2, 3]), sampleRate: "44100", id: 9, icecast: null }
        a.postMessage({ channel: "AUDIO", payload })
        b.postMessage({ channel: "AUDIO", payload })
        const n = native.frames.length
        await until(() => source.frames.length > n && native.frames.length > n)
        assert.deepEqual([...native.frames.at(-1).bytes], source.frames.at(-1).bytes)
        assert.deepEqual(native.frames.at(-1).metadata, { sampleRate: "44100", id: 9, icecast: null })
        a.close()
        b.close()
    }
    assert.deepEqual(native.errors, [])
})

test("raw PCM/CAPTURE snapshots match actual source bytes for views, offsets, shared views and serialized Buffers", async (t) => {
    const source = oracle(t),
        native = adapter(t)
    const storage = Uint8Array.from({ length: 32 }, (_, n) => n + 1)
    const samples = [storage, storage.buffer, storage.subarray(5, 11), new Int16Array(storage.buffer, 4, 5), new Float32Array(storage.buffer, 8, 3), new DataView(storage.buffer, 3, 7), Buffer.from(storage.buffer, 2, 4), { type: "Buffer", data: [256, -1, 3.7, "8", null, true, [9], {}, NaN, Infinity, undefined] }]
    if (typeof SharedArrayBuffer !== "undefined") samples.push(new Uint8Array(new SharedArrayBuffer(4), 1, 2))
    for (const [i, buffer] of samples.entries()) {
        const message = { channel: i % 2 ? "PCM" : "CAPTURE", data: { buffer, id: i, sampleRate: null, icecast: { enabled: false, password: "secret" } } }
        source.handle(message)
        const expected = source.frames.at(-1).bytes
        native.bridge.handle(message)
        await until(() => native.frames.length === i + 1)
        assert.deepEqual([...native.frames[i].bytes], expected)
        assert.deepEqual(native.frames[i].context, ctx())
        assert.equal(native.frames[i].origin, message.channel)
    }
    const mutable = new Uint8Array([7, 8, 9])
    const config = { password: "before" },
        id = ["original"]
    native.bridge.handle({ channel: "PCM", data: { buffer: mutable, id, icecast: config } })
    mutable.fill(99)
    config.password = "after"
    id.push("mutated")
    await until(() => native.frames.length === samples.length + 1)
    assert.deepEqual([...native.frames.at(-1).bytes], [7, 8, 9])
    assert.deepEqual(native.frames.at(-1).metadata, { id: ["original"], icecast: { password: "before" } })
    // Reachable source quirk: if data itself is a view, data.buffer selects its
    // entire backing allocation; a normal {buffer:view} selects the view only.
    const directView = storage.subarray(3, 5)
    source.handle({ channel: "PCM", data: directView })
    native.bridge.handle({ channel: "PCM", data: directView })
    await until(() => native.frames.length === samples.length + 2)
    assert.deepEqual([...native.frames.at(-1).bytes], source.frames.at(-1).bytes)
    assert.equal(native.frames.at(-1).bytes.length, storage.byteLength)
    assert.deepEqual(native.errors, [])
})

test("absent/null/false metadata remain distinct and source coercion/latest-config stays outside the adapter", async (t) => {
    const source = oracle(t),
        native = adapter(t)
    const configs = [{}, { sampleRate: undefined, id: undefined, icecast: undefined }, { sampleRate: null, id: null, icecast: null }, { sampleRate: "0", id: false, icecast: false }, { sampleRate: ["44100"], id: { a: 1 }, icecast: { enabled: true } }, { sampleRate: -0, id: "icecast", icecast: null }]
    for (const [n, fields] of configs.entries()) {
        native.bridge.handle({ channel: "PCM", data: { buffer: new Uint8Array([n]), ...fields } })
        await until(() => native.frames.length === n + 1)
        const frame = native.frames[n]
        assert.deepEqual(frame.metadata, fields)
        // Apply the ACTUAL source to the forwarded raw metadata, rather than
        // copying Number()/ID/latest-Icecast algorithms into the adapter test.
        source.handle({ channel: "PCM", data: { buffer: frame.bytes, ...frame.metadata } })
    }
    assert.equal(source.frames[2].sampleRate, 48000)
    assert.equal(source.frames[4].sampleRate, 44100)
    assert.deepEqual(source.frames[5].icecast, { enabled: true })
    assert.equal(native.frames[5].metadata.icecast, null, "adapter must not latch source's latest configuration")
    assert.deepEqual(native.errors, [])
})

test("actual retained worklet transfers the returned port and emits exact planar stereo and mono PCM", async (t) => {
    const source = oracle(t),
        native = adapter(t)
    async function run(owner) {
        const pagePort = await opened(owner, "icecast")
        const control = new MessageChannel()
        let Processor
        new vm.Script(sourceWorklet).runInContext(
            vm.createContext({
                AudioWorkletProcessor: class {
                    constructor() {
                        this.port = control.port1
                    }
                },
                registerProcessor: (name, ctor) => {
                    assert.equal(name, "pcm-sender-processor")
                    Processor = ctor
                },
                Float32Array,
                Uint8Array
            }),
            { timeout: 1000 }
        )
        const processor = new Processor()
        t.after(() => {
            control.port1.close()
            control.port2.close()
        })
        control.port2.postMessage({ type: "INIT_PORT", targetId: "icecast", sampleRate: 8000, icecastConfig: { enabled: true } }, [pagePort])
        await until(() => processor.mainPort !== null)
        assert.equal(processor.frameSize, 160)
        const left = Float32Array.from({ length: 160 }, (_, i) => i / 160)
        const right = Float32Array.from(left, (x) => -x)
        assert.equal(processor.process([[left, right]]), true)
        assert.equal(processor.process([[left]]), true)
        control.port2.postMessage({ type: "DESTROY" })
        await until(() => processor.isDestroyed)
        assert.equal(processor.process([]), false)
    }
    await run(source)
    await run({ handle: native.bridge.handle, page: native.page })
    await until(() => source.frames.length === 2 && native.frames.length === 2)
    for (let i = 0; i < 2; i++) assert.deepEqual([...native.frames[i].bytes], source.frames[i].bytes)
    assert.equal(native.frames[0].metadata.sampleRate, 8000)
    assert.equal(native.frames[0].origin, "port")
    assert.deepEqual(native.errors, [])
})

test("source icecast replacement and CLOSE own disconnect effects, including absent-port and close-event duplicates", async (t) => {
    const source = oracle(t),
        native = adapter(t)
    const actual = { handle: native.bridge.handle, page: native.page }
    await opened(source, "icecast")
    await opened(actual, "icecast")
    await opened(source, "icecast")
    await opened(actual, "icecast")
    await until(() => source.trace.length === 1 && native.trace.length === 1)
    source.handle({ channel: "CLOSE_PORT", data: { id: "icecast" } })
    native.bridge.handle({ channel: "CLOSE_PORT", data: { id: "icecast" } })
    await until(() => source.trace.length === 3 && native.trace.length === 3)
    source.handle({ channel: "CLOSE_PORT", data: { id: "icecast" } })
    native.bridge.handle({ channel: "CLOSE_PORT", data: { id: "icecast" } })
    await until(() => source.trace.length === 4 && native.trace.length === 4)
    assert.deepEqual(native.trace, source.trace)
    assert.equal(native.bridge.status().ports, 0)
})

test("port bounds, ignored source packets and explicit malformed/oversize errors use no fake ACK", async (t) => {
    const source = oracle(t),
        native = adapter(t)
    const ignored = [undefined, null, false, 0, [], {}, new Uint8Array(), new ArrayBuffer(0), { type: "Buffer", data: "wrong" }]
    for (const buffer of ignored) {
        source.handle({ channel: "PCM", data: { buffer } })
        assert.equal(native.bridge.handle({ channel: "PCM", data: { buffer } }), undefined)
    }
    assert.deepEqual(native.frames, source.frames)
    const sourcePort = await opened(source, "protocol"),
        nativePort = await opened({ handle: native.bridge.handle, page: native.page }, "protocol")
    for (const message of [undefined, null, false, 0, "primitive", [], {}, { channel: "AUDIO" }, { channel: "AUDIO", payload: null }, { channel: "AUDIO", payload: false }, { channel: "AUDIO", payload: { buffer: [] } }, { channel: "OTHER", payload: { buffer: new Uint8Array([99]) } }]) {
        sourcePort.postMessage(message)
        nativePort.postMessage(message)
    }
    sourcePort.postMessage({ channel: "AUDIO", payload: { buffer: new Uint8Array([42]) } })
    nativePort.postMessage({ channel: "AUDIO", payload: { buffer: new Uint8Array([42]) } })
    await until(() => source.frames.length === 1 && native.frames.length === 1)
    assert.deepEqual([...native.frames[0].bytes], source.frames[0].bytes)
    assert.deepEqual(native.errors, [])
    native.bridge.handle({ channel: "CLOSE_PORT", data: { id: "protocol" } })
    native.frames.length = 0
    for (let i = 0; i < 16; i++) await opened({ handle: native.bridge.handle, page: native.page }, `port-${i}`)
    native.bridge.handle({ channel: "INIT_PORT", data: { id: "seventeenth" } })
    assert.equal(native.bridge.status().ports, 16)
    native.bridge.handle({ channel: "PCM", data: { buffer: new Uint8Array(32769) } })
    const detached = new ArrayBuffer(4)
    structuredClone(detached, { transfer: [detached] })
    native.bridge.handle({ channel: "PCM", data: { buffer: detached } })
    for (const value of [
        Infinity,
        NaN,
        1n,
        () => {},
        Symbol("id"),
        {
            toJSON() {
                throw Error("must not run")
            }
        }
    ])
        native.bridge.handle({ channel: "PCM", data: { buffer: new Uint8Array([1]), id: value } })
    const cycle = {}
    cycle.self = cycle
    native.bridge.handle({ channel: "PCM", data: { buffer: new Uint8Array([1]), icecast: cycle } })
    native.bridge.handle({ channel: "PCM", data: { buffer: new Uint8Array([1]), icecast: { password: "x".repeat(8193) } } })
    native.bridge.handle({ channel: "RESET_DECODER", data: cycle })
    native.bridge.handle({ channel: "unknown" })
    assert.ok(native.errors.some((e) => e.code === "audio_port_limit"))
    assert.ok(native.errors.some((e) => e.code === "audio_packet_limit"))
    assert.ok(native.errors.some((e) => e.code === "audio_packet_invalid"))
    assert.ok(native.errors.some((e) => e.code === "audio_metadata_invalid"))
    assert.ok(native.errors.some((e) => e.code === "audio_metadata_limit"))
    assert.equal(native.errors.at(-1).code, "audio_unknown_channel")
    assert.deepEqual(native.frames, [])
})

test("INIT/CLOSE retain previously accepted packets in FIFO order, and disconnect failure still attempts the independent close effect", async (t) => {
    const gate = deferred(),
        trace = []
    let calls = 0
    const native = adapter(t, {
        transport: async (bytes) => {
            trace.push(`packet-${bytes[0]}`)
            if (bytes[0] === 0) await gate.promise
        },
        disconnect: async () => {
            trace.push("disconnect")
            if (++calls === 2) throw Error("first CLOSE failed")
        }
    })
    t.after(() => gate.resolve())
    await opened({ handle: native.bridge.handle, page: native.page }, "icecast")
    native.bridge.handle({ channel: "PCM", data: { buffer: new Uint8Array([0]) } })
    await until(() => trace.length === 1)
    native.bridge.handle({ channel: "PCM", data: { buffer: new Uint8Array([1]) } })
    const replacement = await opened({ handle: native.bridge.handle, page: native.page }, "icecast")
    replacement.postMessage({ channel: "AUDIO", payload: { buffer: new Uint8Array([2]) } })
    await until(() => native.bridge.status().accepted === 4)
    native.bridge.handle({ channel: "CLOSE_PORT", data: { id: "icecast" } })
    gate.resolve()
    await until(() => native.bridge.status().accepted === 0)
    assert.deepEqual(trace, ["packet-0", "packet-1", "disconnect", "packet-2", "disconnect", "disconnect"])
    assert.equal(native.errors.at(-1).code, "audio_disconnect")
})

test("one FIFO pump bounds seven ordinary leases, owns caller-ignored work, and reserved cleanup bypasses a held transport", async (t) => {
    const packet = deferred(),
        cleanup = deferred(),
        calls = [],
        closes = []
    const native = adapter(t, {
        drainTimeoutMs: 20,
        transport: async (bytes, metadata, context) => {
            calls.push({ bytes, metadata, context })
            await packet.promise
        },
        closeContext: async (context) => {
            closes.push(context)
            await cleanup.promise
        }
    })
    t.after(() => {
        packet.resolve()
        cleanup.resolve()
    })
    for (let n = 0; n < 8; n++) native.bridge.handle({ channel: "PCM", data: { buffer: new Uint8Array([n]), id: n } })
    await until(() => calls.length === 1)
    assert.equal(native.bridge.status().accepted, 7)
    assert.equal(native.errors[0].code, "audio_busy")
    const abandoned = native.bridge.close().catch((error) => error.code)
    const extra = native.bridge.close().catch((error) => error.code)
    assert.equal(await extra, "audio_control_busy")
    await until(() => closes.length === 1)
    assert.equal(await abandoned, "audio_drain_timeout")
    assert.equal(native.bridge.status().accepted, 2, "one actual packet plus the reserved actual cleanup")
    assert.equal(native.bridge.status().cleanupRunning, true)
    cleanup.resolve()
    await tick()
    assert.equal(native.bridge.status().cleanupRunning, true, "cleanup receipt alone does not discard the accepted packet")
    packet.resolve()
    await native.bridge.close()
    assert.equal(closes.length, 1, "retry waits the same actual cleanup, without a new operation")
    assert.equal(calls.length, 1, "queued old-context packets were explicitly canceled")
    assert.equal(native.bridge.status().accepted, 0)
    assert.deepEqual(calls[0].context, ctx())
})

test("context retirement retains actual cleanup across timeout, rejects stale ports and permits only the new full context", async (t) => {
    const cleanup = deferred(),
        closes = []
    const native = adapter(t, {
        drainTimeoutMs: 20,
        closeContext: async (context) => {
            closes.push(context)
            await cleanup.promise
        }
    })
    t.after(() => cleanup.resolve())
    const old = await opened({ handle: native.bridge.handle, page: native.page }, "output")
    native.switch(ctx(2))
    old.postMessage({ channel: "AUDIO", payload: { buffer: new Uint8Array([99]) } })
    await until(() => native.errors.some((e) => e.code === "audio_stale_context"))
    await assert.rejects(native.bridge.contextChanged(), { code: "audio_drain_timeout" })
    assert.deepEqual(closes, [ctx()])
    native.bridge.handle({ channel: "PCM", data: { buffer: new Uint8Array([88]) } })
    assert.equal(native.errors.at(-1).code, "audio_closed")
    cleanup.resolve()
    await native.bridge.contextChanged()
    native.bridge.handle({ channel: "CAPTURE", data: { buffer: new Uint8Array([7]) } })
    await until(() => native.frames.length === 1)
    assert.deepEqual(native.frames[0].context, ctx(2))
    assert.equal(native.bridge.status().ports, 0)
})

test("actual cleanup failure blocks new effects until a real retry; transport/report/transfer failures remain controlled", async (t) => {
    let attempts = 0,
        writes = 0
    const packet = deferred()
    const native = adapter(t, {
        closeContext: async () => {
            if (++attempts === 1) throw null
        },
        transport: async () => {
            if (++writes === 1) return packet.promise
            throw new Error("native unavailable")
        }
    })
    t.after(() => packet.resolve())
    native.bridge.handle({ channel: "PCM", data: { buffer: new Uint8Array([0]) } })
    await until(() => writes === 1)
    native.switch(ctx(2))
    const retirement = native.bridge.contextChanged()
    await until(() => native.bridge.status().cleanupError === "audio_cleanup")
    assert.equal(native.bridge.status().accepted, 2, "failed cleanup must still drain the actual old packet")
    packet.resolve()
    await assert.rejects(retirement, { code: "audio_cleanup" })
    assert.equal(native.bridge.status().retiring, true)
    native.bridge.handle({ channel: "PCM", data: { buffer: new Uint8Array([1]) } })
    assert.equal(native.errors.at(-1).code, "audio_closed")
    await native.bridge.contextChanged()
    native.bridge.handle({ channel: "PCM", data: { buffer: new Uint8Array([1]) } })
    await until(() => native.errors.some((e) => e.code === "audio_transport"))
    assert.equal(native.bridge.status().accepted, 0)
    const broken = adapter(t, {
        postPort: () => {
            throw new Error("transfer denied")
        },
        report: () => {
            throw Error("diagnostic sink")
        }
    })
    assert.equal(broken.bridge.handle({ channel: "INIT_PORT", data: { id: "output" } }), undefined)
    assert.equal(broken.bridge.status().ports, 0)
})
