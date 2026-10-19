import assert from "node:assert/strict"
import { spawn } from "node:child_process"
import fs from "node:fs"
import http from "node:http"
import net from "node:net"
import os from "node:os"
import path from "node:path"
import { createInterface } from "node:readline"
import { test } from "node:test"
import { io } from "socket.io-client"
import { originalServers, root } from "./remotes-oracle.mjs"

const binary = process.env.FREESHOW_REMOTES_BIN || path.join(root, "target/debug/examples", process.platform === "win32" ? "remotes-parity.exe" : "remotes-parity")
const pause = (ms = 5) => new Promise((resolve) => setTimeout(resolve, ms))
async function until(get, ms = 3000) {
    const end = Date.now() + ms
    while (Date.now() < end) {
        const value = await get()
        if (value) return value
        await pause()
    }
    throw new Error("Remote condition timed out")
}
const startPayload = (extra = {}) => ({ ports: { remote: "0", stage: "0", controller: "0" }, max: 10, disabled: { remote: false, stage: false, controller: false }, data: { remote: { password: "renderer-auth-only" } }, ...extra })
function fixture() {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-remotes-test-"))
    const roots = {}
    for (const name of ["remote", "stage", "controller"]) {
        const directory = path.join(base, name)
        fs.mkdirSync(directory)
        fs.writeFileSync(path.join(directory, "index.html"), `<!doctype html><title>${name} TEST FIXTURE</title>`)
        fs.writeFileSync(path.join(directory, "asset.txt"), "Unicode été 漢字 0123456789\n")
        fs.mkdirSync(path.join(directory, "nested"))
        fs.writeFileSync(path.join(directory, "nested/index.html"), "nested fixture")
        fs.writeFileSync(path.join(directory, ".hidden"), "private")
        for (const extension of ["js", "css", "json", "svg", "woff2"]) {
            fs.writeFileSync(path.join(directory, `asset.${extension}`), `fixture ${extension}`)
        }
        roots[name] = directory
    }
    return { base, roots, close: () => fs.rmSync(base, { recursive: true, force: true }) }
}
function rust(limits = {}) {
    const child = spawn(binary, [], { stdio: ["pipe", "pipe", "pipe"], env: { ...process.env, FREESHOW_REMOTES_TEST_LIMITS: JSON.stringify({ handshake_ms: 300, body_ms: 1000, ...limits }) } })
    const pending = new Map(),
        events = []
    let next = 0,
        stderr = ""
    child.stderr.on("data", (chunk) => {
        stderr += chunk
        if (process.env.REMOTES_DEBUG) process.stderr.write(chunk)
    })
    createInterface({ input: child.stdout }).on("line", (line) => {
        const value = JSON.parse(line)
        if (value.event) events.push(value.event)
        else {
            const entry = pending.get(value.id)
            if (entry) {
                pending.delete(value.id)
                clearTimeout(entry.timer)
                entry.resolve(value)
            }
        }
    })
    child.on("exit", (code) => {
        for (const entry of pending.values()) {
            clearTimeout(entry.timer)
            entry.reject(new Error(`Remotes exited ${code}: ${stderr}`))
        }
    })
    const request = (input) =>
        new Promise((resolve, reject) => {
            const id = next++,
                timer = setTimeout(() => {
                    pending.delete(id)
                    reject(new Error(`Remotes timeout ${JSON.stringify(input)} ${stderr}`))
                }, 6000)
            pending.set(id, { resolve, reject, timer })
            child.stdin.write(JSON.stringify({ ...input, id }) + "\n")
        })
    return {
        rawEvents: events,
        get events() {
            return events.filter((event) => event.kind === "renderer").map((event) => ({ server: event.server, message: event.message, generation: event.generation, source: event.source_socket, sequence: event.sequence, origin: event.origin }))
        },
        request,
        async start(payload = startPayload(), context = 1) {
            const result = await request({ op: "start", data: payload, context })
            assert.equal(result.ok, true, JSON.stringify(result))
            this.generation = result.value.generation
            return Object.fromEntries(Object.entries(result.value.endpoints).map(([kind, address]) => [kind, Number(address.slice(address.lastIndexOf(":") + 1))]))
        },
        async send(op, server, data, generation = this.generation) {
            const result = await request({ op, server, data, generation })
            assert.equal(result.ok, true, JSON.stringify(result))
        },
        async close() {
            child.stdin.end()
            const code = await new Promise((resolve) => child.once("close", resolve))
            assert.equal(code, 0, stderr)
        },
        kill: () => child.kill()
    }
}
async function socket(port, kind, options = {}) {
    const client = io(`http://127.0.0.1:${port}`, { forceNew: true, reconnection: false, autoConnect: false, timeout: 2000, ...options })
    client.messages = []
    client.on(kind, (message) => client.messages.push(message))
    await new Promise((resolve, reject) => {
        client.once("connect", resolve)
        client.once("connect_error", reject)
        client.connect()
    })
    return client
}
async function httpRequest(port, { url = "/", method = "GET", headers = {}, body } = {}) {
    return new Promise((resolve, reject) => {
        const req = http.request({ hostname: "127.0.0.1", port, path: url, method, headers, agent: false }, (res) => {
            const chunks = []
            res.on("data", (chunk) => chunks.push(chunk))
            res.on("end", () => resolve({ status: res.statusCode, headers: res.headers, body: Buffer.concat(chunks) }))
            res.on("error", reject)
        })
        req.on("error", reject)
        req.setTimeout(3000, () => req.destroy(new Error("HTTP timeout")))
        req.end(body)
    })
}
async function liveOriginal(directory, payload = startPayload()) {
    const service = originalServers(directory)
    service.start(payload)
    const ports = await service.endpoints()
    return { service, ports }
}
async function barrier(service, kind, clients, source = false) {
    const marker = `barrier-${Date.now()}-${Math.random()}`
    if (source) service.renderer(kind, { channel: marker })
    else await service.send("renderer", kind, { channel: marker })
    await until(() => clients.every((client) => client.messages.some((message) => message?.channel === marker)))
}

