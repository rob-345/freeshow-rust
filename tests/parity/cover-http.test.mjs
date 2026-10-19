// Actual pinned handler + installed Express/send over controlled loopback.
// Only fixed path lookup / existence boundaries are injected; files, fs.Stats,
// HTTP parsing, finalhandler, body bytes and transport are genuine.
import assert from "node:assert/strict"
import { execFileSync } from "node:child_process"
import { createHash, randomInt } from "node:crypto"
import fs from "node:fs"
import http from "node:http"
import { createRequire } from "node:module"
import os from "node:os"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import express from "express"
import finalhandler from "finalhandler"
import { Server } from "socket.io"
import ts from "typescript"

const root = fileURLToPath(new URL("../../", import.meta.url))
const binary = process.env.FREESHOW_COVER_HTTP_BIN || path.join(root, "target/debug/examples", process.platform === "win32" ? "cover-http-parity.exe" : "cover-http-parity")
const require = createRequire(import.meta.url)
const source = fs.readFileSync(path.join(root, "src/electron/servers.ts"), "utf8")
assert.equal(createHash("sha256").update(source.replace(/\r\n/g, "\n")).digest("hex"), "2b8c81b56e77b754dc617e31777e4b3e1d9d15b368c369efc9d58030c1af4b41", "immutable servers.ts oracle")
for (const [name, version] of Object.entries({ express: "4.22.3", send: "0.19.0", etag: "1.8.1", fresh: "0.5.2", "range-parser": "1.2.1", finalhandler: "1.3.1" })) assert.equal(require(`${name}/package.json`).version, version)
const ast = ts.createSourceFile("servers.ts", source, ts.ScriptTarget.Latest, true)
let callback
function visit(node) {
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression) && node.expression.name.text === "get" && ts.isStringLiteral(node.arguments[0]) && node.arguments[0].text === "/nowplayingcover") callback = node.arguments[1].getText(ast)
    ts.forEachChild(node, visit)
}
visit(ast)
assert.ok(callback)
const handlerJS = ts.transpileModule(`const handler = ${callback};`, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
const actualHandler = new Function("getNowPlayingMediaPath", "doesPathExist", handlerJS + "\nreturn handler")
const createDeclaration = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "createServerInstance")
assert.ok(createDeclaration)
const createJS = ts.transpileModule(
    `
const servers = {}, ioServers = {}, serverPorts = { REMOTE:5510 };
let stageStreamSubscribers = {};
function initialize() { throw new Error('no Socket.IO test clients admitted in HTTP route fixture'); }
${createDeclaration.getText(ast)}
createServerInstance('REMOTE');`,
    { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }
).outputText
const actualCreate = new Function("express", "http", "Server", "join", "__dirname", "getNowPlayingMediaPath", "doesPathExist", createJS + "\nreturn servers.REMOTE")
// Actual Node fs Date constructor, not an independently copied Math.round
// oracle. This supplements real files when OS timestamp resolution cannot
// express half-millisecond ties. No internal Node capability ships in product.
const fsUtils = process.binding("natives")["internal/fs/utils"]
const fsAST = ts.createSourceFile("node-internal-fs-utils.js", fsUtils, ts.ScriptTarget.Latest, true)
const dateDeclaration = fsAST.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "dateFromMs")
assert.ok(dateDeclaration, "actual Node fs dateFromMs declaration")
const nodeStatDate = new Function("MathRound", `return (${dateDeclaration.getText(fsAST)})`)(Math.round)
const fixtureBytes = Buffer.from("89504e470d0a1a0a00112233445566778899aabbccddeeffeeddccbbaa99887766554433221100", "hex")
const stableHeaders = (headers) => Object.fromEntries(Object.entries(headers).filter(([name]) => name !== "date" && name !== "connection"))
const usedPorts = new Set()
let completedRequests = 0

