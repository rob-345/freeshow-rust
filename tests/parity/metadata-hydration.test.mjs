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
const source = fs.readFileSync(path.join(root, "src/frontend/rust/metadata.ts"), "utf8")
const compiled = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS }, reportDiagnostics: true })
assert.equal(compiled.diagnostics.length, 0)
const exported = {}
new Function("exports", compiled.outputText)(exported)
const { hydrateAudioMetadata } = exported
const empty = () => ({ undefinedPaths: [], specialNumbers: [], byteBuffers: [] })
const data = (metadata = {}) => ({ filePath: "/trusted/音楽 🎵.mp3", metadata })
const coverage = { sourceFiles: 0, sourcePictures: 0, largePictures: 0, rawUndefined: 0, specialNumbers: 0, invalidCases: 0 }
test.after(() => console.log("Audio metadata hydration coverage:", JSON.stringify(coverage)))

// Controlled future producer, not a replacement metadata parser. Fixtures below
// execute actual pinned source+music-metadata; only its native JSON envelope is mocked.
function wire(common) {
    const hydration = { ...empty(), objectKeys: [] }
    const escape = (key) => key.replace(/~/g, "~0").replace(/\//g, "~1")
    const encode = (value, pointer) => {
        if (value === undefined) {
            hydration.undefinedPaths.push(pointer)
            coverage.rawUndefined++
            return null
        }
        if (typeof value === "number" && (!Number.isFinite(value) || Object.is(value, -0))) {
            hydration.specialNumbers.push({ path: pointer, kind: Number.isNaN(value) ? "NaN" : value === Infinity ? "PositiveInfinity" : value === -Infinity ? "NegativeInfinity" : "NegativeZero" })
            coverage.specialNumbers++
            return Object.is(value, -0) ? 0 : null
        }
        if (ArrayBuffer.isView(value)) {
            assert.match(pointer, /^\/metadata\/picture\/[0-7]\/data$/)
            hydration.byteBuffers.push(pointer)
            return Buffer.from(value.buffer, value.byteOffset, value.byteLength).toString("base64")
        }
        if (Array.isArray(value)) return value.map((v, i) => encode(v, `${pointer}/${i}`))
        if (value && typeof value === "object") {
            hydration.objectKeys.push({ path: pointer, keys: Object.keys(value) })
            // Deliberately disturb wire insertion order. Hydration must reconstruct source.
            return Object.fromEntries(
                Object.keys(value)
                    .sort()
                    .reverse()
                    .map((key) => [key, encode(value[key], `${pointer}/${escape(key)}`)])
            )
        }
        return value
    }
    return { payload: data(encode(common, "/metadata")), hydration }
}
function rawEqual(actual, expected, location = "metadata") {
    if (ArrayBuffer.isView(expected)) {
        assert.equal(actual.constructor, Uint8Array, location)
        assert.equal(Buffer.isBuffer(actual), false)
        assert.deepEqual([...actual], [...expected], location)
    } else if (expected && typeof expected === "object") {
        assert.equal(Array.isArray(actual), Array.isArray(expected), location)
        assert.deepEqual(Object.keys(actual), Object.keys(expected), `${location} own/insertion keys`)
        for (const key of Object.keys(expected)) {
            assert.equal(Object.hasOwn(actual, key), true)
            rawEqual(actual[key], expected[key], `${location}.${key}`)
        }
    } else assert.equal(Object.is(actual, expected), true, `${location}: ${String(actual)} != ${String(expected)}`)
}
function reject(payload, hydration) {
    const before = structuredClone(payload),
        metadata = payload.metadata
    assert.throws(() => hydrateAudioMetadata(payload, hydration), /native audio metadata/)
    assert.deepEqual(payload, before, "invalid late entry must not partially alter callback data")
    assert.equal(payload.metadata, metadata, "root identity must survive rejection")
    coverage.invalidCases++
}
const fixedRevision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const original = spawnSync("git", ["show", `${fixedRevision}:src/electron/audio/nowPlaying.ts`], { cwd: root, encoding: "utf8", timeout: 10000 })
assert.equal(original.status, 0, original.stderr)
const ast = ts.createSourceFile("nowPlaying.ts", original.stdout, ts.ScriptTarget.Latest, true)
const declaration = ast.statements.find((s) => ts.isFunctionDeclaration(s) && s.name?.text === "getAudioMetadata").getText(ast)
const sourceHelper = ts.transpileModule(declaration + ";return getAudioMetadata", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
async function sourceCommon(file) {
    const events = []
    const get = new Function("parseFile", "sendToMain", "ToMain", "console", sourceHelper)(parseFile, (channel, payload) => events.push({ channel, payload }), { AUDIO_METADATA: "AUDIO_METADATA" }, { error() {} })
    const common = await get(file)
    assert.equal(events.length, 1)
    assert.equal(events[0].channel, "AUDIO_METADATA")
    assert.equal(events[0].payload.metadata, common)
    assert.equal(events[0].payload.filePath, file)
    coverage.sourceFiles++
    return common
}
function infoWav(tags) {
    const base = wav()
    const info = Buffer.concat([
        Buffer.from("INFO"),
        ...tags.map(([key, value]) => {
            const b = Buffer.from(value + "\0")
            return Buffer.concat([Buffer.from(key), u32(b.length, true), b, ...(b.length % 2 ? [Buffer.alloc(1)] : [])])
        })
    ])
    const output = Buffer.concat([base, Buffer.from("LIST"), u32(info.length, true), info])
    output.writeUInt32LE(output.length - 8, 4)
    return output
}

test("actual pinned source common and APIC/FLAC pictures restore raw values, byte data and insertion order", async (t) => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-metadata-hydration-"))
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }))
    const cover = png(512, 512)
    assert.ok(cover.length > 65536, "fixture must exercise more than EXIF's old byte bound")
    const fixtures = [
        [
            "invalid-year.wav",
            infoWav([
                ["YEAR", "invalid"],
                ["INAM", "Title"]
            ])
        ],
        ["signed-zero.wav", infoWav([["YEAR", "-0"]])],
        ["overflow.wav", infoWav([["YEAR", "9".repeat(400)]])],
        [
            "common.mp3",
            mp3([], 4, [
                ["TCON", Buffer.concat([Buffer.from([3]), Buffer.from("9999")])],
                ["TIT2", Buffer.concat([Buffer.from([3]), Buffer.from("Café 音楽 🎵")])]
            ])
        ],
        ["APIC.mp3", mp3([apic(cover, { type: 255 }), apic(png(1, 1), { type: 3 })])],
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
    for (const [name, bytes] of fixtures) {
        const file = path.join(directory, name)
        fs.writeFileSync(file, bytes)
        const common = await sourceCommon(file)
        if (common.picture) {
            coverage.sourcePictures += common.picture.length
            coverage.largePictures += common.picture.filter((p) => p.data.byteLength > 65536).length
            const first = common.picture[0]
            assert.equal(Object.hasOwn(first, "type"), true)
            assert.equal(first.type, undefined)
            if (name.startsWith("APIC")) assert.deepEqual(Object.keys(first), ["format", "type", "description", "data"])
            else assert.deepEqual(Object.keys(first), ["type", "format", "description", "width", "height", "colour_depth", "indexed_color", "data"])
        }
        const { payload, hydration } = wire(common),
            filePath = payload.filePath
        hydrateAudioMetadata(payload, hydration)
        assert.equal(payload.filePath, filePath)
        rawEqual(payload.metadata, common, name)
    }
})

