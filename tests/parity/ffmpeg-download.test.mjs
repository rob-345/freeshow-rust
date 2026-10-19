import assert from "node:assert/strict"
import fs from "node:fs"
import fsp from "node:fs/promises"
import os from "node:os"
import path from "node:path"
import vm from "node:vm"
import http from "node:http"
import zlib from "node:zlib"
import { Readable } from "node:stream"
import { pipeline } from "node:stream/promises"
import { spawn, spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import { once } from "node:events"
import { test } from "node:test"
import ts from "typescript"
const root = fileURLToPath(new URL("../../", import.meta.url))
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const binary = process.env.FREESHOW_FFMPEG_DOWNLOAD_PARITY || path.join(root, "target/debug/freeshow-ffmpeg-download-parity" + (process.platform === "win32" ? ".exe" : ""))
const coverage = { urlCases: 0, downloads: 0, progressSamples: 0, resourceCases: 0, paddingChunkCases: 0 }
test.after(() => console.log("FFmpeg download parity coverage:", JSON.stringify(coverage)))
function original(filename) {
    const result = spawnSync("git", ["show", `${revision}:${filename}`], { cwd: root, encoding: "utf8", timeout: 10000 })
    assert.equal(result.status, 0, result.stderr)
    return ts.createSourceFile(filename, result.stdout, ts.ScriptTarget.Latest, true)
}
const source = original("src/electron/streaming/ffmpegManager.ts"),
    responses = original("src/electron/IPC/responsesMain.ts")
function declaration(name, variable = false) {
    const node = source.statements.find((node) => (variable ? ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText(source) === name) : ts.isFunctionDeclaration(node) && node.name?.text === name))
    assert.ok(node, "Pinned source " + name)
    return node.getText(source).replace(/^export\s+/, "")
}
const main = responses.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText(responses) === "mainResponses")).declarationList.declarations.find((item) => item.name.getText(responses) === "mainResponses").initializer
const handler = main.properties.find((item) => item.name?.getText(responses) === "[Main.FFMPEG_DOWNLOAD]").initializer.getText(responses)
function oracle(directory, platform, arch, url, options = {}) {
    const events = [],
        trace = [],
        chmods = [],
        chunks = [],
        requested = []
    let artifact, mode
    const boundaryFs = {
        ...fsp,
        chmod: async (file, mode) => {
            chmods.push(mode)
            return fsp.chmod(file, mode)
        },
        rename: async (from, to) => {
            artifact = await fsp.readFile(from)
            mode = (await fsp.stat(from)).mode & 0o777
            trace.push(["rename", to])
        },
        rm: async (...args) => {
            trace.push(["cleanup"])
            return fsp.rm(...args)
        }
    }
    const code = ['"use strict";', ...["FFMPEG_VERSION", "IS_WIN", "BIN_NAME", "getFfmpegDir", "getFfmpegPathLocal"].map((name) => declaration(name, true)), declaration("getDownloadUrl"), declaration("downloadFfmpeg"), `globalThis.result = { getDownloadUrl, downloadFfmpeg, handler: (${handler}) }`].join("\n")
    const context = vm.createContext({
        process: { platform, arch },
        app: {
            getPath: (name) => {
                assert.equal(name, "userData")
                return directory
            }
        },
        path,
        fs: boundaryFs,
        createWriteStream: fs.createWriteStream,
        Readable: {
            fromWeb: (body) => {
                const stream = Readable.fromWeb(body)
                stream.on("data", (chunk) => chunks.push(chunk.length))
                return stream
            }
        },
        pipeline,
        zlib,
        fetch: async (requestedUrl) => {
            requested.push(requestedUrl)
            return options.response ? options.response() : fetch(url)
        },
        execFileAsync: async (program, args) => {
            trace.push([program, args])
            if (options.failMacCommands) throw new Error("controlled command failure")
        },
        clearFfmpegPathCache: () => trace.push(["clearResolverCache"]),
        ToMain: { MEDIA_DOWNLOAD_PROGRESS: "MEDIA_DOWNLOAD_PROGRESS" },
        sendToMain: (channel, data) => events.push({ channel, data }),
        Date: { now: () => 123456 },
        console: { error() {}, warn() {} }
    })
    new vm.Script(ts.transpileModule(code, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText).runInContext(context, { timeout: 1000 })
    return { ...context.result, events, trace, chmods, chunks, requested, artifact: () => artifact, mode: () => mode }
}
async function native(cases) {
    const child = spawn(binary, [], { stdio: ["pipe", "pipe", "pipe"] })
    let output = "",
        error = ""
    child.stdout.setEncoding("utf8").on("data", (data) => {
        output += data
        assert.ok(output.length <= 32 * 1024 * 1024)
    })
    child.stderr.setEncoding("utf8").on("data", (data) => {
        error += data
    })
    const closed = once(child, "close")
    child.stdin.end(cases.map(JSON.stringify).join("\n") + "\n")
    const timer = setTimeout(() => child.kill("SIGKILL"), 20000)
    const [status] = await closed
    clearTimeout(timer)
    assert.equal(status, 0, error)
    return output.trim().split("\n").map(JSON.parse)
}
function temporary(t) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-download-parity-"))
    t.after(() => fs.rmSync(dir, { recursive: true, force: true }))
    return dir
}
async function server(t, fixtures) {
    const connections = new Set()
    const server = http.createServer(async (request, response) => {
        const item = fixtures[request.url] || { status: 404, bytes: Buffer.alloc(0) }
        response.statusCode = item.status || 200
        if (item.location) response.setHeader("Location", item.location)
        if (item.encoding) response.setHeader("Content-Encoding", item.encoding)
        if (!item.unknownLength) response.setHeader("Content-Length", item.bytes?.length || 0)
        if (item.chunked) response.setHeader("Transfer-Encoding", "chunked")
        const bytes = item.bytes || Buffer.alloc(0)
        for (let at = 0; at < bytes.length; at += 13) {
            response.write(bytes.subarray(at, at + 13))
            await new Promise((resolve) => setImmediate(resolve))
        }
        response.end()
    })
    server.on("connection", (socket) => {
        connections.add(socket)
        socket.on("close", () => connections.delete(socket))
    })
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve))
    t.after(async () => {
        for (const connection of connections) connection.destroy()
        await new Promise((resolve) => server.close(resolve))
    })
    return `http://127.0.0.1:${server.address().port}`
}
const normalize = (value, directory) => JSON.parse(JSON.stringify(value).replaceAll(directory.replaceAll("\\", "\\\\"), "<root>"))

