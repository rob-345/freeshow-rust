import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import ts from "typescript"
import { parseFile } from "music-metadata"
import { apic, mp3, picture, flac, comments, png, wav, u32 } from "../../crates/freeshow-cover-images/tools/fixtures.mjs"

const root = fileURLToPath(new URL("../../", import.meta.url))
const binary = process.env.FREESHOW_METADATA_WIRE_BIN || path.join(root, "target/debug/freeshow-metadata-wire-parity" + (process.platform === "win32" ? ".exe" : ""))
const pinned = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const original = spawnSync("git", ["show", `${pinned}:src/electron/audio/nowPlaying.ts`], { cwd: root, encoding: "utf8", timeout: 10000 })
assert.equal(original.status, 0, original.stderr)
const ast = ts.createSourceFile("nowPlaying.ts", original.stdout, ts.ScriptTarget.Latest, true)
const declaration = ast.statements.find((s) => ts.isFunctionDeclaration(s) && s.name?.text === "getAudioMetadata").getText(ast)
const sourceHelper = ts.transpileModule(declaration + ";return getAudioMetadata", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
const hydratedSource = ts.transpileModule(fs.readFileSync(path.join(root, "src/frontend/rust/metadata.ts"), "utf8"), {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
    reportDiagnostics: true
})
assert.equal(hydratedSource.diagnostics.length, 0)
const exported = {}
new Function("exports", hydratedSource.outputText)(exported)
const { hydrateAudioMetadata } = exported
const coverage = { sourceFiles: 0, sourcePictures: 0, largeSourcePictures: 0, sourceOwnUndefined: 0, sourceSpecialNumbers: 0, rawUndefined: 0, specialNumbers: 0, accepted: 0, rejected: 0 }
test.after(() => console.log("Metadata wire coverage:", JSON.stringify(coverage)))

// Typed source input, not an alternative metadata parser/wire implementation.
function typed(value) {
    if (value === undefined) {
        coverage.rawUndefined++
        return ["undefined"]
    }
    if (value === null) return ["null"]
    if (typeof value === "boolean") return ["boolean", value]
    if (typeof value === "string") return ["string", value]
    if (typeof value === "number") {
        let number = value
        if (!Number.isFinite(value) || Object.is(value, -0)) {
            number = Number.isNaN(value) ? "NaN" : Object.is(value, -0) ? "-0" : value === Infinity ? "Infinity" : "-Infinity"
            coverage.specialNumbers++
        }
        return ["number", number]
    }
    if (Array.isArray(value)) return ["array", value.map(typed)]
    assert.equal(ArrayBuffer.isView(value), false, "only separate trusted picture receipts contain bytes")
    return ["object", Object.keys(value).map((key) => [key, typed(value[key])])]
}
function input(common = {}, pictures = [], filePath = "/trusted/音楽 🎵.mp3") {
    return { filePath, common: typed(common), pictures: pictures.map(({ fields = {}, bytes = Buffer.alloc(0) }) => ({ fields: typed(fields), base64: bytes.toString("base64") })) }
}
function run(cases) {
    const child = spawnSync(binary, [], { input: JSON.stringify(cases), encoding: "utf8", maxBuffer: 34 * 1024 * 1024, timeout: 30000 })
    assert.equal(child.status, 0, child.stderr || child.error?.message)
    const output = JSON.parse(child.stdout)
    assert.equal(output.length, cases.length)
    for (const result of output) coverage[result.ok ? "accepted" : "rejected"]++
    return output
}
function rawEqual(actual, expected, location = "metadata") {
    if (ArrayBuffer.isView(expected)) {
        assert.equal(actual.constructor, Uint8Array, location)
        assert.equal(Buffer.isBuffer(actual), false)
        assert.deepEqual(Buffer.from(actual.buffer, actual.byteOffset, actual.byteLength), Buffer.from(expected.buffer, expected.byteOffset, expected.byteLength), location)
    } else if (expected && typeof expected === "object") {
        assert.equal(Array.isArray(actual), Array.isArray(expected), location)
        assert.deepEqual(Object.keys(actual), Object.keys(expected), `${location}: own/insertion keys`)
        for (const key of Object.keys(expected)) {
            assert.equal(Object.hasOwn(actual, key), true)
            rawEqual(actual[key], expected[key], `${location}.${key}`)
        }
    } else assert.equal(Object.is(actual, expected), true, `${location}: ${String(actual)} != ${String(expected)}`)
}
function restore(result, expected, filePath) {
    assert.equal(result.ok, true, JSON.stringify(result.error))
    assert.equal(result.wire.data.filePath, filePath)
    hydrateAudioMetadata(result.wire.data, result.wire.hydration)
    assert.equal(result.wire.data.filePath, filePath)
    rawEqual(result.wire.data.metadata, expected)
    return result.wire
}
async function sourceCommon(filePath) {
    const events = []
    const get = new Function("parseFile", "sendToMain", "ToMain", "console", sourceHelper)(parseFile, (channel, payload) => events.push({ channel, payload }), { AUDIO_METADATA: "AUDIO_METADATA" }, { error() {} })
    const common = await get(filePath)
    assert.equal(events.length, 1)
    assert.equal(events[0].channel, "AUDIO_METADATA")
    assert.equal(events[0].payload.filePath, filePath)
    assert.equal(events[0].payload.metadata, common)
    coverage.sourceFiles++
    return common
}
function infoWav(tags) {
    const info = Buffer.concat([
        Buffer.from("INFO"),
        ...tags.map(([key, value]) => {
            const b = Buffer.from(value + "\0")
            return Buffer.concat([Buffer.from(key), u32(b.length, true), b, ...(b.length % 2 ? [Buffer.alloc(1)] : [])])
        })
    ])
    const output = Buffer.concat([wav(), Buffer.from("LIST"), u32(info.length, true), info])
    output.writeUInt32LE(output.length - 8, 4)
    return output
}

test("actual pinned common → native producer → actual TS hydrator, including APIC/FLAC bytes and own undefined", async (t) => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-metadata-wire-"))
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }))
    const cover = png(512, 512)
    assert.ok(cover.length > 65536)
    const fixtures = [
        ["bad-year.wav", infoWav([["YEAR", "invalid"]])],
        ["negative-zero.wav", infoWav([["YEAR", "-0"]])],
        ["overflow.wav", infoWav([["YEAR", "9".repeat(400)]])],
        [
            "common.mp3",
            mp3([], 4, [
                ["TCON", Buffer.from([3, ...Buffer.from("9999")])],
                ["TIT2", Buffer.from([3, ...Buffer.from("Café 音楽 🎵")])]
            ])
        ],
        ["APIC.mp3", mp3([apic(cover, { type: 255 }), apic(png(1, 1))])],
        [
            "FLAC.flac",
            flac(
                [picture(cover, { type: 255 })],
                [
                    [
                        4,
                        comments([
                            ["BPM", "invalid"],
                            ["REPLAYGAIN_TRACK_GAIN", "-Infinity dB"]
                        ])
                    ]
                ]
            )
        ]
    ]
    const expected = [],
        cases = []
    for (const [name, bytes] of fixtures) {
        const filePath = path.join(directory, name)
        fs.writeFileSync(filePath, bytes)
        const source = await sourceCommon(filePath)
        const common = Object.fromEntries(Object.entries(source).filter(([key]) => key !== "picture"))
        const pictures = (source.picture || []).map((p) => ({ fields: Object.fromEntries(Object.entries(p).filter(([key]) => key !== "data")), bytes: Buffer.from(p.data) }))
        if (source.picture) {
            coverage.sourcePictures += source.picture.length
            coverage.largeSourcePictures += source.picture.filter((p) => p.data.byteLength > 65536).length
            assert.equal(Object.hasOwn(source.picture[0], "type"), true)
            assert.equal(source.picture[0].type, undefined)
            assert.deepEqual(Object.keys(source.picture[0]), name.startsWith("APIC") ? ["format", "type", "description", "data"] : ["type", "format", "description", "width", "height", "colour_depth", "indexed_color", "data"])
        }
        // Root picture is explicitly appended by production's skipCovers+separate receipt API.
        // This does not certify the original top-level common.picture insertion position.
        expected.push(pictures.length ? { ...common, picture: source.picture } : common)
        const priorUndefined = coverage.rawUndefined,
            priorSpecial = coverage.specialNumbers
        cases.push(input(common, pictures, filePath))
        coverage.sourceOwnUndefined += coverage.rawUndefined - priorUndefined
        coverage.sourceSpecialNumbers += coverage.specialNumbers - priorSpecial
    }
    const results = run(cases)
    for (let i = 0; i < cases.length; i++) restore(results[i], expected[i], cases[i].filePath)
    assert.ok(Number.isNaN(expected[0].year), "actual source bad YEAR is NaN")
    assert.ok(Object.is(expected[1].year, -0))
    assert.equal(expected[2].year, Infinity)
    assert.equal(Object.hasOwn(expected[3].genre, 0), true)
    assert.equal(expected[3].genre[0], undefined)
})

