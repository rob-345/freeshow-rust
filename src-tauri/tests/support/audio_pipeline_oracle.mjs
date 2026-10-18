// Reproducible private oracle: immutable original functions, actual original N-API
// encoder, inert loopback TCP only. No migrated frontend or production endpoint.
import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { createHash, randomBytes } from "node:crypto"
import { createRequire } from "node:module"
import net from "node:net"
import path from "node:path"
import { fileURLToPath } from "node:url"
import vm from "node:vm"
import ts from "typescript"
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..")
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
let input = ""
for await (const chunk of process.stdin) {
    input += chunk
    assert.ok(input.length <= 1024 * 1024)
}
const watchdog = setTimeout(() => {
    process.stderr.write("source oracle deadline\n")
    process.exit(2)
}, 10000)
watchdog.unref()
const request = JSON.parse(input)
assert.ok(path.isAbsolute(request.addon))
assert.ok(request.frames.length <= 8)
const { OpusEncoder: OriginalEncoder } = createRequire(import.meta.url)(request.addon)
const encoded = [],
    pcm = [],
    statuses = [],
    errors = [],
    sourceHashes = {}
let attempts = 0
class ObservedEncoder {
    constructor(rate, channels) {
        assert.equal(rate, 48000)
        assert.equal(channels, 2)
        this.encoder = new OriginalEncoder(rate, channels)
    }
    encode(bytes) {
        pcm.push(bytes.toString("hex"))
        const packet = this.encoder.encode(bytes)
        encoded.push(packet.toString("hex"))
        if (request.failEncode === ++attempts) throw new Error("controlled exception after actual native encode")
        return packet
    }
}
const context = vm.createContext({
    Buffer,
    BigInt,
    Uint32Array,
    DataView,
    Number,
    Math,
    require(name) {
        assert.equal(name, "@discordjs/opus")
        return { OpusEncoder: ObservedEncoder }
    },
    NdiSender: { NDI: {}, sendAudioBufferNDI() {}, sendAudioBufferNDITarget() {} },
    OmtSender: { OMT: {}, sendAudioBufferOMT() {} },
    BlackmagicSender: { playbackData: {}, sendAudioBuffer() {} },
    getServerData() {
        return { sendAudio: false }
    },
    toServer() {},
    WebRtcHost: {
        isRunning() {
            return false
        }
    },
    RtmpStreamer: {
        anyRunning() {
            return false
        }
    },
    crypto: { randomBytes },
    net,
    Date,
    setInterval,
    clearInterval,
    ToMain: { ICECAST_STATUS: "ICECAST_STATUS" },
    sendToMain(channel, data) {
        assert.equal(channel, "ICECAST_STATUS")
        assert.ok(Object.hasOwn(data, "error"))
        statuses.push(JSON.parse(JSON.stringify(data)))
    },
    console: {
        log() {},
        warn() {},
        error(...args) {
            errors.push(args.map(String))
        }
    }
})
function load(file, expose) {
    const original = spawnSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 })
    assert.equal(original.status, 0, original.stderr)
    const source = original.stdout.replace(/\r\n/g, "\n")
    sourceHashes[file] = createHash("sha256").update(source).digest("hex")
    const ast = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true)
    const code = ast.statements
        .filter((n) => !ts.isImportDeclaration(n))
        .map((n) => n.getText(ast).replace(/^export\s+/, ""))
        .join("\n")
    vm.runInContext(ts.transpileModule(`${code}\n${expose}`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText, context)
}
load("src/electron/audio/IcecastSender.ts", "globalThis.sender=IcecastSender;globalThis.crc=oggCrc32")
load("src/electron/audio/processAudio.ts", "globalThis.process=processAudio;globalThis.pending=()=>icecastPcmAccumulator.toString('hex')")
const pause = (ms) => new Promise((r) => setTimeout(r, ms))
const wait = async (condition) => {
    const start = Date.now()
    while (!condition()) {
        assert.ok(Date.now() - start < 5000, "loopback source deadline")
        await pause(1)
    }
}
const chunks = []
let close
const closed = new Promise((r) => {
    close = r
})
const server = net.createServer((socket) => {
    socket.on("data", (bytes) => {
        assert.ok(chunks.reduce((n, b) => n + b.length, 0) + bytes.length <= 256 * 1024)
        chunks.push(bytes)
    })
    socket.on("end", () => socket.end())
    socket.on("close", close)
    socket.on("error", close)
})
await new Promise((r) => server.listen(0, "127.0.0.1", r))
const port = server.address().port
const config = { enabled: request.failEncode ? 0 : true, host: "127.0.0.1", port, mount: "audio", password: "秘密" }
try {
    context.sender.updateMetadata("演奏者 - One")
    for (let i = 0; i < request.frames.length; i++) {
        const bytes = Buffer.from(request.frames[i], "hex")
        assert.ok(bytes.length <= 32768)
        if (i === 2) context.sender.updateMetadata("Artist - Two")
        await context.process(bytes, 48000, "icecast", config)
        // First real encode triggers asynchronous connection and is dropped by source.
        if (i === 0 && !request.failEncode) await wait(() => statuses.some((s) => s.connected))
    }
    await pause(10) // Let the real socket flush before source's immediate destroy.
    context.sender.disconnect()
    if (chunks.length) await closed
    await new Promise((r) => server.close(r))
    const raw = Buffer.concat(chunks)
    if (raw.length) {
        let offset = raw.indexOf("\r\n\r\n") + 4
        assert.ok(offset >= 4)
        while (offset < raw.length) {
            assert.equal(raw.toString("ascii", offset, offset + 4), "OggS")
            const count = raw[offset + 26],
                header = 27 + count
            const size = [...raw.subarray(offset + 27, offset + header)].reduce((a, b) => a + b, 0)
            const page = Buffer.from(raw.subarray(offset, offset + header + size)),
                stored = page.readUInt32LE(22)
            assert.equal(page.length, header + size)
            page.writeUInt32LE(0, 22)
            assert.equal(context.crc(page), stored, "actual source raw on-wire CRC, before normalization")
            offset += page.length
        }
        assert.equal(offset, raw.length)
    }
    process.stdout.write(JSON.stringify({ revision, sourceHashes, node: process.version, raw: raw.toString("hex"), port, pcm, encoded, pending: context.pending(), statuses, errors }))
} finally {
    context.sender.disconnect()
    server.close()
}