test("actual source configuration/defaults, update data and first-match UA quirks", async () => {
    const files = fixture(),
        source = originalServers(files.base, { mockedListen: true }),
        service = rust()
    try {
        for (const disabled of [{}, { remote: false }, { remote: 0, stage: null, controller: "false" }, { remote: false, stage: false, controller: false }]) {
            const payload = { ports: { remote: 0, stage: 5521 }, disabled, data: { remote: false, stage: { password: "x" } }, max: "2" }
            source.start(payload)
            const result = await service.request({ op: "prepare", data: payload })
            assert.equal(result.ok, true)
            const selected = Object.keys(result.value.ports)
            assert.deepEqual(
                selected,
                Object.keys(disabled)
                    .filter((key) => disabled[key] === false)
                    .map((key) => key.toUpperCase())
            )
            for (const kind of selected) assert.equal(result.value.ports[kind], kind === "STAGE" ? 5521 : kind === "REMOTE" ? 5510 : 5512)
        }
        const ua = ["Linux Android", "Android", "Linux Windows Macintosh", "EF500 Android", "CrOS", "TC70 CT50", "DL-AXIS", "iPad iPhone", "IEMobile", "Something Else"]
        for (const value of ua) assert.equal((await service.request({ op: "os", data: value })).value, source.os(value))
        for (const data of [{ remote: { password: "a" }, stage: false }, [], false, "abc"]) {
            source.update(data)
            await service.request({ op: "update", data })
            for (const kind of ["REMOTE", "STAGE", "CONTROLLER"]) assert.deepEqual((await service.request({ op: "get_data", server: kind })).value, JSON.parse(JSON.stringify(source.data(kind))))
        }
        assert.equal((await service.request({ op: "prepare", data: { ports: {}, disabled: { output_stream: false }, data: {} } })).error.code, "unsupported_output_stream")
    } finally {
        await Promise.all([service.close(), source.close()])
        files.close()
    }
})

for (const transports of [["polling"], ["websocket"], ["polling", "websocket"]])
    test(`real source/native Socket.IO ${transports.join("+")} retains messages, UA and spoofable id`, async () => {
        const files = fixture(),
            original = await liveOriginal(files.base),
            service = rust({ roots: files.roots })
        const ports = await service.start()
        const clients = []
        try {
            for (const [backend, endpoints, isSource] of [
                [original.service, original.ports, true],
                [service, ports, false]
            ]) {
                const client = await socket(endpoints.REMOTE, "REMOTE", { transports, extraHeaders: { "User-Agent": "Linux Android" } })
                clients.push(client)
                await until(() => backend.events.some((event) => event.server === "REMOTE" && event.message.channel === "CONNECTION"))
                const connected = backend.events.find((event) => event.server === "REMOTE" && event.message.channel === "CONNECTION")
                assert.equal(connected.message.data.name, "Generic Linux")
                assert.equal(connected.message.id, client.id)
                if (transports.length === 2) await until(() => client.io.engine.transport.name === "websocket")
                client.emit("REMOTE", { channel: "PASSWORD", id: "unrelated-spoofed-id", data: { password: "wrong", text: "été 🙌🏾 漢字", zero: -0, nested: [false, null, 1.0000000000000002] } })
                await until(() => backend.events.some((event) => event.message?.channel === "PASSWORD"))
                const received = backend.events.find((event) => event.message?.channel === "PASSWORD")
                assert.equal(received.message.id, "unrelated-spoofed-id")
                assert.equal(received.message.data.password, "wrong")
                if (!isSource) assert.equal(received.source, client.id)
                await barrier(backend, "REMOTE", [client], isSource)
                client.disconnect()
                await until(() => backend.events.some((event) => event.message?.channel === "DISCONNECT" && event.message.id === connected.message.id))
            }
        } finally {
            for (const client of clients) client.disconnect()
            await Promise.all([service.close(), original.service.close()])
            files.close()
        }
    })

