import assert from "node:assert/strict"
import { spawn } from "node:child_process"
import dgram from "node:dgram"
import fs from "node:fs"
import http from "node:http"
import net from "node:net"
import path from "node:path"
import { createInterface } from "node:readline"
import { test } from "node:test"
import { gzipSync, deflateSync } from "node:zlib"
import { io } from "socket.io-client"
import { apiOracle, ActualOSC, sourceAuth, sourceEncode, queryOracle, root } from "./api-oracle.mjs"

const binary = process.env.FREESHOW_API_BIN || path.join(root, "target/debug/examples", process.platform === "win32" ? "api-parity.exe" : "api-parity")
const pause = (ms = 10) => new Promise((resolve) => setTimeout(resolve, ms))
async function until(get, ms = 2000) {
    const end = Date.now() + ms
    while (Date.now() < end) {
        const value = await get()
        if (value) return value
        await pause(2)
    }
    throw new Error("condition timed out")
}
async function freePort() {
    for (let attempt = 0; attempt < 10; attempt++) {
        const first = net.createServer()
        await new Promise((resolve) => first.listen(0, "127.0.0.1", resolve))
        const port = first.address().port
        const second = net.createServer()
        const udp = dgram.createSocket("udp4")
        try {
            await new Promise((resolve, reject) => {
                second.once("error", reject)
                second.listen(port + 1, "127.0.0.1", resolve)
            })
            await new Promise((resolve, reject) => {
                udp.once("error", reject)
                udp.bind(port, "127.0.0.1", resolve)
            })
            return port
        } catch {
        } finally {
            await Promise.all([first, second].map((server) => (server.listening ? new Promise((resolve) => server.close(resolve)) : null)))
            try {
                udp.close()
            } catch {}
        }
    }
    throw new Error("unable to reserve loopback API ports")
}
function rust(limits = {}) {
    const child = spawn(binary, [], { stdio: ["pipe", "pipe", "pipe"], env: { ...process.env, FREESHOW_API_TEST_LIMITS: JSON.stringify({ reply_ms: 120, dynamic_ms: 150, handshake_ms: 300, ...limits }) } })
    const pending = new Map(),
        events = [],
        observers = new Set()
    let next = 0,
        stderr = ""
    child.stderr.on("data", (value) => {
        stderr += value
        if (process.env.API_DEBUG) process.stderr.write(value)
    })
    createInterface({ input: child.stdout }).on("line", (line) => {
        const value = JSON.parse(line)
        if (value.event) {
            events.push(value.event)
            for (const observer of observers) observer(value.event)
        } else {
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
            entry.reject(new Error(`API harness exited ${code}: ${stderr}`))
        }
    })
    const request = (input) =>
        new Promise((resolve, reject) => {
            const id = next++
            const timer = setTimeout(() => reject(new Error(`API harness timeout: ${JSON.stringify(input)} ${stderr}`)), 5000)
            pending.set(id, { resolve, reject, timer })
            child.stdin.write(JSON.stringify({ ...input, id }) + "\n")
        })
    return {
        events,
        request,
        observe(callback) {
            observers.add(callback)
            return () => observers.delete(callback)
        },
        async start(data, context = 1) {
            const result = await request({ op: "start", data, context })
            assert.equal(result.ok, true, JSON.stringify(result))
            return result.value
        },
        async complete(data, generation) {
            return request({ op: "complete", data, generation })
        },
        async dynamic(listenerId, data, generation) {
            return request({ op: "dynamic", listenerId, data, generation })
        },
        async stop() {
            const result = await request({ op: "stop" })
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
function autoReply(service) {
    return service.observe((event) => {
        if (event.kind === "trigger" && event.data.returnId) void service.complete({ ...event.data, data: { text: "🙌🏾", falseValue: false, count: 0 } }, event.generation)
        if (event.kind === "dynamic_values") void service.dynamic(event.listener_id, { timer_status: "Playing", 日本語: "a\nb" }, event.generation)
    })
}
async function httpRequest(port, { path = "/", method = "GET", headers = {}, body } = {}) {
    return new Promise((resolve, reject) => {
        const request = http.request({ hostname: "127.0.0.1", port, path, method, headers, agent: false }, (response) => {
            const chunks = []
            response.on("data", (chunk) => chunks.push(chunk))
            response.on("end", () => {
                const text = Buffer.concat(chunks).toString()
                let json
                try {
                    json = JSON.parse(text)
                } catch {}
                resolve({ status: response.statusCode, text, json, headers: response.headers })
            })
        })
        request.setTimeout(3000, () => request.destroy(new Error("HTTP test timed out")))
        request.on("error", reject)
        request.end(body)
    })
}
async function socket(port, options = {}) {
    const client = io(`http://127.0.0.1:${port}`, { forceNew: true, reconnection: false, timeout: 2000, autoConnect: false, ...options })
    client.received = []
    client.errors = []
    client.on("data", (value) => client.received.push(value))
    client.on("error", (value) => client.errors.push(value))
    await new Promise((resolve, reject) => {
        client.once("connect", resolve)
        client.once("connect_error", reject)
        client.connect()
    })
    return client
}
const normalizedEvents = (events) => events.filter((event) => event.kind === "trigger").map(({ data }) => ({ ...data, ...(data.returnId ? { returnId: "<private-correlation>" } : {}) }))

test("auth sources match actual getAuthToken precedence and JS object/array coercion", async () => {
    const service = rust()
    try {
        const sources = [
            null,
            false,
            0,
            1e21,
            1e-6,
            { token: 1e21 },
            { token: 1e-6 },
            { token: "\u00a0secret\u2000" },
            { token: "\u0085secret\u0085" },
            "",
            "secret",
            " secret ",
            "Bearer secret",
            "bEaReR  secret \t",
            "Bearer wrong",
            ["wrong", "secret"],
            ["Bearer wrong", "secret"],
            { auth: "wrong", password: "secret" },
            { auth: "", token: " secret " },
            { authorization: "Bearer secret" },
            { apiKey: ["secret"] },
            { token: true },
            { token: {} },
            { token: [null, "secret"] },
            { password: 0, key: "secret" }
        ]
        for (const first of sources)
            for (const second of ["Bearer secret", { token: "secret" }, "wrong", null]) {
                const result = await service.request({ op: "auth", password: "secret", sources: [first, second] })
                assert.equal(result.value, sourceAuth(first, second) ?? null, JSON.stringify([first, second]))
            }
        await service.close()
    } finally {
        service.kill()
    }
})

test("Express extended query parser matches actual qs defaults including duplicates, arrays and malformed escapes", async () => {
    const service = rust()
    try {
        const queries = [
            "",
            "action=next_slide",
            "action=a&action=b",
            "auth[]=wrong&auth[]=secret",
            "auth[0]=x&auth[2]=y",
            "a[b]=1&a[c]=2",
            "a=one&a[b]=two",
            "a[b]=two&a=one",
            "a[]=one&a[x]=two",
            "a[0]=x&a[0]=y",
            "a[20]=x&a[1]=y",
            "a[18446744073709551615]=x&a[]=y",
            "a[9007199254740992]=x&a=y",
            "a[999999999999999900000]=x&a=y",
            "a[4294967295]=x&a[]=y",
            "__proto__[x]=bad&constructor[prototype][x]=ok",
            "x=%C0%AF&y=%ZZ&a+b=c+d",
            "a[unclosed=x",
            "a[b[c]]=x",
            "a%5Bfoo=bar%5D=x",
            "a[b][c][d][e][f][g]=x",
            "=v&&flag&flag=",
            Array.from({ length: 25 }, (_, i) => `a[]=${i}`).join("&"),
            Array.from({ length: 1002 }, (_, i) => `v${i}=x`).join("&")
        ]
        for (const raw of queries) {
            const result = await service.request({ op: "query", raw })
            assert.deepEqual(result.value, JSON.parse(JSON.stringify(queryOracle(raw))), raw)
        }
        await service.close()
    } finally {
        service.kill()
    }
})

test("real REST service matches original TypeScript/Express on both ports, auth and body/query precedence", { timeout: 30000 }, async () => {
    const original = apiOracle(),
        service = rust()
    const results = []
    try {
        for (const server of [original, service]) {
            const port = await freePort(),
                stopAuto = autoReply(server)
            await server.start({ port, password: "  secret \t" })
            const cases = [
                {},
                { path: "/?action=next_slide" },
                { headers: { authorization: "Bearer wrong", "x-api-key": "secret" }, path: "/?action=next_slide" },
                { headers: { authorization: "wrong", "x-api-key": "secret" }, path: "/?action=next_slide" },
                { path: "/?password=secret&action=next_slide" },
                { path: "/?token=wrong&password=secret&action=next_slide" },
                { headers: { authorization: "bEaReR secret" }, path: "/?action=next_slide" },
                { path: "/?auth=secret&action=get_shows" },
                { path: "/?auth=secret&action=unknown&data=%7B%22returnId%22%3A%22caller%22%7D" },
                { path: "/?auth=secret&action=get_shows&data=" + encodeURIComponent(JSON.stringify({ action: "next_slide", index: 2 })) },
                { path: "/?auth=secret&action=next_slide&data=not-json" },
                { path: "/?auth=secret&action=next_slide&data=" + encodeURIComponent(JSON.stringify(["a", "b"])) },
                { path: "/?auth=secret&action=next_slide&data=" + encodeURIComponent(JSON.stringify("hello")) },
                { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ auth: "secret", action: "get_variables", data: { value: "日本語" } }) },
                { method: "POST", headers: { "content-type": "application/json", authorization: "Bearer secret" }, path: "/?action=next_slide", body: JSON.stringify({ action: "get_show", index: 3 }) },
                { method: "POST", headers: { "content-type": "text/plain", authorization: "Bearer secret" }, body: JSON.stringify({ action: "get_ignored" }) },
                { method: "POST", headers: { "content-type": "application/json", authorization: "Bearer secret" }, body: "[]" },
                { method: "POST", headers: { "content-type": "application/json", authorization: "Bearer secret" }, body: "null" },
                { method: "POST", headers: { "content-type": "application/json", authorization: "Bearer secret" }, body: "{" },
                { method: "POST", headers: { "content-type": "application/json; charset=latin1", authorization: "Bearer secret" }, body: "{}" },
                ...[
                    ["gzip", gzipSync],
                    ["deflate", deflateSync]
                ].map(([encoding, compress]) => ({ method: "POST", headers: { "content-type": "application/json", "content-encoding": encoding, authorization: "Bearer secret" }, body: compress(JSON.stringify({ action: "get_show", name: "مرحبا 🙌" })) })),
                { method: "POST", headers: { "content-type": "application/json; charset=utf-16le", authorization: "Bearer secret" }, body: Buffer.from(JSON.stringify({ action: "get_show", name: "日本語" }), "utf16le") },
                { method: "POST", headers: { "content-type": "application/json", "content-encoding": "br", authorization: "Bearer secret" }, body: "{}" },
                { path: "/missing" },
                { method: "OPTIONS", headers: { authorization: "Bearer secret" } },
                { method: "HEAD", path: "/?auth=secret&action=get_shows" }
            ]
            const rows = []
            for (const [index, input] of cases.entries()) {
                const response = await httpRequest(index % 2 ? port : port + 1, input)
                rows.push({ status: response.status, json: response.status < 400 || response.status === 401 ? response.json : undefined })
                assert.equal(response.headers["access-control-allow-origin"], undefined)
            }
            results.push({ rows, events: normalizedEvents(server.events) })
            stopAuto()
            await server.stop()
        }
        assert.deepEqual(results[1], results[0])
        await service.close()
    } finally {
        await original.stop()
        service.kill()
    }
})

test("genuine Socket.IO polling, WebSocket and transport upgrade match source actions/auth/dynamic replies", { timeout: 30000 }, async () => {
    const original = apiOracle(),
        service = rust(),
        outputs = []
    try {
        for (const server of [original, service]) {
            const port = await freePort(),
                stopAuto = autoReply(server)
            await server.start({ port, password: "secret" })
            const rows = []
            for (const transports of [["polling"], ["websocket"], ["polling", "websocket"]]) {
                const client = await socket(port, { transports, auth: { password: "secret" } })
                try {
                    client.emit("data", { action: "get_shows", index: 2 })
                    await until(() => client.received.length === 1)
                    client.emit("data", JSON.stringify({ isVariable: true, keys: ["{show}", "{timer}"] }))
                    await until(() => client.received.length === 2)
                    client.emit("data", { action: "next_slide" })
                    client.emit("data", "{invalid")
                    client.emit("data", {})
                    await until(() => client.errors.length === 1)
                    if (transports.length > 1) await until(() => client.io.engine.transport.name === "websocket")
                    rows.push({ received: client.received, errors: client.errors, transport: client.io.engine.transport.name })
                } finally {
                    client.disconnect()
                }
            }
            const wrong = await socket(port, { transports: ["websocket"], auth: { password: "wrong" }, extraHeaders: { authorization: "Bearer secret" } })
            wrong.emit("data", { action: "get_shows", password: "secret" })
            await until(() => wrong.received.length === 1)
            rows.push({ received: wrong.received, errors: wrong.errors })
            wrong.disconnect()
            outputs.push({ rows, triggers: normalizedEvents(server.events), connected: server.events.filter((e) => e.kind === "connected").length })
            stopAuto()
            await server.stop()
        }
        assert.deepEqual(outputs[1], outputs[0])
        await service.close()
    } finally {
        await original.stop()
        service.kill()
    }
})

test("private renderer IDs reject stale, unknown and duplicate completions across restart with reused caller IDs", { timeout: 15000 }, async () => {
    const service = rust({ reply_ms: 400 })
    try {
        const port = await freePort()
        const oldRun = await service.start({ port }, 9)
        const oldRequest = httpRequest(port, { path: "/?action=next_slide&data=" + encodeURIComponent(JSON.stringify({ returnId: "same-caller-id", n: 1 })) }).catch((error) => error)
        const oldEvent = await until(() => service.events.find((e) => e.kind === "trigger"))
        const newRun = await service.start({ port }, 9)
        assert.notEqual(oldRun.generation.run, newRun.generation.run)
        await oldRequest
        const currentRequest = httpRequest(port, { path: "/?action=next_slide&data=" + encodeURIComponent(JSON.stringify({ returnId: "same-caller-id", n: 2 })) })
        const current = await until(() => service.events.find((e) => e.kind === "trigger" && e.generation.run === newRun.generation.run))
        assert.notEqual(current.data.returnId, oldEvent.data.returnId)
        assert.match(current.data.returnId, new RegExp(newRun.generation.run))
        assert.equal((await service.complete({ ...oldEvent.data, data: "old" }, oldEvent.generation)).error.code, "stale_generation")
        assert.equal((await service.complete({ ...oldEvent.data, data: "mis-stamped" }, current.generation)).error.code, "stale_generation")
        assert.equal((await service.request({ op: "stats" })).value.pending_replies, 1)
        assert.equal((await service.complete({ ...current.data, data: false }, current.generation)).ok, true)
        const reply = await currentRequest
        assert.deepEqual(reply.json, { action: "next_slide", n: 2, data: false })
        assert.equal((await service.complete({ ...current.data, data: "duplicate" }, current.generation)).error.code, "duplicate_reply")
        assert.equal((await service.complete({ returnId: `api:${current.generation.run}:unknown` }, current.generation)).error.code, "unknown_reply")
        const timed = httpRequest(port, { path: "/?action=get_timeout" })
        const timeoutEvent = await until(() => service.events.find((e) => e.kind === "trigger" && e.data.action === "get_timeout"))
        assert.equal((await timed).status, 204)
        assert.equal((await service.complete({ ...timeoutEvent.data, data: "late" }, current.generation)).error.code, "duplicate_reply")
        const cancelled = httpRequest(port, { path: "/?action=get_cancelled" })
        await until(() => service.events.find((e) => e.kind === "trigger" && e.data.action === "get_cancelled"))
        await service.request({ op: "cancel" })
        assert.equal((await cancelled).status, 503)
        assert.equal((await service.request({ op: "stats" })).value.pending_replies, 0)
        await service.stop()
        await service.close()
    } finally {
        service.kill()
    }
})

test("Engine.IO leases bound abandoned polling sessions across TCP connections and are reclaimed on close/restart", { timeout: 15000 }, async () => {
    const service = rust({ sessions: 2, handshake_ms: 1000 })
    try {
        const port = await freePort()
        await service.start({ port })
        const opens = []
        for (let n = 0; n < 2; n++) {
            const response = await httpRequest(port, { path: "/socket.io/?EIO=4&transport=polling" })
            assert.equal(response.status, 200)
            assert.equal(response.text[0], "0")
            opens.push(JSON.parse(response.text.slice(1)).sid)
        }
        assert.equal((await service.request({ op: "stats" })).value.engine_sessions, 2)
        assert.equal((await httpRequest(port, { path: "/socket.io/?EIO=4&transport=polling" })).status, 503)
        // Complete Socket.IO on one session through a fresh TCP connection.
        const endpoint = `/socket.io/?EIO=4&transport=polling&sid=${opens[0]}`
        assert.equal((await httpRequest(port, { path: endpoint, method: "POST", headers: { "content-type": "text/plain;charset=UTF-8" }, body: "40" })).status, 200)
        const joined = await httpRequest(port, { path: endpoint })
        assert.ok(joined.text.split("\x1e").some((part) => part.startsWith("40")))
        assert.equal((await service.request({ op: "stats" })).value.engine_sessions, 2)
        // The other session never joins a Socket.IO namespace.
        const started = Date.now()
        await service.stop()
        assert.ok(Date.now() - started < 900)
        await service.start({ port })
        assert.equal((await service.request({ op: "stats" })).value.engine_sessions, 0)
        assert.equal((await httpRequest(port, { path: `/socket.io/?EIO=4&transport=polling&sid=${opens[1]}` })).status, 400)
        const client = await socket(port, { transports: ["polling"] })
        client.disconnect()
        await until(async () => (await service.request({ op: "stats" })).value.engine_sessions === 0)
        await service.stop()
        await service.close()
    } finally {
        service.kill()
    }
})

test("pending reply, message and event capacities reject saturation and recover after disconnect", { timeout: 15000 }, async () => {
    const service = rust({ pending: 1, inflight: 3, client_inflight: 2, reply_ms: 500 })
    try {
        const port = await freePort()
        const started = await service.start({ port })
        const client = await socket(port, { transports: ["websocket"] })
        client.emit("data", { action: "get_pending" })
        const event = await until(() => service.events.find((e) => e.kind === "trigger"))
        assert.equal((await httpRequest(port, { path: "/?action=get_full" })).json.code, "pending_limit")
        client.disconnect()
        await until(() => service.events.some((e) => e.kind === "connected"))
        await until(async () => (await service.request({ op: "stats" })).value.pending_replies === 0)
        assert.equal((await service.request({ op: "stats" })).value.pending_replies, 0)
        assert.equal((await service.complete({ ...event.data, data: "after-disconnect" }, started.generation)).ok, false)
        const next = httpRequest(port, { path: "/?action=get_recovered" })
        const recovered = await until(() => service.events.find((e) => e.kind === "trigger" && e.data.action === "get_recovered"))
        await service.complete({ ...recovered.data, data: null }, recovered.generation)
        assert.equal((await next).status, 200)
        await service.stop()
        await service.close()
    } finally {
        service.kill()
    }
})

test("occupied listener rolls back all endpoints and restart closes active clients", { timeout: 15000 }, async () => {
    const service = rust(),
        blocker = net.createServer()
    try {
        const port = await freePort()
        await new Promise((resolve) => blocker.listen(port + 1, "127.0.0.1", resolve))
        const result = await service.request({ op: "start", data: { port } })
        assert.equal(result.ok, false)
        assert.equal(result.error.code, "bind_failed")
        const probe = net.createServer()
        await new Promise((resolve, reject) => {
            probe.once("error", reject)
            probe.listen(port, "127.0.0.1", resolve)
        })
        await new Promise((resolve) => probe.close(resolve))
        await new Promise((resolve) => blocker.close(resolve))
        await service.start({ port })
        const client = await socket(port, { transports: ["websocket"] })
        await service.start({ port })
        await until(() => !client.connected)
        assert.equal((await service.request({ op: "stats" })).value.pending_replies, 0)
        client.disconnect()
        await service.stop()
        await service.close()
    } finally {
        blocker.close()
        service.kill()
    }
})

test("OSC UDP listener matches actual osc-js/auth/renderer flow and preserves default reply target/encoding failure", { timeout: 20000 }, async () => {
    const responseReceiver = dgram.createSocket("udp4")
    await new Promise((resolve, reject) => {
        responseReceiver.once("error", reject)
        responseReceiver.bind(41235, "127.0.0.1", resolve)
    })
    const original = apiOracle(),
        service = rust(),
        outputs = [],
        allSenders = []
    const responses = []
    responseReceiver.on("message", (bytes) => {
        const decoded = new ActualOSC.Message()
        decoded.unpack(new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength))
        responses.push({ address: decoded.address, args: decoded.args })
    })
    try {
        for (const server of [original, service]) {
            const port = await freePort(),
                stopAuto = autoReply(server),
                sender = dgram.createSocket("udp4")
            allSenders.push(sender)
            await new Promise((resolve) => sender.bind(0, "127.0.0.1", resolve))
            let senderResponses = 0
            sender.on("message", () => senderResponses++)
            await server.start({ port, password: "secret" })
            const beforeResponses = responses.length
            const items = [
                ["/freeshow/empty", [], 0, 1],
                ["/ignored/path", [JSON.stringify({ password: "secret" })], 0, 0],
                ["/freeshow/next_slide", [JSON.stringify({ password: "secret", index: 2 })], 1, 0],
                ["/freeshow/get_shows", [JSON.stringify({ password: "secret" })], 1, 0],
                ["/freeshow/custom", [JSON.stringify({ password: "secret", action: "get_shows", n: "日本語" })], 1, 0],
                ["/freeshow/next_slide", [JSON.stringify({ password: "secret", returnId: "caller" })], 1, 0],
                ["/freeshow/wrong", [JSON.stringify({ token: "wrong" }), "Bearer secret"], 0, 1],
                ["/freeshow/raw", ["not-json", "Bearer secret"], 1, 0],
                ["/freeshow/array", [JSON.stringify(["x"]), "Bearer secret"], 1, 0],
                ["/freeshow/blob", [Uint8Array.from([1, 2, 255]), "Bearer secret"], 1, 0],
                ["/prefix/freeshow/path", [JSON.stringify({ password: "secret" })], 1, 0]
            ]
            for (const [address, args, count, responseCount] of items) {
                const startEvents = normalizedEvents(server.events).length,
                    startResponses = responses.length
                const bytes = new ActualOSC.Message(address, ...args).pack()
                await new Promise((resolve, reject) => sender.send(bytes, port, "127.0.0.1", (error) => (error ? reject(error) : resolve())))
                if (count) await until(() => normalizedEvents(server.events).length === startEvents + count)
                if (responseCount) await until(() => responses.length === startResponses + responseCount)
                await pause(5)
                assert.equal(normalizedEvents(server.events).length, startEvents + count)
            }
            for (const [type, value] of [
                ["i", 123],
                ["f", 1.25],
                ["h", 123n],
                ["t", 123n],
                ["d", 1.23456789],
                ["T", true],
                ["F", false],
                ["N", null],
                ["I", Infinity]
            ]) {
                const count = normalizedEvents(server.events).length
                const bytes = new ActualOSC.TypedMessage(`/freeshow/typed_${type}`, [
                    { type, value },
                    { type: "s", value: "Bearer secret" }
                ]).pack()
                await new Promise((resolve, reject) => sender.send(bytes, port, "127.0.0.1", (error) => (error ? reject(error) : resolve())))
                await until(() => normalizedEvents(server.events).length === count + 1)
            }
            const count = normalizedEvents(server.events).length
            const bundle = new ActualOSC.Bundle([new ActualOSC.Message("/freeshow/bundle", JSON.stringify({ password: "secret", n: 1 })), new ActualOSC.Bundle([new ActualOSC.Message("/freeshow/nested", JSON.stringify({ password: "secret", n: 2 }))], Date.now() - 10000)], Date.now() - 5000)
            await new Promise((resolve, reject) => sender.send(bundle.pack(), port, "127.0.0.1", (error) => (error ? reject(error) : resolve())))
            await until(() => normalizedEvents(server.events).length === count + 2)
            assert.equal(senderResponses, 0, "OSC replies must use localhost:41235 rather than sender rinfo")
            await until(() => server.events.some((e) => e.kind === "diagnostic" && /unknown value type/.test(e.message)))
            outputs.push({ events: normalizedEvents(server.events), responses: responses.slice(beforeResponses) })
            sender.close()
            stopAuto()
            await server.stop()
        }
        assert.deepEqual(outputs[1], outputs[0])
        await service.close()
    } finally {
        for (const sender of allSenders) {
            try {
                sender.close()
            } catch {}
        }
        responseReceiver.close()
        await original.stop()
        service.kill()
    }
})