function rust(cases) {
    return JSON.parse(execFileSync(binary, { input: JSON.stringify(cases), timeout: 5000, maxBuffer: 4 * 1024 * 1024, encoding: "utf8" }))
}
function input(stat, headers = {}, extra = {}) {
    return { source: "file", length: stat.size, mtime_ms: stat.mtimeMs, method: "GET", headers: Object.entries(headers), production: true, ...extra }
}
function expectedBody(plan, bytes) {
    switch (plan.body.kind) {
        case "empty":
            return Buffer.alloc(0)
        case "file":
            return bytes.subarray(plan.body.offset, plan.body.offset + plan.body.length)
        case "production_error":
            return Buffer.from(plan.body.html)
        case "options":
            return Buffer.from("GET,HEAD")
        default:
            throw new Error("deferred errors are not complete HTTP representations")
    }
}
function compare(native, actual, bytes, label) {
    assert.equal(native.ok, true, label + JSON.stringify(native))
    assert.equal(native.plan.status, actual.status, label)
    // Exclude only volatile Date and connection. Keep ALL entity/security/header
    // differences, including content-length and framing, as test failures.
    assert.deepEqual(native.plan.headers, stableHeaders(actual.headers), label)
    assert.deepEqual(expectedBody(native.plan, bytes), actual.body, label)
}

async function oracle(env, run, { fullSourceRoutes = false } = {}) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-cover-http-"))
    const file = path.join(directory, "NowPlayingCover.png")
    fs.writeFileSync(file, fixtureBytes)
    fs.utimesSync(file, new Date(1_700_000_000_123), new Date(1_700_000_000_123))
    const requests = new Map()
    const sockets = new Set()
    let sequence = 0,
        disappear = false,
        lookups = 0,
        io
    const coverPath = () => {
        lookups++
        return file
    }
    const exists = (value) => {
        const exists = fs.existsSync(value)
        if (exists && disappear) {
            disappear = false
            fs.unlinkSync(value)
        }
        return exists
    }
    const createHTTP = (app) => {
        app.set("env", env) // Explicit actual finalhandler policy, not release inference.
        app.use((req, _res, next) => {
            requests.set(req.headers["x-fixture-id"], { ...req.headers })
            next()
        })
        // Genuine finalhandler renderer; only stderr logging is quiet.
        return http.createServer((req, res) => app.handle(req, res, finalhandler(req, res, { env, onerror() {} })))
    }
    let server
    if (fullSourceRoutes) {
        fs.mkdirSync(path.join(directory, "remote"))
        fs.writeFileSync(path.join(directory, "remote/index.html"), "PRIVATE TEST INDEX FIXTURE")
        fs.writeFileSync(path.join(directory, "remote/nowplayingcover"), "PRIVATE STATIC FALLBACK FIXTURE")
        const actual = actualCreate(express, { createServer: createHTTP }, Server, path.join, directory, coverPath, exists)
        server = actual.server
        io = actual.io
    } else {
        const app = express()
        server = createHTTP(app)
        app.get("/nowplayingcover", actualHandler(coverPath, exists))
    }
    server.on("connection", (socket) => {
        assert.ok(sockets.size < 8, "bounded sequential fixture sockets")
        sockets.add(socket)
        socket.on("close", () => sockets.delete(socket))
    })
    server.requestTimeout = 2000
    server.headersTimeout = 2000
    server.maxRequestsPerSocket = 1
    let port, reason
    try {
        for (let attempt = 0; attempt < 30; attempt++) {
            const candidate = randomInt(32768, 49151)
            if (usedPorts.has(candidate)) continue
            usedPorts.add(candidate)
            try {
                await new Promise((resolve, reject) => {
                    server.once("error", reject)
                    server.listen(candidate, "127.0.0.1", () => {
                        server.removeListener("error", reject)
                        resolve()
                    })
                })
                port = candidate
                break
            } catch (error) {
                reason = String(error)
            }
        }
        assert.ok(port, `bounded loopback bind failed: ${reason}`)
        const request = async (headers = {}, { method = "GET", url = "/nowplayingcover" } = {}) => {
            const id = String(++sequence)
            const result = await new Promise((resolve, reject) => {
                const req = http.request({ host: "127.0.0.1", port, path: url, method, headers: { ...headers, "x-fixture-id": id }, agent: false }, (res) => {
                    const chunks = []
                    let size = 0
                    res.on("data", (chunk) => {
                        size += chunk.length
                        if (size > 32 * 1024) res.destroy(new Error("fixture response limit"))
                        else chunks.push(chunk)
                    })
                    res.on("error", reject)
                    res.on("end", () => resolve({ status: res.statusCode, headers: res.headers, body: Buffer.concat(chunks) }))
                })
                req.setTimeout(2000, () => req.destroy(new Error("fixture HTTP timeout")))
                req.on("error", reject)
                req.end()
            })
            const normalized = requests.get(id)
            requests.delete(id)
            assert.ok(normalized)
            completedRequests++
            return { ...result, requestHeaders: normalized }
        }
        await run({
            file,
            request,
            lookups: () => lookups,
            disappear: () => {
                disappear = true
            }
        })
    } finally {
        const closed = Promise.all([...sockets].map((socket) => new Promise((resolve) => socket.once("close", resolve))))
        server.closeAllConnections()
        if (io) await new Promise((resolve) => io.close(resolve))
        else if (server.listening) await new Promise((resolve) => server.close(resolve))
        await closed // Server close callback alone is not a socket close barrier.
        assert.equal(sockets.size, 0, "all controlled loopback sockets closed")
        assert.equal(requests.size, 0)
        fs.rmSync(directory, { recursive: true, force: true })
    }
}