test("truthy client payloads retain missing ids and source sends no application ACK", async () => {
    const files = fixture(),
        original = await liveOriginal(files.base),
        service = rust()
    const ports = await service.start()
    const clients = []
    try {
        for (const [backend, endpoints] of [
            [original.service, original.ports],
            [service, ports]
        ]) {
            const client = await socket(endpoints.REMOTE, "REMOTE", { transports: ["websocket"] })
            clients.push(client)
            await until(() => backend.events.some((event) => event.message?.channel === "CONNECTION"))
            const before = backend.events.length
            for (const value of [{ channel: "NO_ID", data: "untouched" }, "truthy string", ["array", { n: 2 }], 17, true, false, 0, ""]) client.emit("REMOTE", value)
            let ack = false
            client.emit("REMOTE", { channel: "NO_ACK" }, () => {
                ack = true
            })
            client.emit("REMOTE", { channel: "FENCE" })
            await until(() => backend.events.some((event) => event.message?.channel === "FENCE"))
            assert.deepEqual(
                backend.events.slice(before).map((event) => event.message),
                [{ channel: "NO_ID", data: "untouched" }, "truthy string", ["array", { n: 2 }], 17, true, { channel: "NO_ACK" }, { channel: "FENCE" }]
            )
            assert.equal(ack, false)
        }
    } finally {
        for (const client of clients) client.disconnect()
        await Promise.all([service.close(), original.service.close()])
        files.close()
    }
})

test("trusted native lifecycle origin cannot be selected by a real client's channel or id", async () => {
    const files = fixture(),
        original = await liveOriginal(files.base),
        service = rust()
    const ports = await service.start()
    const clients = []
    try {
        for (const [backend, endpoints, isSource] of [
            [original.service, original.ports, true],
            [service, ports, false]
        ]) {
            const client = await socket(endpoints.REMOTE, "REMOTE", { transports: ["websocket"] })
            clients.push(client)
            await until(() => backend.events.some((event) => event.message?.channel === "CONNECTION"))
            const connected = backend.events.find((event) => event.message?.channel === "CONNECTION")
            if (!isSource) {
                assert.equal(connected.origin, "connection")
                assert.equal(connected.source, client.id)
            }
            const forged = [
                { channel: "DISCONNECT", id: client.id, origin: "disconnect", source_socket: client.id },
                { channel: "CONNECTION", id: client.id, data: { name: "client chosen" }, origin: "connection" }
            ]
            for (const message of forged) client.emit("REMOTE", message)
            client.emit("REMOTE", { channel: "AFTER_FORGED_LIFECYCLE" })
            await until(() => backend.events.some((event) => event.message?.channel === "AFTER_FORGED_LIFECYCLE"))
            for (const message of forged) {
                const received = backend.events.find((event) => event.message?.origin === message.origin)
                assert.deepEqual(received.message, message)
                if (!isSource) {
                    assert.equal(received.origin, "client")
                    assert.equal(received.source, client.id)
                }
            }
            assert.equal(client.connected, true)
            const id = client.id
            client.disconnect()
            await until(() => backend.events.filter((event) => event.message?.channel === "DISCONNECT").length === 2)
            if (!isSource) {
                const disconnected = backend.events.find((event) => event.origin === "disconnect")
                assert.deepEqual(disconnected.message, { channel: "DISCONNECT", id })
                assert.equal(disconnected.source, id)
            }
        }
    } finally {
        for (const client of clients) client.disconnect()
        await Promise.all([service.close(), original.service.close()])
        files.close()
    }
})

test("renderer targets truthy room ids while backend broadcasts ignore ids, including array quirks", async () => {
    const files = fixture(),
        original = await liveOriginal(files.base),
        service = rust()
    const ports = await service.start()
    const clients = []
    try {
        for (const [backend, endpoints, isSource] of [
            [original.service, original.ports, true],
            [service, ports, false]
        ]) {
            const a = await socket(endpoints.CONTROLLER, "CONTROLLER"),
                b = await socket(endpoints.CONTROLLER, "CONTROLLER")
            clients.push(a, b)
            await until(() => backend.events.filter((event) => event.server === "CONTROLLER" && event.message.channel === "CONNECTION").length === 2)
            const send = async (op, message) => {
                if (isSource) backend[op]("CONTROLLER", message)
                else await backend.send(op, "CONTROLLER", message)
            }
            for (const [id, expected] of [
                [a.id, [true, false]],
                ["unknown", [false, false]],
                [0, [true, true]],
                [false, [true, true]],
                ["", [true, true]],
                [[], [true, true]],
                [
                    [a.id, b.id, a.id],
                    [true, true]
                ],
                [[{}], [false, false]],
                [{}, [false, false]]
            ]) {
                const channel = `target-${Math.random()}`
                await send("renderer", { channel, id })
                await barrier(backend, "CONTROLLER", [a, b], isSource)
                assert.deepEqual(
                    [a, b].map((client) => client.messages.some((message) => message.channel === channel)),
                    expected
                )
            }
            const channel = "backend-ignores-id"
            await send("backend", { channel, id: a.id })
            await barrier(backend, "CONTROLLER", [a, b], isSource)
            assert.ok(a.messages.some((m) => m.channel === channel))
            assert.ok(b.messages.some((m) => m.channel === channel))
        }
    } finally {
        for (const client of clients) client.disconnect()
        await Promise.all([service.close(), original.service.close()])
        files.close()
    }
})