test("finite JSON numbers use JS rounding while overflow is an explicit transport-contract gap", async () => {
    const original = apiOracle(),
        service = rust(),
        results = []
    try {
        for (const server of [original, service]) {
            const port = await freePort()
            await server.start({ port })
            const stopAuto = autoReply(server)
            const response = await httpRequest(port, { method: "POST", headers: { "content-type": "application/json" }, body: '{"action":"get_number","large":9007199254740993,"decimal":0.000001,"huge":1e21,"negativeZero":-0}' })
            results.push(response.json)
            stopAuto()
            await server.stop()
        }
        assert.deepEqual(results[1], results[0])
        const port = await freePort()
        await service.start({ port })
        const rejected = await httpRequest(port, { method: "POST", headers: { "content-type": "application/json" }, body: '{"action":"next_slide","value":1e309}' })
        assert.equal(rejected.status, 400)
        assert.equal(rejected.json.code, "json_number_overflow")
        assert.equal(JSON.parse('{"value":1e309}').value, Infinity, "original JS accepts a value the shared native JSON contract cannot carry")
        assert.equal((await httpRequest(port, { path: "/?action=next_slide&data=" + encodeURIComponent('{"value":1e309}') })).json.code, "json_number_overflow")
        await service.stop()
        await service.close()
    } finally {
        await original.stop()
        service.kill()
    }
})

