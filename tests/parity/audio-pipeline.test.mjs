import assert from "node:assert/strict"
import path from "node:path"
import vm from "node:vm"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import ts from "typescript"
import { root, clean } from "./oracle.mjs"
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const binary = process.env.FREESHOW_AUDIO_PIPELINE_BIN || path.join(root, "target/debug", process.platform === "win32" ? "freeshow-audio-pipeline.exe" : "freeshow-audio-pipeline")
const files = new Map()
function original(file) {
    if (!files.has(file)) {
        const r = spawnSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 4 * 1024 * 1024 })
        assert.equal(r.status, 0, r.stderr)
        files.set(file, r.stdout.replace(/\r\n/g, "\n"))
    }
    return files.get(file)
}
function code(file) {
    const source = ts.createSourceFile(file, original(file), ts.ScriptTarget.Latest, true)
    return source.statements
        .filter((n) => !ts.isImportDeclaration(n))
        .map((n) => n.getText(source).replace(/^export\s+/, ""))
        .join("\n")
}
function bits(n) {
    const b = Buffer.alloc(8)
    b.writeDoubleLE(n)
    return b.readBigUInt64LE().toString(16).padStart(16, "0")
}
function scalar(v) {
    if (v?.numberBits) {
        const b = Buffer.alloc(8)
        b.writeBigUInt64LE(BigInt("0x" + v.numberBits))
        return b.readDoubleLE()
    }
    return v
}
function rust(requests) {
    const result = spawnSync(binary, [], { input: requests.map(JSON.stringify).join("\n") + "\n", encoding: "utf8", timeout: 30000, maxBuffer: 64 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return result.stdout.trim().split("\n").map(JSON.parse)
}
function oracle(opus = true) {
    let current = {},
        records = [],
        frameIndex = 0
    const errors = [],
        ports = []
    const record = (kind, buffer, opts, target = null) => records.push({ kind, hex: Buffer.from(buffer).toString("hex"), sampleRateBits: bits(opts.sampleRate), target, channels: opts.channelCount })
    const context = vm.createContext({
        Buffer,
        ArrayBuffer,
        Uint8Array,
        DataView,
        ports,
        require: (id) => {
            assert.equal(id, "@discordjs/opus")
            if (!opus) throw new Error("controlled unavailable codec")
            return {
                OpusEncoder: class {
                    constructor(rate, channels) {
                        assert.equal(rate, 48000)
                        assert.equal(channels, 2)
                    }
                    encode(buffer) {
                        const index = frameIndex++
                        records.push({ kind: "encode", hex: Buffer.from(buffer).toString("hex") })
                        if (current.fail?.index === index && current.fail.stage !== "send") throw new Error("controlled failure")
                        return Buffer.from([index & 255])
                    }
                }
            }
        },
        NdiSender: { NDI: {}, sendAudioBufferNDITarget: (id, b, o) => record("ndiTarget", b, o, id), sendAudioBufferNDI: (b, o) => record("ndiBroadcast", b, o) },
        OmtSender: { OMT: {}, sendAudioBufferOMT: (b, o) => record("omtBroadcast", b, o) },
        BlackmagicSender: { playbackData: {}, sendAudioBuffer: (b, o) => record("blackmagicBroadcast", b, o) },
        getServerData: (id) => {
            assert.equal(id, "OUTPUT_STREAM")
            return { sendAudio: current.sinks?.output || false }
        },
        toServer: (id, msg) => {
            assert.equal(id, "OUTPUT_STREAM")
            assert.equal(msg.channel, "AUDIO_BUFFER")
            record("outputServer", msg.data.buffer, msg.data)
        },
        WebRtcHost: { isRunning: () => !!current.sinks?.webrtc, sendAudio: (b, o) => record("webRtc", b, o) },
        RtmpStreamer: { anyRunning: () => !!current.sinks?.rtmp, updateAudio: (id, b, rate) => record("rtmp", b, { sampleRate: rate, channelCount: 2 }, id ?? null) },
        IcecastSender: {
            disconnect: () => records.push({ kind: "disconnect" }),
            sendAudio: (_b, config) => {
                const index = frameIndex - 1
                records.push({ kind: "send", config })
                if (current.fail?.stage === "send" && current.fail.index === index) throw new Error("controlled failure")
            }
        },
        MessageChannelMain: class {
            constructor() {
                const handlers = {}
                this.port1 = {
                    on: (k, f) => (handlers[k] = f),
                    start() {},
                    close() {
                        handlers.close?.()
                    }
                }
                this.port2 = {}
                ports.push({ handlers, port: this.port1 })
            }
        },
        console: { error: (...args) => errors.push(args) }
    })
    const text = code("src/electron/audio/processAudio.ts") + "\n" + code("src/electron/audio/receiveAudio.ts") + `\n globalThis.probe={processAudio,receiveAudio,toAudioBuffer,convertPlanarFloat32ToInt16Interleaved,resamplePcmInt16Stereo,isAudioEnabled,state:()=>resampleState,accumulator:()=>icecastPcmAccumulator};`
    new vm.Script(ts.transpileModule(text, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).runInContext(context, { timeout: 1000 })
    return {
        ...context.probe,
        errors,
        ports,
        context,
        set(packet) {
            current = packet
            records = []
            frameIndex = 0
            for (const [name, key] of [
                ["NdiSender", "ndi"],
                ["OmtSender", "omt"],
                ["BlackmagicSender", "blackmagic"]
            ])
                context[name][name === "NdiSender" ? "NDI" : name === "OmtSender" ? "OMT" : "playbackData"] = Object.fromEntries((packet.sinks?.[key] || []).map((id) => [id, {}]))
        },
        records: () => clean(records),
        stateBits() {
            const s = context.probe.state()
            return s ? [s.prevL, s.prevR, bits(s.phase), bits(s.inSampleRate)] : null
        }
    }
}
function planar(count, seed = 1) {
    const b = Buffer.alloc(count * 8)
    for (let i = 0; i < count * 2; i++) {
        seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
        b.writeFloatLE(((seed % 200001) - 100000) / 80000, i * 4)
    }
    return b
}
function packet(buffer, other = {}) {
    return { hex: buffer.toString("hex"), ...other }
}
async function compare(packets, { opus = true, port = false } = {}) {
    const [native] = rust([{ opus, packets }]),
        source = oracle(opus)
    assert.equal(source.isAudioEnabled(), opus)
    for (let i = 0; i < packets.length; i++) {
        const p = packets[i]
        source.set(p)
        const before = source.errors.length
        const payload = { buffer: Buffer.from(p.hex, "hex"), sampleRate: scalar(p.sampleRate), id: scalar(p.id), icecast: p.icecast }
        if (port) {
            source.receiveAudio({ sender: { isDestroyed: () => false, postMessage() {} } }, { channel: "INIT_PORT", data: { id: "test-port" } })
            source.ports.at(-1).handlers.message({ data: { channel: "AUDIO", payload } })
        } else source.receiveAudio({}, { channel: i % 2 ? "CAPTURE" : "PCM", data: payload })
        await Promise.resolve()
        const n = native[i]
        assert.equal(n.error, undefined, JSON.stringify(n.error))
        const records = source.records()
        assert.deepEqual(
            n.pre,
            records.filter((r) => r.kind.startsWith("ndi") || r.kind === "omtBroadcast"),
            `planar packet${i}`
        )
        assert.deepEqual(
            n.legacy,
            records.filter((r) => ["blackmagicBroadcast", "outputServer", "webRtc", "rtmp"].includes(r.kind)),
            `legacy packet${i}`
        )
        const attempted = p.fail ? Math.min(n.frames.length, p.fail.index + 1) : n.frames.length
        assert.deepEqual(
            n.frames.slice(0, attempted),
            records.filter((r) => r.kind === "encode").map((r) => r.hex),
            `encoder input packet${i}`
        )
        for (const frame of n.frames) assert.equal(frame.length, 3840 * 2)
        assert.equal(n.accumulator, source.accumulator().toString("hex"), `accumulator packet${i}`)
        assert.deepEqual(n.state, source.stateBits(), `resample state packet${i}`)
        const logged = source.errors.slice(before).filter((e) => e[0] === "Could not encode Opus for Icecast:")
        assert.equal(n.diagnostics.length, logged.length)
        if (logged.length) assert.ok(n.diagnostics[0].message.startsWith(logged[0][0]))
        const sends = records.filter((r) => r.kind === "send")
        for (const send of sends) assert.deepEqual(send.config, n.icecast)
        const expectedOrder = n.pre.map((r) => r.kind)
        for (let index = 0; index < attempted; index++) {
            expectedOrder.push("encode")
            if (!(p.fail?.stage !== "send" && p.fail?.index === index)) expectedOrder.push("send")
        }
        expectedOrder.push(...n.legacy.map((r) => r.kind))
        assert.deepEqual(
            records.map((r) => r.kind),
            expectedOrder,
            `source call ordering packet${i}`
        )
    }
    return native
}
test("actual planar converter: arbitrary Float32 bits, NaNs, infinities, clipping, negative-half ties and byte sizing", () => {
    const source = oracle(),
        cases = []
    const values = [0, 0x80000000, 0x7f800000, 0xff800000, 0x7fc00001, 0xffaabcde, 0x3f800000, 0xbf800000, 0x40000000, 0xc0000000, 1, 0x80000001]
    let seed = 9
    for (let i = 0; i < 4096; i++) {
        seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
        values.push(seed)
    }
    const all = Buffer.alloc(values.length * 4)
    values.forEach((v, i) => all.writeUInt32LE(v, i * 4))
    cases.push(all)
    for (let length = 0; length < 32; length++) cases.push(all.subarray(0, length))
    const ties = Buffer.alloc(8 * 4)
    ;[-1 / 65536, -3 / 65536, -5 / 65536, -7 / 65536, 0.5, 1, -1, NaN].forEach((v, i) => ties.writeFloatLE(v, i * 4))
    cases.push(ties)
    const results = rust(cases.map((buffer) => ({ mode: "convert", hex: buffer.toString("hex") })))
    cases.forEach((buffer, i) => assert.equal(results[i].hex, source.convertPlanarFloat32ToInt16Interleaved(buffer).toString("hex"), `case${i}`))
})
test("actual toAudioBuffer byte views and serialized Buffer coercion feed exact raw bytes", () => {
    const source = oracle(),
        backing = planar(20),
        views = [backing.subarray(3, 101), new Uint8Array(backing.buffer, backing.byteOffset + 7, 99), new DataView(backing.buffer, backing.byteOffset + 9, 80), new Float32Array(new Uint8Array(backing.subarray(0, 128)).buffer, 4, 20), new Uint8Array(backing).buffer, { type: "Buffer", data: [-1, 256, 257, NaN, 1.9, "12", null] }]
    for (const view of views) {
        const buffer = source.toAudioBuffer(view)
        assert.ok(Buffer.isBuffer(buffer))
        const [native] = rust([{ mode: "convert", hex: buffer.toString("hex") }])
        assert.equal(native.hex, source.convertPlanarFloat32ToInt16Interleaved(buffer).toString("hex"))
    }
    for (const value of [null, false, {}, "text", { type: "Buffer", data: "invalid" }]) assert.equal(source.toAudioBuffer(value), null)
})
test("actual resampler has exact bytes/phase across splits,48k bypass, empty/tail packets, invalid rates and rate changes", () => {
    const source = oracle(),
        packets = []
    for (const rate of [44100, 48000, 44100, 96000, 96000, 192000, 8000, 44100.5, -1, Infinity, -Infinity, 5e-324, 44100])
        for (const length of [0, 1, 3, 4, 8, 19, 120]) {
            const b = Buffer.alloc(length)
            for (let i = 0; i < Math.floor(length / 2); i++) b.writeInt16LE(((i * 1731) % 65536) - 32768, i * 2)
            packets.push({ hex: b.toString("hex"), rate: { numberBits: bits(rate) } })
        }
    const [native] = rust([{ mode: "resample", packets }])
    packets.forEach((p, i) => {
        const expected = source.resamplePcmInt16Stereo(Buffer.from(p.hex, "hex"), scalar(p.rate))
        assert.equal(native[i].hex, expected.toString("hex"), `bytes${i}`)
        assert.deepEqual(native[i].state, source.stateBits(), `phase${i}`)
    })
})
test("actual routing includes OMT target broadcast, unrelated-target legacy calls and raw NaN bytes", async () => {
    const sinks = { ndi: ["ndi", "both"], omt: ["omt", "both"], blackmagic: ["bm"], output: true, webrtc: true, rtmp: true }
    const raw = planar(960)
    raw.writeUInt32LE(0x7f812345, 0)
    raw.writeUInt32LE(0xffc00abc, 960 * 4)
    const packets = [undefined, "ndi", "omt", "both", "bm", "unknown", "icecast", 0, [], {}, 1e21, "敬拜🙌"].map((id) => packet(raw, { id, sinks, icecast: { enabled: true, unknown: { retain: "value" } } }))
    await compare(packets)
})
test("actual gating preserves absent encoder, disabled false versus other falsy enabled values and no legacy sinks", async () => {
    for (const opus of [true, false]) {
        const packets = [undefined, { enabled: false }, { enabled: 0 }, { enabled: null }, {}, { enabled: "" }, { enabled: true }, true, 7, "opaque config", false, null].map((icecast) => packet(planar(960), { id: "icecast", icecast, sinks: { output: true } }))
        packets.push(packet(planar(960), { icecast: { enabled: true }, sinks: { output: true, webrtc: true, rtmp: true } }))
        await compare(packets, { opus })
    }
})
test("actual global20ms accumulation spans rates, targets and latched unknown config without resetting on48k", async () => {
    const packets = []
    for (let i = 0; i < 100; i++) packets.push(packet(planar(i % 4 === 0 ? 441 : i % 4 === 1 ? 960 : i % 4 === 2 ? 37 : 1000, i + 1), { sampleRate: [44100, 48000, 44100, 96000][i % 4], id: i % 7 === 0 ? "ndi" : "icecast", icecast: i % 9 === 0 ? { enabled: true, marker: i } : i % 5 === 0 ? false : undefined, sinks: { ndi: ["ndi"], output: true } }))
    await compare(packets)
})
test("actual encode/send failure consumes failed frame, retains later full frames, then continues legacy", async () => {
    for (const stage of ["encode", "send"])
        for (let index = 0; index < 3; index++) {
            const packets = [packet(planar(2880), { icecast: { enabled: true, marker: 1 }, sinks: { output: true }, fail: { index, stage } }), packet(Buffer.from([7]), { icecast: { enabled: true, marker: 2 }, sinks: { output: true } }), packet(planar(960), { id: "icecast", sinks: { rtmp: true } })]
            await compare(packets)
        }
})
test("actual metadata coercions include Unicode whitespace, arrays, JS numeric formatting and nonfinite sample rates", async () => {
    const rates = [undefined, null, 0, -0, "", false, true, "\uFEFF44100\u00A0", "\u008544100", [44100], [[96000]], {}, "0xBB80", "0b1011101110000000", "0o135600", "inf", "+Infinity", "-Infinity", { numberBits: bits(NaN) }, { numberBits: bits(Infinity) }, 44100.5]
    await compare(rates.map((sampleRate, i) => packet(planar(20), { sampleRate, id: [undefined, [], {}, 1e21, 1e-7, 1e-6, true][i % 7], icecast: { enabled: true }, sinks: { output: true, rtmp: true } })))
})
test("actual transferred-port callback shares config/accumulator contract with PCM/CAPTURE", async () => {
    await compare([packet(planar(480), { sampleRate: 44100, id: "icecast", icecast: { enabled: true, extra: "preserved" } }), packet(planar(480), { sampleRate: 44100, id: "icecast" }), packet(planar(960), { sampleRate: 48000, id: "icecast", icecast: { enabled: false } })], { port: true })
})
test("actual retained worklet emits planar stereo bytes with mono fallback and20ms framing", async () => {
    const sent = []
    let Processor
    const context = vm.createContext({
        AudioWorkletProcessor: class {
            constructor() {
                this.port = { onmessage: null }
            }
        },
        registerProcessor: (name, type) => {
            assert.equal(name, "pcm-sender-processor")
            Processor = type
        }
    })
    new vm.Script(original("public/assets/pcm-worklet.js")).runInContext(context, { timeout: 1000 })
    const proc = new Processor()
    proc.port.onmessage({ data: { type: "INIT_PORT", targetId: "icecast", sampleRate: 44100, icecastConfig: { enabled: true } }, ports: [{ start() {}, postMessage: (v) => sent.push(v) }] })
    for (let i = 0; i < 16; i++) {
        const mono = new Float32Array(128)
        for (let j = 0; j < 128; j++) mono[j] = ((i * 128 + j) % 200) / 100 - 1
        assert.equal(proc.process([[mono]]), true)
    }
    assert.equal(sent.length, 2)
    const packets = sent.map((v) => {
        assert.equal(v.channel, "AUDIO")
        const raw = Buffer.from(v.payload.buffer)
        assert.equal(raw.length, 882 * 8)
        assert.deepEqual(raw.subarray(0, 882 * 4), raw.subarray(882 * 4))
        return packet(raw, { sampleRate: v.payload.sampleRate, id: v.payload.id, icecast: v.payload.icecast })
    })
    await compare(packets)
})
test("pathological rates and excessive expansion are explicit native rejection, never empty-success parity", () => {
    const [native] = rust([{ opus: true, packets: [packet(planar(960), { sampleRate: 1, id: "icecast", icecast: { enabled: true } }), packet(planar(960), { sampleRate: 48000, id: "icecast", icecast: { enabled: true } })] }])
    assert.equal(native[0].error.code, "unsupported_sample_rate")
    assert.equal(native[1].error, undefined)
    assert.equal(native[1].frames.length, 1)
    const [result] = rust([{ mode: "convert", hex: Buffer.alloc(128 * 1024 + 1).toString("hex") }])
    assert.equal(result.error.code, "audio_pipeline_resource_limit")
})