test("null-or-absent own undefined, nested arrays, escaped keys and all special numbers", () => {
    const payload = data({ year: null, negative: 0, nested: [null, { "a/b~c": null }], unknown: { keep: "exact" } })
    const hydration = {
        ...empty(),
        undefinedPaths: ["/metadata/absent", "/metadata/nested/0"],
        specialNumbers: [
            { path: "/metadata/year", kind: "NaN" },
            { path: "/metadata/negative", kind: "NegativeZero" },
            { path: "/metadata/nested/1/a~1b~0c", kind: "PositiveInfinity" }
        ],
        objectKeys: [{ path: "/metadata", keys: ["absent", "nested", "year", "negative", "unknown"] }]
    }
    hydrateAudioMetadata(payload, hydration)
    assert.equal(Object.hasOwn(payload.metadata, "absent"), true)
    assert.equal(Object.hasOwn(payload.metadata.nested, 0), true)
    assert.equal(payload.metadata.absent, undefined)
    assert.ok(Number.isNaN(payload.metadata.year))
    assert.ok(Object.is(payload.metadata.negative, -0))
    assert.equal(payload.metadata.nested[1]["a/b~c"], Infinity)
    assert.deepEqual(Object.keys(payload.metadata), hydration.objectKeys[0].keys)
    const second = data({ x: null })
    hydrateAudioMetadata(second, { ...empty(), specialNumbers: [{ path: "/metadata/x", kind: "NegativeInfinity" }] })
    assert.equal(second.metadata.x, -Infinity)
})