test("stage subscriptions use real socket ids, renew TTL inclusive boundary, and deliver only to subscribers", async () => {
    const files = fixture(),
        original = await liveOriginal(files.base),
        service = rust()
    const ports = await service.start()
    const clients = []
    try {
        for (const [backend, endpoints, isSource] of [
            [original.service, original.ports, true],
            [service, ports, false]
        ]) {
            const first = await socket(endpoints.STAGE, "STAGE"),
                second = await socket(endpoints.STAGE, "STAGE")
            const [a, b] = [first, second].sort((left, right) => (left.id < right.id ? 1 : -1))
            clients.push(a, b)
            a.emit("STAGE", { channel: "STREAM_SUBSCRIBE", id: b.id })
            await until(() => backend.events.some((event) => event.message?.channel === "STREAM_SUBSCRIBE"))
            const subs = async () => (isSource ? backend.subscribers() : (await backend.request({ op: "subscribers", generation: backend.generation })).value)
            const clock = async (millis) => (isSource ? backend.clock(millis) : backend.request({ op: "clock", millis }))
            const stage = async (message) => (isSource ? backend.stage(message) : backend.send("stage", "STAGE", message))
            assert.deepEqual(await subs(), [a.id])
            b.emit("STAGE", { channel: "STREAM_SUBSCRIBE" })
            await until(() => backend.events.filter((event) => event.message?.channel === "STREAM_SUBSCRIBE").length === 2)
            a.emit("STAGE", { channel: "STREAM_SUBSCRIBE" })
            await until(() => backend.events.filter((event) => event.message?.channel === "STREAM_SUBSCRIBE").length === 3)
            assert.deepEqual(await subs(), [a.id, b.id]) // renewal retains source insertion order
            b.emit("STAGE", { channel: "STREAM_UNSUBSCRIBE" })
            await until(() => backend.events.some((event) => event.message?.channel === "STREAM_UNSUBSCRIBE"))
            assert.deepEqual(await subs(), [a.id])
            await stage({ channel: "FRAME_FIXTURE", data: "no capture fabricated" })
            await barrier(backend, "STAGE", [a, b], isSource)
            assert.ok(a.messages.some((m) => m.channel === "FRAME_FIXTURE"))
            assert.ok(!b.messages.some((m) => m.channel === "FRAME_FIXTURE"))
            await clock(11000)
            assert.deepEqual(await subs(), [a.id])
            await clock(11001)
            assert.deepEqual(await subs(), [])
            a.emit("STAGE", { channel: "STREAM_SUBSCRIBE" })
            await until(() => backend.events.filter((event) => event.message?.channel === "STREAM_SUBSCRIBE").length === 4)
            await clock(20000)
            a.emit("STAGE", { channel: "STREAM_SUBSCRIBE" })
            await until(() => backend.events.filter((event) => event.message?.channel === "STREAM_SUBSCRIBE").length === 5)
            await clock(21002)
            assert.deepEqual(await subs(), [a.id])
            a.emit("STAGE", { channel: "STREAM_UNSUBSCRIBE" })
            await until(() => backend.events.filter((event) => event.message?.channel === "STREAM_UNSUBSCRIBE").length === 2)
            assert.deepEqual(await subs(), [])
        }
    } finally {
        for (const client of clients) client.disconnect()
        await Promise.all([service.close(), original.service.close()])
        files.close()
    }
})

test("soft max preserves JS comparison coercion and broadcasts overLimit to existing clients", async () => {
    const files = fixture(),
        source = originalServers(files.base),
        service = rust()
    const clients = []
    try {
        for (const max of [undefined, null, false, 0, -1, "", [], 1, "1", 1.5, "bad", {}, [2], "Infinity", "0x2"]) {
            const payload = startPayload({ max, disabled: { remote: false } })
            source.start(payload)
            const sourcePorts = await source.endpoints(),
                ports = await service.start(payload)
            for (const [backend, endpoints] of [
                [source, sourcePorts],
                [service, ports]
            ]) {
                const before = backend.events.filter((event) => event.server === "REMOTE" && event.message.channel === "CONNECTION").length
                const a = await socket(endpoints.REMOTE, "REMOTE"),
                    b = await socket(endpoints.REMOTE, "REMOTE")
                clients.push(a, b)
                const expected = Number(max) <= 0 ? 0 : Number(max) === 1 ? 1 : 2
                await until(() => (expected === 0 ? a.messages.some((m) => m?.id === "overLimit") && b.messages.some((m) => m?.id === "overLimit") : backend.events.filter((event) => event.server === "REMOTE" && event.message.channel === "CONNECTION").length >= before + expected))
                assert.equal(backend.events.filter((event) => event.server === "REMOTE" && event.message.channel === "CONNECTION").length, before + expected)
                if (expected === 1) {
                    await until(() => a.messages.some((m) => m?.id === "overLimit") && b.messages.some((m) => m?.id === "overLimit"))
                    assert.equal(a.messages.find((m) => m?.id === "overLimit").data, max)
                }
                if (max === 1.5) {
                    const third = await socket(endpoints.REMOTE, "REMOTE")
                    clients.push(third)
                    await until(() => third.messages.some((m) => m?.id === "overLimit"))
                    assert.equal(backend.events.filter((event) => event.server === "REMOTE" && event.message.channel === "CONNECTION").length, before + 2)
                    third.disconnect()
                }
                a.disconnect()
                b.disconnect()
            }
            await source.close()
            await service.request({ op: "stop" })
        }
    } finally {
        for (const client of clients) client.disconnect()
        await Promise.all([service.close(), source.close()])
        files.close()
    }
})