test("all special numbers, nested own undefined, escaped keys and normal unknown fields retain exact raw values", () => {
    const common = { 2: "first", 10: "second", year: NaN, zero: -0, pos: Infinity, neg: -Infinity, "a/b~c": [undefined, { z: undefined, ordinary: 'keep\u0000\n\\"音楽' }], unknown: { nested: true } }
    const fixture = input(common)
    const [result] = run([fixture])
    const wire = restore(result, common, fixture.filePath)
    assert.deepEqual(wire.hydration.undefinedPaths, ["/metadata/a~1b~0c/0", "/metadata/a~1b~0c/1/z"])
    assert.equal(wire.hydration.objectKeys.length, 3)
    assert.equal(Number.isNaN(common.year), true, "producer does not mutate typed source")
})

test("full 4MiB and eight-picture aggregate roundtrip fits actual frontend JSON/decoded budgets", () => {
    for (const pictures of [[{ bytes: Buffer.alloc(4 * 1024 * 1024, 211) }], Array.from({ length: 8 }, () => ({ bytes: Buffer.alloc(512 * 1024, 17) }))]) {
        const fixture = input({}, pictures)
        const [result] = run([fixture])
        const expected = { picture: pictures.map(({ bytes }) => ({ data: bytes })) }
        const wire = restore(result, expected, fixture.filePath)
        assert.equal(
            wire.data.metadata.picture.reduce((n, p) => n + p.data.byteLength, 0),
            4 * 1024 * 1024
        )
        assert.equal(wire.hydration.byteBuffers.length, pictures.length)
    }
    const [result] = run([input({}, [{ bytes: Buffer.alloc(4 * 1024 * 1024) }, { bytes: Buffer.from([1]) }])])
    assert.equal(result.ok, false)
    assert.match(result.error.message, /decoded picture bytes/)
})

