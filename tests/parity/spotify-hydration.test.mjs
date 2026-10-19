import assert from "node:assert/strict"
import fs from "node:fs"
import { fileURLToPath } from "node:url"
import test from "node:test"
import ts from "typescript"
import { Driver, sourceOracle } from "./spotify-oracle.mjs"

const file = fileURLToPath(new URL("../../src/frontend/rust/spotify.ts", import.meta.url))
const compiled = ts.transpileModule(fs.readFileSync(file, "utf8"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS }, reportDiagnostics: true })
assert.equal(compiled.diagnostics.length, 0)
const exported = {}
new Function("exports", compiled.outputText)(exported)
const { hydrateSpotify } = exported
const keys = ["isPlaying", "title", "artist", "positionSec", "durationSec", "albumArt", "volume", "platform"]
const empty = () => ({ undefinedFields: [], specialNumbers: [] })
const state = () => ({ isPlaying: false, title: "Title", artist: "Artist", positionSec: 1, durationSec: 2, albumArt: "https://example.invalid/cover", volume: 0.5, platform: "linux" })
const values = { NaN, PositiveInfinity: Infinity, NegativeInfinity: -Infinity, NegativeZero: -0 }
function compare(actual, expected) {
    if (expected === null) return assert.equal(actual, null)
    assert.deepEqual(Object.keys(actual), Object.keys(expected))
    assert.deepEqual(Object.keys(actual), keys)
    for (const key of keys) {
        assert.equal(Object.hasOwn(actual, key), true, key)
        assert.ok(Object.is(actual[key], expected[key]), key)
    }
}
function reject(data, restoration) {
    const beforeData = structuredClone(data),
        beforeRestoration = structuredClone(restoration)
    assert.throws(() => hydrateSpotify(data, restoration), /Invalid native Spotify restoration/)
    assert.deepEqual(data, beforeData, "failed restoration must not partially alter source data")
    assert.deepEqual(restoration, beforeRestoration, "failed restoration must not alter private metadata")
}

test("actual immutable source and native WireState restore IEEE values, own undefined, source keys and empty capture defaults", async () => {
    const driver = new Driver()
    const common = 'string "xesam:title" variant string "音楽 🎵"\nstring "xesam:artist" variant array [ string "Artist" ]\nstring "mpris:artUrl" variant string "https://example.invalid/cover"\nstring "mpris:length" variant uint64 120000000\n'
    try {
        for (const output of [
            "",
            common,
            common + 'variant string "Playing"\nint64 3000000\ndouble 0.5',
            common.replace('"音楽 🎵"', '"Playing title"') + 'string "Paused"\nint64 -1\ndouble -0.2',
            common.replace("uint64", "int64") + "int64 3\ndouble 0.5.9",
            common.replace('string "Artist"', 'string ""'),
            common.replace('string "https://example.invalid/cover"', 'string ""'),
            common.replace('"音楽 🎵"', '""'),
            common.replace('"音楽 🎵"', '"\0é\ntext"'),
            common + `int64 ${"9".repeat(512)}\ndouble .`,
            common.replace("120000000", "9".repeat(512)) + "double 1e-8"
        ]) {
            const original = await sourceOracle((_, callback) => callback(null, output)).get()
            const { wire } = await driver.value({ op: "parse", text: output })
            const before = structuredClone(wire)
            compare(hydrateSpotify(wire.data, { undefinedFields: wire.undefinedFields, specialNumbers: wire.specialNumbers }), original)
            assert.deepEqual(wire, before)
            if (output.includes('array [ string ""')) assert.equal(original.artist, "Unknown")
            if (output.includes('artUrl" variant string ""')) {
                assert.equal(Object.hasOwn(original, "albumArt"), true)
                assert.equal(original.albumArt, undefined)
                assert.deepEqual(wire.undefinedFields, ["albumArt"])
            }
        }
    } finally {
        await driver.finish()
    }
})