test("finite raw packet numbers match JS parse; overflow and binary gaps fail explicitly", async () => {
    const files = fixture(),
        original = await liveOriginal(files.base),
        service = rust()
    const ports = await service.start(),
        clients = []
    try {
        for (const [backend, endpoints, isSource] of [
            [original.service, original.ports, true],
            [service, ports, false]
        ]) {
            const client = await socket(endpoints.REMOTE, "REMOTE", { transports: ["websocket"] })
            clients.push(client)
            const raw = '{"channel":"NUMBERS","data":{"one":1.0000000000000002,"ulp":8.3163996793004788882e-275,"large":9007199254740993,"tiny":5e-324,"z":-0,"b":1,"b":2}}'
            client.io.engine.write(`2["REMOTE",${raw}]`)
            await until(() => backend.events.some((event) => event.message?.channel === "NUMBERS"))
            const actual = backend.events.find((event) => event.message?.channel === "NUMBERS").message
            assert.deepEqual(actual, JSON.parse(raw))
            if (isSource) {
                client.emit("REMOTE", { channel: "BINARY", data: Buffer.from([1, 2, 3]) })
                await until(() => backend.events.some((event) => event.message?.channel === "BINARY"))
                assert.ok(Buffer.isBuffer(backend.events.find((event) => event.message?.channel === "BINARY").message.data))
                client.io.engine.write('2["REMOTE",{"channel":"OVERFLOW","data":1e309}]')
                await until(() => backend.events.some((event) => event.message?.channel === "OVERFLOW"))
                assert.equal(backend.events.find((event) => event.message?.channel === "OVERFLOW").message.data, Infinity)
            }
            if (!isSource) {
                client.emit("REMOTE", { channel: "BINARY", data: Buffer.from([1, 2, 3]) })
                await until(() => service.rawEvents.some((event) => event.kind === "diagnostic" && event.code === "unsupported_binary"))
                await until(() => !client.connected)
                const overflow = await socket(endpoints.REMOTE, "REMOTE", { transports: ["websocket"] })
                clients.push(overflow)
                overflow.io.engine.write('2["REMOTE",{"channel":"OVERFLOW","data":1e309}]')
                await until(() => service.rawEvents.some((event) => event.code === "json_number_overflow"))
                await until(() => !overflow.connected)
            }
        }
    } finally {
        for (const client of clients) client.disconnect()
        await Promise.all([service.close(), original.service.close()])
        files.close()
    }
})

test("explicit trusted static fixtures match Express GET/HEAD/range/cache/directory behavior", async () => {
    const files = fixture(),
        original = await liveOriginal(files.base),
        service = rust({ roots: files.roots })
    const ports = await service.start()
    try {
        for (const options of [{ url: "/" }, { url: "/asset.txt" }, { url: "/asset.txt", method: "HEAD" }, { url: "/asset.txt", headers: { range: "bytes=2-7" } }, { url: "/asset.txt", headers: { range: "bytes=-5" } }, { url: "/nested" }, { url: "/nested/" }, { url: "/.hidden" }, { url: "/missing" }, ...["js", "css", "json", "svg", "woff2"].map((extension) => ({ url: `/asset.${extension}` }))]) {
            const expected = await httpRequest(original.ports.REMOTE, options),
                actual = await httpRequest(ports.REMOTE, options)
            assert.equal(actual.status, expected.status, JSON.stringify(options))
            if (expected.status === 200 || expected.status === 206) assert.deepEqual(actual.body, expected.body)
            for (const key of ["content-length", "content-type", "content-range", "location", "etag", "last-modified", "cache-control", "accept-ranges"]) if (expected.status === 200 || expected.status === 206 || key === "location") assert.equal(actual.headers[key], expected.headers[key], `${key} ${JSON.stringify(options)}`)
        }
        const expected = await httpRequest(original.ports.REMOTE, { url: "/asset.txt" })
        for (const headers of [{ "if-none-match": expected.headers.etag }, { "if-modified-since": expected.headers["last-modified"] }]) {
            assert.equal((await httpRequest(ports.REMOTE, { url: "/asset.txt", headers })).status, 304)
        }
        for (const headers of [
            { "if-match": expected.headers.etag },
            { "if-match": '"wrong"' },
            { "if-match": "*" },
            { "if-unmodified-since": "Thu, 01 Jan 1970 00:00:00 GMT" },
            { "if-match": expected.headers.etag, "if-unmodified-since": "Thu, 01 Jan 1970 00:00:00 GMT" },
            { "if-none-match": expected.headers.etag, "if-modified-since": "Thu, 01 Jan 1970 00:00:00 GMT" },
            { "if-none-match": expected.headers.etag, "cache-control": "public, no-cache" },
            { range: "bytes=1-4", "if-range": expected.headers.etag },
            { range: "bytes=1-4", "if-range": '"wrong"' },
            { range: "bytes=1-4", "if-range": expected.headers["last-modified"] }
        ]) {
            const a = await httpRequest(original.ports.REMOTE, { url: "/asset.txt", headers }),
                b = await httpRequest(ports.REMOTE, { url: "/asset.txt", headers })
            assert.equal(b.status, a.status, JSON.stringify(headers))
            if (a.status === 200 || a.status === 206) assert.deepEqual(a.body, b.body)
        }
        assert.equal((await httpRequest(ports.REMOTE, { url: "/nowplayingcover" })).status, 501)
    } finally {
        await Promise.all([service.close(), original.service.close()])
        files.close()
    }
})

