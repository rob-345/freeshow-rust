import assert from "node:assert/strict"
import { execFileSync, spawn } from "node:child_process"
import { createHash } from "node:crypto"
import fs from "node:fs"
import http from "node:http"
import net from "node:net"
import os from "node:os"
import path from "node:path"
import { createInterface } from "node:readline"
import { test } from "node:test"
import { fileURLToPath } from "node:url"
import express from "express"
import finalhandler from "finalhandler"
import ts from "typescript"

const root = fileURLToPath(new URL("../../", import.meta.url))
const binary = process.env.FREESHOW_REMOTES_COVER_BIN || path.join(root, "target/debug/examples/cover-serving-parity" + (process.platform === "win32" ? ".exe" : ""))
const sourceCommit = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const sourceHash = "2b8c81b56e77b754dc617e31777e4b3e1d9d15b368c369efc9d58030c1af4b41"
const source = execFileSync("git", ["show", `${sourceCommit}:src/electron/servers.ts`], { cwd: root, encoding: "utf8", maxBuffer: 1_048_576 }).replace(/\r\n/g, "\n")
assert.equal(createHash("sha256").update(source).digest("hex"), sourceHash)
const ast = ts.createSourceFile("servers.ts", source, ts.ScriptTarget.Latest, true)
let callback
function visit(node) {
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression) && node.expression.name.text === "get" && ts.isStringLiteral(node.arguments[0]) && node.arguments[0].text === "/nowplayingcover") callback = node.arguments[1].getText(ast)
    ts.forEachChild(node, visit)
}
visit(ast)
assert.ok(callback)
const javascript = ts.transpileModule(`const handler = ${callback};`, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
const originalHandler = new Function("getNowPlayingMediaPath", "doesPathExist", javascript + "\nreturn handler")
const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX1sAAAAASUVORK5CYII=", "base64")

async function bounded(promise, label) {
    let timer
    try {
        return await Promise.race([promise, new Promise((_, reject) => (timer = setTimeout(() => reject(new Error(label)), 5000)))])
    } finally {
        clearTimeout(timer)
    }
}

// This sends the supplied target byte-for-byte, without URL/client normalization.
// Only the bounded test response is parsed; this is not a production HTTP parser.
async function rawRequest(port, method, target) {
    const wire = Buffer.from(`${method} ${target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n`, "latin1")
    assert.ok(wire.length < 16_384)
    const socket = net.createConnection({ host: "127.0.0.1", port })
    const parts = []
    let size = 0
    try {
        await new Promise((resolve, reject) => {
            socket.setTimeout(5000, () => socket.destroy(new Error("raw cover request timed out")))
            socket.once("error", reject)
            socket.once("connect", () => socket.write(wire))
            socket.on("data", (part) => {
                size += part.length
                if (size > 65_536) socket.destroy(new Error("raw cover response limit exceeded"))
                else parts.push(part)
            })
            socket.once("close", (hadError) => {
                if (!hadError) resolve()
            })
        })
        const response = Buffer.concat(parts)
        const boundary = response.indexOf("\r\n\r\n")
        assert.ok(boundary >= 0, "a complete HTTP response head was received")
        const [statusLine, ...fields] = response.subarray(0, boundary).toString("latin1").split("\r\n")
        assert.match(statusLine, /^HTTP\/1\.[01] \d{3} /)
        const headers = {}
        for (const field of fields) {
            const colon = field.indexOf(":")
            assert.ok(colon > 0)
            const name = field.slice(0, colon).toLowerCase()
            const value = field.slice(colon + 1).trim()
            assert.equal(Object.hasOwn(headers, name), false, `unexpected duplicate response field ${name}`)
            if (name !== "date" && name !== "connection") headers[name] = value
        }
        const body = response.subarray(boundary + 4)
        assert.equal(headers["transfer-encoding"], undefined, "these small fixed representations have explicit lengths")
        if (method === "HEAD") assert.equal(body.length, 0)
        else assert.equal(body.length, Number(headers["content-length"]))
        return { status: Number(statusLine.split(" ")[1]), headers, body, targetBytes: Buffer.byteLength(target, "latin1"), headBytes: wire.length }
    } finally {
        socket.destroy()
    }
}

async function fixture(t) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-cover-raw-target-"))
    const file = path.join(directory, "NowPlayingCover.png")
    const assets = path.join(directory, "assets")
    fs.mkdirSync(assets)
    fs.writeFileSync(file, png)
    fs.utimesSync(file, new Date(1_700_000_000_123), new Date(1_700_000_000_123))
    const stat = fs.statSync(file)
    let lookups = 0
    const app = express()
    app.set("env", "production")
    app.get(
        "/nowplayingcover",
        originalHandler(() => {
            lookups++
            return file
        }, fs.existsSync)
    )
    app.use(express.static(assets))
    const server = http.createServer((req, res) => app.handle(req, res, finalhandler(req, res, { env: "production", onerror() {} })))
    const sockets = new Set()
    server.on("connection", (socket) => {
        sockets.add(socket)
        socket.once("close", () => sockets.delete(socket))
    })
    let child
    let exited
    let lines
    let reader
    let started = false
    let stderr = ""
    t.after(async () => {
        try {
            if (started) {
                child.stdin.end("stop\n")
                const receipt = await bounded(lines.next(), "native actual joined receipt timed out")
                assert.equal(receipt.done, false, stderr)
                assert.deepEqual(JSON.parse(receipt.value), { joined: true }, stderr)
                const result = await bounded(exited, "native fixture exit timed out")
                assert.ifError(result.error)
                assert.equal(result.code, 0, stderr)
                assert.equal(result.signal, null)
            }
        } finally {
            try {
                if (child) {
                    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL")
                    await bounded(exited, "native failed-fixture kill/reap timed out")
                    reader.close()
                }
            } finally {
                for (const socket of sockets) socket.destroy()
                if (server.listening) await bounded(new Promise((resolve) => server.close(resolve)), "source server close timed out")
                fs.rmSync(directory, { recursive: true, force: true })
            }
        }
    })
    await bounded(
        new Promise((resolve, reject) => {
            server.once("error", reject)
            server.listen(0, "127.0.0.1", resolve)
        }),
        "source listener startup timed out"
    )
    child = spawn(binary, ["file", file, String(stat.mtimeMs), "production", assets], { stdio: ["pipe", "pipe", "pipe"] })
    exited = new Promise((resolve) => {
        child.once("exit", (code, signal) => resolve({ code, signal }))
        child.once("error", (error) => resolve({ error }))
    })
    child.stdin.on("error", () => {})
    child.stderr.on("data", (chunk) => {
        const part = chunk.toString()
        const remaining = Math.max(0, 65_536 - stderr.length)
        stderr += part.slice(0, remaining)
        if (part.length > remaining) child.kill("SIGKILL")
    })
    reader = createInterface({ input: child.stdout })
    lines = reader[Symbol.asyncIterator]()
    const first = await bounded(lines.next(), "native listener startup timed out")
    assert.equal(first.done, false, stderr)
    const bound = JSON.parse(first.value)
    assert.deepEqual(Object.keys(bound.endpoints).sort(), ["CONTROLLER", "REMOTE", "STAGE"])
    started = true
    return { original: server.address().port, ports: Object.values(bound.endpoints).map((address) => Number(address.split(":").at(-1))), lookups: () => lookups }
}

