import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import net from "node:net"
import vm from "node:vm"
import { once } from "node:events"
import { spawnSync } from "node:child_process"
import test from "node:test"
import ts from "typescript"
import { clean, root, upstreamFunctions } from "./oracle.mjs"
import { Host, request, fixtures, context, origin } from "../../crates/freeshow-assets/tools/host.mjs"

async function until(predicate, timeout = 2000) {
    const end = Date.now() + timeout
    while (Date.now() < end) {
        const value = await predicate()
        if (value) return value
        await new Promise((resolve) => setTimeout(resolve, 10))
    }
    assert.fail("Asset state did not settle before its test deadline")
}
async function setup(t, config = {}) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-assets-"))
    const host = await new Host().start(config)
    t.after(async () => {
        await host.close()
        fs.rmSync(directory, { recursive: true, force: true })
    })
    return { directory, host, ...fixtures(directory) }
}

test("source protected range policy, including malformed, suffix, empty and first-match conditions", async (t) => {
    const host = new Host()
    t.after(() => host.close())
    const source = upstreamFunctions("src/electron/data/protected.ts", ["parseRange"])
    const headers = [null, "", "bytes=0-0", "bytes=0-", "bytes=-1", "bytes=-0", "bytes=-", "bytes=1-0", "bytes=2-1", "bytes=999-", "bytes=0-999999", "bytes=0-1,4-6", "items=0-1", "Bytes=0-1", "prefix bytes=2-4 trailing", "bytes=bad bytes=1-2", "bytes= 0-1", "bytes=-01", "bytes=1.5-2", "bytes=+1-2", "bytes=0-1\nbytes=2-3", "bytes=" + "9".repeat(1000) + "-", "bytes=-" + "9".repeat(1000)]
    const cases = []
    for (const length of [0, 1, 2, 100, 8193, 2 ** 40]) for (const header of headers) cases.push({ op: "range", header, length })
    const actual = await host.call({ op: "specs", cases })
    for (const [index, input] of cases.entries()) assert.deepEqual(actual[index], clean(source.parseRange(input.header, input.length)), JSON.stringify(input))
    const mimeSource = fs.readFileSync(path.join(root, "src/electron/data/media.ts"), "utf8")
    const ast = ts.createSourceFile("media.ts", mimeSource, ts.ScriptTarget.Latest, true)
    let expression
    for (const statement of ast.statements) if (ts.isVariableStatement(statement)) for (const declaration of statement.declarationList.declarations) if (declaration.name.getText(ast) === "mimeTypes") expression = declaration.initializer.getText(ast)
    const mimeTypes = vm.runInNewContext("(" + expression + ")")
    const mime = upstreamFunctions("src/electron/utils/files.ts", ["getMimeType"], { path, mimeTypes })
    const names = [...Object.keys(mimeTypes)].flatMap((extension) => ["/fixture/a." + extension, "/fixture/a." + extension.toUpperCase()])
    const types = await host.call({ op: "specs", cases: names.map((path) => ({ op: "mime", path })) })
    for (const [index, name] of names.entries()) assert.equal(types[index], mime.getMimeType(name))
})

test("real GET/HEAD/range byte responses and source protocol headers", async (t) => {
    const { host, image, audio, data, empty } = await setup(t)
    const grant = await host.grant(data)
    const full = fs.readFileSync(data)
    const source = upstreamFunctions("src/electron/data/protected.ts", ["parseRange"])
    for (const range of [undefined, "bytes=0-0", "bytes=123-456", "bytes=8192-", "bytes=-17", "bytes=9000-", "bytes=9-8", "bytes=0-1,6-8", "malformed", "bytes=-0"]) {
        const expected = source.parseRange(range || null, full.length)
        for (const method of ["GET", "HEAD"]) {
            const response = await request(grant.url, { method, headers: { Origin: origin, ...(range ? { Range: range } : {}) } })
            assert.equal(response.status, expected.statusCode)
            assert.equal(response.headers["accept-ranges"], "bytes")
            assert.equal(response.headers["cache-control"], "no-store")
            assert.equal(response.headers["access-control-allow-origin"], origin)
            if (expected.statusCode === 416) {
                assert.equal(response.headers["content-range"], `bytes */${full.length}`)
                assert.equal(response.bytes.length, 0)
            } else {
                const bytes = full.subarray(expected.start, expected.end + 1)
                assert.equal(response.headers["content-type"], "video/mp4")
                assert.equal(Number(response.headers["content-length"]), bytes.length)
                assert.equal(response.headers["content-disposition"], "inline")
                assert.deepEqual(response.bytes, method === "HEAD" ? Buffer.alloc(0) : bytes)
                if (expected.statusCode === 206) assert.equal(response.headers["content-range"], `bytes ${expected.start}-${expected.end}/${full.length}`)
                else assert.equal(response.headers["content-range"], undefined)
            }
        }
    }
    for (const file of [image, audio, empty]) {
        const grant = await host.grant(file)
        const response = await request(grant.url)
        assert.equal(response.status, 200)
        assert.deepEqual(response.bytes, fs.readFileSync(file))
        assert.equal(response.headers["content-type"], file === audio ? "audio/wav" : "image/png")
        if (file === empty) assert.equal((await request(grant.url, { headers: { Range: "bytes=0-0" } })).status, 200)
    }
    const before = await host.call({ op: "status" })
    await request(grant.url, { method: "HEAD" })
    assert.equal((await host.call({ op: "status" })).readBytes, before.readBytes, "HEAD performs no file content read")
})

