import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import fs from "node:fs"
import path from "node:path"
import { test } from "node:test"
import vm from "node:vm"
import { get, writable } from "svelte/store"
import ts from "typescript"
import { root } from "./oracle.mjs"

const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const analyserFile = "src/frontend/audio/audioAnalyser.ts"
const originals = new Map()
function original(file) {
    if (!originals.has(file)) {
        const result = spawnSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 })
        assert.equal(result.status, 0, result.stderr)
        originals.set(file, result.stdout.replace(/\r\n/g, "\n"))
    }
    return originals.get(file)
}
assert.equal(createHash("sha256").update(original(analyserFile)).digest("hex"), "ff000b9ddcd06eb88069a210abe09fb293e75a34e116bc702de0045768146eff")
const clean = (value) => JSON.parse(JSON.stringify(value))
const modules = ["src/frontend/audio/dBUtils.ts", "src/frontend/audio/audioMultichannel.ts", "src/frontend/audio/routing/audioInputCapture.ts", "src/frontend/audio/audioSidechain.ts", "src/frontend/audio/routing/audioRoutingManager.ts", "src/frontend/audio/audioSender.ts", analyserFile]
function body(source, file) {
    const ast = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true)
    return ast.statements
        .filter((node) => !ts.isImportDeclaration(node))
        .map((node) => node.getText(ast))
        .join("\n")
}
function declaration(file, name, source = original(file)) {
    const ast = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true)
    return ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === name).getText(ast)
}
const meterFile = "src/frontend/components/drawer/audio/AudioMeter.svelte"
function meterPoll(source) {
    return declaration("meter.ts", "getChannelDbs", source.match(/<script[^>]*>([\s\S]*?)<\/script>/)[1])
}