test("actual Express/send GET, HEAD, cache/precondition precedence and combined range representations", async () => {
    await oracle("production", async ({ file, request }) => {
        const stat = fs.statSync(file)
        const baseline = await request()
        const tag = baseline.headers.etag,
            date = baseline.headers["last-modified"]
        const older = new Date(stat.mtime.getTime() - 5000).toUTCString(),
            newer = new Date(stat.mtime.getTime() + 5000).toUTCString()
        const corpus = [
            {},
            { "if-none-match": tag },
            { "if-none-match": tag.slice(2) },
            { "if-none-match": "*" },
            { "if-none-match": `"other", ${tag}` },
            { "if-none-match": tag, "if-modified-since": older },
            { "if-none-match": '"other"', "if-modified-since": newer },
            { "if-modified-since": date },
            { "if-modified-since": older },
            { "if-modified-since": "invalid" },
            { "if-none-match": tag, "cache-control": "max-age=0, no-cache" },
            { "if-none-match": tag, "cache-control": "No-Cache" },
            { "if-none-match": tag, "cache-control": 'no-cache="x"' },
            { "if-none-match": tag, "cache-control": "max-age=0,\u00a0no-cache\t" },
            { "if-match": '"wrong"', "if-none-match": tag },
            { "if-match": "*", "if-unmodified-since": older },
            { "if-match": tag.slice(2), "if-unmodified-since": older },
            { "if-match": `W/${tag}` },
            { "if-match": " , , " },
            { "if-unmodified-since": older },
            { "if-unmodified-since": newer },
            { "if-unmodified-since": "invalid" },
            ...["bytes=0-3", "bytes=3-", "bytes=-4", "bytes=-999", "bytes=-0", "bytes=999-", "bytes=", "bytes=garbage", "bytes=bad,1-2", "bytes=1.5-3.8", "bytes=1e3-2e3", "bytes=0x10-20", "bytes=0-2,3-4", "bytes=3-4,0-2", "bytes=0-3,2-4", "bytes=0-1,8-9", "bytes=0-1,999-", "bytes=2-1", "bytes=0001-0003", "bytes=+1-+3", "bytes=1-3-extra", "bytes=\u00a01-3", "Bytes=1-2", "items=1-2", "garbage", "bytes=1-99999999999999999999999999999999999999999999999999"].map((range) => ({ range })),
            { range: "bytes=1-2", "if-range": tag },
            { range: "bytes=1-2", "if-range": tag.slice(2) },
            { range: "bytes=1-2", "if-range": `garbage ${tag} suffix` },
            { range: "bytes=1-2", "if-range": date },
            { range: "bytes=1-2", "if-range": older },
            { range: "bytes=999-", "if-range": older },
            { range: "bytes=999-", "if-range": "invalid" },
            { range: "bytes=999-", "if-none-match": tag },
            { range: "bytes=999-", "if-match": '"bad"' }
        ]
        const actual = [],
            native = []
        for (const method of ["GET", "HEAD"])
            for (const headers of corpus) {
                const response = await request(headers, { method })
                actual.push({ response, label: JSON.stringify({ method, headers }) })
                native.push(input(stat, response.requestHeaders, { method }))
            }
        const plans = rust(native)
        actual.forEach(({ response, label }, index) => compare(plans[index], response, fixtureBytes, label))
        for (const url of ["/NOWPLAYINGCOVER", "/nowplayingcover/", "/nowplayingcover?path=/etc/passwd&cache=changed"]) {
            const response = await request({}, { url })
            compare(rust([input(stat, response.requestHeaders)])[0], response, fixtureBytes, url)
        }
        // Source cache identity is size + rounded mtime, not content or ctime.
        const changed = Buffer.alloc(fixtureBytes.length, 0x5a)
        fs.writeFileSync(file, changed)
        fs.utimesSync(file, stat.atime, stat.mtime)
        const sameStat = fs.statSync(file)
        const full = await request()
        assert.equal(full.headers.etag, tag, "same size/restored mtime keeps source ETag after changed bytes")
        compare(rust([input(sameStat, full.requestHeaders)])[0], full, changed, "same-stat changed body")
        const cached = await request({ "if-none-match": tag })
        compare(rust([input(sameStat, cached.requestHeaders)])[0], cached, changed, "same-stat unchanged cache identity")
    })
})

