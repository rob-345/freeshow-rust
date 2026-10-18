import assert from "node:assert/strict"
import { spawn } from "node:child_process"
import { randomInt } from "node:crypto"
import dgram from "node:dgram"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { createInterface } from "node:readline"
import { test, afterEach } from "node:test"
import { dns, originalDiscovery as loadOriginal, root } from "./discovery-oracle.mjs"
const cleanups = []
afterEach(async () => {
    let failure
    while (cleanups.length) {
        try {
            await cleanups.pop()()
        } catch (error) {
            failure ||= error
        }
    }
    if (failure) throw failure
})
function originalDiscovery(...args) {
    const value = loadOriginal(...args)
    cleanups.push(() => value.close())
    return value
}
const binary = process.env.FREESHOW_DISCOVERY_BIN || path.join(root, "target/debug/examples", process.platform === "win32" ? "discovery-parity.exe" : "discovery-parity")
const sleep = (ms = 5) => new Promise((resolve) => setTimeout(resolve, ms))
async function until(get, ms = 5000) {
    const end = Date.now() + ms
    while (Date.now() < end) {
        const result = await get()
        if (result) return result
        await sleep()
    }
    throw new Error("Discovery condition timed out")
}
const snapshot = {
    hostname: "fixture-host",
    interfaces: [
        {
            name: "eth0",
            addresses: [
                { family: "IPv4", address: "192.0.2.20", internal: false, mac: "aa:bb:cc:dd:ee:ff" },
                { family: "IPv6", address: "2001:db8::1", internal: false, mac: "aa:bb:cc:dd:ee:ff" }
            ]
        },
        { name: "lo", addresses: [{ family: "IPv4", address: "127.0.0.1", internal: true, mac: "00:00:00:00:00:00" }] }
    ]
}
const generation = { context: 1, run: "0123456789abcdef0123456789abcdef" },
    type = "_freeshow._udp.local",
    fullname = `freeshow-REMOTE-abcdef.${type}`
const allocated = new Set()
async function peer() {
    let reason
    for (let i = 0; i < 40; i++) {
        const port = randomInt(32768, 49151)
        if (allocated.has(port)) continue
        allocated.add(port)
        const socket = dgram.createSocket("udp4")
        try {
            await new Promise((resolve, reject) => {
                socket.once("error", reject)
                socket.bind(port, "127.0.0.1", resolve)
            })
            socket.removeAllListeners("error")
            let closed = false
            const packets = []
            socket.on("message", (bytes, source) => {
                packets.push({ bytes: Buffer.from(bytes), packet: dns.decode(bytes), source })
            })
            const value = {
                port,
                packets,
                send: (packet, destination) => new Promise((resolve, reject) => socket.send(dns.encode(packet), destination, "127.0.0.1", (error) => (error ? reject(error) : resolve()))),
                raw: (bytes, destination) => new Promise((resolve, reject) => socket.send(bytes, destination, "127.0.0.1", (error) => (error ? reject(error) : resolve()))),
                close: () => {
                    if (closed) return Promise.resolve()
                    closed = true
                    return new Promise((resolve) => socket.close(resolve))
                }
            }
            cleanups.push(() => value.close())
            return value
        } catch (error) {
            reason = error
            try {
                socket.close()
            } catch {}
        }
    }
    throw new Error(`No loopback UDP port available: ${reason}`)
}
async function freePort() {
    const value = await peer()
    await value.close()
    return value.port
}
function rust(config = {}, extraEnv = {}) {
    const child = spawn(binary, [], { stdio: ["pipe", "pipe", "pipe"], env: { ...process.env, ...extraEnv, FREESHOW_DISCOVERY_TEST_CONFIG: JSON.stringify(config) } })
    const exited = new Promise((resolve) => child.once("close", resolve))
    let closed = false
    let next = 0,
        stderr = ""
    const events = [],
        pending = new Map()
    child.stderr.on("data", (v) => (stderr += v))
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
            entry.reject(new Error(`Discovery exited ${code}: ${stderr}`))
        }
    })
    const value = {
        events,
        request(input) {
            return new Promise((resolve, reject) => {
                const id = next++,
                    timer = setTimeout(() => {
                        pending.delete(id)
                        reject(new Error(`Discovery request timed out ${JSON.stringify(input)} ${stderr}`))
                    }, 6000)
                pending.set(id, { resolve, reject, timer })
                child.stdin.write(JSON.stringify({ ...input, id }) + "\n")
            })
        },
        async call(op, extra = {}) {
            const result = await this.request({ op, generation, snapshot, ...extra })
            assert.equal(result.ok, true, JSON.stringify(result))
            return result.value
        },
        async close() {
            if (closed) return
            closed = true
            child.stdin.end()
            const code = await exited
            assert.equal(code, 0, stderr)
        }
    }
    cleanups.push(() => value.close())
    return value
}
async function fixture() {
    const receiver = await peer(),
        bind = await freePort()
    const service = rust({ bind: `127.0.0.1:${bind}`, target: `127.0.0.1:${receiver.port}` })
    await service.call("begin")
    return {
        receiver,
        service,
        bind,
        async close() {
            await service.close()
            await receiver.close()
        }
    }
}
function announcement(value) {
    return value.packet.type === "response" && value.packet.answers.some((a) => a.type === "PTR" && a.ttl > 0)
}
function recordsBuffer(records) {
    return dns.encode({ type: "response", flags: dns.AUTHORITATIVE_ANSWER, answers: records })
}