test("raw duplicate CONNECT flood stays bounded and normal connect/disconnect/reconnect remains compatible", { timeout: 15000 }, async () => {
    const service = rust({ sessions: 2, handshake_ms: 1000 })
    try {
        const port = await freePort()
        await service.start({ port })
        async function openAndConnect() {
            const response = await httpRequest(port, { path: "/socket.io/?EIO=4&transport=polling" })
            const sid = JSON.parse(response.text.slice(1)).sid,
                endpoint = `/socket.io/?EIO=4&transport=polling&sid=${sid}`
            const body = Array.from({ length: 2000 }, () => "40").join("\x1e")
            assert.equal((await httpRequest(port, { path: endpoint, method: "POST", headers: { "content-type": "text/plain;charset=UTF-8" }, body })).status, 200)
            return endpoint
        }
        const endpoint = await openAndConnect()
        await until(() => service.events.filter((e) => e.kind === "connected").length >= 1)
        const baseline = (await service.request({ op: "stats" })).value
        assert.equal(baseline.engine_sessions, 1)
        assert.equal(service.events.filter((e) => e.kind === "connected").length, 1)
        for (let n = 0; n < 3; n++) {
            assert.equal((await httpRequest(port, { path: endpoint, method: "POST", headers: { "content-type": "text/plain;charset=UTF-8" }, body: Array.from({ length: 2000 }, () => "40").join("\x1e") })).status, 200)
            const stats = (await service.request({ op: "stats" })).value
            assert.equal(stats.engine_sessions, 1)
            assert.ok(stats.runtime_tasks <= baseline.runtime_tasks + 1, JSON.stringify(stats))
        }
        assert.equal(service.events.filter((e) => e.kind === "connected").length, 1)
        assert.equal((await httpRequest(port, { path: endpoint, method: "POST", headers: { "content-type": "text/plain;charset=UTF-8" }, body: "1" })).status, 200)
        await until(async () => (await service.request({ op: "stats" })).value.engine_sessions === 0)
        await openAndConnect()
        await until(() => service.events.filter((e) => e.kind === "connected").length === 2)
        await service.stop()
        await service.close()
    } finally {
        service.kill()
    }
})