test("abandoned Engine.IO polling sessions are leased across requests and reclaim on timeout/stop", async () => {
    const service = rust({ sessions: 2, handshake_ms: 150 })
    const ports = await service.start(startPayload({ disabled: { remote: false } }))
    try {
        const a = await httpRequest(ports.REMOTE, { url: "/socket.io/?EIO=4&transport=polling" }),
            b = await httpRequest(ports.REMOTE, { url: "/socket.io/?EIO=4&transport=polling" })
        assert.equal(a.status, 200)
        assert.equal(b.status, 200)
        assert.equal((await service.request({ op: "stats" })).value.engine_sessions, 2)
        assert.equal((await httpRequest(ports.REMOTE, { url: "/socket.io/?EIO=4&transport=polling" })).status, 503)
        for (const sid of ["bad", "%62ad", "%21", "a&sid=b"]) {
            assert.equal((await httpRequest(ports.REMOTE, { url: `/socket.io/?EIO=4&transport=polling&sid=${sid}` })).status, 400)
        }
        await until(async () => (await service.request({ op: "stats" })).value.engine_sessions === 0)
        const client = await socket(ports.REMOTE, "REMOTE")
        client.disconnect()
        await service.request({ op: "stop" })
        const fresh = await service.start(startPayload({ disabled: { remote: false } }))
        assert.ok(fresh.REMOTE)
    } finally {
        await service.close()
    }
})

test("restart rejects old generations, rebinds and shutdown needs no event-drain capacity", async () => {
    const service = rust({ events: 1, clients: 2 })
    const ports = await service.start(startPayload({ disabled: { remote: false } })),
        old = service.generation
    let client
    try {
        client = await socket(ports.REMOTE, "REMOTE")
        await until(() => service.events.some((event) => event.message.channel === "CONNECTION"))
        await service.request({ op: "drain", enabled: false })
        for (let i = 0; i < 20; i++) client.emit("REMOTE", { channel: "FLOOD", data: i })
        await until(async () => (await service.request({ op: "stats" })).value.clients.REMOTE === 0)
        const stopped = await service.request({ op: "stop" })
        assert.equal(stopped.ok, true)
        await service.request({ op: "drain", enabled: true })
        await until(() => service.events.some((event) => event.message.channel === "DISCONNECT"))
        await service.start(startPayload({ disabled: { remote: false }, ports: { remote: ports.REMOTE } }), 2)
        assert.notDeepEqual(service.generation, old)
        const stale = await service.request({ op: "renderer", server: "REMOTE", generation: old, data: { channel: "OLD" } })
        assert.equal(stale.error.code, "stale_generation")
        const next = await socket(ports.REMOTE, "REMOTE")
        next.disconnect()
    } finally {
        client?.disconnect()
        await service.close()
    }
})

test("raw CONNECT flooding cannot grow namespace tasks and normal root reconnect remains live", async () => {
    const service = rust({ sessions: 2, handshake_ms: 1000 })
    const ports = await service.start(startPayload({ disabled: { remote: false } }))
    let client
    try {
        const raw = new WebSocket(`ws://127.0.0.1:${ports.REMOTE}/socket.io/?EIO=4&transport=websocket`)
        const frames = []
        raw.addEventListener("message", (event) => frames.push(String(event.data)))
        await until(() => frames.some((frame) => frame.startsWith("0")))
        const before = (await service.request({ op: "stats" })).value.runtime_tasks
        let peak = before
        for (let batch = 0; batch < 20; batch++) {
            for (let i = 0; i < 100; i++) raw.send("40")
            peak = Math.max(peak, (await service.request({ op: "stats" })).value.runtime_tasks)
        }
        await until(() => service.events.some((event) => event.message.channel === "CONNECTION"))
        assert.ok(peak <= before + 4, `pre-namespace CONNECT task peak ${peak}, baseline ${before}`)
        raw.send('42["REMOTE",{"channel":"RAW_FLOOD_DONE"}]')
        await until(() => service.events.some((event) => event.message.channel === "RAW_FLOOD_DONE"))
        raw.close()
        await until(() => service.events.some((event) => event.message.channel === "DISCONNECT"))
        client = await socket(ports.REMOTE, "REMOTE", { transports: ["websocket"] })
        await until(() => service.events.filter((event) => event.message.channel === "CONNECTION").length === 2)
        const baseline = (await service.request({ op: "stats" })).value.runtime_tasks
        for (let i = 0; i < 2000; i++) client.io.engine.write("0")
        await pause(30)
        const after = (await service.request({ op: "stats" })).value
        assert.equal(after.clients.REMOTE, 1)
        assert.ok(after.runtime_tasks <= baseline + 3, `${after.runtime_tasks} > ${baseline}`)
        client.emit("REMOTE", { channel: "AFTER_FLOOD" })
        await until(() => service.events.some((event) => event.message.channel === "AFTER_FLOOD"))
        client.disconnect()
        await until(() => service.events.filter((event) => event.message.channel === "DISCONNECT").length === 2)
        const next = await socket(ports.REMOTE, "REMOTE", { transports: ["websocket"] })
        next.emit("REMOTE", { channel: "RECONNECTED" })
        await until(() => service.events.some((event) => event.message.channel === "RECONNECTED"))
        next.disconnect()
    } finally {
        client?.disconnect()
        await service.close()
    }
})