test("all four explicit native numeric tags work only in the three sealed numeric fields", () => {
    // Direct State::wire envelope controls, not claims that every value is
    // reachable through the current original Linux regular expressions.
    for (const field of ["positionSec", "durationSec", "volume"]) {
        for (const [kind, value] of Object.entries(values)) {
            const data = state(),
                restoration = empty()
            data[field] = null
            restoration.specialNumbers.push({ field, kind })
            Object.freeze(data)
            Object.freeze(restoration.specialNumbers[0])
            Object.freeze(restoration.specialNumbers)
            Object.freeze(restoration.undefinedFields)
            Object.freeze(restoration)
            const result = hydrateSpotify(data, restoration)
            assert.ok(Object.is(result[field], value))
            assert.deepEqual(Object.keys(result), keys)
            assert.notEqual(result, data)
            assert.equal(data[field], null)
        }
    }
    const data = state()
    data.albumArt = null
    const result = hydrateSpotify(data, { undefinedFields: ["albumArt"], specialNumbers: [] })
    assert.equal(Object.hasOwn(result, "albumArt"), true)
    assert.equal(result.albumArt, undefined)
    assert.equal(data.albumArt, null)
    const simultaneous = { ...state(), positionSec: null, durationSec: null, volume: null }
    const restored = hydrateSpotify(simultaneous, {
        undefinedFields: [],
        specialNumbers: [
            { field: "positionSec", kind: "PositiveInfinity" },
            { field: "durationSec", kind: "NegativeZero" },
            { field: "volume", kind: "NaN" }
        ]
    })
    assert.equal(restored.positionSec, Infinity)
    assert.ok(Object.is(restored.durationSec, -0))
    assert.ok(Number.isNaN(restored.volume))
})

test("source null requires exact empty restoration and is never inferred from invalid data", () => {
    assert.equal(hydrateSpotify(null, empty()), null)
    for (const restoration of [undefined, null, {}, [], { ...empty(), extra: 1 }, { undefinedFields: ["albumArt"], specialNumbers: [] }, { undefinedFields: [], specialNumbers: [{ field: "volume", kind: "NaN" }] }]) reject(null, restoration)
    for (const data of [undefined, false, 0, "", [], {}]) reject(data, empty())
})

test("exact Linux source schema and insertion order reject unknown, missing and non-JSON fields", () => {
    for (const key of keys) {
        const data = state()
        delete data[key]
        reject(data, empty())
    }
    const reversed = Object.fromEntries(Object.entries(state()).reverse())
    reject(reversed, empty())
    for (const data of [
        { ...state(), extra: true },
        { ...state(), platform: "darwin" },
        { ...state(), isPlaying: 1 },
        { ...state(), title: "" },
        { ...state(), artist: "" },
        { ...state(), albumArt: "" },
        { ...state(), title: [] },
        { ...state(), artist: null },
        { ...state(), albumArt: undefined }
    ])
        reject(data, empty())
    for (const extra of [Symbol("unknown"), "hidden"]) {
        const data = state()
        Object.defineProperty(data, extra, { value: 1 })
        assert.throws(() => hydrateSpotify(data, empty()))
    }
    const inherited = Object.assign(Object.create({ hidden: true }), state())
    assert.throws(() => hydrateSpotify(inherited, empty()))
    let reads = 0
    const accessor = state()
    Object.defineProperty(accessor, "title", {
        enumerable: true,
        get() {
            reads++
            return "Title"
        }
    })
    assert.throws(() => hydrateSpotify(accessor, empty()))
    assert.equal(reads, 0, "unsupported getters must not execute")
})