test("opaque capabilities, exact authority/origin, preflight and explicit unsupported protection", async (t) => {
    const { host, image, directory } = await setup(t)
    const grant = await host.grant(image)
    const parsed = new URL(grant.url)
    assert.equal(parsed.hostname, "127.0.0.1")
    assert.ok(!grant.url.includes(encodeURIComponent(directory)))
    for (const url of [parsed.origin + "/" + encodeURIComponent(image), grant.url.replace(/\/[0-9a-f]{64}\//, "/" + "0".repeat(64) + "/"), grant.url.slice(0, -32) + "0".repeat(32), parsed.origin + "/../" + encodeURIComponent(image)]) assert.equal((await request(url)).status, 404)
    assert.equal((await request(grant.url, { headers: { Host: "evil.test" } })).status, 400)
    const rejected = await request(grant.url, { headers: { Origin: "https://evil.test" } })
    assert.equal(rejected.status, 403)
    assert.equal(rejected.headers["access-control-allow-origin"], undefined)
    assert.equal((await request(grant.url, { method: "POST" })).status, 405)
    const preflight = await request(grant.url, { method: "OPTIONS", headers: { Origin: origin, "Access-Control-Request-Method": "GET", "Access-Control-Request-Headers": "Range" } })
    assert.equal(preflight.status, 204)
    assert.equal(preflight.headers["access-control-allow-headers"], "Range")
    assert.equal((await request(grant.url, { method: "OPTIONS", headers: { Origin: origin, "Access-Control-Request-Method": "GET", "Access-Control-Request-Headers": "Authorization" } })).status, 403)
    assert.equal((await host.command({ op: "grant", path: image, protected: true })).error.code, "assets_protected")
    assert.equal((await host.command({ op: "grant", path: image, sourcePath: "freeshow-protected://asset" })).error.code, "assets_protected")
    assert.equal((await host.command({ op: "grant", path: "relative.png" })).error.code, "assets_invalid")
    assert.equal((await host.command({ op: "grant", path: directory })).error.code, "assets_invalid")
    assert.equal((await host.command({ op: "grant", path: path.join(directory, "missing.png") })).error.code, "assets_missing")
    assert.equal((await host.command({ op: "grant", path: image, cancelled: true })).error.code, "assets_cancelled")
    assert.equal((await host.command({ op: "grant", path: image, deadlineMs: 0 })).error.code, "assets_deadline")
})

async function heldStream(t, grant) {
    const url = new URL(grant.url)
    const socket = net.createConnection({ host: url.hostname, port: Number(url.port) })
    t.after(() => socket.destroy())
    socket.on("error", () => {})
    await once(socket, "connect")
    socket.pause()
    socket.write(`GET ${url.pathname} HTTP/1.1\r\nHost: ${url.host}\r\n\r\n`)
    return socket
}
async function consumeClosed(socket) {
    let bytes = 0
    socket.on("data", (chunk) => {
        bytes += chunk.length
    })
    // Revocation can close the paused socket before this observer is installed.
    const closed = socket.closed ? Promise.resolve() : once(socket, "close").catch(() => {})
    socket.resume()
    let timer
    try {
        await Promise.race([
            closed,
            new Promise((_, reject) => {
                timer = setTimeout(() => reject(new Error("Held asset socket did not close")), 2000)
            })
        ])
    } finally {
        clearTimeout(timer)
    }
    return bytes
}

test("slow clients have bounded buffering, overload and shutdown; active context/revoke cancel streams", async (t) => {
    const { host, directory } = await setup(t, { connections: 1, chunkBytes: 4096, socketSendBytes: 1024, requestMs: 1000, headerMs: 200 })
    const file = path.join(directory, "large.mp4")
    const fd = fs.openSync(file, "w")
    fs.ftruncateSync(fd, 64 * 1024 * 1024)
    fs.writeSync(fd, Buffer.from("real first bytes"), 0, 16, 0)
    fs.closeSync(fd)
    const grant = await host.grant(file)
    let socket = await heldStream(t, grant)
    await until(async () => (await host.call({ op: "status" })).readBytes > 0)
    assert.equal((await request(grant.url)).status, 503, "active connection cap is enforced before spawning")
    await host.call({ op: "revoke", id: grant.id })
    await until(async () => (await host.call({ op: "status" })).connections === 0)
    assert.ok((await consumeClosed(socket)) < 64 * 1024 * 1024, "revocation prevents a complete stale transfer")
    assert.equal((await request(grant.url)).status, 404)
    const replacement = await host.grant(file)
    socket = await heldStream(t, replacement)
    await until(async () => (await host.call({ op: "status" })).connections === 1)
    const next = { ...context, libraryGeneration: 2, dataPath: "/fixture/next" }
    await host.call({ op: "context", context: next })
    await until(async () => (await host.call({ op: "status" })).connections === 0)
    assert.ok((await consumeClosed(socket)) < 64 * 1024 * 1024)
    assert.equal((await request(replacement.url)).status, 404)
    assert.equal((await host.command({ op: "grant", context, path: file })).error.code, "assets_stale_context")
    const fresh = await host.grant(file)
    socket = await heldStream(t, fresh)
    await until(async () => (await host.call({ op: "status" })).connections === 1)
    const before = Date.now()
    const stopped = await host.call({ op: "stop" })
    assert.ok(Date.now() - before < 2000)
    assert.equal(stopped.listenerRunning, false)
    assert.equal(stopped.connections, 0)
    assert.equal(stopped.openFiles, 0)
    assert.equal(stopped.queuedIo, 0)
    assert.equal(stopped.activeIo, 0)
    assert.equal(stopped.grants, 0)
    assert.ok(stopped.readBytes < 64 * 1024 * 1024)
    assert.ok(stopped.peakBufferedBytes <= 4096 * 2)
    assert.ok((await consumeClosed(socket)) < 64 * 1024 * 1024)
    assert.equal((await host.command({ op: "grant", path: file })).error.code, "assets_shutdown")
    await host.call({ op: "stop" })
})

test("slow headers expire; range/registry/batch budgets fail explicitly", async (t) => {
    const { host, image, data, directory } = await setup(t, { connections: 2, grants: 2, batch: 2, responseBytes: 16, requestMs: 300, headerMs: 50 })
    const grant = await host.grant(data)
    assert.equal((await request(grant.url)).status, 413)
    const partial = await request(grant.url, { headers: { Range: "bytes=5-9" } })
    assert.deepEqual(partial.bytes, fs.readFileSync(data).subarray(5, 10))
    assert.equal((await request(grant.url, { headers: { "X-Large": "x".repeat(20000) } })).status, 431)
    const batch = await host.call({ op: "batch", grants: [{ path: image }, { path: path.join(directory, "absent") }] })
    assert.equal(batch.grants.length, 1)
    assert.equal(batch.rejected[0].index, 1)
    assert.equal(batch.rejected[0].error.code, "assets_missing")
    assert.equal((await host.command({ op: "grant", path: data, sourcePath: "another-source" })).error.code, "assets_limit")
    assert.equal((await host.command({ op: "batch", grants: [{ path: data }, { path: data }, { path: data }] })).error.code, "assets_limit")
    const parsed = new URL(grant.url)
    const slow = net.createConnection({ host: parsed.hostname, port: Number(parsed.port) })
    t.after(() => slow.destroy())
    slow.on("error", () => {})
    await once(slow, "connect")
    slow.write("GET /")
    await until(async () => (await host.call({ op: "status" })).connections > 0)
    await until(async () => (await host.call({ op: "status" })).connections === 0)
})

test("native authorization reports permissions and rejects special files without blocking", { skip: process.platform === "win32" }, async (t) => {
    const { host, image, directory } = await setup(t)
    const grant = await host.grant(image)
    fs.chmodSync(image, 0)
    try {
        if (process.getuid?.() !== 0) assert.equal((await request(grant.url)).status, 403)
        if (process.getuid?.() !== 0) assert.equal((await host.command({ op: "grant", path: image })).error.code, "assets_permission")
    } finally {
        fs.chmodSync(image, 0o600)
    }
    const pipe = path.join(directory, "media.fifo")
    const created = spawnSync("mkfifo", [pipe], { encoding: "utf8", timeout: 1000 })
    assert.equal(created.status, 0, created.stderr)
    assert.equal((await host.command({ op: "grant", path: pipe })).error.code, "assets_invalid")
})

test("the whole-request deadline closes an otherwise valid slow body", async (t) => {
    const { host, directory } = await setup(t, { connections: 1, chunkBytes: 4096, socketSendBytes: 1024, requestMs: 120 })
    const file = path.join(directory, "deadline.mp4")
    const fd = fs.openSync(file, "w")
    fs.ftruncateSync(fd, 64 * 1024 * 1024)
    fs.closeSync(fd)
    const grant = await host.grant(file)
    const socket = await heldStream(t, grant)
    await until(async () => (await host.call({ op: "status" })).readBytes > 0)
    await until(async () => (await host.call({ op: "status" })).connections === 0)
    assert.ok((await consumeClosed(socket)) < 64 * 1024 * 1024)
    assert.ok((await host.call({ op: "status" })).readBytes < 64 * 1024 * 1024)
})

test("file identity pins authorization across path replacement and explicit symlink grants", async (t) => {
    const { host, image, data, directory } = await setup(t)
    const grant = await host.grant(image)
    fs.renameSync(image, image + ".old")
    fs.writeFileSync(image, "replacement bytes")
    assert.equal((await request(grant.url)).status, 404, "replacement requires a fresh native grant")
    const fresh = await host.grant(image)
    assert.equal((await request(fresh.url)).bytes.toString(), "replacement bytes")
    if (process.platform !== "win32") {
        const alias = path.join(directory, "link.mp4")
        fs.symlinkSync(data, alias)
        const link = await host.grant(alias)
        fs.unlinkSync(alias)
        fs.symlinkSync(image, alias)
        assert.deepEqual((await request(link.url)).bytes, fs.readFileSync(data), "retargeting the original symlink does not authorize another file")
    }
})

test("synchronous frontend registry preserves original nonlocal URLs, exact grants and stale contexts", () => {
    const javascript = ts.transpileModule(fs.readFileSync(path.join(root, "src/frontend/rust/mediaUrl.ts"), "utf8"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
    const exports = {}
    vm.runInNewContext(javascript, { exports, URL, TextEncoder })
    let current = { ...context }
    const errors = []
    const registry = exports.createMediaUrlRegistry(
        () => current,
        (error) => errors.push(error),
        2
    )
    const original = upstreamFunctions("src/frontend/components/helpers/media.ts", ["isLocalFile", "encodeFilePath", "splitPath", "joinPath"], {}, 'let pathJoiner = "";')
    for (const value of ["https://example.com/a%20b.mp4?x=é", "http://example.com/image.png", "data:image/png;base64,AA==", "blob:local/123", "freeshow-protected://provider-id"]) assert.equal(registry.resolve(value), original.encodeFilePath(value))
    assert.equal(errors.at(-1).code, "assets_protected")
    assert.equal(registry.resolve("./assets/metronome/wood.wav"), undefined)
    const file = "/日本語 é#%/a?.png",
        id = "1".repeat(32)
    const grant = { id, sourcePath: file, url: "http://127.0.0.1:43210/" + "a".repeat(64) + "/" + id, mimeType: "image/png", length: 1, context: current }
    assert.equal(registry.publish(current, [grant]), true)
    assert.equal(registry.resolve(file), grant.url)
    assert.equal(registry.resolve("/unregistered.png"), "")
    assert.equal(errors.at(-1).code, "assets_unregistered")
    assert.equal(registry.publish(current, [{ ...grant, url: "http://evil.test/file" }]), false)
    assert.equal(registry.publish(current, [{ ...grant, url: grant.url.replace("43210", "99999") }]), false)
    assert.equal(registry.resolve(file), grant.url)
    const another = (digit, sourcePath) => ({ ...grant, sourcePath, id: digit.repeat(32), url: grant.url.slice(0, -32) + digit.repeat(32) })
    assert.equal(registry.publish(current, [another("2", "/second.png"), another("3", "/third.png")]), false)
    assert.equal(registry.resolve("/second.png"), "", "Failed publication is atomic")
    assert.equal(registry.resolve(file), grant.url)
    current = { ...current, libraryGeneration: 2 }
    assert.equal(registry.resolve(file), "")
    assert.equal(registry.publish(context, [grant]), false)
    assert.equal(registry.status().entries, 0)
})
