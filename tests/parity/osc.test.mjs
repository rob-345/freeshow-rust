import assert from "node:assert/strict"
import dgram from "node:dgram"
import { EventEmitter } from "node:events"
import fs from "node:fs"
import { createRequire } from "node:module"
import path from "node:path"
import { spawn, spawnSync, execFileSync } from "node:child_process"
import { createInterface } from "node:readline"
import { test } from "node:test"
import vm from "node:vm"
import { root, upstreamFunctions } from "./oracle.mjs"

const require = createRequire(import.meta.url)
const binary = process.env.FREESHOW_OSC_BIN || path.join(root, "target/debug/examples", process.platform === "win32" ? "osc-parity.exe" : "osc-parity")
const original = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"

// Load the real dependency, substituting only its dgram OS boundary. No copied
// coercion, address normalization or OSC encoder is used as the oracle.
let sockets = []
const oscModule = { exports: {} }
new vm.Script(fs.readFileSync(require.resolve("osc-js"), "utf8"), { filename: require.resolve("osc-js") }).runInContext(
    vm.createContext({
        module: oscModule,
        exports: oscModule.exports,
        Buffer,
        DataView,
        Uint8Array,
        ArrayBuffer,
        TextDecoder,
        global: { TextDecoder },
        require: (name) =>
            name === "dgram"
                ? {
                      createSocket(type) {
                          assert.equal(type, "udp4")
                          const socket = new EventEmitter()
                          socket.bind = (options, callback) => {
                              socket.bound = options
                              socket.ready = callback
                          }
                          socket.send = (buffer, offset, length, port, host) => {
                              socket.sent = { packet: Array.from(buffer.subarray(offset, offset + length)), port, host }
                          }
                          socket.close = (callback) => {
                              if (socket.closed) throw new Error("Socket already closed")
                              socket.closed = true
                              callback?.()
                          }
                          sockets.push(socket)
                          return socket
                      }
                  }
                : require(name)
    })
)
const OSC = oscModule.exports
upstreamFunctions("src/electron/utils/oscPatch.ts", ["pad", "packString", "packInt32", "packInt64", "packUInt64", "packFloat32", "packFloat64", "packBlob", "patchOSC"], {
    OSC,
    TextEncoder,
    Uint8Array,
    ArrayBuffer,
    DataView,
    BigInt
}).patchOSC()

// Observe the real error event without replacing validation. osc-js catches
// encoding failures inside its open notification and routes them to on(error).
let lastOscError
const originalOn = OSC.prototype.on
OSC.prototype.on = function (name, callback) {
    return originalOn.call(
        this,
        name,
        name === "error"
            ? (error) => {
                  lastOscError = error
                  return callback(error)
              }
            : callback
    )
}

let timers = []
const { emitOSC } = upstreamFunctions("src/electron/utils/api.ts", ["emitOSC", "convertToType"], {
    OSC,
    setTimeout(callback, delay) {
        assert.equal(delay, 100)
        timers.push(callback)
    }
})

function oracle(payload, injectError = false) {
    sockets = []
    timers = []
    lastOscError = undefined
    try {
        emitOSC(payload)
        if (!sockets.length) return { ignored: true }
        assert.equal(sockets.length, 1)
        const [socket] = sockets
        assert.deepEqual(JSON.parse(JSON.stringify(socket.bound)), { address: "0.0.0.0", port: 0, exclusive: false })
        socket.ready()
        if (injectError) socket.emit("error", new Error("injected OS failure"))
        for (const callback of timers) callback()
        assert.equal(socket.closed, true)
        return socket.sent || { error: lastOscError?.message }
    } catch (error) {
        return { error: error.message }
    }
}

function rust(cases) {
    const child = spawnSync(binary, [], {
        input: cases.map((input) => JSON.stringify(input)).join("\n") + "\n",
        encoding: "utf8",
        maxBuffer: 64 * 1024 * 1024,
        timeout: 30000
    })
    assert.equal(child.error, undefined)
    assert.equal(child.status, 0, child.stderr)
    return child.stdout
        .trim()
        .split("\n")
        .map((line) => JSON.parse(line))
}