test("encoded/empty/invalid/duplicate reserved session IDs cannot bypass Engine.IO admission", async () => {
    const service = rust({ sessions: 1 })
    try {
        const port = await freePort()
        await service.start({ port })
        for (const query of ["sid=invalid", "s%69d=invalid", "sid=invalid&sid=other", "sid=%00"]) {
            assert.equal((await httpRequest(port, { path: `/socket.io/?EIO=4&transport=polling&${query}` })).status, 400, query)
            assert.equal((await service.request({ op: "stats" })).value.engine_sessions, 0)
        }
        const response = await httpRequest(port, { path: "/socket.io/?E%49O=4&tr%61nsport=polling&sid=&sid[]=ignored" })
        assert.equal(response.status, 200)
        const sid = JSON.parse(response.text.slice(1)).sid
        assert.equal((await httpRequest(port, { path: "/socket.io/?EIO=4&transport=polling" })).status, 503)
        const encodedSid = Array.from(sid)
            .map((c) => `%${c.charCodeAt(0).toString(16)}`)
            .join("")
        assert.equal((await httpRequest(port, { method: "POST", path: `/socket.io/?EIO=4&transport=polling&s%69d=${encodedSid}`, headers: { "content-type": "text/plain;charset=UTF-8" }, body: "1" })).status, 200)
        await until(async () => (await service.request({ op: "stats" })).value.engine_sessions === 0)
        await service.stop()
        await service.close()
    } finally {
        service.kill()
    }
})

