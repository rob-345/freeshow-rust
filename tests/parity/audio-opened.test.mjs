import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import ts from "typescript"
import { parseFile } from "music-metadata"

const root = fileURLToPath(new URL("../../", import.meta.url))
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const binary = process.env.FREESHOW_AUDIO_OPENED_BIN || path.join(root, "target/debug/freeshow-audio-opened-parity" + (process.platform === "win32" ? ".exe" : ""))
const source = spawnSync("git", ["show", `${revision}:src/electron/audio/nowPlaying.ts`], { cwd: root, encoding: "utf8", timeout: 10000 })
assert.equal(source.status, 0, source.stderr)
const ast = ts.createSourceFile("nowPlaying.ts", source.stdout, ts.ScriptTarget.Latest, true)
const declaration = ast.statements.find((s) => ts.isFunctionDeclaration(s) && s.name?.text === "getAudioMetadata").getText(ast)
const js = ts.transpileModule(declaration + "; return getAudioMetadata", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
const coverage = { realFiles: 0, rawSpecials: 0, rawUndefined: 0, heldReads: 0, sourceFailures: 0, unsupported: 0 }
test.after(() => console.log("Opened common metadata source coverage:", JSON.stringify(coverage)))

function raw(value, top = false) {
    if (value === undefined) {
        coverage.rawUndefined++
        return ["undefined"]
    }
    if (value === null) return ["null"]
    if (typeof value === "number") {
        if (!Number.isFinite(value)) {
            coverage.rawSpecials++
            return ["number", String(value)]
        }
        if (Object.is(value, -0)) {
            coverage.rawSpecials++
            return ["number", "-0"]
        }
        return ["number", value]
    }
    if (Array.isArray(value)) return ["array", value.map((v) => raw(v))]
    if (typeof value === "object")
        return [
            "object",
            Object.keys(value)
                .filter((k) => !(top && k === "picture"))
                .map((k) => {
                    return [k, raw(value[k])]
                })
        ]
    return [typeof value, value]
}
async function original(file) {
    const sent = []
    const fn = new Function("parseFile", "sendToMain", "ToMain", "console", js)(parseFile, (channel, data) => sent.push({ channel, data }), { AUDIO_METADATA: "AUDIO_METADATA" }, { error() {} })
    const common = await fn(file)
    if (common) {
        assert.equal(sent.length, 1)
        assert.equal(sent[0].channel, "AUDIO_METADATA")
        assert.equal(sent[0].data.filePath, file)
        assert.equal(sent[0].data.metadata, common, "actual source publishes this exact common object")
    } else {
        coverage.sourceFailures++
        assert.equal(sent.length, 0)
    }
    return common
}
function native(cases) {
    const r = spawnSync(binary, [], { cwd: root, input: JSON.stringify(cases), encoding: "utf8", timeout: 30000, maxBuffer: 16 * 1024 * 1024 })
    assert.equal(r.error, undefined)
    assert.equal(r.status, 0, r.stderr)
    return JSON.parse(r.stdout)
}
function equal(actual, expected, location = "common") {
    if (typeof expected === "number") {
        assert.equal(typeof actual, "number", location)
        assert.ok(Math.abs(actual - expected) <= 2e-14 * Math.max(1, Math.abs(expected)), `${location}: ${actual} != ${expected}`)
    } else if (expected && typeof expected === "object") {
        assert.deepEqual(Object.keys(actual), Object.keys(expected), location)
        for (const key of Object.keys(expected)) equal(actual[key], expected[key], `${location}.${key}`)
    } else assert.deepEqual(actual, expected, location)
}
function temporary(t) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-audio-opened-"))
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }))
    return (name, bytes) => {
        const p = path.join(directory, name)
        if (bytes !== undefined) fs.writeFileSync(p, bytes)
        return p
    }
}
const u32 = (n, little = false) => {
    const b = Buffer.alloc(4)
    little ? b.writeUInt32LE(n) : b.writeUInt32BE(n)
    return b
}
const safe = (n) => Buffer.from([(n >> 21) & 127, (n >> 14) & 127, (n >> 7) & 127, n & 127])
function wav(tags = []) {
    const chunk = (k, b) => Buffer.concat([Buffer.from(k), u32(b.length, true), b, ...(b.length % 2 ? [Buffer.alloc(1)] : [])])
    const fmt = Buffer.from([1, 0, 1, 0, 64, 31, 0, 0, 128, 62, 0, 0, 2, 0, 16, 0])
    const data = Buffer.concat([Buffer.from("WAVE"), chunk("fmt ", fmt), chunk("LIST", Buffer.concat([Buffer.from("INFO"), ...tags.map(([k, v]) => chunk(k, Buffer.from(v + "\0")))])), chunk("data", Buffer.alloc(160))])
    return Buffer.concat([Buffer.from("RIFF"), u32(data.length, true), data])
}
function mp3(items, pictures = false) {
    const frames = items.map(([key, value]) => [key, Buffer.concat([Buffer.from([3]), Buffer.from(value)])])
    if (pictures) frames.push(["APIC", Buffer.from([3, ...Buffer.from("image/png"), 0, 255, 0, 137, 80, 78, 71])])
    const tag = Buffer.concat(frames.map(([key, b]) => Buffer.concat([Buffer.from(key), safe(b.length), Buffer.alloc(2), b])))
    const frame = Buffer.alloc(417)
    frame.set([255, 251, 144, 100])
    return Buffer.concat([Buffer.from([73, 68, 51, 4, 0, 0]), safe(tag.length), tag, ...Array(8).fill(frame)])
}
function flac(items) {
    const info = Buffer.alloc(34)
    info.writeUInt16BE(4096)
    info.writeUInt16BE(4096, 2)
    info.writeBigUInt64BE((44100n << 44n) | (1n << 41n) | (15n << 36n) | 4410n, 10)
    const comments = Buffer.concat([
        u32(3, true),
        Buffer.from("src"),
        u32(items.length, true),
        ...items.map(([k, v]) => {
            const b = Buffer.from(`${k}=${v}`)
            return Buffer.concat([u32(b.length, true), b])
        })
    ])
    return Buffer.concat([
        Buffer.from("fLaC"),
        ...[
            [0, info],
            [132, comments]
        ].map(([k, b]) => {
            const h = Buffer.alloc(4)
            h[0] = k
            h.writeUIntBE(b.length, 1, 3)
            return Buffer.concat([h, b])
        })
    ])
}
async function compare(files) {
    const expected = []
    for (const file of files) {
        const common = await original(file)
        assert.ok(common, file)
        expected.push(raw(common, true))
        coverage.realFiles++
    }
    const replies = native(files.map((p) => ({ path: p })))
    replies.forEach((reply, i) => {
        const read = reply.passes[0]
        assert.equal(read.ok, true, `${files[i]}: ${JSON.stringify(read)}`)
        assert.equal(read.covers, "unknown")
        assert.equal(read.unchanged, true)
        equal(read.raw, expected[i], files[i])
    })
    return replies
}

