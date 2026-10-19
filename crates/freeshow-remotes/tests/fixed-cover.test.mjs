// Actual pinned GET handler + Express/send, compared with actual native TCP.
// Native source is an explicitly CLI-selected inert fixture, not app authority.
import assert from "node:assert/strict"
import { spawn, execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import fs from "node:fs"
import http from "node:http"
import os from "node:os"
import path from "node:path"
import { createInterface } from "node:readline"
import { test } from "node:test"
import { fileURLToPath } from "node:url"
import express from "express"
import finalhandler from "finalhandler"
import ts from "typescript"

const root = fileURLToPath(new URL("../../../", import.meta.url))
const binary = process.env.FREESHOW_REMOTES_COVER_BIN || path.join(root, "target/debug/examples", "cover-serving-parity" + (process.platform === "win32" ? ".exe" : ""))
const source = execFileSync("git", ["show", "2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/servers.ts"], { cwd: root, encoding: "utf8" }).replace(/\r\n/g, "\n")
assert.equal(createHash("sha256").update(source).digest("hex"), "2b8c81b56e77b754dc617e31777e4b3e1d9d15b368c369efc9d58030c1af4b41")
const ast = ts.createSourceFile("servers.ts", source, ts.ScriptTarget.Latest, true)
let callback
function visit(node) {
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression) && node.expression.name.text === "get" && ts.isStringLiteral(node.arguments[0]) && node.arguments[0].text === "/nowplayingcover") callback = node.arguments[1].getText(ast)
    ts.forEachChild(node, visit)
}
visit(ast)
assert.ok(callback)
const javascript = ts.transpileModule(`const handler = ${callback};`, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
const handler = new Function("getNowPlayingMediaPath", "doesPathExist", javascript + "\nreturn handler")
const stable = (headers) => Object.fromEntries(Object.entries(headers).filter(([name]) => name !== "date" && name !== "connection"))
let comparisons = 0

async function bounded(promise, label) {
    let timer
    try {
        return await Promise.race([promise, new Promise((_, reject) => (timer = setTimeout(() => reject(new Error(label)), 5000)))])
    } finally {
        clearTimeout(timer)
    }
}

async function request(port, method, target, headers = {}) {
    return await new Promise((resolve, reject) => {
        const request = http.request({ hostname: "127.0.0.1", port, method, path: target, headers, agent: false }, (response) => {
            const parts = []
            response.on("data", (part) => parts.push(part))
            response.on("error", reject)
            response.on("end", () => resolve({ status: response.statusCode, headers: stable(response.headers), body: Buffer.concat(parts) }))
        })
        request.setTimeout(5000, () => request.destroy(new Error("controlled request timed out")))
        request.on("error", reject)
        request.end()
    })
}
async function fixture(t, bytes, mode = "file", environment = "production", modified = 1_700_000_000_123) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-native-fixed-cover-"))
    const file = path.join(directory, "NowPlayingCover.png")
    fs.writeFileSync(file, bytes)
    fs.utimesSync(file, new Date(modified), new Date(modified))
    const stat = fs.statSync(file)
    const assets = path.join(directory, "assets")
    fs.mkdirSync(assets)
    fs.writeFileSync(path.join(assets, "nowplayingcover"), "PUBLIC FALLBACK")
    fs.writeFileSync(path.join(directory, "other.png"), "NEVER SELECTED BY QUERY")
    if (mode === "missing") fs.unlinkSync(file)
    let lookups = 0
    const app = express()
    app.set("env", environment)
    app.get(
        "/nowplayingcover",
        handler(() => {
            lookups++
            return file
        }, fs.existsSync)
    )
    app.use(express.static(assets))
    const server = http.createServer((req, res) => app.handle(req, res, finalhandler(req, res, { env: environment, onerror() {} })))
    const sockets = new Set()
    server.on("connection", (socket) => {
        sockets.add(socket)
        socket.once("close", () => sockets.delete(socket))
    })
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve))
    const child = spawn(binary, [mode, file, String(stat.mtimeMs), environment, assets], { stdio: ["pipe", "pipe", "pipe"] })
    const exited = new Promise((resolve) => {
        child.once("exit", (code, signal) => resolve({ code, signal }))
        child.once("error", (error) => resolve({ error }))
    })
    child.stdin.on("error", () => {})
    let stderr = ""
    child.stderr.on("data", (chunk) => {
        stderr += chunk
        assert.ok(stderr.length <= 65536)
    })
    const reader = createInterface({ input: child.stdout })
    const lines = reader[Symbol.asyncIterator]()
    let started = false
    t.after(async () => {
        try {
            if (started) {
                child.stdin.end("stop\n")
                const receipt = await bounded(lines.next(), "native joined receipt timed out")
                assert.deepEqual(JSON.parse(receipt.value), { joined: true }, stderr)
                const { code, signal, error } = await bounded(exited, "native process exit timed out")
                assert.ifError(error)
                assert.equal(code, 0, stderr)
                assert.equal(signal, null)
            }
        } finally {
            try {
                if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL")
                await bounded(exited, "native fixture cleanup exit timed out")
            } finally {
                reader.close()
                for (const socket of sockets) socket.destroy()
                await bounded(new Promise((resolve) => server.close(resolve)), "source server close timed out")
                fs.rmSync(directory, { recursive: true, force: true })
            }
        }
    })
    const first = await bounded(lines.next(), "native listener startup timed out")
    assert.equal(first.done, false, stderr)
    const bound = JSON.parse(first.value)
    assert.deepEqual(Object.keys(bound.endpoints).sort(), ["CONTROLLER", "REMOTE", "STAGE"])
    started = true
    return {
        ports: Object.values(bound.endpoints).map((address) => Number(address.split(":").at(-1))),
        original: server.address().port,
        lookups: () => lookups
    }
}