test("real empty/opaque file bytes and absent guard 404, distinct from disappearing sendFile failure", async () => {
    await oracle("production", async ({ file, request, disappear }) => {
        for (const bytes of [Buffer.alloc(0), Buffer.from("not a PNG; opaque source bytes\0\xff", "latin1")]) {
            fs.writeFileSync(file, bytes)
            const stat = fs.statSync(file)
            for (const method of ["GET", "HEAD"])
                for (const headers of [{}, { range: "bytes=0-1" }]) {
                    const actual = await request(headers, { method })
                    compare(rust([input(stat, actual.requestHeaders, { method })])[0], actual, bytes, `${method}, len=${bytes.length}`)
                }
        }
        fs.unlinkSync(file)
        for (const method of ["GET", "HEAD"]) {
            const actual = await request({ range: "bytes=999-", "if-match": '"wrong"' }, { method })
            compare(rust([input({ size: 0, mtimeMs: 0 }, actual.requestHeaders, { source: "missing", method })])[0], actual, Buffer.alloc(0), `missing ${method}`)
        }
        fs.writeFileSync(file, fixtureBytes)
        disappear()
        const lateFailure = await request()
        assert.equal(lateFailure.status, 404)
        assert.ok(lateFailure.body.length > 0, "actual finalhandler HTML, not empty guard404")
        assert.equal(lateFailure.headers["content-type"], "text/html; charset=utf-8")
        assert.deepEqual(rust([input({ size: 0, mtimeMs: 0 }, {}, { source: "failed" })])[0], { ok: false, code: "cover_source_failure" })
    })
})

