// Genuine packaged Electron nativeImage reference; no runtime subprocess fallback.
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import { createHash } from "node:crypto"
import assert from "node:assert/strict"
import { _electron as electron } from "playwright"
import { png } from "./fixtures.mjs"
const root = fileURLToPath(new URL("../../../", import.meta.url))
const executablePath = process.env.FREESHOW_ELECTRON_BIN
if (!executablePath) throw Error("Set FREESHOW_ELECTRON_BIN to the verified untouched packaged oracle")
const binary = process.env.FREESHOW_COVER_IMAGES_PARITY || path.join(root, "target/debug/freeshow-cover-images-parity" + (process.platform === "win32" ? ".exe" : ""))
const folder = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-cover-images-electron-")),
    data = path.join(folder, "data"),
    config = path.join(folder, "app")
for (const p of [path.join(data, "Config"), config]) fs.mkdirSync(p, { recursive: true })
fs.writeFileSync(path.join(config, "config.json"), JSON.stringify({ dataPath: data, maximized: false }))
fs.writeFileSync(path.join(config, "settings.json"), JSON.stringify({ initialized: true, autoUpdates: false, language: "en" }))
const hash = (b) => createHash("sha256").update(b).digest("hex")
const fixtures = [
    { name: "empty", bytes: Buffer.alloc(0) },
    ...[
        { w: 2, h: 1, alpha: true },
        { w: 9, h: 5, rgb: true },
        { w: 640, h: 480 },
        { w: 641, h: 1 },
        { w: 640, h: 481 },
        { w: 2000, h: 1 },
        { w: 2001, h: 1 },
        { w: 800, h: 600, solid: true }
    ].map(({ w, h, ...options }) => ({ name: `${w}x${h}${options.solid ? "-solid" : ""}`, bytes: png(w, h, options) }))
]
for (const f of fixtures) {
    const r = spawnSync(binary, [], { input: JSON.stringify({ ops: [{ op: "create", base64: f.bytes.toString("base64"), alternate: true }, { op: "png" }, { op: "jpeg" }] }), encoding: "utf8", timeout: 20000, maxBuffer: 16 * 1024 * 1024 })
    assert.equal(r.status, 0, r.stderr)
    f.native = JSON.parse(r.stdout).results
    assert.equal(f.native[0].error, undefined, JSON.stringify(f.native))
}
let app
try {
    app = await electron.launch({ executablePath, args: ["--no-sandbox", "--disable-gpu"], env: { ...process.env, NODE_ENV: "production", FS_MOCK_STORE_PATH: config, XDG_CONFIG_HOME: path.join(folder, "xdg"), XDG_CACHE_HOME: path.join(folder, "cache"), XDG_DATA_HOME: path.join(folder, "xdg-data") }, timeout: 45000 })
    const response = await app.evaluate(
        ({ nativeImage, app }, fixtures) => {
            const digest = (bytes) => ({ length: bytes.length, base64: bytes.toString("base64") })
            return {
                runtime: { version: app.getVersion(), versions: process.versions },
                fixtures: fixtures.map((f) => {
                    const original = nativeImage.createFromBuffer(Buffer.from(f.base64, "base64")),
                        originalPng = original.toPNG(),
                        nativePng = Buffer.from(f.native[1].bytes.base64, "base64"),
                        decoded = nativeImage.createFromBuffer(nativePng),
                        size = original.getSize()
                    const result = { name: f.name, sourceBytes: digest(Buffer.from(f.base64, "base64")), source: { size, bitmap: digest(original.toBitmap()), png: digest(originalPng) }, rust: { size: decoded.getSize(), bitmap: digest(decoded.toBitmap()), png: digest(nativePng), diagnostics: f.native[1].diagnostics } }
                    if (size.width > 640 || size.height > 480) {
                        const scale = Math.min(640 / size.width, 480 / size.height),
                            target = { width: Math.round(size.width * scale), height: Math.round(size.height * scale) },
                            source = original.resize(target),
                            sourceJpeg = source.toJPEG(90),
                            nativeJpeg = Buffer.from(f.native[2].bytes.base64, "base64"),
                            rust = nativeImage.createFromBuffer(nativeJpeg),
                            sourceDecoded = nativeImage.createFromBuffer(sourceJpeg)
                        result.resize = { target, source: { size: source.getSize(), bytes: digest(sourceJpeg), bitmap: digest(sourceDecoded.toBitmap()) }, rust: { size: rust.getSize(), bytes: digest(nativeJpeg), bitmap: digest(rust.toBitmap()), diagnostics: f.native[2].diagnostics } }
                    }
                    return result
                })
            }
        },
        fixtures.map((f) => ({ name: f.name, base64: f.bytes.toString("base64"), native: f.native }))
    )
    function digestTree(value) {
        if (value && typeof value === "object") {
            if (typeof value.base64 === "string" && typeof value.length === "number") {
                value.sha256 = hash(Buffer.from(value.base64, "base64"))
                delete value.base64
            } else for (const v of Object.values(value)) digestTree(v)
        }
    }
    digestTree(response)
    for (const f of response.fixtures) {
        assert.deepEqual(f.rust.size, f.source.size, f.name)
        assert.deepEqual(f.rust.bitmap, f.source.bitmap, f.name)
        f.pngBytesEqual = f.rust.png.sha256 === f.source.png.sha256
        if (f.resize) {
            assert.deepEqual(f.resize.rust.size, f.resize.source.size, f.name)
            f.resize.bytesEqual = f.resize.rust.bytes.sha256 === f.resize.source.bytes.sha256
            f.resize.pixelsEqual = f.resize.rust.bitmap.sha256 === f.resize.source.bitmap.sha256
            assert.deepEqual(f.resize.rust.diagnostics, ["native_lanczos3_jpeg90_differs"])
        }
    }
    // Source-supported JPEG and partial-alpha are recorded as explicit backend gaps.
    const gapInputs = [
        { name: "partial-alpha", base64: png(2, 1, { partial: true }).toString("base64") },
        { name: "unknown-short", base64: Buffer.from([1, 2, 3]).toString("base64") }
    ]
    const gaps = await app.evaluate(({ nativeImage }, inputs) => {
        const result = inputs.map(({ name, base64 }) => ({ name, base64, size: nativeImage.createFromBuffer(Buffer.from(base64, "base64")).getSize() }))
        const cover = nativeImage.createFromBuffer(Buffer.from(inputs[0].base64, "base64"))
        const jpeg = cover.toJPEG(90)
        result.push({ name: "source-jpeg", base64: jpeg.toString("base64"), size: nativeImage.createFromBuffer(jpeg).getSize() })
        return result
    }, gapInputs)
    response.unsupported = []
    for (const gap of gaps) {
        const r = spawnSync(binary, [], { input: JSON.stringify({ ops: [{ op: "create", base64: gap.base64 }] }), encoding: "utf8", timeout: 20000 })
        assert.equal(r.status, 0, r.stderr)
        const actual = JSON.parse(r.stdout).results[0]
        assert.equal(actual.error.code, "cover_unsupported")
        response.unsupported.push({ name: gap.name, sourceSize: gap.size, inputBytes: Buffer.from(gap.base64, "base64").length, inputSha256: hash(Buffer.from(gap.base64, "base64")), nativeError: actual.error.code })
    }
    response.fixtureInputSha256 = hash(Buffer.concat(fixtures.map((f) => f.bytes)))
    const target = process.env.FREESHOW_COVER_IMAGES_RECEIPT
    if (target) {
        fs.mkdirSync(path.dirname(target), { recursive: true })
        fs.writeFileSync(target, JSON.stringify(response, null, 2) + "\n")
    }
    console.log(JSON.stringify(response))
} finally {
    if (app) {
        await app.evaluate(({ app }) => app.exit(0)).catch(() => {})
        await app.close().catch(() => {})
    }
    fs.rmSync(folder, { recursive: true, force: true })
}
