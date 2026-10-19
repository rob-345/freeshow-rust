// The recorded original uses actual packaged nativeImage, not a JS JPEG decoder.
// The Rust candidate is a measurement example; production stays PNG-only.
import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { execFileSync, spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import { inflateSync } from "node:zlib"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import ts from "typescript"
import { parseFile } from "music-metadata"
import { apic, mp3, flac, picture, crc, png } from "../../crates/freeshow-cover-images/tools/fixtures.mjs"

const root = fileURLToPath(new URL("../../", import.meta.url))
const receipt = JSON.parse(fs.readFileSync(path.join(root, "crates/freeshow-cover-images/fixtures/jpeg/electron-linux.json")))
const candidate = process.env.FREESHOW_JPEG_CANDIDATE || path.join(root, "target/debug/examples/cover-jpeg-candidate" + (process.platform === "win32" ? ".exe" : ""))
const native = process.env.FREESHOW_COVER_IMAGES_PARITY || path.join(root, "target/debug/freeshow-cover-images-parity" + (process.platform === "win32" ? ".exe" : ""))
const hash = (b) => createHash("sha256").update(b).digest("hex")
function run(binary, data) {
    const result = spawnSync(binary, [], { input: JSON.stringify(data), encoding: "utf8", timeout: 20000, maxBuffer: 16 * 1024 * 1024 })
    assert.equal(result.status, 0, result.stderr || String(result.error))
    return JSON.parse(result.stdout)
}
function frame(bytes) {
    assert.deepEqual(bytes.subarray(0, 2), Buffer.from([255, 216]))
    const markers = []
    for (let p = 2; p < bytes.length; ) {
        assert.equal(bytes[p], 255)
        const marker = bytes[p + 1],
            length = bytes.readUInt16BE(p + 2)
        assert.ok(length >= 2 && p + length + 2 <= bytes.length)
        const data = bytes.subarray(p + 4, p + length + 2)
        markers.push({ marker, data })
        if (marker === 0xda) break
        p += length + 2
        assert.ok(markers.length <= 256)
    }
    const sof = markers.find((f) => f.marker === 0xc0)?.data
    assert.ok(sof, "controlled fixture must be baseline JPEG")
    return { size: { width: sof.readUInt16BE(3), height: sof.readUInt16BE(1) }, components: sof[5], markers }
}
function sourcePixels(f) {
    const bytes = inflateSync(Buffer.from(f.source.rgba.deflateBase64, "base64"), { maxOutputLength: 16 * 1024 * 1024 })
    assert.equal(bytes.length, f.source.rgba.length)
    assert.equal(hash(bytes), f.source.rgba.sha256)
    assert.equal(hash(bytes), f.source.png.rgbaSha256)
    return bytes
}
function decodedPng(bytes) {
    assert.ok(bytes.length <= 4 * 1024 * 1024)
    assert.deepEqual(bytes.subarray(0, 8), Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))
    const chunks = []
    let offset = 8,
        width,
        height,
        ended = false,
        count = 0
    while (offset < bytes.length) {
        assert.ok(++count <= 256 && offset + 12 <= bytes.length)
        const length = bytes.readUInt32BE(offset),
            end = offset + length + 12
        assert.ok(end <= bytes.length)
        const type = bytes.subarray(offset + 4, offset + 8),
            data = bytes.subarray(offset + 8, end - 4)
        assert.equal(crc(bytes.subarray(offset + 4, end - 4)), bytes.readUInt32BE(end - 4), "PNG CRC")
        if (type.toString() === "IHDR") {
            assert.equal(offset, 8)
            assert.equal(length, 13)
            width = data.readUInt32BE(0)
            height = data.readUInt32BE(4)
            assert.ok(width > 0 && width <= 4096 && height > 0 && height <= 4096 && width * height <= 4 * 1024 * 1024)
            assert.deepEqual(data.subarray(8), Buffer.from([8, 6, 0, 0, 0]))
        } else if (type.toString() === "IDAT") {
            assert.ok(width)
            chunks.push(data)
        } else if (type.toString() === "IEND") {
            assert.equal(length, 0)
            ended = true
        } else assert.fail(`Unexpected candidate PNG chunk ${type}`)
        offset = end
        if (ended) break
    }
    assert.ok(ended && offset === bytes.length && chunks.length > 0)
    const compressed = Buffer.concat(chunks),
        stride = width * 4,
        expected = (stride + 1) * height
    const inflated = inflateSync(compressed, { maxOutputLength: expected + 1, info: true })
    assert.equal(inflated.engine.bytesWritten, compressed.length, "PNG trailing compressed bytes")
    assert.equal(inflated.buffer.length, expected)
    const rgba = Buffer.alloc(width * height * 4)
    for (let y = 0; y < height; y++) {
        const filter = inflated.buffer[y * (stride + 1)]
        assert.ok(filter <= 4)
        for (let x = 0; x < stride; x++) {
            const p = y * stride + x,
                left = x < 4 ? 0 : rgba[p - 4],
                above = y === 0 ? 0 : rgba[p - stride],
                corner = x < 4 || y === 0 ? 0 : rgba[p - stride - 4]
            let prediction = 0
            if (filter === 1) prediction = left
            if (filter === 2) prediction = above
            if (filter === 3) prediction = Math.floor((left + above) / 2)
            if (filter === 4) {
                const sum = left + above - corner,
                    a = Math.abs(sum - left),
                    b = Math.abs(sum - above),
                    c = Math.abs(sum - corner)
                prediction = a <= b && a <= c ? left : b <= c ? above : corner
            }
            rgba[p] = (inflated.buffer[y * (stride + 1) + x + 1] + prediction) & 255
        }
    }
    return { width, height, rgba }
}
test("independent candidate PNG check enforces CRC, complete framing and exact raster", () => {
    const bytes = png(2, 1)
    assert.deepEqual(decodedPng(bytes), { width: 2, height: 1, rgba: Buffer.from([0, 0, 0, 255, 17, 3, 7, 255]) })
    const corrupt = Buffer.from(bytes)
    corrupt[corrupt.length - 1] ^= 1
    assert.throws(() => decodedPng(corrupt), /PNG CRC/)
    assert.throws(() => decodedPng(bytes.subarray(0, bytes.length - 1)))
    assert.throws(() => decodedPng(Buffer.concat([bytes, Buffer.from([0])])))
})
test("JPEG evidence pins immutable actual source declarations, actual packaged versions and measurement source", () => {
    assert.equal(receipt.schema, 1)
    assert.equal(receipt.sourceCommit, "2d94defcf98175e4f5c9504e7f4fd1d33c35c975")
    const source = execFileSync("git", ["show", `${receipt.sourceCommit}:src/electron/audio/nowPlaying.ts`], { cwd: root, encoding: "utf8", timeout: 10000 }).replace(/\r\n/g, "\n")
    assert.equal(hash(source), receipt.sourceSha256)
    const tree = ts.createSourceFile("source.ts", source, ts.ScriptTarget.Latest, true)
    const declarations = ["setPlayingState", "convertDynamicValues", "getArtist"].map((name) => tree.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === name).getText(tree))
    const dynamic = tree.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText(tree) === "dynamicValues")).getText(tree)
    assert.equal(hash(declarations.join("\n") + "\n" + dynamic), receipt.sourceDeclarationsSha256)
    assert.equal(receipt.runtime.version, "1.6.7-beta.2")
    assert.equal(receipt.runtime.packaged, true)
    assert.equal(receipt.runtime.versions.electron, "37.10.3")
    assert.equal(receipt.runtime.versions.node, "22.21.1")
    assert.equal(receipt.executableSha256, "eb3263aad96ce7c35df3db4516bb04c67bdc6c2f30ae8ebc0cb5649783c82a15")
    for (const [file, key] of [
        ["examples/cover-jpeg-candidate.rs", "candidateSourceSha256"],
        ["tools/jpeg-reference.mjs", "toolSourceSha256"]
    ])
        assert.equal(hash(fs.readFileSync(path.join(root, "crates/freeshow-cover-images", file), "utf8").replace(/\r\n/g, "\n")), receipt[key])
})
test("actual source JPEG decode and PNG write preserve opaque pixels, ordering, small originals and large resize semantics", () => {
    assert.equal(receipt.fixtures.length, 15)
    for (const f of receipt.fixtures) {
        const input = Buffer.from(f.input.base64, "base64")
        assert.equal(input.length, f.input.length)
        assert.equal(hash(input), f.input.sha256)
        const rgba = sourcePixels(f)
        assert.ok(
            rgba.every((b, i) => i % 4 !== 3 || b === 255),
            f.name
        )
        assert.equal(rgba.length, f.source.size.width * f.source.size.height * 4)
        assert.deepEqual(frame(input).size, f.source.size, f.name)
        assert.deepEqual(f.source.effects, ["title", "cover-path", "png-write", "cover-path", "cover-path", "text-write"])
        const cover = f.source.cover.original ? input : Buffer.from(f.source.cover.base64, "base64")
        assert.equal(hash(cover), f.source.cover.sha256)
        assert.equal(cover.length, f.source.cover.length)
        if (f.source.size.width > 640 || f.source.size.height > 480) {
            assert.equal(f.source.cover.original, false)
            assert.deepEqual(frame(cover).size, { width: 640, height: 480 })
        } else assert.equal(f.source.cover.original, true)
    }
    const named = (name) => receipt.fixtures.find((f) => f.name === name)
    for (const name of ["solid-17x11", "texture-9x5", "texture-17x11", "texture-64x48", "large-solid-641x481"]) {
        assert.equal(named(name).source.rgba.sha256, named(`${name}-no-icc`).source.rgba.sha256, "removing actual standard sRGB ICC does not explain decoder mismatch")
        assert.ok(frame(Buffer.from(named(name).input.base64, "base64")).markers.some((m) => m.marker === 0xe2))
        assert.ok(!frame(Buffer.from(named(`${name}-no-icc`).input.base64, "base64")).markers.some((m) => m.marker === 0xe2))
    }
    for (const orientation of [2, 6, 8]) {
        const f = named(`texture-exif-${orientation}`)
        const exif = frame(Buffer.from(f.input.base64, "base64")).markers.find((m) => m.marker === 0xe1).data
        assert.equal(exif.readUInt16LE(24), orientation)
        assert.equal(f.source.rgba.sha256, named("texture-17x11-no-icc").source.rgba.sha256, "this actual nativeImage buffer API ignores the checked EXIF orientation")
    }
    assert.equal(frame(Buffer.from(named("native-encoded-gray").input.base64, "base64")).components, 1)
})
test("pinned real Rust JPEG candidate has measured pixel mismatches without relaxing the exact oracle", () => {
    let mismatching = 0
    for (const f of receipt.fixtures) {
        const result = run(candidate, { base64: f.input.base64 })
        assert.deepEqual({ width: result.width, height: result.height }, f.source.size)
        const actual = Buffer.from(result.rgba, "base64"),
            expected = sourcePixels(f)
        assert.equal(actual.length, expected.length)
        assert.ok(actual.every((b, i) => i % 4 !== 3 || b === 255))
        const delta = { channels: 0, pixels: 0, max: 0, total: 0 }
        for (let p = 0; p < actual.length; p++) {
            const difference = Math.abs(actual[p] - expected[p])
            if (difference) delta.channels++
            delta.max = Math.max(delta.max, difference)
            delta.total += difference
        }
        for (let p = 0; p < actual.length; p += 4) if (!actual.subarray(p, p + 4).equals(expected.subarray(p, p + 4))) delta.pixels++
        if (delta.pixels) mismatching++
        if (process.platform === "linux" && process.arch === "x64") {
            assert.equal(hash(actual), f.candidate.rgbaSha256, f.name)
            assert.deepEqual(delta, f.candidate.delta, f.name)
        }
        const decoded = decodedPng(Buffer.from(result.png, "base64"))
        assert.deepEqual({ width: decoded.width, height: decoded.height }, f.source.size)
        assert.deepEqual(decoded.rgba, actual, "candidate PNG must preserve its actual decoded JPEG raster")
        assert.notEqual(hash(Buffer.from(result.png, "base64")), f.source.png.sha256, "compressed PNG equality is separate from decoded pixels")
    }
    assert.ok(mismatching >= 1, "ordinary JPEG candidate pixel mismatch remains observable")
})
test("actual cover service keeps JPEG unsupported under both policies, returns all leases and joins both workers", () => {
    const ops = receipt.fixtures.flatMap((f) => [false, true].map((alternate) => ({ op: "create", base64: f.input.base64, alternate })))
    const result = run(native, { ops })
    assert.equal(result.results.length, 30)
    for (const item of result.results) assert.equal(item.error?.code, "cover_unsupported")
    assert.equal(result.status.finished_workers, 2)
    assert.equal(result.status.reserved_bytes, 0)
})
test("real tagged MP3/FLAC parsing retains original JPEG bytes independently of unsupported image decoding", async (t) => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-cover-jpeg-"))
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }))
    const covers = ["texture-17x11-no-icc", "native-encoded-gray"].map((name) => Buffer.from(receipt.fixtures.find((f) => f.name === name).input.base64, "base64"))
    for (const [name, bytes] of [
        ["cover-音楽.mp3", mp3(covers.map((data) => apic(data, { mime: "image/jpeg" })))],
        ["cover.flac", flac(covers.map((data) => picture(data, { mime: "image/jpeg" })))]
    ]) {
        const file = path.join(directory, name)
        fs.writeFileSync(file, bytes)
        const original = (await parseFile(file)).common.picture
        assert.equal(original.length, 2)
        const result = run(native, { ops: [{ op: "covers", path: file }] })
        assert.equal(result.results[0].error, undefined)
        for (let i = 0; i < covers.length; i++) {
            assert.deepEqual(original[i].data, covers[i])
            const item = result.results[0].covers[i]
            assert.deepEqual(Buffer.from(item.data.base64, "base64"), covers[i])
            assert.equal(item.data.sha256, hash(original[i].data))
            assert.equal(item.fields.format, original[i].format)
        }
        assert.equal(result.status.finished_workers, 2)
        assert.equal(result.status.reserved_bytes, 0)
    }
})