test("immutable platform/architecture URL table includes exact ia32 legacy version and unsupported cases", async (t) => {
    const dir = temporary(t),
        cases = []
    for (const platform of ["linux", "darwin", "win32", "freebsd", "unknown"]) for (const arch of ["x64", "arm64", "arm", "ia32", "riscv64"]) cases.push({ op: "table", platform, arch })
    const actual = await native(cases)
    cases.forEach((item, index) => {
        assert.deepEqual(actual[index], { ok: true, value: oracle(dir, item.platform, item.arch).getDownloadUrl() })
        coverage.urlCases++
    })
    const source = oracle(dir, "freebsd", "x64")
    const reply = structuredClone(await source.handler())
    assert.deepEqual(reply, { success: false, error: "Unsupported platform: freebsd/x64" })
    assert.equal(source.requested.length, 0)
    assert.deepEqual((await native([{ op: "completion", installed: false, error: reply.error }]))[0].value, { reply, progress: structuredClone(source.events.at(-1).data) })
})

test("real HTTP/gzip inert bytes match actual source pipeline and exact host mode/mac/cache/reset/completion plans", async (t) => {
    const dir = temporary(t),
        bytes = Buffer.from("INERT generated fixture — 敬拜 🙌\0\xff"),
        compressed = zlib.gzipSync(bytes)
    const base = await server(t, { "/good": { bytes: compressed }, "/unknown": { bytes: compressed, unknownLength: true }, "/redirect": { status: 302, location: "/nested/../good" }, "/nested/../good": { bytes: compressed } })
    for (const [platform, arch] of [
        ["linux", "x64"],
        ["darwin", "arm64"],
        ["win32", "x64"],
        ["win32", "ia32"]
    ])
        for (const route of ["/good", "/unknown", "/redirect"]) {
            const sourceRoot = path.join(dir, `source-${coverage.downloads}`),
                nativeRoot = path.join(dir, `native-${coverage.downloads}`)
            fs.mkdirSync(nativeRoot)
            const expected = oracle(sourceRoot, platform, arch, base + route, { failMacCommands: true })
            const reply = structuredClone(await expected.handler())
            const [actual] = await native([{ url: base + route, root: nativeRoot, platform, arch }])
            assert.equal(actual.ok, true, JSON.stringify(actual))
            const value = actual.value
            assert.deepEqual(Buffer.from(value.bytes), expected.artifact())
            assert.deepEqual(Buffer.from(value.bytes), bytes)
            assert.equal(expected.requested[0], expected.getDownloadUrl())
            assert.deepEqual(value.completionPlanIfInstalled, { reply, progress: structuredClone(expected.events.at(-1).data) })
            // Compare the actual source chmod request with the native install
            // intent. A simulated Unix platform still executes on the host FS;
            // Node on Windows cannot expose Unix execute bits through stat.
            assert.deepEqual(expected.chmods, platform === "win32" ? [] : [0o755])
            assert.equal(value.install.mode, expected.chmods[0] ?? null)
            if (process.platform !== "win32" && platform !== "win32") assert.equal(expected.mode(), 0o755)
            assert.equal(value.install.destination, path.join(nativeRoot, platform === "win32" ? "ffmpeg.exe" : "ffmpeg"))
            const commands = normalize(value.install.postInstall, nativeRoot).map(({ program, args }) => [program, args])
            assert.deepEqual(
                commands,
                normalize(
                    expected.trace.filter(([step]) => ["xattr", "codesign"].includes(step)),
                    path.join(sourceRoot, "bin")
                )
            )
            assert.deepEqual(
                expected.trace.map(([step]) => step),
                platform === "darwin" ? ["rename", "xattr", "codesign", "clearResolverCache", "cleanup"] : ["rename", "clearResolverCache", "cleanup"]
            )
            assert.equal(value.install.clearResolverCacheAfterPostInstall, true)
            assert.equal(value.remainingEntries, 0)
            assert.equal(fs.existsSync(value.install.destination), false, "no final binary installed")
            if (route === "/unknown") {
                assert.equal(value.latestProgress, null)
                assert.equal(expected.events.filter((event) => event.data.status === "downloading").length, 0)
            } else assert.equal(value.latestProgress.progress, 100)
            coverage.downloads++
        }
})