test("actual fixed-file cover HTTP agrees with source across three native ports, conditionals, ranges and duplicate headers", async (t) => {
    const bytes = Buffer.from("89504e470d0a1a0a00ff112233445566778899aabbccddeeff", "hex")
    const f = await fixture(t, bytes)
    const initial = await request(f.original, "GET", "/nowplayingcover")
    const etag = initial.headers.etag
    const cases = [
        ["GET", "/nowplayingcover"],
        ["HEAD", "/nowplayingcover"],
        ["GET", "/NOWPLAYINGCOVER/?filePath=other.png&path=../../other.png"],
        ["GET", "/nowplayingcover", { Range: "bytes=1-3" }],
        ["HEAD", "/nowplayingcover", { Range: "bytes=2-5" }],
        ["GET", "/nowplayingcover", { Range: "bytes=-4" }],
        ["GET", "/nowplayingcover", { Range: "bytes=2-" }],
        ["GET", "/nowplayingcover", { Range: "bytes=0-1,4-5" }],
        ["GET", "/nowplayingcover", { Range: "bytes=1-2,3-4" }],
        ["GET", "/nowplayingcover", { "If-None-Match": etag }],
        ["GET", "/nowplayingcover", { "If-None-Match": "*" }],
        ["GET", "/nowplayingcover", { "If-None-Match": etag, "Cache-Control": "no-cache" }],
        ["GET", "/nowplayingcover", { "If-Modified-Since": initial.headers["last-modified"] }],
        ["GET", "/nowplayingcover", { "If-Match": "bad" }],
        ["HEAD", "/nowplayingcover", { "If-Match": "bad" }],
        ["GET", "/nowplayingcover", { Range: "bytes=99999-" }],
        ["HEAD", "/nowplayingcover", { Range: "bytes=99999-" }],
        ["GET", "/nowplayingcover", { Range: "bytes=1-3", "If-Range": 'W/"different"' }],
        ["GET", "/nowplayingcover", ["Host", "localhost", "Connection", "close", "If-Match", "bad", "If-Match", etag]],
        ["GET", "/nowplayingcover", ["Host", "localhost", "Connection", "close", "If-Modified-Since", "Fri, 01 Jan 2099 00:00:00 GMT", "If-Modified-Since", "Thu, 01 Jan 1970 00:00:00 GMT"]],
        ["GET", "/nowplayingcover", { "If-None-Match": "*", "Cache-Control": "\u00a0no-cache\u00a0" }],
        ["OPTIONS", "/nowplayingcover?other=file", { Range: "bytes=99999-" }]
    ]
    for (const [method, target, headers] of cases) {
        const before = f.lookups()
        const actual = await request(f.original, method, target, headers)
        if (method === "OPTIONS") assert.equal(f.lookups(), before)
        for (const port of f.ports) {
            const native = await request(port, method, target, headers)
            assert.deepEqual(native, actual, `${method} ${target} ${JSON.stringify(headers)} port ${port}`)
            comparisons++
        }
    }
    // Encoded spelling is ordinary static fallthrough, never fixed-file lookup.
    const before = f.lookups()
    for (const port of [f.original, ...f.ports]) {
        const response = await request(port, "GET", "/nowplaying%63over")
        assert.equal(response.status, 200)
        assert.equal(response.body.toString(), "PUBLIC FALLBACK")
    }
    assert.equal(f.lookups(), before)
})

test("empty, opaque and absent source guard keep exact source headers/body, including HEAD and pre-epoch dates", async (t) => {
    for (const [bytes, mode, modified] of [
        [Buffer.alloc(0), "file", 1_700_000_000_123],
        [Buffer.from("arbitrary bytes"), "file", -50654],
        [Buffer.from("missing"), "missing", 1_700_000_000_123]
    ]) {
        await t.test(`${mode} ${bytes.length} ${modified}`, async (t) => {
            const f = await fixture(t, bytes, mode, "production", modified)
            for (const method of ["GET", "HEAD"]) {
                const actual = await request(f.original, method, "/nowplayingcover?file=other.png")
                for (const port of f.ports) {
                    assert.deepEqual(await request(port, method, "/nowplayingcover?file=other.png"), actual)
                    comparisons++
                }
            }
        })
    }
})

test("unconfigured capability stays501 and development error rendering is explicitly unsupported", async (t) => {
    await t.test("unconfigured", async (t) => {
        const f = await fixture(t, Buffer.from("file"), "unconfigured")
        for (const port of f.ports) assert.equal((await request(port, "GET", "/nowplayingcover")).status, 501)
    })
    await t.test("development", async (t) => {
        const f = await fixture(t, Buffer.from("file"), "file", "development")
        const actual = await request(f.original, "GET", "/nowplayingcover", { "If-Match": "wrong" })
        assert.equal(actual.status, 412)
        for (const port of f.ports) {
            const native = await request(port, "GET", "/nowplayingcover", { "If-Match": "wrong" })
            assert.equal(native.status, 501)
            assert.equal(native.body.toString(), "cover_error_rendering_unsupported")
        }
    })
})

test.after(() => console.log(`Fixed-cover native/source complete HTTP comparisons: ${comparisons}; only Date/connection excluded, no empty-success or renderer/native-authority claim`))