test("canonical base64 empty/padding/binary cases become genuine Uint8Array", () => {
    for (const bytes of [Buffer.alloc(0), Buffer.from([0]), Buffer.from([0, 255]), Buffer.from([0, 1, 255]), Buffer.from([254, 255, 0, 127])]) {
        const payload = data({ picture: [{ data: bytes.toString("base64") }] })
        hydrateAudioMetadata(payload, { ...empty(), byteBuffers: ["/metadata/picture/0/data"] })
        assert.equal(payload.metadata.picture[0].data.constructor, Uint8Array)
        assert.deepEqual([...payload.metadata.picture[0].data], [...bytes])
    }
    for (const encoded of ["A", "AAA", "A===", "====", "AA=Z", "AA==\n", "AA--", "AA__", "AB==", "AAB=", "éAAA", "data:image/png;base64,AA=="]) reject(data({ picture: [{ data: encoded }] }), { ...empty(), byteBuffers: ["/metadata/picture/0/data"] })
})

test("4MiB single and eight-picture aggregate fit base64 wire budget; excess rejects before decode", () => {
    const full = Buffer.alloc(4 * 1024 * 1024, 211)
    const payload = data({ picture: [{ data: full.toString("base64") }] })
    hydrateAudioMetadata(payload, { ...empty(), byteBuffers: ["/metadata/picture/0/data"] })
    assert.equal(payload.metadata.picture[0].data.byteLength, full.length)
    assert.equal(payload.metadata.picture[0].data.at(-1), 211)
    const part = Buffer.alloc(512 * 1024, 17).toString("base64")
    const eight = data({ picture: Array.from({ length: 8 }, () => ({ data: part })) })
    hydrateAudioMetadata(eight, { ...empty(), byteBuffers: Array.from({ length: 8 }, (_, i) => `/metadata/picture/${i}/data`) })
    assert.equal(
        eight.metadata.picture.reduce((n, p) => n + p.data.byteLength, 0),
        4 * 1024 * 1024
    )
    const tooLarge = Buffer.alloc(2 * 1024 * 1024 + 1).toString("base64")
    const invalid = data({ picture: [{ data: tooLarge }, { data: tooLarge }] })
    const actual = globalThis.Uint8Array
    let allocations = 0
    globalThis.Uint8Array = new Proxy(actual, {
        construct(target, args) {
            allocations++
            return Reflect.construct(target, args)
        }
    })
    try {
        reject(invalid, { ...empty(), byteBuffers: ["/metadata/picture/0/data", "/metadata/picture/1/data"] })
        assert.equal(allocations, 0)
    } finally {
        globalThis.Uint8Array = actual
    }
})

test("late malformed entries and invalid orders leave all original references and data untouched", () => {
    const payload = data({ year: null, picture: [{ format: "image/png", data: "AA==" }], nested: { x: null } })
    const nested = payload.metadata.nested,
        pictures = payload.metadata.picture
    for (const tail of [
        { undefinedPaths: ["/metadata/nested/x", "/metadata/missing/child"] },
        {
            specialNumbers: [
                { path: "/metadata/year", kind: "NaN" },
                { path: "/metadata/nested/x", kind: "Bad" }
            ]
        },
        {
            objectKeys: [
                { path: "/metadata/picture/0", keys: ["data", "format"] },
                { path: "/metadata", keys: ["year"] }
            ]
        },
        { byteBuffers: ["/metadata/picture/0/data", "/metadata/nested/x"] }
    ])
        reject(payload, { ...empty(), byteBuffers: ["/metadata/picture/0/data"], ...tail })
    assert.equal(payload.metadata.nested, nested)
    assert.equal(payload.metadata.picture, pictures)
    const actual = globalThis.Uint8Array
    let allocations = 0
    globalThis.Uint8Array = new Proxy(actual, {
        construct(target, args) {
            allocations++
            return Reflect.construct(target, args)
        }
    })
    try {
        reject(payload, { ...empty(), byteBuffers: ["/metadata/picture/0/data"], objectKeys: [{ path: "/metadata", keys: [] }] })
        assert.equal(allocations, 0)
    } finally {
        globalThis.Uint8Array = actual
    }
})