test("bounded Socket.IO output failure recovers after the client drains polling packets", async () => {
    const service = rust({ socket_queue: 2 })
    const ports = await service.start(startPayload({ disabled: { remote: false } }))
    try {
        const response = await httpRequest(ports.REMOTE, { url: "/socket.io/?EIO=4&transport=polling" })
        const sid = JSON.parse(response.body.toString().slice(1)).sid
        const url = `/socket.io/?EIO=4&transport=polling&sid=${sid}`
        assert.equal((await httpRequest(ports.REMOTE, { url, method: "POST", headers: { "content-type": "text/plain;charset=UTF-8" }, body: "40" })).status, 200)
        await until(() => service.events.some((event) => event.message.channel === "CONNECTION"))
        const failed = await service.request({ op: "renderer", server: "REMOTE", generation: service.generation, data: { channel: "BUFFERED" } })
        if (failed.ok) {
            const second = await service.request({ op: "renderer", server: "REMOTE", generation: service.generation, data: { channel: "BUFFERED_AGAIN" } })
            assert.equal(second.error.code, "socket_output_full")
        } else assert.equal(failed.error.code, "socket_output_full")
        const ack = await httpRequest(ports.REMOTE, { url })
        assert.ok(ack.body.toString().includes("40"))
        await service.send("renderer", "REMOTE", { channel: "RECOVERED" })
        assert.ok((await httpRequest(ports.REMOTE, { url })).body.toString().includes("RECOVERED"))
    } finally {
        await service.close()
    }
})

test("fixed static workers and queued streams stay bounded and shutdown cancels slow readers", async () => {
    const files = fixture()
    fs.writeFileSync(path.join(files.roots.remote, "stream.bin"), Buffer.alloc(32 * 1024 * 1024, 42))
    const service = rust({ roots: files.roots, file_workers: 1, file_queue: 1, body_ms: 5000, static_bytes: 64 * 1024 * 1024 })
    const ports = await service.start(startPayload({ disabled: { remote: false } }))
    const slow = []
    try {
        for (let i = 0; i < 2; i++) {
            const socket = net.createConnection({ host: "127.0.0.1", port: ports.REMOTE })
            slow.push(socket)
            await new Promise((resolve) => socket.once("connect", resolve))
            // A reserved admission can still be waiting in the one-slot queue.
            // Real response headers prove the sole file worker dequeued/opened
            // the first stream before the second request fills that queue.
            const firstHeaders =
                i === 0
                    ? new Promise((resolve, reject) => {
                          let received = Buffer.alloc(0)
                          const timer = setTimeout(() => {
                              cleanup()
                              reject(new Error("Static worker did not produce response headers"))
                          }, 3000)
                          const cleanup = () => {
                              clearTimeout(timer)
                              socket.off("data", data)
                              socket.off("error", failed)
                          }
                          const failed = (error) => {
                              cleanup()
                              reject(error)
                          }
                          const data = (chunk) => {
                              received = Buffer.concat([received, chunk])
                              const end = received.indexOf("\r\n\r\n")
                              if (end >= 0) {
                                  socket.pause()
                                  cleanup()
                                  resolve(received.subarray(0, end).toString())
                              } else if (received.length > 65536) {
                                  cleanup()
                                  reject(new Error("Static response header exceeds fixture bound"))
                              }
                          }
                          socket.on("data", data)
                          socket.on("error", failed)
                      })
                    : null
            socket.write("GET /stream.bin HTTP/1.1\r\nHost: localhost\r\n\r\n")
            if (i === 0) {
                assert.match(await firstHeaders, /^HTTP\/1\.1 200/)
                await until(async () => (await service.request({ op: "stats" })).value.static_requests === 1)
            }
        }
        await until(async () => (await service.request({ op: "stats" })).value.static_requests === 2)
        assert.equal((await httpRequest(ports.REMOTE, { url: "/asset.txt" })).status, 503)
        assert.equal((await service.request({ op: "stop" })).ok, true)
    } finally {
        for (const socket of slow) socket.destroy()
        await service.close()
        files.close()
    }
})

test("atomic bind failure closes previous ownership and leaves no advertised partial server", async () => {
    const service = rust()
    const occupied = net.createServer()
    await new Promise((resolve) => occupied.listen(0, "127.0.0.1", resolve))
    const port = occupied.address().port
    try {
        const result = await service.request({ op: "start", data: startPayload({ ports: { remote: "0", stage: port, controller: "0" } }) })
        assert.equal(result.error.code, "bind_failed")
        assert.equal((await service.request({ op: "stats" })).value, null)
        const ports = await service.start()
        assert.equal(Object.keys(ports).length, 3)
    } finally {
        await new Promise((resolve) => occupied.close(resolve))
        await service.close()
    }
})