test("actual pinned source IP ordering, duplicates, numeric families, no-interface fallback, record bytes and mac argv", async () => {
    const cases = [
        {
            hostname: "numeric-names",
            interfaces: [
                { name: "10", addresses: [{ family: 4.0, address: "192.0.2.10", internal: false, mac: "aa" }] },
                { name: "2", addresses: [{ family: "IPv4", address: "192.0.2.2", internal: false, mac: "aa" }] }
            ]
        },
        snapshot,
        { hostname: "isolated", interfaces: [] },
        {
            hostname: "Unicode-é.local",
            interfaces: [
                { name: "Wi-Fi", addresses: [{ family: 4, address: "198.51.100.4", internal: false, mac: "aa" }] },
                { name: "en0", addresses: [{ family: "IPv4", address: "198.51.100.1", internal: false, mac: "00:00:00:00:00:00" }] },
                { name: "eth0", addresses: [{ family: "IPv4", address: "198.51.100.2", internal: false, mac: "aa" }] }
            ]
        }
    ]
    const service = rust()
    try {
        for (const current of cases) {
            const original = originalDiscovery(current, { noIO: true }),
                mac = originalDiscovery(current, { mac: true })
            try {
                assert.deepEqual(await service.call("ips", { snapshot: current }), original.ips())
                for (const name of ["REMOTE", "STAGE", "CONTROLLER", "OUTPUT_STREAM"]) {
                    const actual = Buffer.from(await service.call("records", { snapshot: current, name }))
                    assert.deepEqual(actual, recordsBuffer(original.records(name)))
                    assert.deepEqual(await service.call("args", { snapshot: current, name }), Array.from(mac.args(name, 5510)))
                    mac.stop()
                }
            } finally {
                await original.close()
                await mac.close()
            }
        }
    } finally {
        await service.close()
    }
})
test("real loopback publication/probes, query records/additionals and TTL0 stop match actual Bonjour wire", { timeout: 15000 }, async () => {
    const native = await fixture(),
        receiver = await peer(),
        bind = await freePort(),
        original = originalDiscovery(snapshot, { bindPort: bind, peerPort: receiver.port })
    try {
        await native.service.call("publish")
        original.publish()
        await until(() => native.receiver.packets.find(announcement))
        await until(() => receiver.packets.find(announcement))
        assert.deepEqual(
            native.receiver.packets.map((v) => v.bytes),
            receiver.packets.map((v) => v.bytes)
        )
        assert.equal(native.receiver.packets.filter((v) => v.packet.type === "query").length, 4)
        const questions = [
            { name: type, type: "PTR" },
            { name: fullname, type: "ANY" },
            { name: fullname, type: "SRV" },
            { name: fullname, type: "TXT" },
            { name: snapshot.hostname, type: "A" },
            { name: snapshot.hostname, type: "AAAA" },
            { name: "_FREESHOW", type: "PTR" }
        ]
        for (const question of questions) {
            const nativeCount = native.receiver.packets.length,
                sourceCount = receiver.packets.length
            await native.receiver.send({ type: "query", questions: [question] }, native.bind)
            await receiver.send({ type: "query", questions: [question] }, bind)
            await until(() => native.receiver.packets.length > nativeCount)
            await until(() => receiver.packets.length > sourceCount)
            assert.deepEqual(native.receiver.packets.at(-1).bytes, receiver.packets.at(-1).bytes)
        }
        const nativeCount = native.receiver.packets.length,
            sourceCount = receiver.packets.length
        await native.service.call("stop")
        original.stop()
        await until(() => native.receiver.packets.length > nativeCount)
        await until(() => receiver.packets.length > sourceCount)
        assert.deepEqual(native.receiver.packets.at(-1).bytes, receiver.packets.at(-1).bytes)
        assert.ok(native.receiver.packets.at(-1).packet.answers.every((a) => a.ttl === 0))
    } finally {
        await original.close()
        await receiver.close()
        await native.close()
    }
})
test("actual browser prefix suppression requires a positive PTR and matching SRV in the same response", { timeout: 10000 }, async () => {
    for (const complete of [false, true]) {
        const native = await fixture(),
            receiver = await peer(),
            bind = await freePort(),
            original = originalDiscovery(snapshot, { bindPort: bind, peerPort: receiver.port })
        try {
            await native.service.call("publish")
            original.publish()
            await until(() => receiver.packets.length && native.receiver.packets.length)
            const target = `freeshow-REMOTE-other.${type}`,
                answers = [{ name: type, type: "PTR", ttl: 120, data: target }, ...(complete ? [{ name: target, type: "SRV", ttl: 120, data: { port: 9999, target: "other" } }] : [])]
            const packet = { type: "response", answers }
            await receiver.send(packet, bind)
            await native.receiver.send(packet, native.bind)
            if (complete) {
                await until(() => native.service.events.some((e) => e.kind === "suppressed"))
                await sleep(1900)
                assert.equal(receiver.packets.some(announcement), false)
                assert.equal(native.receiver.packets.some(announcement), false)
            } else {
                await until(() => receiver.packets.some(announcement))
                await until(() => native.receiver.packets.some(announcement))
            }
        } finally {
            await original.close()
            await receiver.close()
            await native.close()
        }
    }
})
test("exact instance probe conflict including TTL0 prevents advertisement and matches source goodbye", { timeout: 10000 }, async () => {
    const native = await fixture(),
        receiver = await peer(),
        bind = await freePort(),
        original = originalDiscovery(snapshot, { bindPort: bind, peerPort: receiver.port })
    try {
        await native.service.call("publish")
        original.publish()
        await until(() => native.receiver.packets.some((v) => v.packet.questions[0]?.type === "ANY") && receiver.packets.some((v) => v.packet.questions[0]?.type === "ANY"))
        const packet = { type: "response", answers: [{ name: fullname.toUpperCase(), type: "TXT", ttl: 0, data: [Buffer.from("x=y")] }] }
        await native.receiver.send(packet, native.bind)
        await receiver.send(packet, bind)
        await until(() => native.receiver.packets.some((v) => v.packet.type === "response"))
        await until(() => receiver.packets.some((v) => v.packet.type === "response"))
        assert.deepEqual(native.receiver.packets.at(-1).bytes, receiver.packets.at(-1).bytes)
        assert.equal(native.receiver.packets.some(announcement), false)
        assert.equal(receiver.packets.some(announcement), false)
        assert.equal(await native.service.call("publish"), "already_present")
    } finally {
        await original.close()
        await receiver.close()
        await native.close()
    }
})
test("cancelled pending publication intentionally cannot resurrect after stop as the original timer does", { timeout: 10000 }, async () => {
    const native = await fixture(),
        receiver = await peer(),
        bind = await freePort(),
        original = originalDiscovery(snapshot, { bindPort: bind, peerPort: receiver.port })
    try {
        await native.service.call("publish")
        original.publish()
        await native.service.call("stop")
        original.stop()
        await until(() => receiver.packets.some(announcement))
        assert.equal(native.receiver.packets.some(announcement), false)
        const stale = await native.service.request({ op: "publish", generation })
        assert.equal(stale.error.code, "stale_generation")
        await native.service.call("begin", { generation: { context: 2, run: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } })
        const old = await native.service.request({ op: "stop", generation })
        assert.equal(old.error.code, "stale_generation")
    } finally {
        await original.close()
        await receiver.close()
        await native.close()
    }
})
test("bounded malformed DNS input and response flood permit normal recovery", { timeout: 10000 }, async () => {
    const native = await fixture()
    try {
        await native.service.call("publish")
        await until(() => native.receiver.packets.some(announcement))
        const loop = Buffer.from([0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0xc0, 12, 0, 1, 0, 1])
        await native.receiver.raw(loop, native.bind)
        await until(() => native.service.events.some((e) => e.code === "invalid_discovery_packet"))
        const before = native.receiver.packets.length
        for (let i = 0; i < 90; i++) await native.receiver.send({ type: "query", questions: [{ name: type, type: "PTR" }] }, native.bind)
        await until(async () => (await native.service.call("stats")).dropped_responses > 0)
        assert.ok(native.receiver.packets.length - before <= 64)
        await sleep(1050)
        const count = native.receiver.packets.length
        await native.receiver.send({ type: "query", questions: [{ name: type, type: "PTR" }] }, native.bind)
        await until(() => native.receiver.packets.length > count)
    } finally {
        await native.close()
    }
})
test("source mac child duplicate/stop semantics and genuine supervised helper kill+reap", { skip: process.platform === "win32" }, async () => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-discovery-child-")),
        log = path.join(directory, "child.json"),
        original = originalDiscovery(snapshot, { mac: true }),
        service = rust({ dns_sd: binary }, { FREESHOW_DISCOVERY_HELPER_LOG: log, FREESHOW_DISCOVERY_HELPER_MODE: "ignore_term" })
    try {
        await service.call("begin")
        original.publish()
        await service.call("publish")
        await until(() => fs.existsSync(log))
        const child = JSON.parse(fs.readFileSync(log, "utf8"))
        assert.deepEqual(child.args, Array.from(original.spawns[0].args))
        assert.equal(await service.call("publish", { port: 9999 }), "already_present")
        original.publish("REMOTE", 9999)
        assert.equal(original.spawns.length, 1)
        assert.equal((await service.call("stats")).children, 1)
        await service.call("stop")
        original.stop()
        assert.equal(original.spawns[0].child.signal, "SIGTERM")
        assert.throws(() => process.kill(child.pid, 0), { code: "ESRCH" })
        assert.equal((await service.call("stats")).children, 0)
    } finally {
        await original.close()
        await service.close()
        fs.rmSync(directory, { recursive: true, force: true })
    }
})
test("genuine child early failure is reported and releases its per-name slot", { skip: process.platform === "win32" }, async () => {
    const service = rust({ dns_sd: binary }, { FREESHOW_DISCOVERY_HELPER_MODE: "exit" })
    try {
        await service.call("begin")
        await service.call("publish")
        await until(() => service.events.some((e) => e.code === "dns_sd_exited"))
        assert.equal((await service.call("stats")).children, 0)
        await service.call("publish")
        await until(() => service.events.filter((e) => e.code === "dns_sd_exited").length === 2)
    } finally {
        await service.close()
    }
})

