import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import { createHash } from "node:crypto"
import { test } from "node:test"
import { deflateSync } from "node:zlib"
import { parseFile } from "music-metadata"
import { png, apic, mp3, flac, picture, comments, wav, chunk, syncsafe, u32 } from "../../crates/freeshow-cover-images/tools/fixtures.mjs"
const root = fileURLToPath(new URL("../../", import.meta.url)),
    revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const binary = process.env.FREESHOW_COVER_IMAGES_PARITY || path.join(root, "target/debug/freeshow-cover-images-parity" + (process.platform === "win32" ? ".exe" : ""))
const counts = { sourceFiles: 0, pictures: 0, malformed: 0, unsupported: 0, undefinedTypes: 0 }
test.after(() => console.log("Cover/image source coverage:", JSON.stringify(counts)))
const hash = (b) => createHash("sha256").update(b).digest("hex")
function native(ops) {
    const r = spawnSync(binary, [], { cwd: root, input: JSON.stringify({ ops }), encoding: "utf8", timeout: 20000, maxBuffer: 16 * 1024 * 1024 })
    assert.equal(r.status, 0, r.stderr)
    const parsed = JSON.parse(r.stdout)
    assert.equal(parsed.status.finished_workers, 2)
    assert.equal(parsed.status.reserved_bytes, 0)
    return parsed.results
}
function temporary(t) {
    const folder = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-cover-images-"))
    t.after(() => fs.rmSync(folder, { recursive: true, force: true }))
    return folder
}
async function compare(folder, name, bytes) {
    const file = path.join(folder, name)
    fs.writeFileSync(file, bytes)
    const metadata = await parseFile(file)
    const actual = native([{ op: "covers", path: file }])[0]
    assert.equal(actual.error, undefined, JSON.stringify(actual))
    const expected = (metadata.common.picture || []).map(({ data, ...fields }) => {
        const undefinedFields = Object.keys(fields).filter((k) => fields[k] === undefined)
        for (const k of undefinedFields) delete fields[k]
        return { fields, undefinedFields, data: { base64: data.toString("base64"), length: data.length, sha256: hash(data) } }
    })
    assert.deepEqual(actual.covers, expected, name)
    counts.sourceFiles++
    counts.pictures += expected.length
    for (const item of metadata.common.picture || []) if (Object.hasOwn(item, "type") && item.type === undefined) counts.undefinedTypes++
    return metadata
}
test("immutable source actually requests music-metadata covers and uses only first picture data", () => {
    const original = spawnSync("git", ["show", `${revision}:src/electron/audio/nowPlaying.ts`], { cwd: root, encoding: "utf8", timeout: 10000 })
    assert.equal(original.status, 0, original.stderr)
    assert.match(original.stdout, /await parseFile\(filePath\)/)
    assert.match(original.stdout, /metadata\?\.picture\?\.\[0\]/)
    assert.match(original.stdout, /cover\?\.data/)
    assert.equal(JSON.parse(fs.readFileSync(path.join(root, "node_modules/music-metadata/package.json"))).version, "7.14.0")
})
test("real parser ID3v2.3/2.4 APIC order, duplicates, encodings, MIME and own undefined kind", async (t) => {
    const folder = temporary(t),
        cover = png(2, 1)
    for (const version of [3, 4]) for (const encoding of [0, 1, 2, 3]) await compare(folder, `v${version}-encoding${encoding}-é音楽.mp3`, mp3([apic(cover, { description: encoding === 0 ? "Café" : "音楽 🎵", encoding })], version))
    await compare(folder, "ordered.mp3", mp3([apic(cover), apic(png(1, 1), { type: 4, mime: "PNG", description: "Back" }), apic(cover), apic(Buffer.from([1, 2, 3]), { type: 255, mime: "IMAGE/JPG" })]))
    await compare(folder, "empty-picture.mp3", mp3([apic(Buffer.alloc(0)), apic(cover, { mime: "jpg" })]))
    await compare(folder, "no-cover.mp3", mp3([], 4, [["TIT2", Buffer.from([3, 65])]]))
})
test("real FLAC PICTURE includes extra numeric fields, Unicode, MIME, picture order and no-cover", async (t) => {
    const folder = temporary(t),
        cover = png(3, 2)
    await compare(folder, "cover音楽.flac", flac([picture(cover), picture(Buffer.from([1, 2, 3]), { mime: "image/jpg", type: 255, width: 0xffffffff, height: 0, indexedColor: 42 }), picture(cover, { type: 4, description: "Back" })]))
    await compare(folder, "empty.flac", flac([picture(Buffer.alloc(0))]))
    await compare(folder, "no-cover.flac", flac([]))
    await compare(folder, "ordinary.wav", wav())
})
test("source-supported variants outside bounded extraction are explicitly unsupported", async (t) => {
    const folder = temporary(t)
    const cases = [
        ["empty-mime.mp3", mp3([apic(png(1, 1), { mime: "" })])],
        ["embedded.flac", flac([], [[4, comments([["METADATA_BLOCK_PICTURE", picture(png(1, 1)).toString("base64")]])]])],
        ["id3v1.mp3", Buffer.concat([mp3([]), Buffer.from("TAG"), Buffer.alloc(125)])],
        ["ape.mp3", Buffer.concat([mp3([]), Buffer.from("APETAGEX"), Buffer.alloc(24)])]
    ]
    for (const [name, bytes] of cases) {
        const file = path.join(folder, name)
        fs.writeFileSync(file, bytes)
        await parseFile(file)
        const r = native([{ op: "covers", path: file }])[0]
        assert.equal(r.error?.code, "cover_unsupported", name)
        counts.unsupported++
    }
})
test("adversarial count/offset/truncation/input bounds never become certified empty covers", (t) => {
    const folder = temporary(t)
    const huge = Buffer.concat([Buffer.from([73, 68, 51, 4, 0, 0]), syncsafe(16 * 1024 * 1024), Buffer.alloc(20)]),
        hugeFrame = Buffer.concat([Buffer.from([73, 68, 51, 3, 0, 0]), syncsafe(10), Buffer.from("APIC"), u32(0xffffffff), Buffer.alloc(2), Buffer.from([255, 251, 144])])
    const many = mp3(Array.from({ length: 9 }, () => apic(png(1, 1))))
    const frames = mp3(
        [],
        4,
        Array.from({ length: 257 }, () => ["TIT2", Buffer.from([3, 65])])
    )
    const hugePicture = flac([Buffer.concat([u32(3), u32(0xffffffff)])])
    for (const [i, bytes] of [huge, hugeFrame, many, frames, hugePicture, flac([picture(png(1, 1))]).subarray(0, 45), Buffer.from("?"), Buffer.alloc(30)].entries()) {
        const file = path.join(folder, `${i}.mp3`)
        fs.writeFileSync(file, bytes)
        const r = native([{ op: "covers", path: file }])[0]
        assert.ok(r.error, JSON.stringify(r))
        counts.malformed++
    }
    assert.equal(native([{ op: "covers", path: path.join(folder, "missing.mp3") }])[0].error.code, "cover_io")
})
test("PNG capability is strict before decode, real PNG output and alternate JPEG90 are explicit", () => {
    const original = png(2, 1, { alpha: true })
    const out = native([{ op: "create", base64: original.toString("base64") }, { op: "png" }, { op: "size" }, { op: "release" }])
    assert.deepEqual(out[0], { width: 2, height: 1 })
    assert.equal(out[1].bytes.base64.slice(0, 11), "iVBORw0KGgo")
    assert.deepEqual(out[1].diagnostics, ["native_png_encoding_differs"])
    assert.deepEqual(out[2], out[0])
    assert.deepEqual(out[3], { released: true })
    for (const data of [png(1, 1, { partial: true }), png(1, 1, { extra: [chunk("gAMA", u32(45455))] })]) {
        assert.equal(native([{ op: "create", base64: data.toString("base64") }])[0].error.code, "cover_unsupported")
        counts.unsupported++
    }
    const bomb = png(1, 1)
    bomb.writeUInt32BE(0xffffffff, 16)
    assert.equal(native([{ op: "create", base64: bomb.toString("base64") }])[0].error.code, "cover_resource_limit")
    const resized = native([{ op: "create", base64: png(641, 481).toString("base64"), alternate: true }, { op: "jpeg" }])
    assert.equal(Buffer.from(resized[1].bytes.base64, "base64").subarray(0, 2).toString("hex"), "ffd8")
    assert.deepEqual(resized[1].diagnostics, ["native_lanczos3_jpeg90_differs"])
    const strict = native([{ op: "create", base64: png(641, 1).toString("base64") }, { op: "jpeg" }])
    assert.equal(strict[1].error.code, "cover_unsupported")
    const skinny = native([{ op: "create", base64: png(2000, 1).toString("base64") }, { op: "jpeg" }])
    assert.equal(skinny[1].bytes.length, 0)
})