test("65536 shared nodes and metadata depth16 include generated hydration records", () => {
    const exact = input({ values: Array(65523).fill(null) })
    const [ok, excess] = run([exact, input({ values: Array(65524).fill(null) })])
    restore(ok, { values: Array(65523).fill(null) }, exact.filePath)
    assert.equal(excess.ok, false)
    assert.match(excess.error.message, /shared nodes/)
    const nested = (n) => (n ? { x: nested(n - 1) } : null)
    const [depth16, depth17] = run([input(nested(16)), input(nested(17))])
    restore(depth16, nested(16), exact.filePath)
    assert.equal(depth17.ok, false)
    assert.match(depth17.error.message, /metadata depth/)
})

test("late invalid typed input, duplicate/dangerous keys and nonrepresentable order return bounded diagnostics only", () => {
    const invalid = [
        input({}, [{ bytes: Buffer.alloc(128 * 1024) }, { fields: { constructor: null } }]),
        {
            ...input(),
            common: [
                "object",
                [
                    ["same", ["null"]],
                    ["same", ["null"]]
                ]
            ]
        },
        {
            ...input(),
            common: [
                "object",
                [
                    ["z", ["null"]],
                    ["0", ["null"]]
                ]
            ]
        },
        input({ picture: null }),
        input({}, [{ fields: { data: "encoded must not bypass receipt" } }]),
        input(
            {},
            Array.from({ length: 9 }, () => ({}))
        ),
        input({ ["x".repeat(1025)]: null }),
        input({}, [], "x".repeat(32769))
    ]
    for (const result of run(invalid)) {
        assert.equal(result.ok, false)
        assert.equal(Object.hasOwn(result, "wire"), false)
        assert.match(result.error.code, /^metadata_wire_(invalid|limit)$/)
        assert.ok(result.error.message.length < 128)
    }
})

