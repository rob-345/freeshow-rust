// Each invocation owns one actual addon/library version, avoiding ELF SONAME reuse.
import fs from "node:fs"
import vm from "node:vm"
import { createRequire } from "node:module"
const require = createRequire(import.meta.url)
const request = JSON.parse(fs.readFileSync(0, "utf8"))
const actual = request.unavailable ? null : require(request.addon)
if (request.mode === "decode") {
    const decoder = new actual.OpusEncoder(48000, 2)
    process.stdout.write(JSON.stringify({ decoded: request.packets.map((hex) => decoder.decode(Buffer.from(hex, "hex")).toString("hex")) }))
} else if (request.mode === "errors") {
    const errors = [],
        encoder = new actual.OpusEncoder(48000, 2),
        bitrateBefore = encoder.getBitrate()
    for (const fn of [() => encoder.encode("not Buffer"), () => new actual.OpusEncoder(123, 2).encode(Buffer.alloc(3840))]) {
        try {
            fn()
            errors.push(null)
        } catch (e) {
            errors.push({ name: e.name, message: e.message })
        }
    }
    process.stdout.write(JSON.stringify({ errors, after: encoder.encode(Buffer.alloc(3840)).toString("hex"), bitrateBefore, bitrateAfter: encoder.getBitrate() }))
} else {
    const pcm = [],
        encoded = [],
        logs = [],
        constructors = []
    const context = vm.createContext({
        Buffer,
        DataView,
        Number,
        Math,
        require: (name) => {
            if (name !== "@discordjs/opus") throw new Error("Unexpected dependency")
            if (!actual) throw new Error("controlled actual addon unavailable")
            return {
                OpusEncoder: class {
                    constructor(rate, channels) {
                        constructors.push({ rate, channels })
                        this.native = new actual.OpusEncoder(rate, channels)
                    }
                    encode(bytes) {
                        pcm.push(bytes.toString("hex"))
                        const packet = this.native.encode(bytes)
                        encoded.push(packet.toString("hex"))
                        return packet
                    }
                }
            }
        },
        NdiSender: { NDI: {}, sendAudioBufferNDITarget() {}, sendAudioBufferNDI() {} },
        OmtSender: { OMT: {}, sendAudioBufferOMT() {} },
        BlackmagicSender: { playbackData: {}, sendAudioBuffer() {} },
        getServerData: () => ({}),
        toServer() {},
        WebRtcHost: { isRunning: () => false },
        RtmpStreamer: { anyRunning: () => false },
        IcecastSender: { sendAudio() {} },
        console: { error: (...args) => logs.push(args.map(String)) }
    })
    vm.runInContext(request.code + "\nglobalThis.processAudio=processAudio;globalThis.enabled=isAudioEnabled;", context)
    for (const hex of request.planar || []) await context.processAudio(Buffer.from(hex, "hex"), 48000, "icecast", { enabled: true })
    process.stdout.write(JSON.stringify({ enabled: context.enabled(), constructors, pcm, encoded, logs }))
}