test("actual source helper + real WAV common keeps invalid YEAR NaN and signed zero before JSON", async (t) => {
    const file = temporary(t)
    const files = [
        file("empty.wav", wav()),
        file(
            "tags.wav",
            wav([
                ["INAM", "Title"],
                ["IART", "Artist"],
                ["IPRD", "Album"]
            ])
        )
    ]
    for (const year of ["invalid", "0", "-0", "2025", "2025suffix", "Infinity", "-Infinity", "9".repeat(400)]) files.push(file(`year-${files.length}.wav`, wav([["YEAR", year]])))
    const replies = await compare(files)
    const bad = replies[2].passes[0]
    assert.equal(bad.projection.year, null)
    assert.deepEqual(bad.raw[1].find(([key]) => key === "year")[1], ["number", "NaN"])
})

test("actual MP3/FLAC parser values retain Unicode, ordering and source-supported special fields", async (t) => {
    const file = temporary(t)
    const unknownGenre = file("unknown-genre.mp3", mp3([["TCON", "9999"]]))
    const common = await original(unknownGenre)
    assert.equal(Object.hasOwn(common, "genre"), true)
    assert.equal(Object.hasOwn(common.genre, 0), true)
    assert.equal(common.genre[0], undefined, "actual parser retains an own undefined array entry")
    await compare([
        file(
            "曲 🎵.mp3",
            mp3([
                ["TIT2", "Café 音楽 🎵"],
                ["TPE1", "One\0Two\0One"],
                ["TRCK", "2/12"],
                ["TCON", "(17)(20)"],
                ["TXXX", "REPLAYGAIN_TRACK_GAIN\0bad"]
            ])
        ),
        file(
            "comments.flac",
            flac([
                ["TITLE", "Café 音楽 🎵"],
                ["ARTIST", "A"],
                ["ARTIST", "B"],
                ["BPM", "invalid"],
                ["REPLAYGAIN_TRACK_GAIN", "-Infinity dB"],
                ["RATING:source", "bad"],
                ["UNKNOWN_FIELD", "ignored"]
            ])
        ),
        unknownGenre
    ])
})