test("all 21 actual picture type labels, Latin1 bytes and bare MPEG no-cover", async (t) => {
    const folder = temporary(t),
        cover = png(1, 1)
    for (let start = 0; start < 21; start += 7) await compare(folder, `types-${start}.mp3`, mp3(Array.from({ length: 7 }, (_, i) => apic(cover, { type: start + i, encoding: 0, description: "\u0080\u009fCafé", mime: i % 2 ? "IMAGE/JPEG" : "image/png" }))))
    await compare(folder, "bare.mp3", mp3([]).subarray(10))
})
test("actual permission error is explicit, source parser failure remains separate", async (t) => {
    if (process.platform === "win32" || process.getuid?.() === 0) return t.skip("Requires unprivileged POSIX file permissions")
    const folder = temporary(t),
        file = path.join(folder, "denied.mp3")
    fs.writeFileSync(file, mp3([apic(png(1, 1))]))
    fs.chmodSync(file, 0)
    try {
        await assert.rejects(parseFile(file), { code: "EACCES" })
        assert.equal(native([{ op: "covers", path: file }])[0].error.code, "cover_io")
    } finally {
        fs.chmodSync(file, 0o600)
    }
})
test("PNG excess inflate, truncated streams, CRC and chunk floods are bounded errors", () => {
    const base = png(1, 1),
        signature = base.subarray(0, 8),
        ihdr = base.subarray(8, 33),
        end = base.subarray(base.length - 12)
    const excess = Buffer.concat([signature, ihdr, chunk("IDAT", deflateSync(Buffer.alloc(1024 * 1024))), end])
    const truncated = base.subarray(0, base.length - 1),
        crc = Buffer.from(base)
    crc[crc.length - 1] ^= 1
    const flood = Buffer.concat([signature, ihdr, ...Array.from({ length: 257 }, () => chunk("IDAT", Buffer.alloc(0))), end])
    for (const bytes of [excess, truncated, crc, flood]) {
        const result = native([{ op: "create", base64: bytes.toString("base64") }])[0]
        assert.ok(result.error, JSON.stringify(result))
        counts.malformed++
    }
})

