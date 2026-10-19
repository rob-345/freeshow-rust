import assert from "node:assert/strict"
import { createHash } from "node:crypto"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import ts from "typescript"
import { parseFile } from "music-metadata"

const root = fileURLToPath(new URL("../../", import.meta.url))
const musicMetadataVersion = JSON.parse(fs.readFileSync(path.join(root, "node_modules/music-metadata/package.json"), "utf8")).version
assert.equal(musicMetadataVersion, "7.14.0", "actual pinned original parser dependency")
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const sourcePath = "src/electron/audio/nowPlaying.ts"
const loaded = spawnSync("git", ["show", `${revision}:${sourcePath}`], { cwd: root, encoding: "utf8", timeout: 10000 })
assert.equal(loaded.status, 0, loaded.stderr)
const source = loaded.stdout.replace(/\r\n/g, "\n")
const sourceHash = createHash("sha256").update(source).digest("hex")
const javascript = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText
const binary = process.env.FREESHOW_NOW_PLAYING_NATIVE_PARITY || path.join(root, "target/debug/freeshow-now-playing-native-parity" + (process.platform === "win32" ? ".exe" : ""))
const counts = { cases: 0, specials: 0, ownUndefined: 0, realOpenedWav: 0, pictures: 0, partialFailures: 0, unavailable: 0 }
test.after(() => console.log("Native common source evidence:", JSON.stringify({ revision, sourcePath, sourceHash, musicMetadataVersion, ...counts })))

function raw(value) {
    if (value === undefined) return ["undefined"]
    if (value === null) return ["null"]
    if (typeof value === "number") return ["number", Object.is(value, -0) ? "-0" : Number.isFinite(value) ? value : String(value)]
    if (Array.isArray(value)) return ["array", value.map(raw)]
    if (typeof value === "object") return ["object", Object.keys(value).map((key) => [key, raw(value[key])])]
    return [typeof value, value]
}
function restore(tuple) {
    const [type, value] = tuple
    switch (type) {
        case "undefined":
            return undefined
        case "null":
            return null
        case "number":
            return typeof value === "number" ? value : { NaN, Infinity, "-Infinity": -Infinity, "-0": -0 }[value]
        case "object":
            return Object.fromEntries(value.map(([key, value]) => [key, restore(value)]))
        case "array":
            return value.map(restore)
        default:
            return value
    }
}
function metadata(filePath, common) {
    return {
        filePath,
        common: raw(Object.fromEntries(Object.entries(common).filter(([key]) => key !== "picture"))),
        pictures: (common.picture || []).map((picture) => ({ fields: raw(Object.fromEntries(Object.entries(picture).filter(([key]) => key !== "data"))), bytes: [...picture.data] }))
    }
}
const data = (filePath = "/fixture/input.wav") => ({ filePath, name: "filename", unknownLang: ["Unknown A", "Unknown T", "Unknown B"], format: "{artist}|{title}|{album}|{year}|{artwork_path}|{artwork_base64}|{duration_s}", duration: 1 })
function fixture(common, pictures = [], fields = {}) {
    return { common: raw(common), pictures: pictures.map(({ data, ...fields }) => ({ fields: raw(fields), bytes: [...data] })), data: data(), ...fields }
}