test("raw cover targets record parsed-URI fragment matches and the backslash capability gap", { timeout: 45_000 }, async (t) => {
    const f = await fixture(t)
    const observations = []
    async function compare(method, target, label) {
        const before = f.lookups()
        const source = await rawRequest(f.original, method, target)
        assert.equal(f.lookups(), before + 1, "actual original fixed-cover handler was entered")
        assert.equal(source.status, 200)
        assert.deepEqual(source.body, method === "HEAD" ? Buffer.alloc(0) : png)
        for (const port of f.ports) assert.deepEqual(await rawRequest(port, method, target), source, label)
        observations.push({ label, method, sourceStatus: source.status, nativeStatus: 200, targetBytes: source.targetBytes, headBytes: source.headBytes, headers: source.headers })
    }
    await t.test("literal fragments are accepted by both real transports, not rejected by the pure raw matcher", async () => {
        await compare("GET", "/nowplayingcover#fragment", "literal fragment")
        await compare("HEAD", "/NOWPLAYINGCOVER/?ignored=1#fragment", "query and fragment")
        await compare("GET", "/nowplayingcover#\\after-fragment", "backslash after fragment")
    })
    await t.test("a raw target above 8192B survives because the parsed target is short, under the separate 16KiB head cap", async () => {
        const target = "/nowplayingcover#" + "f".repeat(9000)
        assert.ok(Buffer.byteLength(target) > 8192)
        await compare("GET", target, "9000-byte fragment suffix")
    })
    await t.test("a backslash before a fragment enters the source cover handler but falls through to native static rejection", async () => {
        const target = "/nowplayingcover\\#fragment"
        const before = f.lookups()
        const source = await rawRequest(f.original, "GET", target)
        assert.equal(f.lookups(), before + 1)
        assert.equal(source.status, 200)
        assert.deepEqual(source.body, png)
        let native
        for (const port of f.ports) {
            const actual = await rawRequest(port, "GET", target)
            assert.equal(actual.status, 400)
            assert.equal(actual.body.toString(), "Invalid asset path")
            if (native) assert.deepEqual(actual, native)
            native = actual
        }
        observations.push({ label: "backslash before fragment: explicit capability difference", source, native })
    })
    t.diagnostic(JSON.stringify({ node: process.version, sourceCommit, sourceHash, binary, observations }))
})
