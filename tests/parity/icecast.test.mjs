import assert from "node:assert/strict"
import path from "node:path"
import vm from "node:vm"
import net from "node:net"
import { EventEmitter } from "node:events"
import { createHash } from "node:crypto"
import fs from "node:fs"
import { spawn, spawnSync } from "node:child_process"
import { test } from "node:test"
import ts from "typescript"
import { root } from "./oracle.mjs"

const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const file = "src/electron/audio/IcecastSender.ts"
const source = spawnSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 })
assert.equal(source.status, 0, source.stderr)
const ast = ts.createSourceFile(file, source.stdout.replace(/\r\n/g, "\n"), ts.ScriptTarget.Latest, true)
const code = ts.transpileModule(
    ast.statements
        .filter((n) => !ts.isImportDeclaration(n))
        .map((n) => n.getText(ast).replace(/^export\s+/, ""))
        .join("\n") + "\nglobalThis.sender=IcecastSender;globalThis.tags=createOpusTagsPacket;globalThis.crc=oggCrc32;",
    { compilerOptions: { target: ts.ScriptTarget.ES2022 } }
).outputText
const binary = process.env.FREESHOW_ICECAST_BIN || path.join(root, "target/debug", process.platform === "win32" ? "freeshow-icecast.exe" : "freeshow-icecast")
const serial = 0x88776655
function rust(request) {
    const result = spawnSync(binary, [], { input: JSON.stringify(request), encoding: "utf8", timeout: 10000, maxBuffer: 4 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return JSON.parse(result.stdout)
}
function oracle(real = false) {
    let now = 10000
    const sockets = [],
        statuses = [],
        intervals = new Set(),
        closes = []
    class Socket extends EventEmitter {
        writes = []
        destroyed = false
        setKeepAlive(enabled, delay) {
            assert.equal(enabled, true)
            assert.equal(delay, 10000)
        }
        connect(port, host, callback) {
            this.callback = callback
            this.port = port
            this.host = host
            sockets.push(this)
        }
        write(bytes, callback) {
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
    class Clock extends Date {
        static now() {
            return now
        }
    }
    const context = vm.createContext({
        Buffer,
        BigInt,
        Uint32Array,
        crypto: {
            randomBytes: () => {
                const b = Buffer.alloc(4)
                b.writeUInt32LE(serial)
                return b
            }
        },
        net: real ? net : { Socket },
        Date: real ? Date : Clock,
        setInterval: real
            ? setInterval
            : (f, ms) => {
                  assert.equal(ms, 20)
                  intervals.add(f)
                  return f
              },
        clearInterval: real ? clearInterval : (f) => intervals.delete(f),
        ToMain: { ICECAST_STATUS: "ICECAST_STATUS" },
        sendToMain: (channel, data) => {
            assert.equal(channel, "ICECAST_STATUS")
            statuses.push(JSON.parse(JSON.stringify(data)))
        },
        console: { log() {}, error() {}, warn() {} }
    })
    vm.runInContext(code, context)
    return {
        sender: context.sender,
        crc: context.crc,
        sockets,
        statuses,
        intervals,
        flush() {
            while (closes.length) closes.shift()()
        },
        tick(ms) {
            now += ms
            for (const f of intervals) f()
        }
    }
}
function pages(bytes) {
    const output = []
    let cursor = 0
    while (cursor < bytes.length) {
        assert.equal(bytes.toString("ascii", cursor, cursor + 4), "OggS")
        const count = bytes[cursor + 26],
            header = 27 + count
        const size = [...bytes.subarray(cursor + 27, cursor + header)].reduce((a, b) => a + b, 0)
        output.push(Buffer.from(bytes.subarray(cursor, cursor + header + size)))
        cursor += header + size
    }
    assert.equal(cursor, bytes.length)
    return output
}
function normalize(page, crc) {
    page = Buffer.from(page)
    page.writeUInt32LE(serial, 14)
    page.writeUInt32LE(0, 22)
    page.writeUInt32LE(crc(page), 22)
    return page.toString("hex")
}
function rawPages(chunks, crc) {
    const all = Buffer.concat(chunks),
        end = all.indexOf("\r\n\r\n") + 4
    assert.ok(end >= 4)
    const raw = pages(all.subarray(end))
    for (const page of raw) {
        const cleared = Buffer.from(page),
            checksum = cleared.readUInt32LE(22)
        cleared.writeUInt32LE(0, 22)
        assert.equal(crc(cleared), checksum, `raw wire CRC ${page.toString("hex")}`)
    }
    return { header: all.subarray(0, end).toString(), raw }
}

test("actual source Ogg pages: CRC, lace terminators, OpusHead/tags, audio granules and EOS", () => {
    for (const title of ["", "Plain  title ", "Artist - Title - Part", "\uFEFF演奏者\u00a0 -  曲 😀  ", " - Song", "Artist - "]) {
        const o = oracle(),
            packets = [[1], Array(254).fill(17), Array(255).fill(0), Array(510).fill(128)]
        o.sender.updateMetadata(title)
        o.sender.sendAudio(Buffer.from([99]), { enabled: true })
        assert.equal(o.sockets[0].writes.length, 0, "triggering packet is dropped while connecting")
        o.sockets[0].callback()
        for (const packet of packets) o.sender.sendAudio(Buffer.from(packet), { enabled: true })
        o.sender.disconnect()
        o.flush()
        const native = rust({ mode: "pages", serial, title, packets })
        assert.equal(native.error, undefined)
        const original = o.sockets[0].writes.slice(1)
        assert.deepEqual(
            native.ok.map((s) => Buffer.from(s, "base64").toString("hex")),
            [Buffer.concat(original.slice(0, 2)), ...original.slice(2)].map((b) => b.toString("hex"))
        )
        assert.deepEqual(o.statuses, [
            { connected: false, connecting: true },
            { connected: true, connecting: false },
            { connected: false, connecting: false }
        ])
        for (const page of original) {
            const cleared = Buffer.from(page),
                expected = page.readUInt32LE(22)
            cleared.writeUInt32LE(0, 22)
            assert.equal(o.crc(cleared), expected)
        }
    }
})
test("actual config defaults, truthiness, Number coercion and exact PUT bytes", () => {
    for (const config of [{ enabled: true }, { enabled: [], port: "0x1f40", mount: "music", password: 123 }, { enabled: "yes", port: [9000], password: true }, { enabled: true, host: "127.0.0.1", port: "bad", mount: "/x", password: "秘密" }, { enabled: true, port: false, mount: "", password: 0 }, ...["+inf", "-inf", "infinity", "\u00858000", "8000\u0085", "0b1111101000000", "\uFEFF8000\u00a0"].map((port) => ({ enabled: true, port }))]) {
        const o = oracle(),
            title = "演奏者 - Song 😀"
        o.sender.updateMetadata(title)
        o.sender.sendAudio(Buffer.from([3]), config)
        o.sockets[0].callback()
        const native = rust({ mode: "config", config, title })
        assert.equal(native.error, undefined)
        assert.equal(Buffer.from(native.ok.header, "base64").toString("hex"), o.sockets[0].writes[0].toString("hex"))
        o.sender.disconnect()
        o.flush()
    }
    for (const config of [null, false, "", 0]) assert.equal(rust({ mode: "config", config }).ok, null)
})
test("source exact60ms starvation,20ms timer and chained metadata reset", () => {
    const o = oracle()
    o.sender.sendAudio(Buffer.from([1]), { enabled: true })
    o.sockets[0].callback()
    const socket = o.sockets[0]
    assert.equal(socket.writes.length, 3)
    o.tick(20)
    o.tick(20)
    assert.equal(socket.writes.length, 3)
    o.tick(20)
    assert.equal(socket.writes.length, 4)
    assert.equal(socket.writes[3].subarray(28).toString("hex"), "f8fffe")
    o.sender.sendAudio(Buffer.from([7]), { enabled: true })
    o.tick(40)
    assert.equal(socket.writes.length, 5)
    o.sender.updateMetadata("Artist - Next")
    assert.equal(socket.writes.length, 7)
    assert.equal(socket.writes[5].readUInt32LE(18), 0)
    assert.equal(socket.writes[6].readBigUInt64LE(6), 0n)
    o.sender.updateMetadata("Artist - Next")
    assert.equal(socket.writes.length, 7)
    o.tick(60)
    assert.equal(socket.writes.at(-1).readBigUInt64LE(6), 960n)
    o.sender.disconnect()
    o.flush()
    assert.equal(o.intervals.size, 0)
    o.tick(3000)
    assert.equal(o.sockets.length, 1, "no disconnected timer reconnect")
})
test("source cooldown is send-triggered and rejection error is then cleared by close", () => {
    for (const [response, error] of [
        ["HTTP/1.1 409 Conflict\r\n", "Mountpoint in use"],
        ["Mountpoint in use", "Mountpoint in use"],
        ["HTTP/1.1 401 Unauthorized\r\nX: y", "HTTP/1.1 401 Unauthorized"],
        ["HTTP/1.1 403 Forbidden", "HTTP/1.1 403 Forbidden"],
        ["HTTP/1.1 404 Not Found", "HTTP/1.1 404 Not Found"]
    ]) {
        const o = oracle(),
            c = { enabled: true }
        o.sender.sendAudio(Buffer.from([1]), c)
        o.sockets[0].callback()
        o.sockets[0].emit("data", Buffer.from(response))
        o.flush()
        assert.deepEqual(o.statuses.slice(-2), [
            { connected: false, connecting: false, error },
            { connected: false, connecting: false }
        ])
        o.sender.sendAudio(Buffer.from([2]), c)
        assert.equal(o.sockets.length, 1)
        o.tick(2999)
        o.sender.sendAudio(Buffer.from([2]), c)
        assert.equal(o.sockets.length, 1)
        o.tick(1)
        assert.equal(o.sockets.length, 1)
        o.sender.sendAudio(Buffer.from([2]), c)
        assert.equal(o.sockets.length, 2)
        o.sender.disconnect()
        o.flush()
    }
})
test("source response tests operate on individual TCP data chunks and config updates bypass cooldown", () => {
    const o = oracle(),
        c = { enabled: true }
    o.sender.sendAudio(Buffer.from([1]), c)
    o.sockets[0].callback()
    o.sockets[0].emit("data", Buffer.from("HTTP/1.1 401 Un"))
    o.sockets[0].emit("data", Buffer.from("authorized"))
    assert.equal(o.sockets[0].destroyed, false)
    o.sender.disconnect()
    o.flush()
    o.sender.sendAudio(Buffer.from([2]), { ...c, mount: "other" })
    assert.equal(o.sockets.length, 2)
    o.sender.disconnect()
    o.flush()
})
test("pathological values are explicit unsupported/resource errors; source oversized page throws", () => {
    const o = oracle()
    o.sender.sendAudio(Buffer.from([1]), { enabled: true })
    o.sockets[0].callback()
    assert.throws(() => o.sender.sendAudio(Buffer.alloc(65025), { enabled: true }), /range|bounds|255/i)
    assert.equal(rust({ mode: "pages", packets: [Array(65025).fill(0)] }).error.code, "resource_limit")
    for (const config of [{ mount: true }, { host: 42 }, { port: -1 }, { port: "Infinity" }, { password: {} }, { mount: "x\r\nY: z" }]) assert.match(rust({ mode: "config", config }).error.code, /unsupported/)
    o.sender.disconnect()
    o.flush()
})

async function waitFor(predicate) {
    const until = Date.now() + 4000
    while (!predicate()) {
        assert.ok(Date.now() < until, "loopback condition timed out")
        await new Promise((r) => setTimeout(r, 1))
    }
}
async function server(response) {
    const chunks = [],
        sockets = new Set()
    const listener = net.createServer((socket) => {
        sockets.add(socket)
        socket.on("close", () => sockets.delete(socket))
        socket.on("error", () => {})
        let replied = false
        socket.on("data", (b) => {
            chunks.push(Buffer.from(b))
            if (response && !replied) {
                replied = true
                socket.write(response)
            }
        })
    })
    await new Promise((r) => listener.listen(0, "127.0.0.1", r))
    return {
        chunks,
        sockets,
        port: listener.address().port,
        async close() {
            for (const socket of sockets) socket.destroy()
            await new Promise((r) => listener.close(r))
        }
    }
}
async function nativeNetwork(commands) {
    const child = spawn(binary, [], { stdio: ["pipe", "pipe", "pipe"] }),
        out = [],
        err = []
    const timer = setTimeout(() => child.kill(), 8000)
    child.stdout.on("data", (b) => out.push(b))
    child.stderr.on("data", (b) => err.push(b))
    child.stdin.end(JSON.stringify({ mode: "network", commands }))
    const status = await new Promise((resolve, reject) => {
        child.on("error", reject)
        child.on("exit", resolve)
    })
    clearTimeout(timer)
    assert.equal(status, 0, Buffer.concat(err).toString())
    const result = JSON.parse(Buffer.concat(out).toString())
    assert.equal(result.error, undefined)
    return result.ok
}
test("actual source and native real loopback: exact header/pages after serial normalization, metadata chains and EOS", async () => {
    const a = await server(),
        b = await server(),
        o = oracle(true)
    const config = (port) => ({ enabled: true, host: "127.0.0.1", port, mount: "music", password: "秘密" })
    try {
        o.sender.updateMetadata("Artist - One")
        o.sender.sendAudio(Buffer.from([99]), config(a.port))
        await waitFor(() => o.statuses.some((s) => s.connected))
        o.sender.sendAudio(Buffer.from([1, 2, 3]), config(a.port))
        o.sender.updateMetadata("Artist - Two")
        o.sender.sendAudio(Buffer.from([4]), config(a.port))
        o.sender.disconnect()
        await waitFor(() => a.sockets.size === 0 && a.chunks.length > 0)
        const c = config(b.port),
            native = await nativeNetwork([{ op: "metadata", title: "Artist - One" }, { op: "send", packet: [99], config: c }, { op: "wait_connected" }, { op: "send", packet: [1, 2, 3], config: c }, { op: "metadata", title: "Artist - Two" }, { op: "send", packet: [4], config: c }, { op: "disconnect" }])
        await waitFor(() => b.sockets.size === 0 && b.chunks.length > 0)
        assert.equal(native.shutdown, null)
        assert.deepEqual(
            native.receipts.filter((r) => r.disposition).map((r) => r.disposition),
            ["Updated", "ConnectingDropped", "Written", "Updated", "Written", "Disconnected"]
        )
        const sourceRaw = rawPages(a.chunks, o.crc),
            nativeRaw = rawPages(b.chunks, o.crc)
        const normalized = ({ header, raw }) => ({ header: header.replace(/Host: 127\.0\.0\.1:\d+/, "Host: 127.0.0.1:PORT"), pages: raw.map((p) => normalize(p, o.crc)) })
        assert.deepEqual(normalized(nativeRaw), normalized(sourceRaw))
        assert.deepEqual(
            native.events.map((e) => e.data),
            o.statuses
        )
        if (process.env.FREESHOW_ICECAST_RECEIPT)
            fs.writeFileSync(
                process.env.FREESHOW_ICECAST_RECEIPT,
                JSON.stringify(
                    {
                        revision,
                        sourceSha256: createHash("sha256").update(source.stdout.replace(/\r\n/g, "\n")).digest("hex"),
                        node: process.version,
                        platform: process.platform,
                        arch: process.arch,
                        source: { header: sourceRaw.header, pages: sourceRaw.raw.map((p) => p.toString("hex")), statuses: o.statuses },
                        native: { header: nativeRaw.header, pages: nativeRaw.raw.map((p) => p.toString("hex")), statuses: native.events.map((e) => e.data) },
                        normalization: "Only Host port and Ogg serial/CRC; original raw checksums verified first"
                    },
                    null,
                    2
                ) + "\n"
            )
    } finally {
        o.sender.disconnect()
        await a.close()
        await b.close()
    }
})
test("native real loopback rejection/cooldown plus actual source status branch", async () => {
    for (const response of ["HTTP/1.1 401 Unauthorized", "HTTP/1.1 403 Forbidden", "HTTP/1.1 404 Not Found", "HTTP/1.1 409 Conflict"]) {
        const s = await server(response + "\r\n\r\n")
        try {
            const c = { enabled: true, host: "127.0.0.1", port: s.port }
            const result = await nativeNetwork([
                { op: "send", packet: [1], config: c },
                { op: "wait", ms: 50 },
                { op: "send", packet: [2], config: c }
            ])
            assert.equal(result.shutdown, null)
            assert.equal(result.receipts.at(-1).disposition, "CooldownDropped")
            assert.deepEqual(
                result.events.map((e) => e.data),
                [
                    { connected: false, connecting: true },
                    { connected: true, connecting: false },
                    { connected: false, connecting: false, error: response.includes("409") ? "Mountpoint in use" : response },
                    { connected: false, connecting: false }
                ]
            )
            assert.equal(result.events.find((e) => e.data.error).diagnostic.code, "server_rejected")
        } finally {
            await s.close()
        }
    }
})
test("committed paired raw-wire receipt validates immutable provenance and original CRCs before normalization", () => {
    const receipt = JSON.parse(fs.readFileSync(new URL("./icecast-loopback.receipt.json", import.meta.url), "utf8")),
        o = oracle()
    assert.equal(receipt.revision, revision)
    assert.equal(receipt.sourceSha256, createHash("sha256").update(source.stdout.replace(/\r\n/g, "\n")).digest("hex"))
    for (const side of [receipt.source, receipt.native])
        for (const hex of side.pages) {
            const page = Buffer.from(hex, "hex"),
                stored = page.readUInt32LE(22)
            page.writeUInt32LE(0, 22)
            assert.equal(o.crc(page), stored)
        }
    const normalized = (side) => ({ header: side.header.replace(/Host: 127\.0\.0\.1:\d+/, "Host: 127.0.0.1:PORT"), pages: side.pages.map((hex) => normalize(Buffer.from(hex, "hex"), o.crc)), statuses: side.statuses })
    assert.deepEqual(normalized(receipt.native), normalized(receipt.source))
})