test("dynamic variable timeouts and false host values match original, then disconnect cancels pending work", async () => {
    const original = apiOracle(),
        service = rust(),
        outputs = []
    try {
        for (const server of [original, service]) {
            const port = await freePort()
            await server.start({ port })
            const client = await socket(port, { transports: ["websocket"] })
            try {
                client.emit("data", { isVariable: true })
                await until(() => client.received.length === 1)
                const remove = server.observe((event) => {
                    if (event.kind === "dynamic_values") void server.dynamic(event.listener_id, false, event.generation)
                })
                client.emit("data", { isVariable: true, keys: { timer: "a" } })
                await until(() => client.received.length === 2)
                remove()
                outputs.push({ received: client.received, keys: server.events.filter((event) => event.kind === "dynamic_values").map((event) => event.keys) })
            } finally {
                client.disconnect()
            }
            await server.stop()
        }
        assert.deepEqual(outputs[1], outputs[0])
        const port = await freePort()
        await service.start({ port })
        const client = await socket(port, { transports: ["polling"] })
        client.emit("data", { isVariable: true })
        await until(async () => (await service.request({ op: "stats" })).value.pending_replies === 1)
        const event = service.events.filter((event) => event.kind === "dynamic_values").at(-1)
        client.disconnect()
        await until(async () => (await service.request({ op: "stats" })).value.pending_replies === 0)
        assert.equal((await service.dynamic(event.listener_id, {}, event.generation)).error.code, "duplicate_reply")
        await service.stop()
        await service.close()
    } finally {
        await original.stop()
        service.kill()
    }
})