test("source per-body-chunk progress math is compared separately from live HTTP chunking and native coalescing", async (t) => {
    const dir = temporary(t),
        bytes = zlib.gzipSync(Buffer.from("inert"))
    for (const total of [0, bytes.length, bytes.length * 4, 1, 8]) {
        const chunks = [bytes.subarray(0, 1), bytes.subarray(1, 4), bytes.subarray(4, 9), bytes.subarray(9)]
        const expected = oracle(path.join(dir, "progress-" + total), "linux", "x64", undefined, {
            response: () =>
                new Response(
                    new ReadableStream({
                        start(controller) {
                            for (const chunk of chunks) controller.enqueue(chunk)
                            controller.close()
                        }
                    }),
                    { headers: { "content-length": String(total) } }
                )
        })
        await expected.handler()
        let received = 0
        const cases = expected.chunks.map((length) => ({ op: "progress", received: (received += length), total }))
        const actual = await native(cases)
        const source = structuredClone(expected.events.filter((event) => event.data.status === "downloading").map((event) => event.data))
        assert.deepEqual(actual.map((item) => item.value).filter(Boolean), source)
        coverage.progressSamples += cases.length
    }
})

test("actual Node gunzip members/header fields/zero-padding and CRC/truncation failures agree with bounded native staging", async (t) => {
    const dir = temporary(t),
        a = zlib.gzipSync(Buffer.from("A")),
        b = zlib.gzipSync(Buffer.from("B"))
    const crc = Buffer.from(a)
    crc[crc.length - 8] ^= 1
    const length = Buffer.from(a)
    length[length.length - 4] ^= 1
    const fields = Buffer.concat([Buffer.from([0x1f, 0x8b, 8, 4 | 8 | 16, 0, 0, 0, 0, 0, 3, 3, 0, 1, 2, 3]), Buffer.from("name é\0comment\0"), a.subarray(10)])
    const fixtures = { "/one": { bytes: a }, "/two": { bytes: Buffer.concat([a, b]) }, "/zero-padding": { bytes: Buffer.concat([a, Buffer.alloc(30)]) }, "/fields": { bytes: fields }, "/crc": { bytes: crc }, "/length": { bytes: length }, "/truncated": { bytes: a.subarray(0, a.length - 1) }, "/empty": { bytes: Buffer.alloc(0) }, "/bad-header": { bytes: Buffer.from("not a gzip archive") } }
    const base = await server(t, fixtures)
    for (const route of Object.keys(fixtures)) {
        const sourceRoot = path.join(dir, "source-" + route.slice(1)),
            nativeRoot = path.join(dir, "native-" + route.slice(1))
        fs.mkdirSync(nativeRoot)
        const expected = oracle(sourceRoot, "linux", "x64", base + route),
            reply = structuredClone(await expected.handler())
        const [actual] = await native([{ url: base + route, root: nativeRoot }])
        if (reply.success) {
            assert.equal(actual.ok, true, JSON.stringify(actual))
            assert.deepEqual(Buffer.from(actual.value.bytes), expected.artifact())
            assert.deepEqual(actual.value.completionPlanIfInstalled.reply, reply)
        } else {
            assert.equal(actual.ok, false, JSON.stringify({ route, reply, actual }))
            assert.deepEqual(actual.completionPlan, { reply, progress: structuredClone(expected.events.at(-1).data) })
        }
        assert.equal(fs.readdirSync(nativeRoot).length, 0)
        assert.equal(fs.readdirSync(path.join(sourceRoot, "bin")).length, 0)
        coverage.downloads++
    }
})