test("multiple real services share source query registry ordering/deduplication and ports remain fixed until restart", { timeout: 12000 }, async () => {
    const native = await fixture(),
        receiver = await peer(),
        bind = await freePort(),
        original = originalDiscovery(snapshot, { bindPort: bind, peerPort: receiver.port, jitter: 0 })
    try {
        for (const [name, port] of [
            ["REMOTE", 5510],
            ["STAGE", 5511]
        ]) {
            await native.service.call("publish", { name, port })
            original.publish(name, port)
        }
        await until(() => native.receiver.packets.filter(announcement).length === 2 && receiver.packets.filter(announcement).length === 2)
        assert.equal(await native.service.call("publish", { port: 9999 }), "already_present")
        original.publish("REMOTE", 9999)
        for (const question of [
            { name: type, type: "PTR" },
            { name: snapshot.hostname, type: "ANY" },
            { name: fullname, type: "SRV" }
        ]) {
            const a = native.receiver.packets.length,
                b = receiver.packets.length
            await native.receiver.send({ type: "query", questions: [question] }, native.bind)
            await receiver.send({ type: "query", questions: [question] }, bind)
            await until(() => native.receiver.packets.length > a && receiver.packets.length > b)
            assert.deepEqual(native.receiver.packets.at(-1).bytes, receiver.packets.at(-1).bytes)
        }
    } finally {
        await original.close()
        await receiver.close()
        await native.close()
    }
})