function checkMessages(messages) {
    const [batch] = rust([{ op: "batch", cases: messages.map((data) => ({ op: "prepare", payload: { data } })) }])
    assert.equal(batch.ok, true, JSON.stringify(batch.error))
    for (const [index, data] of messages.entries()) {
        const expected = oracle({ data })
        assert.equal(expected.error, undefined, data)
        assert.deepEqual(batch.value[index].packet, expected.packet, JSON.stringify(data))
        assert.equal(batch.value[index].host, expected.host)
        assert.equal(batch.value[index].port, expected.port)
    }
}

test("OSC oracle is the pinned original handler/patch and installed osc-js 2.4.1", () => {
    assert.equal(require("osc-js/package.json").version, "2.4.1")
    for (const filename of ["src/electron/utils/api.ts", "src/electron/utils/oscPatch.ts"]) {
        assert.equal(fs.readFileSync(path.join(root, filename), "utf8").replaceAll("\r\n", "\n"), execFileSync("git", ["show", `${original}:${filename}`], { cwd: root, encoding: "utf8" }).replaceAll("\r\n", "\n"))
    }
})

test("EMIT_OSC bytes match actual TypeScript/osc-js across reachable tags, Unicode and spacing", () => {
    checkMessages([
        "/",
        "//",
        "///",
        "show/slide/",
        "🙌",
        "/日本語/🙌🏾/ مرحبًا Ελληνικά 日本語 🙌🏾 e\u0301",
        "/types true false True FALSE null undefined NaN nan Infinity +Infinity -Infinity inf -inf",
        "/empty ",
        "/spacing  a   b ",
        "/whitespace \t \n \r \v \f \u00a0 \u1680 \u2000 \u2028 \u2029 \u202f \u205f \u3000 \ufeff \u0085",
        "/numbers 0 -0 +0 0001 -01 1.0 -1.0 1. .5 -.5 +.5 1e-2 1e+2 1E-2 1_000 01n 0b101 0o777 0xdeadBEEF -0x1 +0b1",
        "/range 2147483647 2147483648 -2147483648 -2147483649 4294967295 4294967296 -4294967297 9007199254740991 9007199254740992 9007199254740993 18446744073709551615",
        "/floats 1.1754943508222875e-38 1e-45 -1e-45 1e-46 3.4028234663852886e38 3.4028236e38 1.7976931348623157e308 1e309 -1e309 5e-324 -5e-324 -1e-400 1e-9999999 1e9999999",
        "/grammar . -. +. 1e 1e+ 1e- 0x 0b 0o 0b2 0o8 0xg ١٢ １２ 0xffjunk +Infinityjunk NaNfalse",
        "/nul\0address a\0b",
        "/control\tline\n 1\n \u200b \u180e",
        `/x ${"a".repeat(65492)}`
    ])
})

test("large radix numbers and decimal rounding match JS-to-wire conversions differentially", () => {
    let state = 0x6d2b79f5
    const next = () => {
        state ^= state << 13
        state ^= state >>> 17
        state ^= state << 5
        return state >>> 0
    }
    const messages = []
    for (let index = 0; index < 700; index++) {
        const length = 1 + (next() % 330)
        for (const [radix, prefix] of [
            [2, "0b"],
            [8, "0o"],
            [16, "0x"]
        ]) {
            const number = Array.from({ length }, () => (next() % radix).toString(radix)).join("")
            messages.push(`/radix ${prefix}${number}`)
        }
        const decimal = `${next() % 2 ? "-" : "+"}${next()}${next()}.${next()}e${(next() % 700) - 350}`
        messages.push(`/decimal ${decimal}`)
    }
    for (const number of ["20000000000001", "20000000000003", "3fffffffffffff", "1fffffffffffff", "10000000000000800000000000000000000000000000000000000000000000000000000000000001"]) {
        messages.push(`/round 0x${number}`)
    }
    checkMessages(messages)
})

