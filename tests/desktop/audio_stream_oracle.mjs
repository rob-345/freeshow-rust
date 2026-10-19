// Actual immutable receiveAudio/processAudio/IcecastSender plus original NAPI.
// Replay the COMPLETE ordered phase series once: never reset the Opus encoder.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { createRequire } from "node:module"
import { fileURLToPath } from "node:url"
import { createHash } from "node:crypto"
import { EventEmitter } from "node:events"
import ts from "typescript"

const root = fileURLToPath(new URL("../../", import.meta.url))
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex")
const hashes = {
    "src/electron/audio/processAudio.ts": "51fa05f893c126856ce60f23cda79c1784bfdfceb818ca184ed82da6426d9198",
    "src/electron/audio/IcecastSender.ts": "0492c69db5c5ace4637e1a469e3e14a1cc3b48189f7ab78dc8abc64b3e274bc9",
    "src/electron/audio/receiveAudio.ts": "0b79baab29495da7acb5b4ffe32f064b98032781c3b30d4ccd5161831a44067c"
}
function original(file) {
    const source = fs.readFileSync(path.join(root, file), "utf8").replace(/\r\n/g, "\n")
    assert.equal(sha(source), hashes[file], "actual immutable 2d94def source: " + file)
    const ast = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true)
    return ast.statements
        .filter((node) => !ts.isImportDeclaration(node))
        .map((node) => node.getText(ast).replace(/^export\s+/, ""))
        .join("\n")
}
function base64(text, limit) {
    assert.equal(typeof text, "string")
    assert.ok(text.length <= Math.ceil(limit / 3) * 4)
    const bytes = Buffer.from(text, "base64")
    assert.ok(bytes.length <= limit)
    assert.equal(bytes.toString("base64"), text, "canonical bounded base64")
    return bytes
}
function parsed(wire, crc) {
    assert.ok(wire.length <= 1048576)
    const end = wire.indexOf("\r\n\r\n") + 4
    assert.ok(end >= 4 && end <= 16384, "actual HTTP PUT header bound")
    const pages = []
    let cursor = end
    while (cursor < wire.length) {
        assert.ok(pages.length < 4096 && cursor + 27 <= wire.length)
        assert.equal(wire.toString("ascii", cursor, cursor + 4), "OggS")
        assert.equal(wire[cursor + 4], 0)
        const header = 27 + wire[cursor + 26]
        assert.ok(cursor + header <= wire.length)
        const length = header + [...wire.subarray(cursor + 27, cursor + header)].reduce((a, b) => a + b, 0)
        assert.ok(cursor + length <= wire.length)
        const page = Buffer.from(wire.subarray(cursor, cursor + length))
        const cleared = Buffer.from(page),
            checksum = cleared.readUInt32LE(22)
        cleared.writeUInt32LE(0, 22)
        assert.equal(crc(cleared), checksum, "every ORIGINAL raw CRC must pass before filtering or normalization")
        pages.push({ bytes: page, payload: page.subarray(header) })
        cursor += length
    }
    assert.ok(pages.length >= 3)
    let frames = 0
    for (const [index, page] of pages.entries()) {
        assert.equal(page.bytes.readUInt32LE(18), index, "original Ogg sequence")
        assert.equal(page.bytes.readUInt32LE(14), pages[0].bytes.readUInt32LE(14), "one controlled stream serial")
        if (index === 0) {
            assert.equal(page.bytes[5], 2)
            assert.equal(page.payload.subarray(0, 8).toString(), "OpusHead")
        } else if (index === 1) {
            assert.equal(page.bytes[5], 0)
            assert.equal(page.payload.subarray(0, 8).toString(), "OpusTags")
        } else if (index === pages.length - 1) {
            assert.equal(page.bytes[5], 4, "real EOS")
            assert.equal(page.payload.length, 0)
        } else {
            assert.equal(page.bytes[5], 0)
            assert.ok(page.payload.length)
            frames++
        }
        assert.equal(page.bytes.readBigUInt64LE(6), BigInt(frames * 960), "original granule counts ALL real/idle frames")
    }
    return { header: wire.subarray(0, end).toString("utf8"), pages, rawCrcsVerified: pages.length }
}
function normalized(raw, crc) {
    // Only the source's exact hardcoded idle payload can be removed; all raw
    // CRC/sequence/granule checks above have ALREADY passed for every page.
    const pages = raw.pages.filter((page) => !(page.bytes[5] === 0 && page.payload.equals(Buffer.from([0xf8, 0xff, 0xfe]))))
    let frames = 0
    return {
        header: raw.header,
        idlePagesIgnored: raw.pages.length - pages.length,
        pages: pages.map((page, index) => {
            const copy = Buffer.from(page.bytes)
            copy.writeUInt32LE(0x12345678, 14)
            copy.writeUInt32LE(index, 18)
            if (index > 1 && copy[5] !== 4) frames++
            copy.writeBigUInt64LE(BigInt(frames * 960), 6)
            copy.writeUInt32LE(0, 22)
            copy.writeUInt32LE(crc(copy), 22)
            return copy.toString("hex")
        })
    }
}
function receiptStatus(data) {
    return { keys: Object.keys(data), errorOwn: Object.hasOwn(data, "error"), errorUndefined: data.error === undefined, connected: data.connected, connecting: data.connecting, ...(data.error === undefined ? {} : { error: data.error }) }
}