test("aggregate keys, pointers, escaped envelope and metadata bytes are enforced before publication", () => {
    const long = "x".repeat(1000)
    const entries = Object.fromEntries(Array.from({ length: 600 }, (_, i) => [`${long}${i}`, null]))
    const repeated = Object.fromEntries(Array.from({ length: 1100 }, (_, i) => [`v${i}`, undefined]))
    const control = "\u0000".repeat(1000)
    const manyEscaped = Object.fromEntries(Array.from({ length: 400 }, (_, i) => [`v${i}`, undefined]))
    const invalid = [input(entries), input({ [long]: repeated }), input({ [control]: manyEscaped }), input({ a: "x".repeat(4 * 1024 * 1024), b: "x".repeat(4 * 1024 * 1024) })]
    // Keep private driver input below16MiB by running each budget independently.
    const expected = [/aggregate keys/, /aggregate pointers/, /envelope JSON/, /metadata JSON/]
    for (let i = 0; i < invalid.length; i++) {
        const [result] = run([invalid[i]])
        assert.equal(result.ok, false)
        assert.match(result.error.message, expected[i])
    }
})

test("deterministic generated source-shaped trees roundtrip through real producer and hydrator", () => {
    let seed = 19317
    const random = () => (seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 4294967296
    const value = (depth) => {
        const leaf = [undefined, null, true, false, NaN, -0, Infinity, -Infinity, 0.001, 1e200, '音楽/\u0001~\\"']
        if (!depth || random() < 0.5) return leaf[Math.floor(random() * leaf.length)]
        return random() < 0.5 ? Array.from({ length: 3 }, () => value(depth - 1)) : { "a/b~c": value(depth - 1), "値🎵": value(depth - 1) }
    }
    for (let batch = 0; batch < 4; batch++) {
        const common = Array.from({ length: 16 }, () => ({ unknown: value(4), title: "keep" }))
        const cases = common.map((v) => input(v))
        const results = run(cases)
        for (let i = 0; i < common.length; i++) restore(results[i], common[i], cases[i].filePath)
    }
})

test("actual producer wire still rejects late envelope tampering atomically in actual TS hydrator", () => {
    const fixture = input({ year: NaN }, [{ fields: { type: undefined }, bytes: Buffer.from([1, 2, 3]) }])
    const [result] = run([fixture])
    assert.equal(result.ok, true)
    const { data, hydration } = result.wire
    const root = data.metadata,
        picture = root.picture[0],
        before = structuredClone(data)
    hydration.objectKeys.push({ path: "/metadata/picture/0", keys: ["data", "missing"] })
    assert.throws(() => hydrateAudioMetadata(data, hydration), /native audio metadata/)
    assert.deepEqual(data, before)
    assert.equal(data.metadata, root)
    assert.equal(data.metadata.picture[0], picture)
})