test("payload/default handling preserves JS truthiness, host key and port numeric strings", () => {
    const payloads = [false, true, 0, 1, "text", [], {}, { data: null }, { data: ["/typed", true] }, { data: 42 }]
    for (const signal of [undefined, null, false, 0, "", [], {}, { url: "127.0.0.1" }, { host: "", port: "" }, { host: false, port: false }, { host: 0, port: 0 }, { host: "127.0.0.1", port: "8081" }, { host: "localhost", port: "0x2000" }, { host: "127.0.0.1", port: "1e2" }, { port: "0" }, { port: "-0" }, { port: "0x0" }, { port: "\t8080\ufeff" }]) {
        payloads.push({ signal, data: "/defaults" })
    }
    const results = rust(payloads.map((payload) => ({ op: "prepare", payload })))
    for (const [index, payload] of payloads.entries()) {
        const expected = oracle(payload)
        const actual = results[index]
        assert.equal(actual.ok, true, JSON.stringify(payload))
        if (expected.ignored) assert.equal(actual.value, null)
        else {
            assert.equal(actual.value.host, expected.host, JSON.stringify(payload))
            assert.equal(actual.value.port, Number(expected.port) | 0, JSON.stringify(payload))
            assert.deepEqual(actual.value.packet, expected.packet)
        }
    }
})

test("invalid addresses fail like the actual patched encoder, including UTF-16 length quirks", () => {
    const messages = ["", " leading", "x", "é", "a/", "\t", "\n"]
    const results = rust(messages.map((data) => ({ op: "prepare", payload: { data } })))
    for (const [index, data] of messages.entries()) {
        assert.equal(oracle({ data }).error, "OSC Message has an invalid address", JSON.stringify(data))
        assert.equal(results[index].ok, false)
        assert.equal(results[index].error.kind, "invalid_address")
    }
    assert.ok(oracle(null).error)
    assert.equal(rust([{ op: "prepare", payload: null }])[0].error.kind, "invalid_payload")
    assert.ok(oracle({ data: "/close" }, true).packet, "upstream error and delayed close may both run")
})

test("invalid host/port inputs are explicit errors where actual Node UDP validation throws", () => {
    const payloads = []
    for (const port of [true, [], {}, -1, 65536, 1.5, " ", "\t", "bad", "Infinity", "-Infinity", "NaN", "-0x1", "0b2", "65536", "2.5"]) {
        payloads.push({ data: "/invalid", signal: { host: "127.0.0.1", port } })
    }
    for (const host of [true, 1, [], {}]) payloads.push({ data: "/invalid", signal: { host, port: 8080 } })
    const results = rust(payloads.map((payload) => ({ op: "prepare", payload })))
    for (const [index, payload] of payloads.entries()) {
        const socket = dgram.createSocket("udp4")
        // These cases synchronously fail in Node before binding or resolving;
        // no packet can leave the process, including for a non-string host.
        assert.throws(() => socket.send(Buffer.from([0]), 0, 1, payload.signal.port, payload.signal.host))
        socket.close()
        assert.equal(results[index].ok, false, JSON.stringify(payload))
        assert.equal(results[index].error.kind, index < 16 ? "invalid_port" : "invalid_host")
    }
})

test("validation precedence encodes first, then checks port before host like Node UDP", () => {
    const cases = [
        { payload: { data: "x", signal: { port: -1, host: true } }, kind: "invalid_address" },
        { payload: { data: "/x", signal: { port: -1, host: true } }, kind: "invalid_port" },
        { payload: { data: "/x", signal: { port: 1, host: true } }, kind: "invalid_host" }
    ]
    const results = rust(cases.map(({ payload }) => ({ op: "prepare", payload })))
    for (const [index, { kind }] of cases.entries()) assert.equal(results[index].error.kind, kind)
})

