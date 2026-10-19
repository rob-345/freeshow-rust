// Decode real desktop UDP bytes and compare with the immutable original module's
// actual bonjour-service records. Normalize only native random instance suffixes.
import assert from "node:assert/strict"
import fs from "node:fs"
import dgram from "node:dgram"
import { dns, originalDiscovery, sourceHash } from "../parity/discovery-oracle.mjs"

const raw = fs.readFileSync(0, "utf8")
assert.ok(raw.length <= 2 * 1024 * 1024)
const input = JSON.parse(raw)
assert.ok(input.packets.length <= 128)
const original = originalDiscovery(input.snapshot, { noIO: true })
const normalizeName = (name) => name.replace(/^(freeshow-(?:REMOTE|STAGE|CONTROLLER)-)[0-9a-f]{6}(\._freeshow\._udp\.local)$/, "$1abcdef$2")
const normalize = (record) => ({ name: normalizeName(record.name), type: record.type, ttl: record.ttl, data: record.type === "PTR" ? normalizeName(record.data) : record.type === "TXT" ? record.data.map((bytes) => Array.from(bytes)) : record.data })
const unique = (records) => [...new Map(records.map((record) => [JSON.stringify(record), record])).entries()].sort(([a], [b]) => a.localeCompare(b)).map(([, record]) => record)
try {
    const actual = [],
        services = new Set(),
        suffixes = new Set()
    const observe = (encoded) => {
        const bytes = Buffer.from(encoded, "base64")
        assert.ok(bytes.length <= 9000)
        const packet = dns.decode(bytes)
        const current = packet.answers.filter((record) => {
            if (record.type !== "SRV" || record.ttl === 0) return false
            const match = /^freeshow-(REMOTE|STAGE|CONTROLLER)-([0-9a-f]{6})\._freeshow\._udp\.local$/.exec(record.name)
            if (!match || record.data.port !== Number(input.endpoints[match[1]].split(":").at(-1))) return false
            services.add(match[1])
            suffixes.add(match[2])
            return true
        })
        if (current.length) actual.push(...packet.answers.filter((record) => record.ttl > 0).map(normalize))
    }
    for (const encoded of input.packets) observe(encoded)
    if (services.size < 3 && process.argv[2] !== undefined) {
        const fd = Number(process.argv[2])
        assert.ok(Number.isInteger(fd) && fd > 0)
        // Node22 bind({fd}) wraps the already-bound Linux fixture socket. The
        // parent retains its descriptor, so no close/rebind packet gap exists.
        // https://nodejs.org/download/release/v22.21.1/docs/api/dgram.html#socketbindoptions-callback
        const socket = dgram.createSocket("udp4")
        try {
            await new Promise((resolve, reject) => {
                const timer = setTimeout(
                    () =>
                        reject(
                            new Error(
                                "Current native discovery packets missing for " +
                                    Object.keys(input.endpoints)
                                        .filter((name) => !services.has(name))
                                        .join(",")
                            )
                        ),
                    5000
                )
                const finish = (error) => {
                    clearTimeout(timer)
                    error ? reject(error) : resolve()
                }
                socket.on("error", finish)
                socket.on("message", (bytes, sender) => {
                    try {
                        assert.equal(sender.address, "127.0.0.1")
                        assert.ok(input.packets.length < 128)
                        const encoded = bytes.toString("base64")
                        input.packets.push(encoded)
                        observe(encoded)
                        if (services.size === 3) finish()
                    } catch (error) {
                        finish(error)
                    }
                })
                socket.bind({ fd })
            })
        } finally {
            await new Promise((resolve) => socket.close(resolve))
        }
    }
    assert.deepEqual([...services].sort(), ["CONTROLLER", "REMOTE", "STAGE"])
    assert.equal(suffixes.size, 1)
    // Use the source dependency's real wire encoder/decoder so omitted SRV
    // priority/weight become their actual encoded zero defaults on both sides.
    const expected = unique(Object.entries(input.endpoints).flatMap(([kind, endpoint]) => dns.decode(dns.encode({ type: "response", answers: original.records(kind, Number(endpoint.split(":").at(-1))) })).answers.map(normalize)))
    const observed = unique(actual)
    assert.deepEqual(observed, expected)
    process.stdout.write(JSON.stringify({ sourceHash, instanceSuffix: [...suffixes][0], expected, observed, packets: input.packets.length, rawPackets: input.packets }) + "\n")
} finally {
    await original.close()
}