test("HTTP no-body/errors and redirects use actual fetch status behavior and source error completion", async (t) => {
    const dir = temporary(t),
        fixtures = {}
    for (const status of [204, 205, 304, 404, 503]) fixtures["/" + status] = { status, bytes: Buffer.alloc(0) }
    const base = await server(t, fixtures)
    for (const route of Object.keys(fixtures)) {
        const sourceRoot = path.join(dir, "source" + route),
            nativeRoot = path.join(dir, "native" + route)
        fs.mkdirSync(nativeRoot, { recursive: true })
        const expected = oracle(sourceRoot, "linux", "x64", base + route),
            reply = structuredClone(await expected.handler()),
            [actual] = await native([{ url: base + route, root: nativeRoot }])
        assert.equal(actual.ok, false, JSON.stringify({ route, reply, actual }))
        assert.deepEqual(actual.completionPlan, { reply, progress: structuredClone(expected.events.at(-1).data) })
        assert.equal(fs.readdirSync(nativeRoot).length, 0)
        coverage.downloads++
    }
})

test("native resource and redirect/encoding limits reject explicitly while keeping private staging clean", async (t) => {
    const dir = temporary(t),
        a = zlib.gzipSync(Buffer.alloc(65536, 1)),
        b = zlib.gzipSync(Buffer.from("B")),
        named = Buffer.concat([Buffer.from([0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 0, 3]), Buffer.alloc(100, 65), Buffer.from([0]), a.subarray(10)])
    const base = await server(t, { "/good": { bytes: a }, "/members": { bytes: Buffer.concat([a, b]) }, "/header": { bytes: named }, "/redirect": { status: 302, location: "/redirect" }, "/encoding": { bytes: zlib.gzipSync(a), encoding: "gzip" }, "/padding-member": { bytes: Buffer.concat([a, Buffer.from([0]), b]) } })
    const cases = [
        { route: "/good", compressedLimit: 1, code: "download_limit" },
        { route: "/good", uncompressedLimit: 32, code: "download_limit" },
        { route: "/members", memberLimit: 1, code: "download_limit" },
        { route: "/header", headerLimit: 20, code: "download_limit" },
        { route: "/redirect", redirectLimit: 2, code: "download_source" },
        { route: "/encoding", code: "download_content_encoding" },
        { route: "/padding-member", code: "download_gzip_padding" }
    ]
    for (const item of cases) {
        const nativeRoot = path.join(dir, "native-" + coverage.resourceCases)
        fs.mkdirSync(nativeRoot)
        const [actual] = await native([{ ...item, root: nativeRoot, url: base + item.route }])
        assert.equal(actual.ok, false)
        assert.equal(actual.error.code, item.code, JSON.stringify(actual))
        assert.equal(fs.readdirSync(nativeRoot).length, 0)
        coverage.resourceCases++
    }
})

test("source zero-padding followed by a member has chunk-dependent behavior and is explicitly unsupported natively", async (t) => {
    const dir = temporary(t),
        a = zlib.gzipSync(Buffer.from("A")),
        b = zlib.gzipSync(Buffer.from("B"))
    for (const split of [false, true]) {
        const sourceRoot = path.join(dir, split ? "split" : "whole")
        const expected = oracle(sourceRoot, "linux", "x64", undefined, {
            response: () =>
                new Response(
                    new ReadableStream({
                        async start(controller) {
                            if (split) {
                                controller.enqueue(Buffer.concat([a, Buffer.from([0])]))
                                await new Promise((resolve) => setTimeout(resolve, 30))
                                controller.enqueue(b)
                            } else controller.enqueue(Buffer.concat([a, Buffer.from([0]), b]))
                            controller.close()
                        }
                    })
                )
        })
        const reply = structuredClone(await expected.handler())
        if (split) {
            assert.equal(reply.success, false)
            assert.match(reply.error, /^(stream\.push\(\) after EOF|incorrect header check)$/)
            assert.equal(expected.artifact(), undefined)
        } else {
            assert.deepEqual(reply, { success: true })
            assert.deepEqual(expected.artifact(), Buffer.from("A"))
        }
        assert.equal(fs.readdirSync(path.join(sourceRoot, "bin")).length, 0)
        coverage.paddingChunkCases++
    }
})