test("separate installed consumer evidence distinguishes matching turbo defaults from different libjpeg9 mode results", () => {
    const measured = JSON.parse(fs.readFileSync(path.join(root, "crates/freeshow-cover-images/fixtures/jpeg/system-linux.json")))
    assert.equal(measured.sourceReceiptSha256, hash(fs.readFileSync(path.join(root, "crates/freeshow-cover-images/fixtures/jpeg/electron-linux.json"), "utf8").replace(/\r\n/g, "\n")))
    assert.equal(measured.probeSourceSha256, hash(fs.readFileSync(path.join(root, "crates/freeshow-cover-images/tools/jpeg-system-probe.py"), "utf8").replace(/\r\n/g, "\n")))
    assert.equal(measured.gdkPixbufVersion, "2.42.10")
    assert.ok(measured.loadedNativeLibraries.some((f) => f.path.endsWith("libjpeg.so.8.2.2") && f.sha256 === "ac09f2cd6f8a3b8e3cb123999ce275f9bcbe31c6cf9e20212c8f297772591d15"))
    for (const f of measured.consumerDefault.fixtures) {
        assert.equal(f.differentPixels, 0)
        assert.equal(f.differentChannels, 0)
        assert.equal(f.maximumDifference, 0)
        assert.equal(f.rgbaSha256, receipt.fixtures.find((original) => original.name === f.name).source.rgba.sha256)
    }
    assert.equal(measured.consumerDefault.fixtures.length, 15)
    assert.equal(measured.djpeg.available, true)
    assert.ok(measured.djpeg.linkedJpegLibraries.some((f) => f.path.endsWith("libjpeg.so.9.5.0")))
    assert.deepEqual(
        measured.djpeg.modes.map((mode) => [mode.name, mode.fixtures.filter((f) => f.differentPixels === 0).length]),
        [
            ["accurate-fancy", 6],
            ["accurate-no-smooth", 6],
            ["fast-fancy", 4],
            ["fast-no-smooth", 4]
        ]
    )
    assert.equal(measured.directAbi.probed, false)
    assert.equal(measured.productionDecoderEnabled, false)
})