test("a later decoder allocation failure also leaves the original callback payload untouched", () => {
    const payload = data({ year: null, picture: [{ data: "AA==" }, { data: "AQ==" }] })
    const before = structuredClone(payload),
        metadata = payload.metadata
    const actual = globalThis.Uint8Array
    let allocations = 0
    globalThis.Uint8Array = new Proxy(actual, {
        construct(target, args) {
            if (++allocations === 2) throw new RangeError("controlled allocation failure")
            return Reflect.construct(target, args)
        }
    })
    try {
        assert.throws(() => hydrateAudioMetadata(payload, { ...empty(), specialNumbers: [{ path: "/metadata/year", kind: "NaN" }], byteBuffers: ["/metadata/picture/0/data", "/metadata/picture/1/data"] }), /controlled allocation/)
        assert.equal(allocations, 2)
        assert.deepEqual(payload, before)
        assert.equal(payload.metadata, metadata)
    } finally {
        globalThis.Uint8Array = actual
    }
})

test("root confinement, dangerous properties, missing parents, duplicate/overlapping targets and placeholders", () => {
    for (const pointer of ["/filePath", "/metadataOther/x", "/metadata", "/metadata/__proto__/x", "/metadata/constructor", "/metadata/prototype", "/metadata/a~2b", "/metadata/array/01", "/metadata/array/length", "/metadata/array/9", "/metadata/missing/x"]) reject(data({ x: null, array: [null] }), { ...empty(), undefinedPaths: [pointer] })
    for (const hydration of [
        { ...empty(), undefinedPaths: ["/metadata/x", "/metadata/x"] },
        { ...empty(), undefinedPaths: ["/metadata/x"], specialNumbers: [{ path: "/metadata/x", kind: "NaN" }] },
        { ...empty(), undefinedPaths: ["/metadata/x", "/metadata/x/y"] },
        { ...empty(), undefinedPaths: ["/metadata/x/y", "/metadata/x"] },
        { ...empty(), specialNumbers: [{ path: "/metadata/nope", kind: "NaN" }] },
        { ...empty(), specialNumbers: [{ path: "/metadata/x", kind: "NegativeZero" }] }
    ])
        reject(data({ x: null }), hydration)
    reject(data({ x: 1 }), { ...empty(), undefinedPaths: ["/metadata/x"] })
    reject(data({ x: "0" }), { ...empty(), specialNumbers: [{ path: "/metadata/x", kind: "NegativeZero" }] })
    reject(data({ x: 0 }), { ...empty(), specialNumbers: [{ path: "/metadata/x", kind: "NaN" }] })
    reject(data(JSON.parse('{"__proto__":{"x":null}}')), empty())
    assert.equal(Object.prototype.x, undefined)
})

test("orders include every own restored key, reject duplicates and respect JS integer enumeration", () => {
    for (const order of [
        { path: "/metadata", keys: ["b", "a"] },
        { path: "/metadata", keys: ["a", "a", "new"] },
        { path: "/metadata", keys: ["a", "b", "wrong"] },
        { path: "", keys: ["a", "b", "new"] },
        { path: "/filePath", keys: [] }
    ])
        reject(data({ a: 1, b: 2 }), { ...empty(), undefinedPaths: ["/metadata/new"], objectKeys: [order] })
    reject(data({ a: 1 }), {
        ...empty(),
        objectKeys: [
            { path: "/metadata", keys: ["a"] },
            { path: "/metadata", keys: ["a"] }
        ]
    })
    reject(data({ a: 1 }), { ...empty(), objectKeys: [{ path: "/metadata/a", keys: [] }] })
    reject(data({ 2: null, 10: null, z: null }), { ...empty(), objectKeys: [{ path: "/metadata", keys: ["10", "2", "z"] }] })
    reject(data({ 2: null, z: null }), { ...empty(), objectKeys: [{ path: "/metadata", keys: ["z", "2"] }] })
    const payload = data({ 2: null, 10: null, z: null, a: null })
    hydrateAudioMetadata(payload, { ...empty(), objectKeys: [{ path: "/metadata", keys: ["2", "10", "a", "z"] }] })
    assert.deepEqual(Object.keys(payload.metadata), ["2", "10", "a", "z"])
})