test("skip-covers result remains Unknown even when real source has own undefined picture type", async (t) => {
    const file = temporary(t)("cover.mp3", mp3([["TIT2", "Cover kept outside common parser"]], true))
    const common = await original(file)
    assert.equal(common.picture.length, 1)
    assert.equal(Object.hasOwn(common.picture[0], "type"), true)
    assert.equal(common.picture[0].type, undefined)
    assert.ok(ArrayBuffer.isView(common.picture[0].data))
    coverage.rawUndefined++
    const reply = (await compare([file]))[0].passes[0]
    assert.equal(reply.covers, "unknown")
    assert.equal(Object.hasOwn(reply.projection, "picture"), false, "absence in skip-covers projection is not certified absence in source")
})

test("opened descriptor reads original replaced identity and rewinds for sequential consumers", async (t) => {
    const file = temporary(t)
    const originalPath = file("音楽.wav", wav([["INAM", "Original held bytes"]]))
    const expected = raw(await original(originalPath), true)
    const replacement = file("replacement.wav", wav([["INAM", "Replacement bytes"]]))
    const result = native([{ path: originalPath, rename: file("held.wav"), replacement, passes: 3, source: "/not/opened/source.wav" }])[0]
    for (const read of result.passes) {
        assert.equal(read.ok, true)
        equal(read.raw, expected)
        assert.equal(read.unchanged, true)
        coverage.heldReads++
    }
    assert.equal((await parseFile(originalPath)).common.title, "Replacement bytes")
    assert.deepEqual(result.passes[0].observations, result.passes[2].observations)
})

test("actual source parse failures and explicit native hint/resource limitations remain distinct", async (t) => {
    const file = temporary(t)
    const corrupt = file("corrupt.wav", Buffer.from("not audio")),
        missing = file("missing.wav")
    assert.equal(await original(corrupt), null)
    assert.equal(await original(missing), null)
    const valid = file("real.wav", wav())
    const replies = native([{ path: corrupt }, { path: missing }, { path: valid, source: "misleading.mp3" }, { path: valid, source: "not-supported.aiff" }, { path: valid, source: "nonexistent.bin" }])
    assert.equal(replies[0].passes[0].error, "audio_input_rejected")
    assert.equal(replies[1].error, "audio_input_rejected")
    assert.equal(replies[2].passes[0].error, "audio_unsupported_variant")
    assert.equal(replies[3].passes[0].error, "audio_unsupported_variant")
    coverage.unsupported += 2
    assert.equal(replies[4].passes[0].ok, true)
})
