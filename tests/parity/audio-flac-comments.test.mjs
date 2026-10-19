import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import { parseFile } from "music-metadata"
import { root, upstreamFunctions, clean } from "./oracle.mjs"
import { mp3, flac, comments, u32 } from "../../crates/freeshow-cover-images/tools/fixtures.mjs"
import { writeFixtures } from "../desktop/jpeg_now_playing_oracle.mjs"

const executable = (name) => path.join(root, "target/debug", name + (process.platform === "win32" ? ".exe" : "")),
    binary = process.env.FREESHOW_AUDIO_BIN || executable("freeshow-audio"),
    openedBinary = process.env.FREESHOW_AUDIO_OPENED_BIN || executable("freeshow-audio-opened-parity"),
    hash = (bytes) => createHash("sha256").update(bytes).digest("hex"),
    publicOriginal = upstreamFunctions("src/electron/audio/audio.ts", ["getAudioMetadata"], { parseFile }).getAudioMetadata,
    coverage = { files: 0, openedPasses: 0, publicReplies: 0, sourceFailures: 0, guardedInputs: 0, unsupported: 0, sourceAcceptedGuarded: 0 }
test.after(() => console.log("FLAC physical comment source coverage:", JSON.stringify(coverage)))
function temporary(t) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-flac-comments-"))
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }))
    return {
        directory,
        file: (name, bytes) => {
            const file = path.join(directory, name)
            fs.writeFileSync(file, bytes)
            return file
        }
    }
}
function raw(value, top = false) {
    if (value === undefined) return ["undefined"]
    if (value === null) return ["null"]
    if (typeof value === "number") return ["number", Object.is(value, -0) ? "-0" : Number.isFinite(value) ? value : String(value)]
    if (Array.isArray(value)) return ["array", value.map((v) => raw(v))]
    if (typeof value === "object")
        return [
            "object",
            Object.keys(value)
                .filter((key) => !(top && key === "picture"))
                .map((key) => [key, raw(value[key])])
        ]
    return [typeof value, value]
}
function run(binary, input) {
    const result = spawnSync(binary, [], { input, encoding: "utf8", timeout: 30000, maxBuffer: 16 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return result.stdout.trim()
}
function opened(cases) {
    return JSON.parse(run(openedBinary, JSON.stringify(cases)))
}
function publicRead(files) {
    return run(binary, files.map((filePath) => JSON.stringify({ command: "READ_AUDIO_METADATA", data: { filePath } })).join("\n") + "\n")
        .split("\n")
        .map(JSON.parse)
}
async function source(file) {
    const sent = [],
        original = upstreamFunctions("src/electron/audio/nowPlaying.ts", ["getAudioMetadata"], { parseFile, sendToMain: (channel, data) => sent.push({ channel, data }), ToMain: { AUDIO_METADATA: "AUDIO_METADATA" } }).getAudioMetadata,
        common = await original(file)
    assert.ok(common)
    assert.equal(sent.length, 1)
    assert.equal(sent[0].channel, "AUDIO_METADATA")
    assert.equal(sent[0].data.filePath, file)
    assert.equal(sent[0].data.metadata, common)
    return common
}
async function compare(files) {
    const before = files.map((file) => hash(fs.readFileSync(file))),
        expected = []
    for (const file of files) expected.push({ common: await source(file), projected: clean(await publicOriginal(file)) })
    const native = opened(files.map((file) => ({ path: file, passes: 2 }))),
        projected = publicRead(files)
    for (let i = 0; i < files.length; i++) {
        for (const answer of native[i].passes) {
            assert.equal(answer.ok, true, files[i] + ": " + JSON.stringify(answer))
            assert.deepEqual(answer.raw, raw(expected[i].common, true), files[i] + " raw/order/special values")
            assert.equal(answer.covers, "unknown")
            assert.equal(answer.unchanged, true)
            assert.deepEqual(answer.observations.before, answer.observations.after)
            coverage.openedPasses++
        }
        assert.equal(projected[i].ok, true)
        // Raw common above checks signed zero; this checks the separate lossy
        // JSON projection, not byte-for-byte zero spelling in the CLI stream.
        assert.deepEqual(clean(projected[i].value), expected[i].projected)
        assert.deepEqual(projected[i].diagnostics, [])
        assert.equal(hash(fs.readFileSync(files[i])), before[i], "Private view cannot mutate the original file")
        coverage.publicReplies++
        coverage.files++
    }
    return expected.map((item) => item.common)
}
// Preserve the source-only STREAMINFO bytes, replace only private fixture blocks.
function container(blocks) {
    return Buffer.concat([
        flac([]).subarray(0, 42),
        ...blocks.map(([type, bytes], i) => {
            const h = Buffer.alloc(4)
            h[0] = type | (i === blocks.length - 1 ? 128 : 0)
            h.writeUIntBE(bytes.length, 1, 3)
            return Buffer.concat([h, bytes])
        })
    ])
}
const comment = (entries) => [4, comments(entries)]
function id3Prefix(version, fields) {
    const bytes = mp3(
            [],
            version,
            fields.map(([k, v]) => [k, Buffer.concat([Buffer.from([3]), Buffer.from(v)])])
        ),
        length = bytes.subarray(6, 10).reduce((n, b) => (n << 7) | b, 0)
    return bytes.subarray(0, 10 + length)
}
test("unchanged packaged JPEG FLAC retains the earlier TITLE block before cover decoding", async (t) => {
    const generated = await writeFixtures(temporary(t).directory),
        entry = generated.cases.find((item) => item.id === "jpeg-flac"),
        recording = JSON.parse(fs.readFileSync(path.join(root, "tests/desktop/jpeg_now_playing_original.json")))
    assert.ok(entry, "unchanged source JPEG fixture")
    const [common] = await compare([entry.path]),
        recorded = recording.cases.find((item) => item.id === entry.id).metadataRaw
    assert.deepEqual(raw(common, true), [recorded[0], recorded[1].filter(([key]) => key !== "picture")])
    assert.equal(common.title, "real container")
    assert.equal(common.album, "FLAC JPEG album")
})
test("physical block and item order preserves duplicates, arrays, unknowns and intervening blocks", async (t) => {
    const f = temporary(t),
        files = [
            f.file("two.flac", container([comment([["TITLE", "earlier"]]), comment([["ALBUM", "later"]])])),
            f.file(
                "interleaved.flac",
                container([
                    comment([
                        ["title", "early"],
                        ["ARTIST", "α"],
                        ["GENRE", "Rock"],
                        ["DATE", "invalid"],
                        ["UNKNOWN", "ignored"]
                    ]),
                    [1, Buffer.alloc(17)],
                    comment([
                        ["TITLE", "late"],
                        ["ARTIST", "β"],
                        ["GENRE", "Jazz"],
                        ["ALBUM", "album"]
                    ]),
                    [2, Buffer.from("src! opaque")],
                    comment([
                        ["ARTIST", "α"],
                        ["DATE", "1999"],
                        ["ARTIST", "γ"],
                        ["TRACKNUMBER", "3/12"],
                        ["TRACKTOTAL", "14"]
                    ])
                ])
            ),
            f.file("empty-between.flac", container([comment([]), comment([["TITLE", "kept"]]), comment([])]))
        ],
        expected = await compare(files)
    assert.equal(expected[0].title, "earlier")
    assert.deepEqual(expected[1].artists, ["α", "β", "γ"])
    assert.equal(expected[1].title, "late")
    assert.deepEqual(expected[1].genre, ["Rock", "Jazz"])
    assert.equal(expected[1].date, "1999")
    assert.equal(expected[1].year, 1999)
    assert.deepEqual(expected[1].track, { no: 3, of: 14 })
})
test("leading ID3 ordering and original Vorbis priority survive multiple comment blocks", async (t) => {
    const f = temporary(t),
        files = []
    for (const version of [3, 4])
        files.push(
            f.file(
                `id3-${version}.flac`,
                Buffer.concat([
                    id3Prefix(version, [
                        ["TIT2", "ID3 title"],
                        ["TPE1", "ID3 artist"],
                        ["TALB", "ID3 album"],
                        ["TDRC", "invalid"],
                        ["TCOM", "ID3 composer"]
                    ]),
                    container([
                        comment([
                            ["ARTIST", "Vorbis A"],
                            ["TITLE", "Vorbis title"],
                            ["COMPOSER", "Vorbis composer"]
                        ]),
                        comment([
                            ["ARTIST", "Vorbis B"],
                            ["ALBUM", "Vorbis album"],
                            ["DATE", "2024-03"]
                        ])
                    ])
                ])
            )
        )
    const expected = await compare(files)
    for (const common of expected) {
        assert.equal(common.title, "Vorbis title")
        assert.equal(common.album, "Vorbis album")
        assert.equal(common.date, "2024-03")
        assert.equal(common.year, 2024)
        assert.deepEqual(common.composer, ["Vorbis composer"])
    }
})
test("ASCII keys retain exact Unicode/NUL/equal text and source special numeric values", async (t) => {
    const f = temporary(t),
        files = [
            f.file(
                "text.flac",
                container([
                    comment([
                        ["TITLE", "  Café 音楽 🎵 =\0 exact  "],
                        ["ARTIST", "A\0B"],
                        ["ALBUM", ""]
                    ]),
                    comment([
                        ["BPM", "invalid"],
                        ["ORIGINALYEAR", "-0"],
                        ["REPLAYGAIN_TRACK_GAIN", "-Infinity dB"],
                        ["REPLAYGAIN_TRACK_PEAK", "Infinity"],
                        ["RATING:source", "bad"]
                    ])
                ])
            ),
            f.file("trailing.flac", container([[4, Buffer.concat([comments([["TITLE", "kept"]]), Buffer.from("ignored trailing bytes")])]])),
            f.file(
                "many-blocks.flac",
                container(
                    Array.from({ length: 32 }, (_, i) =>
                        comment([
                            ["ARTIST", `artist${i}`],
                            ["TITLE", `title${i}`]
                        ])
                    )
                )
            )
        ]
    const expected = await compare(files)
    assert.equal(expected[0].title, "  Café 音楽 🎵 =\0 exact  ")
    assert.ok(Number.isNaN(expected[0].bpm))
    assert.ok(Object.is(expected[0].originalyear, -0))
    assert.equal(expected[2].artists.length, 32)
    assert.equal(expected[2].title, "title31")
})
test("malformed/huge declarations fail and source-accepted resource/encoding gaps are explicit", async (t) => {
    const f = temporary(t),
        bad = [
            ["vendor.flac", Buffer.concat([u32(0xffffffff, true), Buffer.alloc(4)]), "audio_resource_limit"],
            ["count.flac", Buffer.concat([u32(0, true), u32(0xffffffff, true)]), "audio_resource_limit"],
            ["field.flac", Buffer.concat([u32(0, true), u32(1, true), u32(0xffffffff, true)]), "audio_resource_limit"]
        ]
    const truncated = container([comment([["TITLE", "early"]]), comment([["ALBUM", "late"]])]).subarray(0, -2)
    const files = bad.map(([name, body]) => f.file(name, container([[4, body]])))
    files.push(f.file("truncated.flac", truncated))
    const answers = opened(files.map((file) => ({ path: file })))
    for (let i = 0; i < files.length; i++) {
        if (i >= 2) {
            const actual = (await parseFile(files[i])).common
            assert.ok(actual, "Pinned source accepts a clamped field length or discards a truncated last block")
            if (i === 3) {
                assert.equal(actual.title, "early")
                assert.equal(Object.hasOwn(actual, "album"), false)
            }
            coverage.sourceAcceptedGuarded++
        } else {
            await assert.rejects(parseFile(files[i]))
            coverage.sourceFailures++
        }
        assert.equal(answers[i].passes[0].error, i < bad.length ? bad[i][2] : "audio_input_rejected")
        coverage.guardedInputs++
    }
    const malformed = (raw) => Buffer.concat([u32(0, true), u32(1, true), u32(raw.length, true), raw]),
        gaps = [f.file("separator.flac", container([[4, malformed(Buffer.from("TITLE"))]])), f.file("key.flac", container([comment([["TÍTLE", "source ignored"]])])), f.file("invalid-utf8.flac", container([[4, malformed(Buffer.from([84, 73, 84, 76, 69, 61, 255]))]]))]
    for (const file of gaps) {
        assert.ok((await parseFile(file)).common)
        assert.equal(opened([{ path: file }])[0].passes[0].error, "audio_unsupported_variant")
        coverage.unsupported++
    }
    const over = f.file("aggregate-count.flac", container(Array.from({ length: 2 }, () => comment(Array.from({ length: 2050 }, () => ["UNKNOWN", "ignored"]))))),
        allocated = f.file("aggregate-bytes.flac", container(Array.from({ length: 3 }, () => comment([["UNKNOWN", "x".repeat(1500000)]]))))
    for (const file of [over, allocated]) {
        assert.ok((await parseFile(file)).common)
        assert.equal(opened([{ path: file }])[0].passes[0].error, "audio_resource_limit")
        coverage.guardedInputs++
        coverage.sourceAcceptedGuarded++
    }
})
test("opened multi-block reads stay sequential on the original replaced descriptor", async (t) => {
    const f = temporary(t),
        original = f.file("original.flac", container([comment([["TITLE", "held original"]]), comment([["ALBUM", "late held"]])])),
        replacement = f.file("replacement.flac", container([comment([["TITLE", "new path bytes"]])])),
        expected = raw(await source(original), true),
        held = path.join(f.directory, "held.flac"),
        answer = opened([{ path: original, rename: held, replacement, passes: 3 }])[0]
    for (const pass of answer.passes) {
        assert.equal(pass.ok, true)
        assert.deepEqual(pass.raw, expected)
        assert.equal(pass.unchanged, true)
        assert.deepEqual(pass.observations.before, pass.observations.after)
        coverage.openedPasses++
    }
    assert.notDeepEqual(raw(await source(original), true), expected)
    assert.deepEqual(raw(await source(held), true), expected)
})