async function reference(c) {
    const trace = []
    const input = structuredClone(c.data)
    let common = c.opened ? undefined : restore(c.common)
    if (!c.opened && c.pictures?.length) common.picture = c.pictures.map((picture) => ({ ...restore(picture.fields), data: Buffer.from(picture.bytes) }))
    const exports = {}
    const modules = {
        electron: {
            nativeImage: {
                createFromBuffer(bytes) {
                    trace.push(["image.create", [...bytes]])
                    if (c.fail === "image") throw Error("controlled native image failure")
                    return {
                        toPNG() {
                            trace.push(["image.png"])
                            return Buffer.from([10, 20])
                        },
                        getSize() {
                            trace.push(["image.size"])
                            return { width: 2, height: 1 }
                        }
                    }
                }
            }
        },
        "music-metadata": {
            parseFile: async (filePath) => {
                trace.push(["parse", filePath])
                if (c.opened) {
                    const result = await parseFile(filePath)
                    common = result.common
                    assert.equal(common.picture, undefined, "generated WAV corpus has authoritative complete empty covers")
                    return result
                }
                return { common }
            }
        },
        path: path.posix,
        "../../types/IPC/ToMain": { ToMain: { AUDIO_METADATA: "AUDIO_METADATA" } },
        "../IPC/main": {
            sendToMain(channel, value) {
                assert.equal(channel, "AUDIO_METADATA")
                assert.equal(value.metadata, common, "actual source publishes its original exact common object")
                trace.push(["metadata", metadata(value.filePath, value.metadata)])
            }
        },
        "../utils/files": {
            getDataFolderPath(id) {
                trace.push(["folder", id])
                return "/fixture/Audio"
            },
            doesPathExist(filePath) {
                trace.push(["exists", filePath])
                return false
            },
            deleteFile(filePath) {
                trace.push(["delete", filePath])
                return true
            },
            writeFile(filePath, value) {
                trace.push(["write", filePath, Buffer.isBuffer(value) ? [...value] : value])
                return true
            },
            openInSystem() {
                throw Error("unexpected OPEN")
            }
        },
        "./IcecastSender": {
            IcecastSender: {
                updateMetadata(title) {
                    trace.push(["icecast", title])
                }
            }
        }
    }
    // Same realm for the source functions, injected promises and original
    // music-metadata objects. No cross-realm promise assimilation assumption.
    new Function("require", "exports", javascript)((id) => {
        assert.ok(Object.hasOwn(modules, id), id)
        return modules[id]
    }, exports)
    let threw = false
    try {
        await exports.setPlayingState(input)
    } catch {
        threw = true
        counts.partialFailures++
    }
    return { trace, format: input.format, threw }
}
function native(cases) {
    const result = spawnSync(binary, [], { cwd: root, input: JSON.stringify(cases), encoding: "utf8", timeout: 30000, maxBuffer: 24 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return JSON.parse(result.stdout)
}
async function compare(cases) {
    const expected = []
    for (const c of cases) expected.push(await reference(c))
    const actual = native(cases)
    actual.forEach((result, i) => {
        counts.cases++
        assert.deepEqual({ trace: result.trace, format: result.format, threw: result.threw }, expected[i], JSON.stringify(cases[i]))
        assert.equal(result.coverReads, 1, "unknown native covers always require a real explicit complete handoff")
        assert.equal(result.parsed, !!cases[i].opened, "host values are not presented as parser evidence")
        assert.equal(result.projection.json_projection_only, true)
        assert.equal(Object.hasOwn(result.projection, "native_receipt"), false, "raw capability is not serialized as source data")
    })
    return actual
}

test("same-realm actual source year NaN/null/zero/empty/undefined/absent and IEEE values stay distinct", async () => {
    const cases = [NaN, null, 0, -0, "", undefined, Infinity, -Infinity, false, 1e21, 1e-7, [NaN, -0, undefined, null, Infinity]].map((year) => fixture({ year, own: undefined, title: "Title", unknown: { preserve: [undefined, NaN, -0, Infinity, -Infinity] } }))
    cases.push(fixture({ title: "No year" }))
    const actual = await compare(cases)
    assert.equal(actual[0].format.split("|")[3], "NaN")
    assert.equal(actual[1].format.split("|")[3], "Unknown Year")
    assert.equal(actual[2].format.split("|")[3], "0")
    assert.equal(actual[3].format.split("|")[3], "0")
    assert.equal(actual[4].format.split("|")[3], "Unknown Year")
    assert.equal(actual[5].format.split("|")[3], "Unknown Year")
    counts.specials += 6
    counts.ownUndefined += 12
})

test("exact native common truthiness, artist filtering, scientific strings and source partial errors", async () => {
    await compare([
        fixture({ artists: [undefined, null, false, 0, -0, NaN, " A ", "A", "a"], title: NaN, album: Infinity }),
        fixture({ artist: NaN, albumartist: "fallback", title: -Infinity, album: [undefined, -0, Infinity] }),
        fixture({ artists: { length: NaN }, originalartist: "original", year: { arbitrary: undefined } }),
        fixture({ year: -0, unknown: { 10: "ten", 2: "two", z: undefined, nested: { 1: NaN, name: "保持" } } }),
        fixture({ artists: [Infinity], title: "metadata precedes trim failure" }),
        fixture({ artists: { length: Infinity }, title: "metadata precedes filter failure" })
    ])
})

test("native cover handoff keeps both typed picture byte sets and ordered own undefined type", async () => {
    const pictures = [
        { format: "image/png", type: undefined, description: "音 🎵", data: Uint8Array.from([137, 80, 78, 71]) },
        { format: "image/jpeg", type: null, description: "second", data: Uint8Array.from([9, 8, 7]) }
    ]
    const actual = await compare([fixture({ year: NaN, own: undefined }, pictures), fixture({ year: -0 }, pictures, { fail: "image" })])
    assert.equal(actual[0].trace[1][1].pictures.length, 2)
    assert.deepEqual(
        actual[0].trace[1][1].pictures[0].fields[1].map(([key]) => key),
        ["format", "type", "description"]
    )
    assert.deepEqual(actual[0].cleanup, [1], "actual planner image cleanup intent remains required")
    assert.deepEqual(actual[1].cleanup, [], "no phantom image is disposed after constructor failure")
    counts.pictures += 4
})

function wav(tags) {
    const u32 = (n) => {
        const b = Buffer.alloc(4)
        b.writeUInt32LE(n)
        return b
    }
    const chunk = (key, bytes) => Buffer.concat([Buffer.from(key), u32(bytes.length), bytes, ...(bytes.length % 2 ? [Buffer.alloc(1)] : [])])
    const bytes = Buffer.concat([Buffer.from("WAVE"), chunk("fmt ", Buffer.from([1, 0, 1, 0, 64, 31, 0, 0, 128, 62, 0, 0, 2, 0, 16, 0])), chunk("LIST", Buffer.concat([Buffer.from("INFO"), ...tags.map(([key, value]) => chunk(key, Buffer.from(value + "\0")))])), chunk("data", Buffer.alloc(160))])
    return Buffer.concat([Buffer.from("RIFF"), u32(bytes.length), bytes])
}
test("actual opened WAV NativeCommon receipt flows through unknown covers to exact publication and NaN formatting", async (t) => {
    const folder = fs.mkdtempSync(path.join(os.tmpdir(), "now-playing-native-common-"))
    t.after(() => fs.rmSync(folder, { recursive: true, force: true }))
    const cases = ["invalid", "-0", "0", "2026", ""].map((year, index) => {
        const filePath = path.join(folder, `音楽-${index}.wav`)
        fs.writeFileSync(
            filePath,
            wav([
                ["INAM", "Title"],
                ["IART", "Artist"],
                ["YEAR", year]
            ])
        )
        return { data: data(filePath), opened: true, pictures: [] }
    })
    const actual = await compare(cases)
    assert.equal(actual[0].format.split("|")[3], "NaN")
    assert.equal(actual[0].projection.common.year, null, "lossy JSON exists only as explicitly marked compatibility data")
    counts.realOpenedWav += cases.length
})

test("unknown/unavailable native cover receipt is explicit and never fabricates metadata publication", () => {
    const [result] = native([fixture({ year: NaN }, [], { fail: "covers" })])
    assert.equal(result.coverReads, 1)
    assert.equal(result.error.code, "fixture_covers_unavailable")
    assert.deepEqual(
        result.trace.map(([kind]) => kind),
        ["parse"]
    )
    assert.equal(result.projection, null)
    counts.unavailable++
})
