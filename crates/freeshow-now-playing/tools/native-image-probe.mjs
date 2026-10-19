// Actual untouched packaged Electron capability evidence, not a Rust codec oracle.
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { createHash } from "node:crypto"
import { deflateSync } from "node:zlib"
import { _electron as electron } from "playwright"
const executablePath = process.env.FREESHOW_ELECTRON_BIN
if (!executablePath) throw Error("Set FREESHOW_ELECTRON_BIN to the verified packaged oracle")
const folder = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-now-playing-image-"))
const data = path.join(folder, "data"),
    config = path.join(folder, "app")
for (const p of [path.join(data, "Config"), config]) fs.mkdirSync(p, { recursive: true })
fs.writeFileSync(path.join(config, "config.json"), JSON.stringify({ dataPath: data, maximized: false }))
fs.writeFileSync(path.join(config, "settings.json"), JSON.stringify({ initialized: true, autoUpdates: false, language: "en" }))
function crc(bytes) {
    let c = 0xffffffff
    for (const b of bytes) {
        c ^= b
        for (let bit = 0; bit < 8; bit++) c = (c >>> 1) ^ (c & 1 ? 0xedb88320 : 0)
    }
    return (c ^ 0xffffffff) >>> 0
}
function chunk(type, bytes) {
    const name = Buffer.from(type),
        n = Buffer.alloc(4),
        c = Buffer.alloc(4)
    n.writeUInt32BE(bytes.length)
    c.writeUInt32BE(crc(Buffer.concat([name, bytes])))
    return Buffer.concat([n, name, bytes, c])
}
function png(w, h) {
    const head = Buffer.alloc(13)
    head.writeUInt32BE(w)
    head.writeUInt32BE(h, 4)
    head[8] = 8
    head[9] = 6
    const rows = Buffer.alloc((w * 4 + 1) * h)
    for (let y = 0; y < h; y++)
        for (let x = 0; x < w; x++) {
            const at = y * (w * 4 + 1) + 1 + x * 4
            rows[at] = 255
            rows[at + 1] = x % 256
            rows[at + 2] = y % 256
            rows[at + 3] = x % 2 ? 0 : 255
        }
    return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", head), chunk("IDAT", deflateSync(rows)), chunk("IEND", Buffer.alloc(0))])
}
const fixtures = [
    { name: "empty", bytes: [] },
    { name: "invalid", bytes: [1, 2, 3] },
    ...[
        [2, 1],
        [640, 480],
        [641, 1],
        [640, 481],
        [2000, 1]
    ].map(([w, h]) => ({ name: `${w}x${h}`, bytes: [...png(w, h)] }))
]
let app
try {
    app = await electron.launch({ executablePath, args: ["--no-sandbox", "--disable-gpu"], env: { ...process.env, NODE_ENV: "production", FS_MOCK_STORE_PATH: config, XDG_CONFIG_HOME: path.join(folder, "xdg"), XDG_CACHE_HOME: path.join(folder, "cache"), XDG_DATA_HOME: path.join(folder, "xdg-data") }, timeout: 45000 })
    const result = await app.evaluate(
        ({ nativeImage, app }, fixtures) => ({
            runtime: { version: app.getVersion(), versions: process.versions },
            fixtures: fixtures.map(({ name, bytes }) => {
                const image = nativeImage.createFromBuffer(Buffer.from(bytes)),
                    size = image.getSize(),
                    png = image.toPNG()
                let resized
                try {
                    if (size.width > 640 || size.height > 480) {
                        const scale = Math.min(640 / size.width, 480 / size.height),
                            target = { width: Math.round(size.width * scale), height: Math.round(size.height * scale) },
                            small = image.resize(target)
                        resized = { target, size: small.getSize(), jpeg: [...small.toJPEG(90)] }
                    }
                } catch (e) {
                    resized = { error: String(e) }
                }
                return { name, size, empty: image.isEmpty(), png: [...png], bitmap: [...image.toBitmap()], resized }
            })
        }),
        fixtures
    )
    for (const item of result.fixtures) {
        for (const field of ["png", "bitmap"]) {
            const bytes = Buffer.from(item[field])
            item[field] = { length: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex"), firstBytes: [...bytes.subarray(0, 16)] }
        }
        if (item.resized?.jpeg) {
            const bytes = Buffer.from(item.resized.jpeg)
            item.resized.jpeg = { length: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex"), firstBytes: [...bytes.subarray(0, 8)] }
        }
    }
    const target = process.env.FREESHOW_NATIVE_IMAGE_RECEIPT
    if (target) {
        fs.mkdirSync(path.dirname(target), { recursive: true })
        fs.writeFileSync(target, JSON.stringify(result, null, 2) + "\n")
    }
    console.log(JSON.stringify(result))
} finally {
    if (app) {
        await app.evaluate(({ app }) => app.exit(0)).catch(() => {})
        await app.close().catch(() => {})
    }
    fs.rmSync(folder, { recursive: true, force: true })
}
