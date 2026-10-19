// Explicit packaged oracle, not a production decoder or automatic Electron test.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { createHash } from "node:crypto"
import { execFileSync, spawnSync } from "node:child_process"
import { deflateSync } from "node:zlib"
import { fileURLToPath } from "node:url"
import ts from "typescript"
import { withElectron } from "../../freeshow-files/tools/electron.mjs"
import { png } from "./fixtures.mjs"

const root = fileURLToPath(new URL("../../../", import.meta.url))
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const hash = (b) => createHash("sha256").update(b).digest("hex")
const source = execFileSync("git", ["show", `${revision}:src/electron/audio/nowPlaying.ts`], { cwd: root, encoding: "utf8", timeout: 10000 }).replace(/\r\n/g, "\n")
const tree = ts.createSourceFile("nowPlaying.ts", source, ts.ScriptTarget.Latest, true)
const names = ["setPlayingState", "convertDynamicValues", "getArtist"]
const declarations = names.map((name) => tree.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === name).getText(tree))
const dynamic = tree.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText(tree) === "dynamicValues")).getText(tree)
const javascript = ts.transpileModule([...declarations.map((s) => s.replace(/^export\s+/, "")), dynamic].join("\n"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
const binary = process.env.FREESHOW_JPEG_CANDIDATE || path.join(root, "target/debug/examples/cover-jpeg-candidate" + (process.platform === "win32" ? ".exe" : ""))
function candidate(input) {
    const result = spawnSync(binary, [], { input: JSON.stringify(input), encoding: "utf8", timeout: 20000, maxBuffer: 16 * 1024 * 1024 })
    assert.equal(result.status, 0, result.stderr)
    return JSON.parse(result.stdout)
}
function stripIcc(bytes) {
    const parts = [bytes.subarray(0, 2)]
    for (let p = 2; p < bytes.length; ) {
        assert.equal(bytes[p], 255)
        const marker = bytes[p + 1]
        if (marker === 0xda) {
            parts.push(bytes.subarray(p))
            break
        }
        const end = p + 2 + bytes.readUInt16BE(p + 2)
        if (marker !== 0xe2 || bytes.subarray(p + 4, p + 16).toString("latin1") !== "ICC_PROFILE\0") parts.push(bytes.subarray(p, end))
        p = end
    }
    return Buffer.concat(parts)
}
function withOrientation(bytes, orientation) {
    // Minimal little-endian TIFF IFD0 orientation, no compressed metadata.
    const payload = Buffer.from("45786966000049492a0008000000010012010300010000000000000000000000", "hex")
    payload.writeUInt16LE(orientation, 24)
    const header = Buffer.alloc(4)
    header.set([255, 225])
    header.writeUInt16BE(payload.length + 2, 2)
    return Buffer.concat([bytes.subarray(0, 2), header, payload, bytes.subarray(2)])
}
const generated = [
    { name: "solid-17x11", w: 17, h: 11, solid: true },
    { name: "texture-9x5", w: 9, h: 5 },
    { name: "texture-17x11", w: 17, h: 11 },
    { name: "texture-64x48", w: 64, h: 48 },
    { name: "large-solid-641x481", w: 641, h: 481, solid: true }
].map((f) => ({ name: f.name, base64: png(f.w, f.h, { solid: f.solid }).toString("base64") }))
const receipt = await withElectron(async (app, runtime, executablePath) => {
    const encoded = await app.evaluate(({ nativeImage }, images) => images.map((f) => ({ name: f.name, base64: nativeImage.createFromBuffer(Buffer.from(f.base64, "base64")).toJPEG(90).toString("base64") })), generated)
    const inputs = encoded.flatMap((f) => [f, { name: `${f.name}-no-icc`, base64: stripIcc(Buffer.from(f.base64, "base64")).toString("base64") }])
    const untagged = Buffer.from(inputs.find((f) => f.name === "texture-17x11-no-icc").base64, "base64")
    for (const orientation of [2, 6, 8]) inputs.push({ name: `texture-exif-${orientation}`, base64: withOrientation(untagged, orientation).toString("base64") })
    for (const gray of [true, false]) inputs.push({ name: gray ? "native-encoded-gray" : "native-encoded-rgb", ...candidate({ op: "generate", gray }) })
    const originals = await app.evaluate(
        async ({ nativeImage }, { inputs, javascript }) => {
            const result = []
            for (const input of inputs) {
                const jpeg = Buffer.from(input.base64, "base64")
                const effects = []
                let written, text
                const run = new Function("nativeImage", "getAudioMetadata", "IcecastSender", "getNowPlayingMediaPath", "doesPathExist", "deleteFile", "writeFile", "getNowPlayingTextPath", javascript + ";return setPlayingState")(
                    nativeImage,
                    async () => ({ picture: [{ data: jpeg }], title: "Title", artist: "Artist" }),
                    { updateMetadata: () => effects.push("title") },
                    () => {
                        effects.push("cover-path")
                        return "/fixture/Audio/NowPlayingCover.png"
                    },
                    () => false,
                    () => effects.push("delete"),
                    (file, data) => {
                        effects.push(file.endsWith(".png") ? "png-write" : "text-write")
                        if (file.endsWith(".png")) written = data
                        else text = data
                    },
                    () => "/fixture/Audio/NowPlaying.txt"
                )
                await run({ filePath: "/fixture/track.mp3", name: "Track", unknownLang: [], duration: 1, format: "{artwork_base64}" })
                const image = nativeImage.createFromBuffer(jpeg)
                const bitmap = image.toBitmap()
                const pngBitmap = nativeImage.createFromBuffer(written).toBitmap()
                const rgba = Buffer.from(bitmap)
                const pngRgba = Buffer.from(pngBitmap)
                for (let p = 0; p < rgba.length; p += 4) {
                    rgba[p] = bitmap[p + 2]
                    rgba[p + 2] = bitmap[p]
                    pngRgba[p] = pngBitmap[p + 2]
                    pngRgba[p + 2] = pngBitmap[p]
                }
                result.push({ ...input, size: image.getSize(), rgba: rgba.toString("base64"), pngRgba: pngRgba.toString("base64"), png: written.toString("base64"), text, effects })
            }
            return result
        },
        { inputs, javascript }
    )
    const fixtures = originals.map((f) => {
        const actual = candidate({ base64: f.base64 })
        const rgba = Buffer.from(actual.rgba, "base64")
        const sourceRgba = Buffer.from(f.rgba, "base64")
        assert.equal(sourceRgba.length, rgba.length)
        assert.ok(sourceRgba.every((b, i) => i % 4 !== 3 || b === 255))
        assert.equal(hash(sourceRgba), hash(Buffer.from(f.pngRgba, "base64")))
        let channels = 0,
            pixels = 0,
            max = 0,
            total = 0
        for (let i = 0; i < rgba.length; i++) {
            const difference = Math.abs(rgba[i] - sourceRgba[i])
            if (difference) channels++
            max = Math.max(max, difference)
            total += difference
        }
        for (let i = 0; i < rgba.length; i += 4) if (!rgba.subarray(i, i + 4).equals(sourceRgba.subarray(i, i + 4))) pixels++
        const input = Buffer.from(f.base64, "base64")
        const cover = Buffer.from(f.text.slice("data:image/jpeg;base64,".length), "base64")
        assert.ok(f.text.startsWith("data:image/jpeg;base64,"))
        return {
            name: f.name,
            input: { base64: f.base64, length: input.length, sha256: hash(input) },
            source: { size: f.size, rgba: { deflateBase64: deflateSync(sourceRgba).toString("base64"), length: sourceRgba.length, sha256: hash(sourceRgba) }, png: { length: Buffer.from(f.png, "base64").length, sha256: hash(Buffer.from(f.png, "base64")), rgbaSha256: hash(Buffer.from(f.pngRgba, "base64")) }, cover: { length: cover.length, sha256: hash(cover), original: cover.equals(input), ...(cover.equals(input) ? {} : { base64: cover.toString("base64") }) }, effects: f.effects },
            candidate: { size: { width: actual.width, height: actual.height }, rgbaSha256: hash(rgba), pngSha256: hash(Buffer.from(actual.png, "base64")), delta: { channels, pixels, max, total } }
        }
    })
    return {
        schema: 1,
        sourceCommit: revision,
        sourceSha256: hash(source),
        sourceDeclarationsSha256: hash(declarations.join("\n") + "\n" + dynamic),
        candidateSourceSha256: hash(fs.readFileSync(new URL("../examples/cover-jpeg-candidate.rs", import.meta.url), "utf8").replace(/\r\n/g, "\n")),
        toolSourceSha256: hash(fs.readFileSync(fileURLToPath(import.meta.url), "utf8").replace(/\r\n/g, "\n")),
        executableSha256: hash(fs.readFileSync(executablePath)),
        runtime: { version: runtime.version, packaged: runtime.packaged, versions: runtime.versions },
        platform: process.platform,
        fixtures
    }
})
if (process.env.FREESHOW_JPEG_RECEIPT) fs.writeFileSync(process.env.FREESHOW_JPEG_RECEIPT, JSON.stringify(receipt, null, 2) + "\n")
console.log(JSON.stringify({ fixtures: receipt.fixtures.length, mismatchingPixels: receipt.fixtures.filter((f) => f.candidate.delta.pixels > 0).map((f) => ({ name: f.name, ...f.candidate.delta })), unsupportedProduction: true }))