test("strict JSON trees reject sparse/custom/accessor/aliased/cyclic values without invoking getters", () => {
    let getterCalls = 0
    const accessor = {}
    Object.defineProperty(accessor, "x", {
        enumerable: true,
        configurable: true,
        get() {
            getterCalls++
            return null
        }
    })
    assert.throws(() => hydrateAudioMetadata(data(accessor), empty()))
    assert.equal(getterCalls, 0)
    for (const value of [new Array(2), Object.assign([null], { extra: null }), new Date(), new Uint8Array(1), new Map(), undefined, NaN, Infinity, 1n, () => 1]) assert.throws(() => hydrateAudioMetadata(data({ value }), empty()))
    const same = { x: null }
    assert.throws(() => hydrateAudioMetadata(data({ a: same, b: same }), empty()))
    const cycle = {}
    cycle.self = cycle
    assert.throws(() => hydrateAudioMetadata(data(cycle), empty()))
    const payload = data({ a: null })
    Object.freeze(payload)
    assert.throws(() => hydrateAudioMetadata(payload, empty()))
    assert.deepEqual(payload.metadata, { a: null })
    const malformed = empty()
    malformed.undefinedPaths = new Array(1)
    reject(data(), malformed)
    const symbols = {}
    symbols[Symbol("x")] = null
    assert.throws(() => hydrateAudioMetadata(data(symbols), empty()))
})

test("picture schema allows only eight canonical data paths and requires complete byte restoration", () => {
    for (const [metadata, hydration] of [
        [{ picture: [{ data: "AA==" }] }, empty()],
        [{ picture: [{ data: [0] }] }, { ...empty(), byteBuffers: ["/metadata/picture/0/data"] }],
        [{ picture: [{}] }, empty()],
        [{ picture: {} }, empty()],
        [{ picture: Array.from({ length: 9 }, () => ({ data: "AA==" })) }, { ...empty(), byteBuffers: Array.from({ length: 9 }, (_, i) => `/metadata/picture/${i}/data`) }],
        [{ other: { data: "AA==" } }, { ...empty(), byteBuffers: ["/metadata/other/data"] }]
    ])
        reject(data(metadata), hydration)
})

test("depth16 and 65536 total nodes boundaries; encoded strings, wire/key/path/envelope budgets", () => {
    const nested = (n) => Array.from({ length: n }).reduce((v) => ({ x: v }), null)
    const payload = data(nested(16)),
        pointer = "/metadata" + "/x".repeat(16)
    hydrateAudioMetadata(payload, { ...empty(), undefinedPaths: [pointer] })
    let leaf = payload.metadata
    for (let i = 0; i < 15; i++) leaf = leaf.x
    assert.equal(Object.hasOwn(leaf, "x"), true)
    assert.equal(leaf.x, undefined)
    reject(data(nested(17)), empty())
    const exact = data({ items: Array(65528).fill(null) }),
        same = exact.metadata
    hydrateAudioMetadata(exact, empty())
    assert.equal(exact.metadata, same, "validated no-op does not clone")
    reject(data({ items: Array(65529).fill(null) }), empty())
    reject(data({ big: "x".repeat(6 * 1024 * 1024) }), empty())
    reject(data({ a: "x".repeat(4 * 1024 * 1024), b: "x".repeat(4 * 1024 * 1024) }), empty())
    reject(data({ ["x".repeat(1025)]: null }), empty())
    reject(data(Object.fromEntries(Array.from({ length: 3000 }, (_, i) => ["x".repeat(400) + i, null]))), empty())
    const parent = "p".repeat(1000)
    reject(data({ [parent]: {} }), { ...empty(), undefinedPaths: Array.from({ length: 1100 }, (_, i) => `/metadata/${parent}/x${i}`) })
    reject(data(), { ...empty(), objectKeys: [{ path: "/metadata", keys: Array(3000).fill("k".repeat(1000)) }] })
    reject(data({ x: null }), { ...empty(), undefinedPaths: ["/metadata/" + "x".repeat(2048)] })
})
