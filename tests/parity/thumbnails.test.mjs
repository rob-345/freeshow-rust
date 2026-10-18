import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import zlib from "node:zlib"
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import ts from "typescript"
import vm from "node:vm"
import { root, upstreamFunctions, clean } from "./oracle.mjs"
const binary = process.env.FREESHOW_THUMBNAILS_BIN || path.join(root, "target/debug", process.platform === "win32" ? "freeshow-thumbnails.exe" : "freeshow-thumbnails")
const linux = process.platform === "linux"
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex")
function temporary(t) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-thumbnail-parity-"))
    t.after(() => fs.rmSync(dir, { recursive: true, force: true }))
    return dir
}
function mediaExtensions() {
    const file = "src/electron/data/media.ts",
        source = fs.readFileSync(path.join(root, file), "utf8"),
        ast = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true)
    const declarations = ["imageExtensions", "videoExtensions"]
        .map((name) =>
            ast.statements
                .find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((entry) => entry.name.getText(ast) === name))
                .getText(ast)
                .replace(/^export\s+/, "")
        )
        .join("\n")
    const context = vm.createContext({})
    new vm.Script(ts.transpileModule(declarations + "\nglobalThis.result={imageExtensions,videoExtensions}", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).runInContext(context)
    return context.result
}
function original(cacheRoot, completion = "success", existing = []) {
    const currentlyGenerating = new Set(existing),
        failedPaths = [],
        captures = [],
        deleted = [],
        waits = [],
        queue = []
    const upstream = upstreamFunctions("src/electron/data/thumbnails.ts", ["doesMediaExist", "deleteThumbnails", "getThumbnail", "getThumbnailPath", "filePathHashCode", "createThumbnail", "addToGenerateQueue", "nextInQueue", "generationFinished", "generateThumbnail", "generate", "parseSize", "getExtension"], {
        path,
        thumbnailFolderPath: cacheRoot,
        currentlyGenerating,
        failedPaths,
        thumbnailQueue: queue,
        sizes: [900, 500, 250, 100],
        ...mediaExtensions(),
        getFileStatsAsync: async (file) => {
            try {
                return fs.statSync(file)
            } catch {
                return null
            }
        },
        doesPathExistAsync: async (file) => {
            try {
                fs.accessSync(file)
                return true
            } catch {
                return false
            }
        },
        doesPathExist: fs.existsSync,
        deleteFile: (file) => deleted.push(file),
        async captureWithCanvas(data) {
            captures.push(clean(data))
            if (completion === "failed") failedPaths.push(data.id)
            if (completion !== "timeout") currentlyGenerating.clear()
        },
        async waitUntilValueIsDefined(value, interval, timeout) {
            waits.push({ interval, timeout })
            for (let i = 0; i < 30; i++) {
                await Promise.resolve()
                if (value()) return true
            }
            return null
        }
    })
    return { ...upstream, captures, deleted, waits }
}
function rust(requests, args = []) {
    const result = spawnSync(binary, args, { input: requests.map((request) => JSON.stringify(request)).join("\n") + "\n", encoding: "utf8", timeout: 30000, maxBuffer: 16 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return result.stdout.trim().split("\n").map(JSON.parse)
}
function request(command, data, cacheRoot, extra = {}) {
    return { command, data, cacheRoot, ...extra }
}
function crc32(bytes) {
    let crc = -1
    for (const byte of bytes) {
        crc ^= byte
        for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0)
    }
    return (crc ^ -1) >>> 0
}
function chunk(type, body) {
    const name = Buffer.from(type),
        header = Buffer.alloc(4),
        crc = Buffer.alloc(4)
    header.writeUInt32BE(body.length)
    crc.writeUInt32BE(crc32(Buffer.concat([name, body])))
    return Buffer.concat([header, name, body, crc])
}
function png(width = 16, height = 8) {
    const header = Buffer.alloc(13)
    header.writeUInt32BE(width)
    header.writeUInt32BE(height, 4)
    header[8] = 8
    header[9] = 6
    const rows = Buffer.alloc(height * (1 + width * 4))
    for (let y = 0; y < height; y++)
        for (let x = 0; x < width; x++) {
            const i = y * (1 + width * 4) + 1 + x * 4
            rows.set([(x * 13) % 256, (y * 29) % 256, 197, (x * 17) % 256], i)
        }
    return Buffer.concat([Buffer.from("89504e470d0a1a0a", "hex"), chunk("IHDR", header), chunk("sRGB", Buffer.from([0])), chunk("IDAT", zlib.deflateSync(rows)), chunk("IEND", Buffer.alloc(0))])
}
function pixels(bytes) {
    let offset = 8,
        width,
        height,
        color,
        compressed = [],
        chunks = []
    while (offset < bytes.length) {
        const size = bytes.readUInt32BE(offset),
            type = bytes.toString("ascii", offset + 4, offset + 8),
            body = bytes.subarray(offset + 8, offset + 8 + size)
        chunks.push(type)
        if (type === "IHDR") {
            width = body.readUInt32BE()
            height = body.readUInt32BE(4)
            assert.equal(body[8], 8)
            color = body[9]
            assert.equal(body[12], 0)
        }
        if (type === "IDAT") compressed.push(body)
        offset += size + 12
    }
    const raw = zlib.inflateSync(Buffer.concat(compressed)),
        bpp = color === 6 ? 4 : 3,
        stride = width * bpp,
        rgba = Buffer.alloc(width * height * 4)
    let previous = Buffer.alloc(stride),
        cursor = 0
    for (let y = 0; y < height; y++) {
        const filter = raw[cursor++],
            row = Buffer.from(raw.subarray(cursor, cursor + stride))
        cursor += stride
        for (let x = 0; x < stride; x++) {
            const a = x >= bpp ? row[x - bpp] : 0,
                b = previous[x],
                c = x >= bpp ? previous[x - bpp] : 0,
                p = a + b - c,
                distances = [Math.abs(p - a), Math.abs(p - b), Math.abs(p - c)],
                paeth = [a, b, c][distances.indexOf(Math.min(...distances))]
            row[x] = (row[x] + [0, a, b, Math.floor((a + b) / 2), paeth][filter]) & 255
        }
        for (let x = 0; x < width; x++) {
            const i = (y * width + x) * 4
            row.copy(rgba, i, x * bpp, x * bpp + 3)
            rgba[i + 3] = bpp === 4 ? row[x * bpp + 3] : 255
        }
        previous = row
    }
    return { width, height, rgba, chunks }
}

test("hash, parse-size and retained canvas dimensions use actual source UTF-16/default/floor rules", () => {
    const oracle = original("/cache")
    const inputs = ["", "abc", "polygenelubricants", "敬拜 🙌.png", "C:\\Photos\\test.jpg", "/tmp/../a.png"]
    const actual = rust(
        inputs.map((input) => ({ input })),
        ["--hash"]
    )
    for (const [index, input] of inputs.entries()) {
        const expected = oracle.filePathHashCode(input)
        assert.equal(actual[index].data, expected)
        assert.match(expected, /^[ai]\d+$/)
        if (input === "polygenelubricants") assert.equal(expected, "i2147483648")
    }
    const { getNewSize } = upstreamFunctions("src/frontend/components/helpers/media.ts", ["getNewSize"])
    for (const [width, height] of [
        [1920, 1080],
        [3, 2],
        [2, 3],
        [0, 0]
    ]) {
        const actual = clean(getNewSize({ width, height }, oracle.parseSize("250x?")))
        assert.equal(actual.width, 250)
        assert.equal(actual.height, Math.floor(250 / ((width || 1920) / (height || 1080))))
    }
})

test("DOES_MEDIA_EXIST matches access/stat ctime floor, noCache, truthy mismatch and four guarded invalidations", { skip: !linux && "Linux statx contract" }, async (t) => {
    const dir = temporary(t),
        cache = path.join(dir, "cache")
    fs.mkdirSync(cache)
    const input = path.join(dir, "音楽 🙌.png")
    fs.writeFileSync(input, png())
    // Negative numeric utimes arguments mean "now" in Node. Dates express
    // actual pre-epoch times without a same-millisecond ctime/mtime race.
    fs.utimesSync(input, new Date(-100123), new Date(-50654))
    fs.chmodSync(input, 0o640)
    const ctime = Math.floor(fs.statSync(input).ctimeMs),
        oracle = original(cache),
        missing = path.join(dir, "missing.png")
    for (const file of [input, missing]) for (const size of [900, 500, 250, 100]) fs.writeFileSync(oracle.getThumbnailPath(file, size), "old cache")
    const cases = [{ path: input }, { path: input, creationTime: 0 }, { path: input, creationTime: ctime }, { path: input, creationTime: ctime + 1 }, { path: input, creationTime: -1 }, { path: input, noCache: true, creationTime: -1 }, { path: missing }, { path: "" }, { path: dir }]
    const actual = rust(cases.map((data) => request("DOES_MEDIA_EXIST", data, cache)))
    for (const [index, data] of cases.entries()) {
        const source = original(cache)
        assert.deepEqual(actual[index].reply, clean(await source.doesMediaExist(data)))
        assert.deepEqual(
            actual[index].effects.map((effect) => path.join(cache, effect.destination)),
            source.deleted
        )
        assert.ok(actual[index].effects.every((effect) => effect.kind === "invalidate" && effect.expected.length > 0))
    }
    assert.notEqual(ctime, Math.floor(fs.statSync(input).mtimeMs))
    assert.ok(fs.statSync(input).mtimeMs < 0)
    assert.equal(actual[3].effects.length, 4)
    assert.equal(actual[5].effects.length, 0)
    for (const effect of actual[3].effects) assert.ok(fs.existsSync(path.join(cache, effect.destination)))
})

test("cache hits, missing inputs and capture requests preserve raw payload, effective size, mtime and canvas wire shape", { skip: !linux && "Linux thumbnail cache contract" }, async (t) => {
    const dir = temporary(t),
        cache = path.join(dir, "cache")
    fs.mkdirSync(cache)
    const input = path.join(dir, "敬拜 🙌.PNG")
    fs.writeFileSync(input, png())
    fs.utimesSync(input, 100, 100)
    for (const size of [100, 250]) {
        const file = original(cache).getThumbnailPath(input, size)
        fs.writeFileSync(file, "opaque corrupt cached bytes")
        fs.utimesSync(file, size === 250 ? 100 : 200, size === 250 ? 100 : 200)
    }
    const cases = [{ input, size: 100, unknown: { preserved: true } }, { input, size: 250 }, { input, size: 16 }, { input, size: 0 }, { input }, { input, size: null }, { input: path.join(dir, "missing.png"), size: 100, output: "stale", future: [1, null] }, { input: "freeshow-protected://missing", size: 250 }]
    const actual = rust(cases.map((data) => request("GET_THUMBNAIL", data, cache)))
    for (const [index, data] of cases.entries()) {
        const source = original(cache)
        assert.deepEqual(actual[index].reply, clean(await source.getThumbnail(data)))
        if (source.captures.length) {
            const capture = { ...actual[index].capture, output: source.captures[0].output }
            assert.deepEqual(capture, source.captures[0])
            assert.equal(actual[index].outcome, "capturePending")
            assert.ok(actual[index].capture.output.includes("freeshow-thumbnail-"))
        } else assert.equal(actual[index].capture, null)
    }
    assert.equal(actual[0].outcome, "cacheHitUnverified")
    assert.equal(actual[7].diagnostics[0].code, "protected_capture_unavailable")
    const stale = original(cache).getThumbnailPath(input, 16)
    fs.writeFileSync(stale, "stale")
    fs.utimesSync(stale, 50, 50)
    const result = rust([request("GET_THUMBNAIL", { input, size: 16 }, cache)])[0]
    assert.equal(result.effects[0].kind, "invalidate")
    assert.ok(fs.existsSync(stale))
})

test("capture failure, predicted timeout and raw/effective failure-key quirks remain distinguishable from generated bytes", { skip: !linux && "Linux preparation contract" }, async (t) => {
    const dir = temporary(t),
        input = path.join(dir, "image.png")
    fs.writeFileSync(input, png())
    const cache = path.join(dir, "cache")
    for (const size of [100, 0, undefined, null])
        for (const finish of ["failed", "timeout"]) {
            const data = { input, ...(size === undefined ? {} : { size }) },
                source = original(cache, finish),
                actual = rust([request("GET_THUMBNAIL", data, cache, { finish })])[0]
            assert.deepEqual(actual.reply, clean(await source.getThumbnail(data)))
            assert.equal(actual.outcome, finish === "failed" ? "captureFailed" : "timedOutPrediction")
            assert.equal(actual.effects.length, 0)
        }
    const inProgress = original(cache, "timeout", [`${input}-undefined`])
    const duplicate = await inProgress.getThumbnail({ input })
    assert.equal(duplicate.output, inProgress.getThumbnailPath(input, 250))
    assert.equal(inProgress.captures.length, 0)
})

test("validated sRGB canvas completion stages exact bounded PNG bytes, pixels, source and destination guards", { skip: !linux && "Linux staging contract" }, (t) => {
    const dir = temporary(t),
        input = path.join(dir, "image.png"),
        canvas = path.join(dir, "canvas.png"),
        cache = path.join(dir, "cache")
    const bytes = png()
    fs.writeFileSync(input, bytes)
    fs.writeFileSync(canvas, bytes)
    const actual = rust([request("GET_THUMBNAIL", { input, size: 16, unknown: "retained" }, cache, { pngPath: canvas })])[0]
    assert.equal(actual.outcome, "generatedPng")
    assert.equal(actual.effects.length, 1)
    const effect = actual.effects[0]
    assert.equal(effect.kind, "install")
    assert.equal(effect.sha256, sha(bytes))
    assert.equal(effect.pixel_sha256, sha(pixels(bytes).rgba))
    assert.deepEqual([effect.width, effect.height], [16, 8])
    assert.deepEqual(effect.expected_destination, { kind: "absent" })
    assert.equal(effect.source.path, input)
    assert.ok(effect.expected_staged.length === bytes.length)
    assert.ok(!fs.existsSync(cache))
    const invalid = Buffer.from(bytes)
    invalid.writeUInt32BE(0xffffffff, 16)
    fs.writeFileSync(canvas, invalid)
    assert.equal(rust([request("GET_THUMBNAIL", { input, size: 16 }, cache, { pngPath: canvas })])[0].error.code, "thumbnail_resource_limit")
})

test("unsupported formats and cache symlinks stay explicit; permission and symlink existence preserve source behavior", { skip: !linux && "Linux filesystem contract" }, async (t) => {
    const dir = temporary(t),
        cache = path.join(dir, "cache"),
        text = path.join(dir, "notes.txt"),
        image = path.join(dir, "image.png")
    fs.mkdirSync(cache)
    fs.writeFileSync(text, "text")
    fs.writeFileSync(image, png())
    const source = original(cache),
        wire = await source.getThumbnail({ input: text, size: 100 })
    assert.ok(wire.output.endsWith("-100.png"))
    assert.equal(fs.existsSync(wire.output), false)
    assert.equal(rust([request("GET_THUMBNAIL", { input: text, size: 100 }, cache)])[0].error.code, "unsupported_capture_format")
    fs.writeFileSync(wire.output, "source accepts corrupt cache")
    fs.utimesSync(text, 100, 100)
    fs.utimesSync(wire.output, 200, 200)
    assert.equal(rust([request("GET_THUMBNAIL", { input: text, size: 100 }, cache)])[0].outcome, "cacheHitUnverified")
    const linked = path.join(dir, "linked.png")
    fs.symlinkSync(image, linked)
    const real = rust([request("DOES_MEDIA_EXIST", { path: linked }, cache)])[0]
    assert.deepEqual(real.reply, clean(await original(cache).doesMediaExist({ path: linked })))
    const target = source.getThumbnailPath(image, 100)
    fs.symlinkSync(image, target)
    assert.equal(rust([request("GET_THUMBNAIL", { input: image, size: 100 }, cache)])[0].error.code, "unsupported_cache_variant")
    const denied = path.join(dir, "denied")
    fs.mkdirSync(denied)
    fs.writeFileSync(path.join(denied, "x.png"), png())
    fs.chmodSync(denied, 0)
    try {
        if (process.getuid?.() !== 0) {
            const data = { path: path.join(denied, "x.png") }
            assert.deepEqual(rust([request("DOES_MEDIA_EXIST", data, cache)])[0].reply, clean(await original(cache).doesMediaExist(data)))
        }
    } finally {
        fs.chmodSync(denied, 0o700)
    }
})

if (!linux)
    test("unverified filesystem backends return unsupported instead of invented ctime", (t) => {
        const cache = temporary(t)
        for (const result of rust([request("DOES_MEDIA_EXIST", { path: "x" }, cache), request("GET_THUMBNAIL", { input: "x" }, cache)])) assert.equal(result.error.code, "unsupported_platform")
    })

if (process.env.FREESHOW_TEST_PACKAGED_THUMBNAILS === "1")
    test("actual packaged canvas/nativeImage PNG pixels and real getThumbnail outputs establish bounded completion equivalence", (t) => {
        assert.ok(linux, "Packaged oracle requires Linux")
        const dir = temporary(t),
            image = path.join(dir, "透明 敬拜 🙌.png"),
            video = path.join(dir, "movie.mp4")
        fs.writeFileSync(image, png())
        const missing = path.join(dir, "missing.png"),
            corrupt = path.join(dir, "corrupt.png"),
            text = path.join(dir, "notes.txt")
        fs.writeFileSync(corrupt, "corrupt PNG bytes")
        fs.writeFileSync(text, "ordinary text")
        const ffmpeg = spawnSync(process.env.FREESHOW_TEST_FFMPEG || "ffmpeg", ["-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i", "color=c=red:s=32x16:r=4:d=1", "-f", "lavfi", "-i", "color=c=blue:s=32x16:r=4:d=1", "-filter_complex", "[0:v][1:v]concat=n=2:v=1:a=0", "-c:v", "libx264", "-threads", "1", "-pix_fmt", "yuv420p", "-y", video], { encoding: "utf8", timeout: 15000 })
        assert.equal(ffmpeg.status, 0, ffmpeg.stderr)
        const requests = [
            { command: "GET_THUMBNAIL", data: { input: image, size: 16, future: "preserved" } },
            { command: "GET_THUMBNAIL", data: { input: image, size: 8 } },
            { command: "GET_THUMBNAIL", data: { input: video, size: 16 } },
            { command: "GET_THUMBNAIL", data: { input: missing, size: 16 } },
            { command: "GET_THUMBNAIL", data: { input: corrupt, size: 16 } },
            { command: "GET_THUMBNAIL", data: { input: corrupt, size: 0 } },
            { command: "GET_THUMBNAIL", data: { input: text, size: 100 } },
            { command: "DOES_MEDIA_EXIST", data: { path: image } }
        ]
        const run = spawnSync("xvfb-run", ["-a", "dbus-run-session", "--", process.execPath, path.join(root, "tests/desktop/thumbnail_probe.js")], { input: JSON.stringify({ requests }), encoding: "utf8", env: process.env, timeout: 90000, maxBuffer: 8 * 1024 * 1024 })
        assert.equal(run.error, undefined)
        assert.equal(run.status, 0, run.stderr)
        const evidence = JSON.parse(run.stdout.split("\n").find((line) => line.startsWith('{"privateProfile"')))
        assert.equal(evidence.temporaryRemoved, true)
        const receipt = { runtime: evidence.runtime.versions, temporaryRemoved: true, pairs: [], thumbnails: [], failures: [] }
        for (const pair of evidence.pngPairs) {
            const canvas = pixels(Buffer.from(pair.canvas, "base64")),
                native = pixels(Buffer.from(pair.native, "base64"))
            assert.deepEqual(canvas.rgba, native.rgba)
            assert.notEqual(pair.canvas, pair.native)
            const file = path.join(dir, `canvas-${pair.mode}.png`)
            fs.writeFileSync(file, Buffer.from(pair.canvas, "base64"))
            const actual = rust([request("GET_THUMBNAIL", { input: image, size: 256 }, path.join(dir, "cache"), { pngPath: file })])[0]
            if (pair.mode === "p3") assert.equal(actual.error.code, "unsupported_canvas_png")
            else assert.equal(actual.effects[0].pixel_sha256, sha(native.rgba))
            receipt.pairs.push({ mode: pair.mode, width: canvas.width, height: canvas.height, canvasSha256: sha(Buffer.from(pair.canvas, "base64")), nativeSha256: sha(Buffer.from(pair.native, "base64")), rgbaSha256: sha(native.rgba), exactRgba: true, byteEqual: false, chunks: canvas.chunks })
        }
        for (const [index, expected] of evidence.replies.entries()) {
            if (index >= 3) {
                assert.equal(expected.png, null)
                const selected = requests[index]
                const result = rust([request(selected.command, selected.data, path.join(dir, "cache"), index === 4 || index === 5 ? { finish: "failed" } : {})])[0]
                if (index === 6) {
                    assert.equal(result.error.code, "unsupported_capture_format")
                    assert.ok(expected.reply.output)
                } else if (index === 7) {
                    assert.deepEqual(result.reply, expected.reply)
                } else {
                    assert.equal(result.reply.input, expected.reply.input)
                    assert.equal(path.basename(result.reply.output), path.basename(expected.reply.output))
                    assert.equal(result.outcome, index === 3 ? "missing" : "captureFailed")
                }
                receipt.failures.push({ kind: ["missing", "corrupt", "corruptSizeZero", "unsupportedExtension", "changeTime"][index - 3], outputEmpty: expected.reply.output === "", artifactPresent: false, sourceMatchesNative: index !== 6, explicitUnsupported: index === 6 })
                continue
            }
            assert.ok(expected.png, "Original must actually generate output")
            const bytes = Buffer.from(expected.png, "base64"),
                decoded = pixels(bytes),
                file = path.join(dir, `original-${index}.png`)
            fs.writeFileSync(file, bytes)
            const actual = rust([request("GET_THUMBNAIL", requests[index].data, path.join(dir, "cache"), { pngPath: file })])[0]
            assert.equal(actual.outcome, "generatedPng")
            assert.equal(actual.effects[0].pixel_sha256, sha(decoded.rgba))
            assert.equal(actual.reply.future, requests[index].data.future)
            assert.deepEqual([decoded.width, decoded.height], [requests[index].data.size, requests[index].data.size / 2])
            if (index === 2) assert.ok(decoded.rgba[2] > 200 && decoded.rgba[0] < 10, "Source video seeks second half blue frame")
            receipt.thumbnails.push({ type: index === 2 ? "video" : "image", width: decoded.width, height: decoded.height, rgbaSha256: sha(decoded.rgba), pngSha256: sha(bytes) })
        }
        if (process.env.FREESHOW_THUMBNAIL_RECEIPT) {
            fs.mkdirSync(path.dirname(process.env.FREESHOW_THUMBNAIL_RECEIPT), { recursive: true })
            fs.writeFileSync(process.env.FREESHOW_THUMBNAIL_RECEIPT, JSON.stringify(receipt, null, 2) + "\n")
        }
    })

test("committed packaged PNG receipt pins source, runtime, decoded pixels and byte differences", () => {
    const fixture = JSON.parse(fs.readFileSync(path.join(root, "crates/freeshow-thumbnails/evidence/packaged-png.json"), "utf8"))
    assert.equal(fixture.runtime.electron, "37.10.3")
    assert.equal(fixture.runtime.node, "22.21.1")
    assert.equal(fixture.privateProfile, true)
    assert.equal(fixture.temporaryRemoved, true)
    for (const pair of fixture.pngPairs) {
        assert.equal(pair.exactRgba, true)
        assert.equal(pair.byteEqual, false)
        assert.notEqual(pair.canvasSha256, pair.nativeSha256)
        assert.deepEqual([pair.width, pair.height], [256, 8])
    }
    assert.deepEqual(
        fixture.thumbnails.map(({ type, width, height }) => ({ type, width, height })),
        [
            { type: "image", width: 16, height: 8 },
            { type: "image", width: 8, height: 4 },
            { type: "video", width: 16, height: 8 }
        ]
    )
    assert.deepEqual(
        fixture.failures.map((item) => item.kind),
        ["missing", "corrupt", "corruptSizeZero", "unsupportedExtension", "changeTime"]
    )
    assert.ok(fixture.failures.every((item) => !item.artifactPresent))
    assert.equal(fixture.failures[2].outputEmpty, false)
    assert.equal(fixture.failures[3].explicitUnsupported, true)
    for (const [file, functions] of Object.entries(fixture.sourceSha256)) {
        // The retained helpers now include native URL/CORS adaptations. This
        // receipt describes the untouched packaged application's pinned source.
        const source = spawnSync("git", ["show", `2d94defcf98175e4f5c9504e7f4fd1d33c35c975:${file}`], { cwd: root, encoding: "utf8" })
        assert.equal(source.status, 0, source.stderr)
        const ast = ts.createSourceFile(file, source.stdout, ts.ScriptTarget.Latest, true)
        for (const [name, expected] of Object.entries(functions)) {
            const node = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === name)
            assert.ok(node)
            assert.equal(sha(node.getText(ast).replace(/\r\n/g, "\n")), expected)
        }
    }
})