function stream() {
    const child = spawn(binary, [], { stdio: ["pipe", "pipe", "pipe"] })
    const lines = createInterface({ input: child.stdout })
    const iterator = lines[Symbol.asyncIterator]()
    let stderr = ""
    child.stderr.on("data", (data) => (stderr += data))
    return {
        async request(payload) {
            child.stdin.write(JSON.stringify({ op: "send", payload }) + "\n")
            const { value, done } = await iterator.next()
            assert.equal(done, false, stderr)
            return JSON.parse(value)
        },
        async close() {
            child.stdin.end()
            const code = await new Promise((resolve) => child.once("close", resolve))
            assert.equal(code, 0, stderr)
        },
        kill: () => child.kill()
    }
}

test("Rust UDP sends interoperate with actual osc-js decoder on controlled loopback", { timeout: 15000 }, async () => {
    const receiver = dgram.createSocket("udp4")
    await new Promise((resolve) => receiver.bind(0, "127.0.0.1", resolve))
    const port = receiver.address().port
    const runtime = stream()
    try {
        for (const data of ["/interop true false 1 -2 1.25 Unicode_🙌", "/日本語/🙌🏾 مرحبًا Ελληνικά 日本語", "/spacing  repeated   spaces ", "/range -2147483648 2147483648 4294967297 9007199254740993", "/float Infinity -Infinity 1e309 1e-45 -1e-45 -0", "/", "/hostname 0x100000001"]) {
            const expected = oracle({ data })
            const packet = new Promise((resolve, reject) => {
                const timer = setTimeout(() => reject(new Error("loopback packet timed out")), 3000)
                receiver.once("message", (bytes, source) => {
                    clearTimeout(timer)
                    assert.equal(source.address, "127.0.0.1")
                    resolve(bytes)
                })
            })
            const outcome = await runtime.request({ signal: { host: data.startsWith("/hostname") ? "localhost" : "127.0.0.1", port }, data })
            assert.equal(outcome.ok, true, JSON.stringify(outcome.error))
            assert.equal(outcome.value.status, "sent")
            const bytes = await packet
            assert.deepEqual(Array.from(bytes), expected.packet, data)
            const decoded = new OSC.Message()
            decoded.unpack(new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength))
            const reference = new OSC.Message()
            reference.unpack(new DataView(Uint8Array.from(expected.packet).buffer))
            assert.equal(decoded.address, reference.address)
            assert.equal(decoded.types, reference.types)
            assert.deepEqual(decoded.args, reference.args)
        }
        assert.equal((await runtime.request({ data: 1 })).value.status, "ignored")
        assert.equal((await runtime.request({ signal: { host: "127.0.0.1", port }, data: "" })).error.kind, "invalid_address")
        // Node checks numeric zero before coercing strings. On an already bound
        // socket (as in the original OSC handler), "0" reaches the host's UDP
        // sender. Compare that actual Node boundary, including its wildcard
        // bind address, rather than the host's bare UDP send_to behavior.
        const probe = dgram.createSocket("udp4")
        try {
            await new Promise((resolve) => probe.bind(0, "0.0.0.0", resolve))
            assert.throws(() => probe.send(Buffer.from([0]), 0, "127.0.0.1"), { code: "ERR_SOCKET_BAD_PORT" })
            const nodeError = await new Promise((resolve) => probe.send(Buffer.from([0]), 0, 1, "0", "127.0.0.1", resolve))
            const actual = await runtime.request({ signal: { host: "127.0.0.1", port: "0" }, data: "/failure" })
            assert.equal(actual.ok, !nodeError, JSON.stringify({ nodeError: nodeError && { code: nodeError.code, message: nodeError.message }, actual }))
            if (nodeError) assert.equal(actual.error.kind, "send_failed")
            else assert.equal(actual.value.status, "sent")
        } finally {
            probe.close()
        }
        // The harness itself rejects accidental non-loopback destinations.
        assert.equal((await runtime.request({ signal: { host: "192.0.2.1", port }, data: "/blocked" })).error.kind, "test_destination_not_loopback")
        await runtime.close()
    } finally {
        runtime.kill()
        receiver.close()
    }
})