test("development 412/416 retain explicit deferred rendering; production oracle bodies are genuine", async () => {
    await oracle("development", async ({ file, request }) => {
        const stat = fs.statSync(file)
        for (const method of ["GET", "HEAD"])
            for (const headers of [{ "if-match": '"wrong"' }, { range: "bytes=999-" }]) {
                const actual = await request(headers, { method })
                const [native] = rust([input(stat, actual.requestHeaders, { method, production: false })])
                assert.equal(native.ok, true)
                assert.equal(native.plan.status, actual.status)
                assert.deepEqual(native.plan.body, { kind: "deferred_error", status: actual.status, head_only: method === "HEAD" })
                assert.equal(native.plan.headers["content-length"], undefined)
                const expectedHeaders = stableHeaders(actual.headers)
                delete expectedHeaders["content-length"] // Explicit unknown error-renderer output, not complete parity.
                assert.deepEqual(native.plan.headers, expectedHeaders)
                if (method === "GET") assert.match(actual.body.toString(), /<br>.*(?:send|SendStream)/, "real development stack remains visible")
            }
    })
})

test("actual file mtime and pre-epoch behavior; direct pinned stat ETags cover fractional Math.round ties", async (t) => {
    await oracle("production", async ({ file, request }) => {
        const times = [1_700_000_000_123.6, 0, -1, -1500, -1_000_000_000_001]
        for (const milliseconds of times) {
            try {
                fs.utimesSync(file, new Date(milliseconds), new Date(milliseconds))
            } catch (error) {
                t.diagnostic(`native filesystem cannot set requested mtime ${milliseconds}: ${error.code}`)
                if (milliseconds < 0) continue
                throw error
            }
            const stat = fs.statSync(file)
            if (milliseconds < 0 && stat.mtimeMs >= 0) {
                t.diagnostic(`native filesystem did not retain pre-epoch mtime ${milliseconds}; observed ${stat.mtimeMs}`)
                continue
            }
            const actual = await request()
            compare(rust([input(stat, actual.requestHeaders)])[0], actual, fixtureBytes, `actual stat ${stat.mtimeMs}`)
            const conditional = await request({ "if-modified-since": actual.headers["last-modified"] })
            compare(rust([input(stat, conditional.requestHeaders)])[0], conditional, fixtureBytes, `actual stat freshness ${stat.mtimeMs}`)
        }
        // Real etag package's native Stats contract, separate from actual FS time
        // precision. Pinned Node fs uses Date(MathRound(Number(mtimeMs))).
        const etag = require("etag")
        const values = [-1500.5, -1.51, -1.5, -0.51, -0.5, -0.49, -0, 0, 0.49, 0.5, 1.5, 1_700_000_000_123.5, 253_402_300_799_998.5]
        for (const ms of values) {
            const native = rust([input({ size: 37, mtimeMs: ms })])[0]
            assert.equal(native.ok, true)
            const stats = { ctime: new Date(0), mtime: nodeStatDate(ms), ino: 1, size: 37 }
            assert.equal(native.plan.headers.etag, etag(stats), `stat projection ${ms}`)
            assert.equal(native.plan.headers["last-modified"], stats.mtime.toUTCString())
        }
    })
})

test("generated real transport ranges and explicit-zone dates compare all stable headers and exact body", async () => {
    await oracle("production", async ({ file, request }) => {
        const stat = fs.statSync(file)
        const actual = [],
            inputs = []
        let seed = 0x13579bdf
        const next = () => {
            seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
            return seed
        }
        for (let index = 0; index < 150; index++) {
            const ranges = Array.from({ length: 1 + (next() % 5) }, () => `${next() % 55}-${next() % 65}`).join(",")
            const headers = index % 3 ? { range: `bytes=${ranges}` } : { "if-modified-since": new Date(stat.mtime.getTime() + (next() % 30000) - 15000).toUTCString() }
            const response = await request(headers)
            actual.push(response)
            inputs.push(input(stat, response.requestHeaders))
        }
        const result = rust(inputs)
        actual.forEach((response, index) => compare(result[index], response, fixtureBytes, `generated ${index}`))
        for (const date of ["Fri, 31 Feb 2023 12:30:00 GMT", "Tue, 32 Feb 2023 12:30:00 GMT", "Wed, 15 Nov 2023 00:00:00 UTC", "tue, 14 nov 2023 22:13:20 gmt"]) {
            const response = await request({ "if-unmodified-since": date })
            compare(rust([input(stat, response.requestHeaders)])[0], response, fixtureBytes, date)
        }
    })
})