test("unknown, duplicate, sparse and inconsistent private metadata is rejected", () => {
    const sparse = new Array(1)
    for (const restoration of [
        { undefinedFields: "albumArt", specialNumbers: [] },
        { undefinedFields: [], specialNumbers: {} },
        { undefinedFields: sparse, specialNumbers: [] },
        { undefinedFields: [], specialNumbers: sparse },
        { undefinedFields: ["title"], specialNumbers: [] },
        { undefinedFields: ["albumArt", "albumArt"], specialNumbers: [] },
        { undefinedFields: [], specialNumbers: [{ field: "title", kind: "NaN" }] },
        { undefinedFields: [], specialNumbers: [{ field: "volume", kind: "unknown" }] },
        { undefinedFields: [], specialNumbers: [{ field: "volume", kind: "NaN", extra: 0 }] },
        {
            undefinedFields: [],
            specialNumbers: [
                { field: "volume", kind: "NaN" },
                { field: "volume", kind: "NaN" }
            ]
        },
        {
            undefinedFields: [],
            specialNumbers: [
                { field: "volume", kind: "NaN" },
                { field: "positionSec", kind: "NaN" }
            ]
        }
    ])
        reject(state(), restoration)
    const data = state()
    data.volume = null
    reject(data, empty())
    data.albumArt = null
    reject(data, { undefinedFields: [], specialNumbers: [{ field: "volume", kind: "NaN" }] })
    const specialNumbers = [{ field: "volume", kind: "NaN" }]
    specialNumbers.extra = true
    reject(data, { undefinedFields: ["albumArt"], specialNumbers })
    // Valid placeholders and a valid first entry make these independently test
    // duplicate/order/kind checks, rather than failing on a finite placeholder.
    const placeholders = { ...state(), positionSec: null, volume: null }
    reject(placeholders, {
        undefinedFields: [],
        specialNumbers: [
            { field: "positionSec", kind: "NaN" },
            { field: "volume", kind: "NaN" },
            { field: "volume", kind: "NaN" }
        ]
    })
    reject(placeholders, {
        undefinedFields: [],
        specialNumbers: [
            { field: "volume", kind: "NaN" },
            { field: "positionSec", kind: "NaN" }
        ]
    })
    reject({ ...state(), volume: null }, { undefinedFields: [], specialNumbers: [{ field: "volume", kind: "unknown" }] })
})

test("stale restoration cannot overwrite real finite values or strings, and never partially mutates data", () => {
    reject(state(), { undefinedFields: ["albumArt"], specialNumbers: [] })
    for (const field of ["positionSec", "durationSec", "volume"]) {
        reject(state(), { undefinedFields: [], specialNumbers: [{ field, kind: "NaN" }] })
        for (const value of [null, undefined, NaN, Infinity, -Infinity, -0, "1"]) reject({ ...state(), [field]: value }, empty())
    }
    const data = state()
    data.positionSec = null
    data.volume = null
    reject(data, {
        undefinedFields: [],
        specialNumbers: [
            { field: "positionSec", kind: "PositiveInfinity" },
            { field: "volume", kind: "invalid" }
        ]
    })
})

test("native Unicode UTF-8 and fixed-schema bounds reject oversized and malformed values", () => {
    const limit = 1024 * 1024
    for (const field of ["title", "artist", "albumArt"]) {
        for (const exact of ["x".repeat(limit), "🎵".repeat(limit / 4)]) {
            const data = { ...state(), [field]: exact }
            assert.equal(hydrateSpotify(data, empty())[field], exact)
        }
        for (const excessive of ["x".repeat(limit + 1), "🎵".repeat(limit / 4) + "x", "\ud800", "\udc00", "x\ud800x"]) reject({ ...state(), [field]: excessive }, empty())
    }
    const data = { ...state(), title: "x".repeat(limit), artist: "x".repeat(limit), albumArt: "x".repeat(limit) }
    assert.equal(hydrateSpotify(data, empty()).title.length, limit, "explicit conservative aggregate bound is3MiB")
    assert.throws(() => hydrateSpotify(state(), { undefinedFields: new Array(100000000), specialNumbers: [] }))
    assert.throws(() => hydrateSpotify(state(), { undefinedFields: [], specialNumbers: new Array(100000000) }))
})