test("per-client request, HTTP inflated-body and Socket.IO message limits fail explicitly and recover", async () => {
    const service = rust({ client_inflight: 1, pending: 4, reply_ms: 1000, payload: 1024 })
    try {
        const port = await freePort()
        await service.start({ port })
        const first = await socket(port, { transports: ["websocket"] }),
            second = await socket(port, { transports: ["polling"] })
        try {
            first.emit("data", { action: "get_first" })
            await until(async () => (await service.request({ op: "stats" })).value.pending_replies === 1)
            first.emit("data", { action: "get_saturated" })
            await until(() => first.received.length === 1)
            assert.equal(first.received[0].status, 503)
            second.emit("data", { action: "get_other" })
            await until(async () => (await service.request({ op: "stats" })).value.pending_replies === 2)
            const events = service.events.filter((event) => event.kind === "trigger")
            for (const event of events) assert.equal((await service.complete({ ...event.data, data: true }, event.generation)).ok, true)
            await until(() => first.received.length === 2 && second.received.length === 1)
            first.emit("data", { action: "next", text: "x".repeat(2048) })
            await until(() => !first.connected)
            const bomb = gzipSync(JSON.stringify({ action: "next", text: "x".repeat(102400) }))
            const response = await httpRequest(port + 1, { method: "POST", headers: { "content-type": "application/json", "content-encoding": "gzip" }, body: bomb })
            assert.equal(response.status, 413)
            assert.equal(response.json.code, "body_too_large")
            assert.equal((await httpRequest(port + 1, { path: "/?action=next" })).status, 204)
            await until(async () => (await service.request({ op: "stats" })).value.active_requests === 0)
        } finally {
            first.disconnect()
            second.disconnect()
        }
        await service.stop()
        await service.close()
    } finally {
        service.kill()
    }
})