test("actual Node exact 24:00:00 rollover drives consulted conditions while invalid hours and unused dates preserve source precedence", async () => {
    await oracle("production", async ({ file, request }) => {
        const inputs = [],
            actual = []
        const record = async (stat, headers, method, status) => {
            const response = await request(headers, { method })
            assert.equal(response.status, status, JSON.stringify({ headers, method }))
            inputs.push(input(stat, response.requestHeaders, { method }))
            actual.push(response)
        }
        for (const [date, normalized] of [
            ["Tue, 14 Nov 2023 24:00:00 GMT", "2023-11-15T00:00:00Z"],
            ["Tue, 31 Oct 2023 24:00:00 GMT", "2023-11-01T00:00:00Z"],
            ["Sun, 31 Dec 2023 24:00:00 GMT", "2024-01-01T00:00:00Z"],
            ["Thu, 29 Feb 2024 24:00:00 UTC", "2024-03-01T00:00:00Z"]
        ]) {
            const midnight = Date.parse(date)
            assert.equal(midnight, Date.parse(normalized), "actual pinned V8 normalizes the midnight spelling")
            for (const delta of [-1000, 1000]) {
                fs.utimesSync(file, new Date(midnight + delta), new Date(midnight + delta))
                const stat = fs.statSync(file)
                assert.equal(stat.mtime.getTime(), midnight + delta, "fixture must retain the decisive side of midnight")
                for (const method of ["GET", "HEAD"]) {
                    await record(stat, { "if-modified-since": date }, method, delta < 0 ? 304 : 200)
                    await record(stat, { "if-unmodified-since": date }, method, delta < 0 ? 200 : 412)
                    await record(stat, { range: "bytes=1-2", "if-range": date }, method, delta < 0 ? 206 : 200)
                }
            }
        }
        const stat = fs.statSync(file)
        for (const time of ["24:00:01", "24:01:00", "25:00:00", "23:60:00"]) {
            const date = `Tue, 14 Nov 2023 ${time} GMT`
            assert.ok(Number.isNaN(Date.parse(date)), "invalid hour form is actually NaN in pinned V8")
            for (const method of ["GET", "HEAD"]) for (const headers of [{ "if-modified-since": date }, { "if-unmodified-since": date }, { range: "bytes=1-2", "if-range": date }]) await record(stat, headers, method, 200)
        }
        const date = "Tue, 14 Nov 2023 24:00:00 GMT" // Earlier than the current March2024 fixture.
        for (const method of ["GET", "HEAD"])
            for (const headers of [
                { "if-match": "*", "if-unmodified-since": date },
                { "if-none-match": "wrong", "if-modified-since": date },
                { "cache-control": "no-cache", "if-modified-since": date },
                { range: "items=1-2", "if-range": date }
            ])
                await record(stat, headers, method, 200)
        rust(inputs).forEach((native, index) => compare(native, actual[index], fixtureBytes, `midnight condition ${index}`))
        assert.equal(inputs.length, 80, "all rollover, invalid-hour and unused-condition exchanges completed")
    })
})