test("first-run TXT IP snapshot remains frozen across replacement while new run records use its supplied interfaces", { timeout: 10000 }, async () => {
    const native = await fixture()
    try {
        await native.service.call("stop")
        const next = structuredClone(snapshot)
        next.interfaces[0].addresses[0].address = "198.51.100.8"
        await native.service.call("begin", { snapshot: next, generation: { context: 2, run: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } })
        await native.service.call("publish", { generation: { context: 2, run: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } })
        const message = await until(() => native.receiver.packets.find(announcement))
        const txt = message.packet.answers.find((r) => r.type === "TXT").data.map((v) => v.toString())
        assert.equal(txt[0], "ip=192.0.2.20")
        assert.equal(message.packet.answers.find((r) => r.type === "A").data, "198.51.100.8")
    } finally {
        await native.close()
    }
})

test("actual source reannouncement uses identical records after the first three-second interval", { timeout: 12000 }, async () => {
    const native = await fixture(),
        receiver = await peer(),
        bind = await freePort(),
        original = originalDiscovery(snapshot, { bindPort: bind, peerPort: receiver.port, jitter: 0 })
    try {
        await native.service.call("publish")
        original.publish()
        await until(() => native.receiver.packets.filter(announcement).length >= 2 && receiver.packets.filter(announcement).length >= 2, 7000)
        const a = native.receiver.packets.filter(announcement),
            b = receiver.packets.filter(announcement)
        assert.deepEqual(a[0].bytes, a[1].bytes)
        assert.deepEqual(a[1].bytes, b[1].bytes)
        assert.equal(a.length, 2)
        assert.equal(b.length, 2)
    } finally {
        await original.close()
        await receiver.close()
        await native.close()
    }
})

test("source inherited-interface field access fails explicitly rather than becoming a successful IP snapshot", async () => {
    const bad = { hostname: "fixture", interfaces: [{ name: "constructor", addresses: snapshot.interfaces[0].addresses }] }
    assert.throws(() => originalDiscovery(bad, { noIO: true }), /push/)
    const service = rust()
    try {
        const result = await service.request({ op: "ips", snapshot: bad })
        assert.equal(result.ok, false)
        assert.equal(result.error.code, "discovery_source_shape")
    } finally {
        await service.close()
    }
})