test("real trailing APE cover priority on MPEG/FLAC and APE+ID3v1 is refused", async (t) => {
    const folder = temporary(t),
        value = Buffer.concat([Buffer.from("cover.png\0"), png(1, 1)]),
        key = Buffer.from("Cover Art (Front)\0")
    const item = Buffer.concat([u32(value.length, true), u32(2, true), key, value]),
        footer = Buffer.concat([Buffer.from("APETAGEX"), u32(2000, true), u32(item.length + 32, true), u32(1, true), Buffer.alloc(12)]),
        ape = Buffer.concat([item, footer])
    for (const [name, prefix, tail] of [
        ["mpeg-ape.mp3", mp3([]).subarray(10), Buffer.alloc(0)],
        ["flac-ape.flac", flac([]), Buffer.alloc(0)],
        ["mpeg-ape-id3v1.mp3", mp3([]).subarray(10), Buffer.concat([Buffer.from("TAG"), Buffer.alloc(125)])]
    ]) {
        const file = path.join(folder, name)
        fs.writeFileSync(file, Buffer.concat([prefix, ape, tail]))
        const source = await parseFile(file)
        assert.ok(source.common.picture?.length, name)
        assert.equal(native([{ op: "covers", path: file }])[0].error.code, "cover_unsupported", name)
        counts.unsupported++
    }
})