test("generated conflicting query structures match installed Express qs parser", async () => {
    const service = rust()
    try {
        const keys = ["a", "a[]", "a[0]", "a[1]", "a[19]", "a[20]", "a[01]", "a[x]", "a[x][]", "a[][x]", "a[x][y]", "b", "b[]", "__proto__[x]", "constructor[x]"]
        let seed = 0x12345678
        const rand = (n) => {
            seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
            return seed % n
        }
        for (let i = 0; i < 160; i++) {
            const raw = Array.from({ length: 1 + rand(7) }, (_, index) => `${keys[rand(keys.length)]}=${index % 3 ? "x" : ""}`).join("&")
            assert.deepEqual((await service.request({ op: "query", raw })).value, JSON.parse(JSON.stringify(queryOracle(raw))), raw)
        }
        await service.close()
    } finally {
        service.kill()
    }
})

test("finite JSON binary64 parsing agrees with JS across fractional and exponent boundaries", async () => {
    const service = rust()
    try {
        const numbers = ["1.0000000000000002", "1.0000000000000001", "0.100000000000000005", "2.2250738585072014e-308", "4.9406564584124654e-324", "1.7976931348623157e308", "9007199254740993", "-0"]
        let seed = 12345
        const rand = () => {
            seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
            return seed
        }
        for (let i = 0; i < 240; i++) numbers.push(`${rand() % 10}.${rand()}${rand()}e${(rand() % 600) - 300}`)
        for (const number of numbers) {
            const text = `{"number":${number}}`
            const result = await service.request({ op: "rest_data", body: {}, query: { action: "next", data: text } })
            assert.equal(result.value.number, JSON.parse(text).number, number)
        }
        await service.close()
    } finally {
        service.kill()
    }
})

test("HTTP and raw Socket.IO JSON retain exact binary64 spelling, duplicate keys and escaped numeric strings", async () => {
    const original = apiOracle(),
        service = rust(),
        outputs = []
    const text = '{"action":"next","value":1,"nested":[8.3163996793004788882e-275,1.0000000000000002,4.9406564584124654e-324,9007199254740993,-0,{"123":"1e309 \\\" 123"}],"value":1.7976931348623157e308}'
    try {
        for (const server of [original, service]) {
            const port = await freePort()
            await server.start({ port })
            const response = await httpRequest(port + 1, { method: "POST", headers: { "content-type": "application/json" }, body: text })
            assert.equal(response.status, 204)
            const opened = await httpRequest(port, { path: "/socket.io/?EIO=4&transport=polling" })
            const sid = JSON.parse(opened.text.slice(1)).sid
            const endpoint = `/socket.io/?EIO=4&transport=polling&sid=${sid}`
            const send = (body) => httpRequest(port, { path: endpoint, method: "POST", headers: { "content-type": "text/plain;charset=UTF-8" }, body })
            assert.equal((await send("40")).status, 200)
            await until(() => server.events.some((event) => event.kind === "connected"))
            assert.equal((await send(`42["data",${text}]`)).status, 200)
            await until(() => normalizedEvents(server.events).length === 2)
            outputs.push(normalizedEvents(server.events))
            await server.stop()
        }
        assert.deepEqual(outputs[1], outputs[0])
        assert.deepEqual(outputs[1][0], JSON.parse(text))
        await service.close()
    } finally {
        await original.stop()
        service.kill()
    }
})