test("backend STREAM response gating consumes STREAM_DONE and preserves state across restart", async () => {
    const files = fixture(),
        original = await liveOriginal(files.base),
        service = rust()
    const initial = await service.start()
    const clients = []
    try {
        for (const [backend, endpoints, isSource] of [
            [original.service, original.ports, true],
            [service, initial, false]
        ]) {
            let client = await socket(endpoints.REMOTE, "REMOTE")
            clients.push(client)
            const send = async (message) => (isSource ? backend.backend("REMOTE", message) : backend.send("backend", "REMOTE", message))
            await send({ channel: "STREAM", data: { id: "slot", n: 1 } })
            await send({ channel: "STREAM", data: { id: "slot", n: 2 } })
            await barrier(backend, "REMOTE", [client], isSource)
            assert.deepEqual(
                client.messages.filter((m) => m.channel === "STREAM").map((m) => m.data.n),
                [1]
            )
            client.emit("REMOTE", { channel: "STREAM_DONE", data: { id: "slot" } })
            client.emit("REMOTE", { channel: "AFTER_DONE" })
            await until(() => backend.events.some((event) => event.message?.channel === "AFTER_DONE"))
            assert.ok(!backend.events.some((event) => event.message?.channel === "STREAM_DONE"))
            await send({ channel: "STREAM", data: { id: "slot", n: 3 } })
            for (let n = 0; n < 2; n++) await send({ channel: "STREAM", data: { id: "__proto__", n: 4 + n } })
            await barrier(backend, "REMOTE", [client], isSource)
            assert.deepEqual(
                client.messages.filter((m) => m.channel === "STREAM").map((m) => m.data.n),
                [1, 3, 4, 5]
            )
            client.disconnect()
            let fresh
            if (isSource) {
                backend.start(startPayload())
                fresh = await backend.endpoints()
            } else fresh = await backend.start()
            client = await socket(fresh.REMOTE, "REMOTE")
            clients.push(client)
            await send({ channel: "STREAM", data: { id: "slot", n: 6 } })
            await barrier(backend, "REMOTE", [client], isSource)
            assert.ok(!client.messages.some((m) => m.channel === "STREAM" && m.data.n === 6))
        }
    } finally {
        for (const client of clients) client.disconnect()
        await Promise.all([service.close(), original.service.close()])
        files.close()
    }
})

test("hard client/resource caps and unsupported capture fail explicitly without inventing auth", async () => {
    const service = rust({ clients: 1, response_ids: 1 })
    const ports = await service.start(startPayload({ max: 100, disabled: { remote: false } }))
    const clients = []
    try {
        const first = await socket(ports.REMOTE, "REMOTE")
        clients.push(first)
        await until(() => service.events.some((event) => event.message.channel === "CONNECTION"))
        const second = await socket(ports.REMOTE, "REMOTE")
        clients.push(second)
        await until(() => service.rawEvents.some((event) => event.kind === "diagnostic" && event.code === "client_limit"))
        await until(() => !second.connected)
        first.emit("REMOTE", { channel: "OUTPUT_FRAME", data: { outputId: "fixture" } })
        await until(() => service.rawEvents.some((event) => event.code === "unsupported_output_frame"))
        assert.ok(!service.events.some((event) => event.message.channel === "OUTPUT_FRAME"))
        await service.send("backend", "REMOTE", { channel: "STREAM", data: { id: "first" } })
        const limited = await service.request({ op: "backend", server: "REMOTE", generation: service.generation, data: { channel: "STREAM", data: { id: "second" } } })
        assert.equal(limited.error.code, "response_id_limit")
    } finally {
        for (const client of clients) client.disconnect()
        await service.close()
    }
})

test("static size/traversal/symlink policy and body/header limits release resources on stop", async () => {
    const files = fixture()
    fs.writeFileSync(path.join(files.roots.remote, "large.bin"), Buffer.alloc(100_000, 42))
    const service = rust({ roots: files.roots, static_bytes: 1000, tcp: 4, body_ms: 100 })
    const ports = await service.start(startPayload({ disabled: { remote: false } }))
    let slow
    try {
        assert.equal((await httpRequest(ports.REMOTE, { url: "/large.bin" })).status, 413)
        for (const [url, status] of [
            ["/%GG", 400],
            ["/%FF", 400],
            ["/a%00", 400],
            ["/%2e%2e/private", 403]
        ])
            assert.equal((await httpRequest(ports.REMOTE, { url })).status, status, url)
        if (process.platform !== "win32") {
            const outside = path.join(files.base, "outside.txt")
            fs.writeFileSync(outside, "controlled outside-root fixture")
            fs.symlinkSync(outside, path.join(files.roots.remote, "link.txt"))
            assert.equal((await httpRequest(ports.REMOTE, { url: "/link.txt" })).status, 403)
        }
        slow = net.createConnection({ host: "127.0.0.1", port: ports.REMOTE })
        await new Promise((resolve) => slow.once("connect", resolve))
        slow.write("GET / HTTP/1.1\r\nHost: ")
        await until(async () => (await service.request({ op: "stats" })).value.tcp_connections >= 1)
        assert.equal((await service.request({ op: "stop" })).ok, true)
        assert.equal((await service.request({ op: "stats" })).value, null)
    } finally {
        slow?.destroy()
        await service.close()
        files.close()
    }
})