test("finite capability limits and V8 date spelling gaps are explicit, with source precedence preserved", async (t) => {
    const stat = { size: 37, mtimeMs: 1_700_000_000_123 }
    const cases = [
        [input(stat, { "if-modified-since": "2023-11-15T00:00:00Z" }), "cover_date_unsupported"],
        [input(stat, { "if-modified-since": "Wednesday, 15-Nov-23 00:00:00 GMT" }), "cover_date_unsupported"],
        [input(stat, { "if-modified-since": "Wed Nov 15 00:00:00 2023" }), "cover_date_unsupported"],
        [input(stat, { range: `bytes=${Array(65).fill("0-1").join(",")}` }), "cover_range_limit"],
        [input(stat, { range: "x".repeat(8193) }), "cover_header_limit"],
        [input({ size: 4 * 1024 * 1024 + 1, mtimeMs: 0 }), "cover_body_limit"],
        [input(stat, {}, { headers: Array.from({ length: 33 }, (_, n) => [`x-${n}`, "v"]) }), "cover_header_limit"],
        [
            input(
                stat,
                {},
                {
                    headers: [
                        ["range", "a"],
                        ["range", "b"]
                    ]
                }
            ),
            "cover_header"
        ]
    ]
    rust(cases.map(([value]) => value)).forEach((result, index) => assert.deepEqual(result, { ok: false, code: cases[index][1] }))
    await oracle("production", async ({ file, request }) => {
        const actualStat = fs.statSync(file)
        const date = "2023-11-15T00:00:00Z"
        const actual = await request({ "if-modified-since": date })
        assert.equal(actual.status, 304, "source accepts a deliberately unsupported V8 spelling")
        assert.equal(rust([input(actualStat, actual.requestHeaders)])[0].code, "cover_date_unsupported")
        for (const headers of [
            { "if-match": "*", "if-unmodified-since": date },
            { "if-none-match": "wrong", "if-modified-since": date },
            { "cache-control": "no-cache", "if-modified-since": date },
            { range: "items=0-1", "if-range": date }
        ]) {
            const response = await request(headers)
            compare(rust([input(actualStat, response.requestHeaders)])[0], response, fixtureBytes, "ignored unsupported lower-priority date")
        }
    })
    t.diagnostic(`actual pinned handler: ${completedRequests} completed sequential loopback HTTP exchanges; all fixture sockets joined`)
})

test("actual full createServerInstance routes: raw matching, HEAD, automatic OPTIONS and static/method fallthrough", async (t) => {
    const before = completedRequests
    await oracle(
        "production",
        async ({ file, request, lookups }) => {
            const stat = fs.statSync(file)
            const targets = ["/nowplayingcover", "/NOWPLAYINGCOVER", "/NowPlayingCover/", "/nowplayingcover?cache=1", "/nowplayingcover/?path=/foreign&x=%2F"]
            for (const method of ["GET", "HEAD", "OPTIONS"])
                for (const url of targets) {
                    const start = lookups()
                    const response = await request({ range: "bytes=0-3", "if-none-match": '"wrong"' }, { method, url })
                    const [native] = rust([input(stat, {}, { method, route_target: url })])
                    assert.equal(native.ok, true)
                    if (method === "OPTIONS") {
                        assert.deepEqual(native.route, { kind: "automatic_options" })
                        assert.equal(lookups(), start, "OPTIONS never calls fixed cover path helper")
                        compare({ ok: true, plan: native.options }, response, Buffer.alloc(0), `full source OPTIONS ${url}`)
                        assert.equal(response.headers.allow, "GET,HEAD")
                    } else {
                        assert.deepEqual(native.route, { kind: "read", method })
                        assert.equal(lookups(), start + 1)
                        const [plan] = rust([input(stat, response.requestHeaders, { method })])
                        compare(plan, response, fixtureBytes, `full source ${method} ${url}`)
                        if (method === "HEAD") assert.equal(response.body.length, 0)
                    }
                }
            const optionsTag = require("etag")("GET,HEAD", { weak: true })
            const start = lookups()
            // OPTIONS ignores all file conditions and still works when cover is gone.
            fs.unlinkSync(file)
            const options = await request({ "if-none-match": optionsTag, "if-match": '"wrong"', range: "bytes=999-" }, { method: "OPTIONS" })
            const [nativeOptions] = rust([input(stat, {}, { method: "OPTIONS", route_target: "/nowplayingcover" })])
            compare({ ok: true, plan: nativeOptions.options }, options, Buffer.alloc(0), "OPTIONS ignores file presence and conditionals")
            assert.equal(lookups(), start)
            fs.writeFileSync(file, fixtureBytes)

            const nonmatches = ["/%6eowplayingcover", "/nowplaying%63over", "/%6Eowplayingcover", "/nowplayingcover%2f", "/nowplayingcover%2F", "//nowplayingcover", "/nowplayingcover//", "/nowplayingcover.png", "/nowplayingcover/more", "/x/../nowplayingcover", "/%2e/nowplayingcover", "/%2e%2e/nowplayingcover", "/nowplayingcover%3Fx", "/nowplayingcover%00", "/nowplayingcover\\", "/?path=/nowplayingcover"]
            for (const url of nonmatches)
                for (const method of ["GET", "HEAD", "OPTIONS"]) {
                    const count = lookups()
                    const response = await request({}, { method, url })
                    const [native] = rust([input(stat, {}, { method, route_target: url })])
                    assert.deepEqual(native.route, { kind: "fallthrough" }, `${method} ${url}`)
                    assert.equal(lookups(), count, `no privileged lookup for ${method} ${url}`)
                    if (method === "HEAD") assert.equal(response.body.length, 0)
                    if (method === "GET" && url === "/%6eowplayingcover") assert.equal(response.body.toString(), "PRIVATE STATIC FALLBACK FIXTURE", "source static may decode after cover route did NOT match")
                }
            for (const method of ["POST", "PUT", "PATCH", "DELETE", "TRACE"]) {
                const count = lookups()
                const response = await request({}, { method })
                const [native] = rust([input(stat, {}, { method, route_target: "/nowplayingcover" })])
                assert.deepEqual(native.route, { kind: "fallthrough" })
                assert.equal(lookups(), count)
                assert.equal(response.status, 404)
                assert.match(response.body.toString(), new RegExp(`Cannot ${method} /nowplayingcover`))
                assert.equal(response.headers.allow, undefined, "unsupported method is not automatic OPTIONS")
            }
            // Other original app routes remain the caller's fallback responsibility.
            const rootOptions = await request({}, { method: "OPTIONS", url: "/" })
            assert.equal(rootOptions.status, 200)
            assert.equal(rootOptions.headers.allow, "GET,HEAD")
            assert.deepEqual(rust([input(stat, {}, { method: "OPTIONS", route_target: "/" })])[0].route, { kind: "fallthrough" })
        },
        { fullSourceRoutes: true }
    )
    t.diagnostic(`full original createServerInstance: ${completedRequests - before} real loopback requests; actual Socket.IO/server and socket closes joined`)
})