// Execute the actual routing, sender, analyser and meter functions. Only Web
// Audio/DOM/processor and application stores are controlled boundaries: no fake
// physical device, browser playback, SoundTouch, Opus or delivery claim.
function load(optimized, options = {}) {
    const trace = [],
        contexts = [],
        scheduled = [],
        sent = [],
        warnings = []
    let nextNode = 0,
        now = 100,
        amplitude = 0
    const store = {
        currentWindow: writable(options.role ?? null),
        disabledServers: writable({ output_stream: true }),
        media: writable({}),
        playingAudio: writable({}),
        playingVideos: writable([]),
        serverData: writable({}),
        special: writable({ icecast: { enabled: false } }),
        outputs: writable({}),
        audioEffects: writable({}),
        audioChannelsData: writable({}),
        channelSidechainMultipliers: writable({}),
        audioRouting: writable({
            channels: [{ id: "main" }],
            connections: [
                { from: "drawer_audio", to: "main" },
                { from: "mic_default", to: "main" },
                { from: "main", to: "speaker_default" }
            ]
        })
    }
    const log = (...args) => trace.push(clean(args))
    class Param {
        value = 1
        constructor(id) {
            this.id = id
        }
        setValueAtTime(value, time) {
            this.value = value
            log("set", this.id, value, time)
        }
        cancelScheduledValues(time) {
            log("cancel", this.id, time)
        }
        linearRampToValueAtTime(value, time) {
            this.value = value
            log("ramp", this.id, value, time)
        }
    }
    class Node {
        channelCount = 2
        numberOfOutputs = 1
        fftSize = 256
        frequencyBinCount = 128
        constructor(context, kind, detail) {
            this.context = context
            this.id = `${kind}:${++nextNode}`
            this.gain = new Param(this.id)
            this.delayTime = new Param(this.id)
            log("node", this.id, detail ?? null)
        }
        connect(target, ...ports) {
            log("connect", this.id, target?.id, ports)
            return target
        }
        disconnect(target) {
            log("disconnect", this.id, target?.id ?? "all")
        }
        getFloatTimeDomainData(bytes) {
            bytes.fill(amplitude)
            log("samples", this.id, bytes.length, amplitude)
        }
    }
    class Context {
        state = options.running ? "running" : "suspended"
        sampleRate = 48000
        currentTime = 12.5
        constructor(config) {
            log("construct", config)
            if (options.failConstruct) throw new Error("controlled actual constructor boundary failure")
            this.destination = { id: "destination", maxChannelCount: 8 }
            if (options.worklet)
                this.audioWorklet = {
                    addModule: async (url) => {
                        log("module", url)
                        if (options.failWorklet) throw new Error("controlled worklet failure")
                    }
                }
            contexts.push(this)
        }
        resume() {
            log("resume")
            if (options.failResume) return Promise.reject(new Error("controlled resume failure"))
            this.state = "running"
            return Promise.resolve()
        }
        suspend() {
            log("suspend")
            this.state = "suspended"
            return Promise.resolve()
        }
        close() {
            log("close")
            this.state = "closed"
            return Promise.resolve()
        }
        createGain() {
            return new Node(this, "gain")
        }
        createAnalyser() {
            return new Node(this, "analyser")
        }
        createChannelSplitter(n) {
            return new Node(this, "splitter", n)
        }
        createChannelMerger(n) {
            return new Node(this, "merger", n)
        }
        createDelay(n) {
            return new Node(this, "delay", n)
        }
        createMediaElementSource(element) {
            if (options.failSource) throw new Error("controlled source failure")
            return new Node(this, "media", element.id)
        }
        createMediaStreamSource(stream) {
            return new Node(this, "stream", stream.id)
        }
        createMediaStreamDestination() {
            const node = new Node(this, "destination")
            node.stream = new Stream("destination")
            return node
        }
    }
    class Element {
        src = ""
        volume = 0.8
        constructor(id) {
            this.id = id
        }
    }
    class Stream {
        constructor(id, channels = 2) {
            this.id = id
            this.channels = channels
        }
        getAudioTracks() {
            return [{ getSettings: () => ({ channelCount: this.channels }), getCapabilities: () => ({ channelCount: { max: this.channels } }) }]
        }
    }
    class PitchShiftNode {
        constructor(context) {
            this.input = new Node(context, "processor-in")
            this.output = new Node(context, "processor-out")
        }
        configureMultichannel(channels) {
            log("processor-channels", channels)
        }
        dispose() {
            log("processor-dispose")
        }
        set pitch(value) {
            log("pitch", value)
        }
        set tempo(value) {
            log("tempo", value)
        }
    }
    class Worklet extends Node {
        constructor(context, name) {
            super(context, "worklet", name)
            this.port = { postMessage: (message) => log("worklet-message", message), close: () => log("worklet-close") }
        }
    }
    const frames = new Map(),
        timers = new Map(),
        listeners = new Map()
    let counter = 0
    const context = vm.createContext({
        get,
        ...store,
        AudioContext: Context,
        AudioNode: Node,
        AudioWorkletNode: Worklet,
        HTMLMediaElement: Element,
        MediaStream: Stream,
        Float32Array,
        Uint8Array,
        ArrayBuffer,
        performance: { now: () => now },
        console: { error: (...v) => warnings.push(v.map(String)), warn: (...v) => warnings.push(v.map(String)), info() {}, debug() {} },
        requestAnimationFrame(callback) {
            frames.set(++counter, callback)
            return counter
        },
        cancelAnimationFrame(id) {
            frames.delete(id)
        },
        setTimeout(callback, delay) {
            timers.set(++counter, callback)
            scheduled.push(delay ?? 0)
            return counter
        },
        clearTimeout(id) {
            timers.delete(id)
        },
        setInterval() {
            throw new Error("unexpected uncontrolled interval")
        },
        fetch() {
            throw new Error("No external fetch permitted")
        },
        window: {
            addEventListener: (name, callback) => {
                if (!listeners.has(name)) listeners.set(name, new Set())
                listeners.get(name).add(callback)
            },
            removeEventListener: (name, callback) => listeners.get(name)?.delete(callback)
        },
        AudioMicrophone: { channelCountCache: new Map() },
        AudioPlayer: { getPath: (id) => id, initCheckLoop: () => log("player-loop") },
        PitchShiftNode,
        AudioProcessor: { createNode: (ctx) => new PitchShiftNode(ctx), isRegistered: () => true, register: async () => true },
        getFirstOutput: () => Object.values(get(store.outputs))[0],
        getFirstActiveOutput: () => Object.entries(get(store.outputs)).map(([id, value]) => ({ id, ...value }))[0],
        keysToID: (values) => Object.entries(values).map(([id, value]) => ({ ...value, id })),
        send: (...args) => {
            sent.push(args)
            log("send", ...args)
        }
    })
    for (const name of ["AudioCompressor", "AudioDelay", "AudioEqualizer", "AudioFilter", "AudioLimiter", "AudioNoiseGate", "AudioReverb", "AudioStereoShaper"])
        context[name] = class {
            constructor() {
                throw new Error("Effect DSP is outside this context ownership corpus")
            }
        }
    const read = (file) => (optimized ? fs.readFileSync(path.join(root, file), "utf8") : original(file))
    let text = body(read("src/types/Channels.ts"), "src/types/Channels.ts") + "\n" + declaration("src/frontend/audio/routing/audioRoutingInit.ts", "deduplicateConnections", read("src/frontend/audio/routing/audioRoutingInit.ts")) + "\n"
    for (const file of modules) text += body(read(file), file) + "\n"
    text += `let channelId="main"; let $audioChannelsData={}; ${meterPoll(read(meterFile))}\nglobalThis.actual={AudioAnalyser,AudioSender,AudioInputCapture,AudioRoutingManager,meter:getChannelDbs};`
    const compiled = ts.transpileModule(text, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
    context.exports = {}
    new vm.Script(compiled, { filename: optimized ? "optimized-audio-context.ts" : "actual-original-audio-context.ts" }).runInContext(context, { timeout: 1000 })
    async function flush() {
        let quiet = 0
        for (let i = 0; i < 32; i++) {
            await Promise.resolve()
            const callbacks = [...frames.values(), ...timers.values()]
            frames.clear()
            timers.clear()
            if (!callbacks.length) {
                if (++quiet >= 8) {
                    if (!options.failSource && !options.failResume && !options.failWorklet) assert.deepEqual(warnings, [], "successful actual source paths must not swallow missing boundary methods")
                    return
                }
                continue
            }
            quiet = 0
            now += 40
            for (const callback of callbacks) callback(now)
        }
        throw new Error("bounded source scheduling did not quiesce")
    }
    return {
        ...context.actual,
        trace,
        contexts,
        sent,
        scheduled,
        store,
        warnings,
        frames,
        timers,
        listeners,
        flush,
        Element,
        Stream,
        sample: (value) => {
            amplitude = value
            now += 25
        },
        reset: () => {
            trace.length = 0
        },
        snapshot: () => clean(trace)
    }
}
const activeTrace = (trace) => trace.filter(([kind]) => kind !== "construct")

test("idle import, actual meter polling and source-only configuration do not construct an optimized context", async () => {
    const source = load(false),
        native = load(true)
    assert.equal(source.contexts.length, 1, "actual original eagerly constructs at module evaluation")
    assert.equal(native.contexts.length, 0, "intentional idle construction difference")
    for (const owner of [source, native]) {
        owner.AudioAnalyser.setSourceVolume("future", 0.35)
        owner.AudioAnalyser.rampSourceVolume("future", 0.45, 500)
        owner.AudioAnalyser.updateChannelCount(4)
        owner.AudioAnalyser.setPitch("future", -2)
        owner.AudioAnalyser.setTempo("future", 1.25)
        owner.AudioAnalyser.recorderActivate()
        for (let i = 0; i < 20; i++) {
            assert.deepEqual(clean(owner.meter()), [-60, -60])
            await owner.flush()
        }
        assert.equal(owner.AudioAnalyser.hasSource("future"), false)
        assert.deepEqual(clean(owner.AudioAnalyser.getActiveSourceIds()), [])
    }
    assert.equal(native.contexts.length, 0, "meter manager initialization and inactive sender cannot recreate eager context")
    assert.equal(native.AudioAnalyser.channels, source.AudioAnalyser.channels)
    await native.AudioAnalyser.attach("future", new native.Element("future"))
    await source.AudioAnalyser.attach("future", new source.Element("future"))
    assert.equal(native.contexts.length, 1)
    for (const owner of [source, native])
        assert.ok(
            owner.trace.some(([kind, id, value]) => kind === "set" && id.startsWith("gain:") && value === 0.45),
            "deferred volume config is applied at actual attachment"
        )
})

test("cold genuine first use preserves source context/resume/routing trace and singleton reuse", async () => {
    for (const running of [false, true]) {
        const source = load(false, { running }),
            native = load(true, { running })
        source.reset()
        native.reset()
        const a = source.AudioAnalyser.getAudioContext(),
            b = native.AudioAnalyser.getAudioContext()
        await source.flush()
        await native.flush()
        assert.deepEqual(activeTrace(native.snapshot()), source.snapshot())
        assert.equal(native.contexts.length, 1)
        assert.equal(native.AudioAnalyser.getAudioContext(), b)
        assert.equal(source.AudioAnalyser.getAudioContext(), a)
        assert.equal(native.AudioAnalyser.getOrCreateDestinationNode("icecast"), native.AudioAnalyser.getOrCreateDestinationNode("icecast"))
        assert.ok(native.contexts.every((c) => c === b))
        assert.equal(
            native.trace.some(([kind]) => kind === "close" || kind === "suspend"),
            false
        )
    }
})

test("actual local and multichannel source graphs, volume ramps, routing and live meter reads match original", async () => {
    for (const channels of [2, 4, 8]) {
        const source = load(false),
            native = load(true)
        source.reset()
        native.reset()
        for (const owner of [source, native]) {
            owner.store.playingAudio.set({ mic_sub_device: { isMic: true, paused: false } })
            await owner.AudioAnalyser.attach("mic_sub_device", new owner.Stream("microphone-boundary", channels))
            await owner.flush()
            owner.AudioAnalyser.setSourceVolume("mic_sub_device", 0.6)
            owner.AudioAnalyser.rampSourceVolume("mic_sub_device", 0.2, 250)
            owner.AudioAnalyser.setPitch("mic_sub_device", -3)
            owner.AudioAnalyser.setTempo("mic_sub_device", 0.9)
            owner.sample(0.5)
            const first = clean(owner.meter())
            owner.sample(0.25)
            const second = clean(owner.meter())
            assert.notDeepEqual(second, first, "real analyser samples change without any store event")
            owner.AudioAnalyser.detach("mic_sub_device")
            await owner.flush()
        }
        assert.deepEqual(activeTrace(native.snapshot()), source.snapshot())
        assert.equal(native.contexts.length, 1)
        assert.equal(
            native.trace.some(([kind]) => kind === "close" || kind === "suspend"),
            false
        )
    }
})

test("cold and initialized sender activation preserves actual worklet, reactivation and shared context traces", async () => {
    for (const warm of [false, true]) {
        const source = load(false, { worklet: true }),
            native = load(true, { worklet: true })
        for (const owner of [source, native]) {
            if (warm) {
                owner.AudioAnalyser.getAudioContext()
                await owner.flush()
            }
            owner.reset()
        }
        for (const owner of [source, native]) {
            owner.store.special.set({ icecast: { enabled: true, host: "127.0.0.1", port: 32769, password: "secret" } })
            owner.store.audioRouting.set({
                channels: [{ id: "main" }],
                connections: [
                    { from: "drawer_audio", to: "main" },
                    { from: "main", to: "icecast" }
                ]
            })
            owner.AudioAnalyser.recorderActivate()
            await owner.flush()
            const ac = owner.AudioAnalyser.getAudioContext()
            assert.ok(owner.sent.some(([channel, commands, data]) => channel === "AUDIO" && commands[0] === "INIT_PORT" && data.id === "icecast"))
            owner.store.special.set({ icecast: { enabled: false } })
            owner.AudioAnalyser.recorderActivate()
            await owner.flush()
            owner.store.special.set({ icecast: { enabled: true } })
            owner.AudioAnalyser.recorderActivate()
            await owner.flush()
            assert.equal(owner.AudioAnalyser.getAudioContext(), ac)
            assert.equal(owner.contexts.length, 1)
            assert.ok(owner.sent.some(([channel, commands]) => channel === "AUDIO" && commands[0] === "CLOSE_PORT"))
            assert.equal(
                owner.trace.some(([kind]) => kind === "close" || kind === "suspend"),
                false
            )
            assert.deepEqual(owner.warnings, [])
        }
        assert.deepEqual(activeTrace(native.snapshot()), source.snapshot())
    }
})

test("already initialized inactive sender retains original resume behavior for local graphs", async () => {
    const source = load(false),
        native = load(true)
    for (const owner of [source, native]) {
        const ac = owner.AudioAnalyser.getAudioContext()
        await owner.flush()
        ac.state = "suspended"
        owner.reset()
        owner.AudioAnalyser.recorderActivate()
        await owner.flush()
        assert.equal(ac.state, "running")
        assert.equal(owner.contexts.length, 1)
    }
    assert.deepEqual(native.snapshot(), source.snapshot())
})

test("output/PDF idle sender activation does not allocate and explicit source consumers still work", async () => {
    for (const role of ["output", "pdf"]) {
        const source = load(false, { role }),
            native = load(true, { role })
        native.AudioAnalyser.recorderActivate()
        await native.flush()
        assert.equal(native.contexts.length, 0)
        source.reset()
        native.reset()
        for (const owner of [source, native]) {
            await owner.AudioAnalyser.attach("video", new owner.Element("video"), "out")
            await owner.flush()
        }
        assert.deepEqual(activeTrace(native.snapshot()), source.snapshot())
        assert.equal(native.contexts.length, 1)
    }
})

test("capability and destination first uses construct one context, preserve source values and never close it", async () => {
    for (const method of ["getChannelInfo", "supportsMultichannel", "getMaxSupportedChannels"]) {
        const source = load(false),
            native = load(true)
        assert.deepEqual(clean(native.AudioAnalyser[method]()), clean(source.AudioAnalyser[method]()))
        assert.equal(native.contexts.length, 1)
        const ac = native.AudioAnalyser.getAudioContext()
        native.AudioAnalyser.updateChannelCount(8)
        const node = native.AudioAnalyser.getOrCreateDestinationNode("speaker")
        assert.equal(node.context, ac)
        assert.equal(node, native.AudioAnalyser.getOrCreateDestinationNode("speaker"))
        assert.equal(native.contexts.length, 1)
        assert.equal(
            native.trace.some(([kind]) => kind === "close" || kind === "suspend"),
            false
        )
    }
})

test("constructor/source/resume/worklet failures remain genuine failures or original diagnostic paths", async () => {
    assert.throws(() => load(false, { failConstruct: true }), /constructor boundary failure/)
    const cold = load(true, { failConstruct: true })
    cold.AudioAnalyser.recorderActivate()
    assert.equal(cold.contexts.length, 0)
    assert.throws(() => cold.AudioAnalyser.getAudioContext(), /constructor boundary failure/, "intentional deferred failure occurs at genuine first use")
    for (const options of [{ failSource: true }, { failResume: true }, { worklet: true, failWorklet: true }]) {
        const source = load(false, options),
            native = load(true, options)
        source.reset()
        native.reset()
        for (const owner of [source, native]) {
            await owner.AudioAnalyser.attach("file", new owner.Element("file"))
            await owner.flush()
            if (options.failWorklet) {
                owner.store.special.set({ icecast: { enabled: true } })
                owner.store.audioRouting.set({ channels: [{ id: "main" }], connections: [{ from: "main", to: "icecast" }] })
                await owner.flush()
                assert.ok(owner.warnings.some((warning) => warning.join(" ").includes("Failed to load pcmWorklet module")))
                assert.equal(
                    owner.sent.some(([channel, commands]) => channel === "AUDIO" && commands[0] === "INIT_PORT"),
                    false
                )
            }
        }
        assert.deepEqual(activeTrace(native.snapshot()), source.snapshot())
        assert.deepEqual(clean(native.warnings), clean(source.warnings))
    }
})