test("Socket.IO request path, CORS preflight and invalid-session statuses match actual server", async () => {
    const original = apiOracle(),
        service = rust(),
        outputs = []
    try {
        for (const server of [original, service]) {
            const port = await freePort()
            await server.start({ port })
            const rows = []
            for (const input of [{ path: "/socket.io" }, { path: "/socket.ioX/?EIO=4&transport=polling" }, { path: "/socket.io/?EIO=4&transport=polling&sid=invalid" }, { path: "/socket.io/?EIO=4&transport=polling", method: "OPTIONS", headers: { "access-control-request-headers": "authorization,x-api-key" } }, { path: "/socket.io/?EIO=4&transport=polling", method: "OPTIONS" }]) {
                const response = await httpRequest(port, input)
                rows.push({ status: response.status, origin: response.headers["access-control-allow-origin"], methods: response.headers["access-control-allow-methods"], allowed: response.headers["access-control-allow-headers"], vary: response.headers.vary })
            }
            const opened = await httpRequest(port, { path: "/socket.io/?E%49O=4&tr%61nsport=polling&sid=" })
            assert.equal(opened.status, 200)
            rows.push({ origin: opened.headers["access-control-allow-origin"], methods: opened.headers["access-control-allow-methods"] })
            outputs.push(rows)
            await server.stop()
        }
        assert.deepEqual(outputs[1], outputs[0])
        await service.close()
    } finally {
        await original.stop()
        service.kill()
    }
})

test("malformed OSC reports errors and shutdown cancels a scheduled bundle while reclaiming request capacity", async () => {
    const service = rust({ inflight: 1 }),
        sender = dgram.createSocket("udp4")
    const send = (bytes, port) => new Promise((resolve, reject) => sender.send(bytes, port, "127.0.0.1", (error) => (error ? reject(error) : resolve())))
    try {
        const port = await freePort()
        await service.start({ port })
        await send(Buffer.from("bad"), port)
        await until(() => service.events.some((event) => event.code === "invalid_osc"))
        const future = new ActualOSC.Bundle([new ActualOSC.Message("/freeshow/future", JSON.stringify({ action: "future" }))], Date.now() + 10000)
        await send(Buffer.from(future.pack()), port)
        await until(async () => (await service.request({ op: "stats" })).value.active_requests === 1)
        await service.stop()
        assert.equal(normalizedEvents(service.events).length, 0)
        await service.start({ port })
        await send(Buffer.from(new ActualOSC.Message("/freeshow/current", JSON.stringify({ action: "current" })).pack()), port)
        await until(() => normalizedEvents(service.events).length === 1)
        assert.equal(normalizedEvents(service.events)[0].action, "current")
        await service.stop()
        await service.close()
    } finally {
        sender.close()
        service.kill()
    }
})

test("aggregate Socket.IO binary attachment bytes are bounded before host dispatch", async () => {
    const service = rust({ payload: 1024 })
    try {
        const port = await freePort()
        await service.start({ port })
        const client = await socket(port, { transports: ["websocket"] })
        client.emit("data", { action: "next", attachments: [Buffer.alloc(600), Buffer.alloc(600)] })
        await until(() => !client.connected)
        assert.equal(normalizedEvents(service.events).length, 0)
        await until(async () => (await service.request({ op: "stats" })).value.engine_sessions === 0)
        await service.stop()
        await service.close()
    } finally {
        service.kill()
    }
})

test("Express JSON Unicode body charsets match real iconv decoding, including UTF-7 and UTF-16 auto-detection", async () => {
    const original = apiOracle(),
        service = rust(),
        outputs = []
    try {
        for (const server of [original, service]) {
            const port = await freePort()
            await server.start({ port })
            const statuses = []
            const text = JSON.stringify({ action: "next", name: "日本語 + & 🙌🏾 العربية", escaped: '" 123 \\ n' })
            for (const [charset, encoding] of [
                ["utf-7", "utf-7"],
                ["utf-7-imap", "utf-7-imap"],
                ["utf-16", "utf-16be"],
                ["utf-16le", "utf-16le"],
                ["utf-16be", "utf-16be"],
                ["utf-32", "utf-16le"]
            ]) {
                const response = await httpRequest(port + 1, { method: "POST", headers: { "content-type": `application/json; charset=${charset}` }, body: sourceEncode(text, encoding) })
                statuses.push(response.status)
            }
            outputs.push({ statuses, events: normalizedEvents(server.events) })
            await server.stop()
        }
        assert.deepEqual(outputs[1], outputs[0])
        await service.close()
    } finally {
        await original.stop()
        service.kill()
    }
})

test("astral string spread reports the native unpaired-surrogate transport gap explicitly", async () => {
    const service = rust()
    try {
        const result = await service.request({ op: "rest_data", body: {}, query: { action: "next", data: JSON.stringify("🙌") } })
        assert.equal(result.error.code, "json_surrogate_spread")
        assert.equal({ ...JSON.parse(JSON.stringify("🙌")) }[0].charCodeAt(0), 0xd83d)
        await service.close()
    } finally {
        service.kill()
    }
})