test("bounded route inputs explicitly reject legacy URL parser capabilities without inventing normalization", async (t) => {
    const stat = { size: 0, mtimeMs: 0 }
    const cases = [
        ["GET", "/nowplayingcover#fragment", "cover_route_unsupported"],
        ["GET", "http://example.invalid/nowplayingcover", "cover_route_unsupported"],
        ["GET", "/nowplayingcover ", "cover_route_unsupported"],
        ["GET", "/nowplayingcover?x=\u00a0", "cover_route_unsupported"],
        ["GET", "/" + "x".repeat(8192), "cover_route_limit"],
        ["A".repeat(33), "/nowplayingcover", "cover_route_limit"],
        ["GET\r\n", "/nowplayingcover", "cover_route_method"],
        ["get", "/nowplayingcover", "cover_route_method"]
    ]
    const actual = rust(cases.map(([method, route_target]) => input(stat, {}, { method, route_target })))
    actual.forEach((result, index) => assert.deepEqual(result, { ok: false, code: cases[index][2] }))
    await oracle(
        "production",
        async ({ request, lookups }) => {
            const start = lookups()
            const response = await request({}, { url: "/nowplayingcover#fragment" })
            assert.equal(response.status, 200, "original legacy parser accepts a deliberately unsupported raw spelling")
            assert.equal(lookups(), start + 1)
            assert.deepEqual(rust([input(stat, {}, { route_target: "/nowplayingcover#fragment" })])[0], { ok: false, code: "cover_route_unsupported" })
        },
        { fullSourceRoutes: true }
    )
    t.diagnostic(`total ${completedRequests} completed real HTTP exchanges; every fixture listener, Socket.IO instance and socket joined`)
})