async function run(fixture) {
    assert.equal(process.versions.node, "22.21.1", "pinned source runtime")
    assert.ok(Array.isArray(fixture.phases) && fixture.phases.length >= 1 && fixture.phases.length <= 8)
    const oracles = path.resolve(fixture.oraclesDir || process.env.FREESHOW_OPUS_ORACLE_DIR || path.join(root, "target/opus-oracles"))
    const addonPath = path.join(oracles, "system/opus.node")
    const build = JSON.parse(fs.readFileSync(path.join(oracles, "build-receipt.json"), "utf8"))
    assert.equal(build.package, "0.10.0")
    assert.equal(sha(fs.readFileSync(addonPath)), build.outputs["system/opus.node"], "actual original system NAPI binary")
    for (const file of ["package.json", "src/node-opus.cc", "src/node-opus.h"]) assert.equal(sha(fs.readFileSync(path.join(root, "node_modules/@discordjs/opus", file))), build.hashes[file], "actual addon source provenance")
    const actual = createRequire(import.meta.url)(addonPath)
    const codecReceipt = JSON.parse(fs.readFileSync(path.join(root, "tests/parity/fixtures/opus/receipt.json"), "utf8"))
    assert.equal(process.platform, "linux", "this actual system 1.4 addon receipt is Linux-only")
    const mapped = [
        ...new Set(
            fs
                .readFileSync("/proc/self/maps", "utf8")
                .split("\n")
                .map((line) => line.match(/\s(\/[^\n]*libopus\.so[^\n]*)$/)?.[1])
                .filter(Boolean)
                .map((file) => fs.realpathSync(file))
        )
    ]
    assert.equal(mapped.length, 1, "no mixed libopus SONAME state")
    assert.equal(sha(fs.readFileSync(mapped[0])), codecReceipt.systemLibrarySha256, "mapped actual source codec matches measured libopus 1.4 receipt")
    const constructors = [],
        encoded = [],
        errors = [],
        sockets = [],
        statusEvents = [],
        intervals = new Set(),
        closes = [],
        ports = []
    let now = 10000
    class Clock extends Date {
        static now() {
            return now
        }
    }
    class Socket extends EventEmitter {
        destroyed = false
        writes = []
        setKeepAlive(enabled, ms) {
            assert.equal(enabled, true)
            assert.equal(ms, 10000)
        }
        connect(port, host, callback) {
            assert.equal(host, "127.0.0.1")
            assert.ok(port >= 1024 && port <= 65535)
            this.connected = callback
            sockets.push(this)
        }
        write(bytes, callback) {
            assert.ok(!this.destroyed)
            this.writes.push(Buffer.from(bytes))
            callback?.()
            return true
        }
        destroy() {
            if (!this.destroyed) {
                this.destroyed = true
                closes.push(() => this.emit("close"))
            }
        }
    }
    class MessageChannelMain {
        constructor() {
            const handlers = {}
            this.port1 = {
                on: (kind, callback) => {
                    handlers[kind] = callback
                },
                start() {},
                close: () => closes.push(() => handlers.close?.())
            }
            this.port2 = {}
            ports.push(handlers)
        }
    }
    const context = vm.createContext({
        Buffer,
        ArrayBuffer,
        Uint8Array,
        Uint32Array,
        DataView,
        BigInt,
        Date: Clock,
        require(name) {
            assert.equal(name, "@discordjs/opus")
            return {
                OpusEncoder: class {
                    constructor(rate, channels) {
                        this.native = new actual.OpusEncoder(rate, channels)
                        constructors.push({ rate, channels, bitrate: this.native.getBitrate() })
                    }
                    encode(bytes) {
                        const packet = this.native.encode(bytes)
                        assert.ok(!packet.equals(Buffer.from([0xf8, 0xff, 0xfe])), "controlled signal cannot alias the filtered idle packet")
                        encoded.push(Buffer.from(packet))
                        return packet
                    }
                }
            }
        },
        crypto: {
            randomBytes: () => {
                const bytes = Buffer.alloc(4)
                bytes.writeUInt32LE(0x12340000 + sockets.length)
                return bytes
            }
        },
        net: { Socket },
        MessageChannelMain,
        setInterval: (fn, ms) => {
            assert.equal(ms, 20)
            intervals.add(fn)
            return fn
        },
        clearInterval: (fn) => intervals.delete(fn),
        ToMain: { ICECAST_STATUS: "ICECAST_STATUS" },
        sendToMain: (channel, data) => {
            assert.equal(channel, "ICECAST_STATUS")
            statusEvents.push(receiptStatus(data))
        },
        NdiSender: { NDI: {}, sendAudioBufferNDITarget() {}, sendAudioBufferNDI() {} },
        OmtSender: { OMT: {}, sendAudioBufferOMT() {} },
        BlackmagicSender: { playbackData: {}, sendAudioBuffer() {} },
        getServerData: () => ({}),
        toServer() {},
        WebRtcHost: { isRunning: () => false },
        RtmpStreamer: { anyRunning: () => false },
        console: { log() {}, info() {}, warn() {}, error: (...args) => errors.push(args.map(String).join(" ").slice(0, 2048)) }
    })
    const text = Object.keys(hashes).map(original).join("\n") + "\nglobalThis.probe={receiveAudio,IcecastSender,oggCrc32};"
    new vm.Script(ts.transpileModule(text, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).runInContext(context, { timeout: 2000 })
    const probe = context.probe,
        phases = []
    for (const [index, phase] of fixture.phases.entries()) {
        const script = phase.script,
            config = script?.config
        assert.equal(script?.title || "", "", "desktop NOW_PLAYING title is not wired in this slice")
        assert.equal(config?.host, "127.0.0.1")
        assert.equal(config.enabled, true)
        assert.ok(Array.isArray(script.packets) && script.packets.length >= 2 && script.packets.length <= 16)
        const startEncoded = encoded.length,
            startStatus = statusEvents.length,
            startSockets = sockets.length
        now += 10000
        probe.receiveAudio({}, { channel: "INIT_PORT", data: { id: "icecast" } })
        for (const [packetIndex, packet] of script.packets.entries()) {
            assert.ok(["port", "PCM", "CAPTURE"].includes(packet.origin))
            assert.equal(packet.sampleRate, 48000)
            assert.equal(packet.id, "icecast")
            assert.equal(!!packet.connectDropped, packetIndex === 0)
            const bytes = base64(packet.planarBase64, 32768)
            assert.equal(bytes.length, 7680, "actual controlled planar stereo960 bytes")
            const payload = { buffer: bytes, sampleRate: packet.sampleRate, id: packet.id, icecast: config }
            if (packet.origin === "port") ports.at(-1).message({ data: { channel: "AUDIO", payload } })
            else probe.receiveAudio({}, { channel: packet.origin, data: payload })
            assert.equal(encoded.length, startEncoded + packetIndex + 1)
            if (packetIndex === 0) {
                assert.equal(sockets.length, startSockets + 1)
                assert.equal(sockets.at(-1).writes.length, 0, "first packet encoded then connecting-dropped")
                sockets.at(-1).connected()
            }
        }
        probe.receiveAudio({}, { channel: "CLOSE_PORT", data: { id: "icecast" } })
        while (closes.length) closes.shift()()
        assert.equal(intervals.size, 0)
        const wire = Buffer.concat(sockets.at(-1).writes)
        const source = parsed(wire, probe.oggCrc32),
            canonicalSource = normalized(source, probe.oggCrc32)
        const result = { phase: script.phase || index, source: { wireBase64: wire.toString("base64"), wireSha256: sha(wire), rawCrcsVerified: source.rawCrcsVerified, statuses: statusEvents.slice(startStatus), encoded: encoded.slice(startEncoded).map((packet) => packet.toString("hex")), normalized: canonicalSource }, nativeCompared: false }
        if (phase.wire) {
            const capability = phase.capability || fixture.capability
            assert.equal(capability?.Available?.version, "libopus 1.4", "compare only the same measured actual codec version")
            assert.equal(capability.Available.default_bitrate, constructors[0].bitrate)
            const native = parsed(base64(phase.wire.wireBase64, 1048576), probe.oggCrc32),
                canonicalNative = normalized(native, probe.oggCrc32)
            assert.deepEqual({ header: canonicalNative.header, pages: canonicalNative.pages }, { header: canonicalSource.header, pages: canonicalSource.pages }, "actual native/source same-version HTTP + Opus/Ogg bytes")
            result.nativeCompared = true
            result.native = { rawCrcsVerified: native.rawCrcsVerified, normalized: canonicalNative }
            if (phase.statuses)
                assert.deepEqual(
                    phase.statuses.map(({ listenerIdUndefined, channel, ...data }) => {
                        assert.equal(listenerIdUndefined, true)
                        assert.equal(channel, "ICECAST_STATUS")
                        return data
                    }),
                    result.source.statuses,
                    "retained status own undefined/key order"
                )
        }
        phases.push(result)
    }
    assert.deepEqual(constructors, [{ rate: 48000, channels: 2, bitrate: 120000 }], "ONE source-global encoder across ALL phases")
    assert.deepEqual(errors, [])
    return {
        revision: "2d94defcf98175e4f5c9504e7f4fd1d33c35c975",
        hashes,
        node: process.version,
        codec: { version: "libopus 1.4", mappedLibrary: mapped[0], librarySha256: sha(fs.readFileSync(mapped[0])), addonSha256: sha(fs.readFileSync(addonPath)) },
        constructors,
        globalEncodedPackets: encoded.length,
        phases,
        normalization: "Verify ALL original CRCs/sequences/granules first; filter only exact f8fffe idle pages; normalize serial and resulting sequence/granules/CRC. HTTP, tags and real Opus payloads exact. No per-phase encoder reset.",
        hardwareCaptureVerified: false
    }
}

try {
    const chunks = []
    let size = 0
    for await (const chunk of process.stdin) {
        size += chunk.length
        assert.ok(size <= 8 * 1048576, "oracle input bound")
        chunks.push(chunk)
    }
    process.stdout.write(JSON.stringify(await run(JSON.parse(Buffer.concat(chunks).toString("utf8")))) + "\n")
} catch (error) {
    process.stderr.write(String(error.message || error).slice(0, 4096) + "\n")
    process.exitCode = 1
}
