import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import { parseFile } from "music-metadata"
import { root, upstreamFunctions, clean } from "./oracle.mjs"
import { mp3, syncsafe } from "../../crates/freeshow-cover-images/tools/fixtures.mjs"
import { writeFixtures } from "../desktop/jpeg_now_playing_oracle.mjs"

const executable = (name) => path.join(root, "target/debug", name + (process.platform === "win32" ? ".exe" : "")),
    binary = process.env.FREESHOW_AUDIO_BIN || executable("freeshow-audio"),
    openedBinary = process.env.FREESHOW_AUDIO_OPENED_BIN || executable("freeshow-audio-opened-parity"),
    hash = (bytes) => createHash("sha256").update(bytes).digest("hex"),
    publicOriginal = upstreamFunctions("src/electron/audio/audio.ts", ["getAudioMetadata"], { parseFile }).getAudioMetadata,
    coverage = { files: 0, openedPasses: 0, publicReplies: 0, flaggedGaps: 0, guardedInputs: 0 }
test.after(() => console.log("ID3 timestamp text recovery source coverage:", JSON.stringify(coverage)))
function temporary(t) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-id3-recovery-"))
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
        assert.deepEqual(projected[i].value, expected[i].projected)
        assert.deepEqual(projected[i].diagnostics, [])
        assert.equal(hash(fs.readFileSync(files[i])), before[i], "Private view cannot mutate the original file")
        coverage.publicReplies++
        coverage.files++
    }
    return expected.map((item) => item.common)
}
function text(value, encoding = 3) {
    let bytes = Buffer.from(value, encoding === 0 ? "latin1" : encoding === 3 ? "utf8" : "utf16le")
    if (encoding === 1) bytes = Buffer.concat([Buffer.from([255, 254]), bytes])
    if (encoding === 2) bytes.swap16()
    return Buffer.concat([Buffer.from([encoding]), bytes])
}
test("unchanged packaged JPEG MP3 fixture metadata is accepted before cover decoding", async (t) => {
    const generated = await writeFixtures(temporary(t).directory),
        fixture = generated.cases.filter((item) => item.id === "jpeg-apic"),
        expected = await compare(fixture.map((entry) => entry.path)),
        recording = JSON.parse(fs.readFileSync(path.join(root, "tests/desktop/jpeg_now_playing_original.json")))
    assert.equal(fixture.length, 1)
    for (let i = 0; i < fixture.length; i++) {
        const recorded = recording.cases.find((item) => item.id === fixture[i].id).metadataRaw
        assert.deepEqual(raw(expected[i], true), [recorded[0], recorded[1].filter(([key]) => key !== "picture")], "actual packaged source recording, excluding independently owned covers")
    }
    assert.equal(expected[0].date, "invalid")
    assert.equal(Object.hasOwn(expected[0], "year"), false)
    assert.deepEqual(expected[0].artists, ["Artist α", "Artist β"])
})
test("v2.3/v2.4 raw invalid/partial dates preserve Latin1/UTF16-BOM/UTF8 and absent versus own NaN year", async (t) => {
    const f = temporary(t),
        files = []
    for (const version of [3, 4])
        for (const encoding of [0, 1, 3])
            for (const date of ["invalid", "2024-99-88T25:70:80Z", " 2024/02 trailing "]) {
                files.push(
                    f.file(
                        `${version}-${encoding}-${files.length}.mp3`,
                        mp3([], version, [
                            ["TIT2", text("Before")],
                            ["TDRC", text(date, encoding)],
                            ["TPE1", text("After")]
                        ])
                    )
                )
            }
    files.push(
        f.file(
            "own-NaN-year.mp3",
            mp3([], 4, [
                ["TYER", text("invalid")],
                ["TDRC", text("invalid")],
                ["TIT2", text("after NaN")]
            ])
        )
    )
    const expected = await compare(files)
    assert.equal(Object.hasOwn(expected[0], "year"), false)
    assert.equal(Object.hasOwn(expected.at(-1), "year"), true)
    assert.equal(Number.isNaN(expected.at(-1).year), true)
})
test("timestamp duplicates, source priority, NUL values, interleaving and occupied aliases preserve observed order", async (t) => {
    const f = temporary(t),
        files = []
    for (const version of [3, 4]) {
        files.push(
            f.file(
                `duplicates-${version}.mp3`,
                mp3([], version, [
                    ["T000", text("unknown occupied")],
                    ["TDRC", text("invalid")],
                    ["T001", text("another occupied")],
                    ["TIT2", text("between")],
                    ["TDRC", text("2024-99")],
                    ["TDOR", text("original raw")],
                    ["TDOR", text("1999")],
                    ["TALB", text("last")]
                ])
            )
        )
        files.push(
            f.file(
                `nul-${version}.mp3`,
                mp3([], version, [
                    ["TDRC", text("2024\0invalid")],
                    ["TPE1", text(" α \0 β ")],
                    ["TDTG", text("not a date")],
                    ["TDRL", text("noncanonical")],
                    ["TDEN", text("invalid")],
                    ["TALB", text("last")]
                ])
            )
        )
    }
    const occupied = Array.from({ length: 80 }, (_, i) => ["T" + i.toString(36).toUpperCase().padStart(3, "0"), text("ignored " + i)])
    files.push(f.file("many-collisions.mp3", mp3([], 4, [...occupied, ["TDRC", text("invalid")], ["TDRC", text("second raw")], ["TIT2", text("still ordered")]])))
    await compare(files)
})
test("flagged canonical timestamp stays supported; flagged invalid and multiple tags remain explicit gaps", async (t) => {
    const f = temporary(t),
        flagged = (date) => {
            const bytes = mp3([], 4, [["TDRC", text(date)]])
            bytes[18] = 0x40 // Read-only/status flag, no altered frame body.
            return bytes
        }
    await compare([f.file("flagged-canonical.mp3", flagged("2024-01-02"))])
    const file = f.file("flagged-invalid.mp3", flagged("invalid"))
    assert.equal((await source(file)).date, "invalid")
    const answer = opened([{ path: file }])[0].passes[0]
    assert.equal(answer.ok, false)
    assert.equal(answer.error, "audio_input_rejected")
    assert.equal(publicRead([file])[0].value, null)
    coverage.flaggedGaps++
    for (const version of [3, 4]) {
        const be = f.file(`be-${version}.mp3`, mp3([], version, [["TDRC", text("invalid", 2)]]))
        assert.equal((await source(be)).date, "椀渀瘀愀氀椀搀", "Pinned source interprets BOM-less encoding2 as UTF16LE")
        assert.equal(opened([{ path: be }])[0].passes[0].error, "audio_unsupported_variant")
        coverage.flaggedGaps++
    }
    const first = mp3([], 4, [["TDRC", text("invalid")]]),
        size = first.subarray(6, 10).reduce((n, b) => (n << 7) | b, 0),
        multiple = f.file("multiple-tags.mp3", Buffer.concat([first.subarray(0, 10 + size), mp3([], 4, [["TIT2", text("second")]])]))
    assert.ok(await source(multiple))
    assert.equal(opened([{ path: multiple }])[0].passes[0].error, "audio_unsupported_variant")
    coverage.flaggedGaps++
})
test("compressed/encrypted/recursive/truncated/huge declarations stay guarded ahead of recovery", (t) => {
    const f = temporary(t),
        files = [],
        expected = []
    for (const flag of [4, 8]) {
        const bytes = mp3([], 4, [["TDRC", text("invalid")]])
        bytes[19] = flag
        files.push(f.file(`flag-${flag}.mp3`, bytes))
        expected.push("audio_unsupported_variant")
    }
    files.push(
        f.file(
            "recursive.mp3",
            mp3([], 4, [
                ["TDRC", text("invalid")],
                ["CHAP", Buffer.alloc(0)]
            ])
        )
    )
    expected.push("audio_unsupported_variant")
    const huge = mp3([], 4, [["TDRC", text("invalid")]])
    syncsafe(0x0fffffff).copy(huge, 6)
    files.push(f.file("huge.mp3", huge))
    expected.push("audio_resource_limit")
    files.push(f.file("truncated.mp3", mp3([], 4, [["TDRC", text("invalid")]]).subarray(0, 12)))
    expected.push("audio_input_rejected")
    const answers = opened(files.map((file) => ({ path: file })))
    for (let i = 0; i < files.length; i++) {
        assert.equal(answers[i].passes[0].error, expected[i])
        coverage.guardedInputs++
    }
})
test("opened replacement keeps the original descriptor and sequential raw date observations", async (t) => {
    const f = temporary(t),
        original = f.file(
            "held.mp3",
            mp3([], 4, [
                ["TDRC", text("invalid")],
                ["TIT2", text("original held")]
            ])
        ),
        replacement = f.file(
            "replacement.mp3",
            mp3([], 4, [
                ["TDRC", text("1999")],
                ["TIT2", text("replacement")]
            ])
        ),
        expected = raw(await source(original), true),
        held = path.join(f.directory, "held-original.mp3"),
        answer = opened([{ path: original, rename: held, replacement, passes: 3 }])[0]
    for (const pass of answer.passes) {
        assert.equal(pass.ok, true)
        assert.deepEqual(pass.raw, expected)
        assert.equal(pass.unchanged, true)
        coverage.openedPasses++
    }
    assert.notDeepEqual(raw(await source(original), true), expected)
    assert.deepEqual(raw(await source(held), true), expected)
})
