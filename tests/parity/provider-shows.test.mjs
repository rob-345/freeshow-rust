import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import ts from "typescript"
const root = fileURLToPath(new URL("../../", import.meta.url)),
    revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975",
    relative = "src/electron/contentProviders/churchApps/ChurchAppsShowBuilder.ts",
    pinned = spawnSync("git", ["show", `${revision}:${relative}`], { cwd: root, encoding: "utf8", timeout: 10000 }),
    source = pinned.stdout.replace(/\r\n/g, "\n"),
    binary = process.env.FREESHOW_PROVIDER_SHOWS_PARITY || path.join(root, "target/debug/freeshow-provider-shows-parity" + (process.platform === "win32" ? ".exe" : "")),
    counts = { comparisons: 0, parse: 0, song: 0, empty: 0, media: 0, unsupported: 0, bounded: 0, electronMappings: 0, electronContexts: 0 }
assert.equal(pinned.status, 0, pinned.stderr)
assert.equal(createHash("sha256").update(source).digest("hex"), "866fc0a1645627b7489a805943d9917b0ec81b5471f79ee4c83912cb88d9232a")
assert.equal(fs.readFileSync(path.join(root, relative), "utf8").replace(/\r\n/g, "\n"), source)
const code = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
const bits = (n) => {
    const b = new DataView(new ArrayBuffer(8))
    b.setFloat64(0, n, false)
    return b.getBigUint64(0, false).toString(16).padStart(16, "0")
}
const typed = (value) => (value === undefined ? ["u"] : value === null ? ["null"] : typeof value === "number" ? ["n", bits(value)] : typeof value === "boolean" ? ["b", value] : typeof value === "string" ? ["s", value] : Array.isArray(value) ? ["a", Array.from(value, typed)] : ["o", Object.keys(value).map((key) => [key, typed(value[key])])])
const uids = (count = 128) => Array.from({ length: count }, (_, i) => i.toString(16).padStart(11, "0"))
const fixture = (method, args, extra = {}) => ({ method, args, uids: uids(), clock: [1700000000123], ...extra })
function original(c) {
    let uidCalls = 0,
        nowCalls = 0,
        dateCalls = 0
    class Clock extends Date {
        constructor(...args) {
            super(...args)
            dateCalls++
        }
        static now() {
            assert.ok(nowCalls < c.clock.length, "controlled source clock exhausted")
            return c.clock[nowCalls++]
        }
    }
    const exports = {},
        uid = () => {
            assert.ok(uidCalls < c.uids.length, "controlled source uid exhausted")
            return c.uids[uidCalls++]
        }
    new vm.Script(`(function(require,exports){${code}})`).runInNewContext({ Date: Clock })((name) => {
        assert.equal(name, "uid")
        return { uid }
    }, exports)
    const method = { parse: "parseLyrics", song: "createSongShow", empty: "createEmptyShow", media: "createMediaShow" }[c.method]
    const result = exports.ChurchAppsShowBuilder[method](...c.args)
    return c.method === "parse" ? { value: JSON.parse(JSON.stringify(result)), uidCalls, nowCalls, dateCalls } : { typed: typed(result), json: JSON.parse(JSON.stringify(result)), uidCalls, nowCalls, dateCalls, result }
}
function native(cases) {
    const input = cases.map((c) => ({ ...c, args: c.args.map(typed), clock: c.clock.map(bits) }))
    const r = spawnSync(binary, [], { input: JSON.stringify(input), encoding: "utf8", timeout: 15000, maxBuffer: 32 * 1024 * 1024 })
    assert.equal(r.status, 0, r.stderr)
    const result = JSON.parse(r.stdout)
    assert.ok(Array.isArray(result), r.stdout.slice(0, 1024))
    assert.equal(result.length, cases.length)
    return result
}
function compare(cases) {
    const actual = native(cases)
    cases.forEach((c, i) => {
        const expected = original(c),
            value = actual[i]
        assert.equal(value.ok, true, JSON.stringify({ c, value }).slice(0, 2048))
        if (c.method === "parse") assert.deepEqual(value.value, expected.value, JSON.stringify(c.args))
        else {
            assert.deepEqual(value.typed, expected.typed, JSON.stringify(c.args))
            // Numerical authority above is the raw IEEE representation, including
            // own undefined/-0/nonfinite. This also checks JSON omission/nulls.
            assert.deepEqual(value.json, expected.json)
            for (const name of ["uidCalls", "nowCalls", "dateCalls"]) assert.equal(value[name], expected[name], name)
        }
        counts.comparisons++
        counts[c.method]++
    })
    return actual
}
const song = (lyrics, overrides = {}) =>
    fixture(
        "song",
        [
            { id: "key-1", keySignature: "D" },
            { lyrics, name: "Main" },
            { dateAdded: "2024-02-29T12:34:56.123Z", notes: "Song notes" },
            { title: "敬拜 Σ", artist: "Artist", keySignature: "E", seconds: 123.25 }
        ].map((v, i) => ({ ...v, ...overrides[i] }))
    )
test.after(() => console.log("Provider show source coverage:", JSON.stringify(counts)))

test("parseLyrics executes immutable branches, whitespace and empty-buffer quirks", () => {
    const values = [undefined, null, false, 0, -0, NaN, "", " \n\n ", "one\nline\n\nsecond\n\n\nthird", "[Verse 1]\nfirst\nline\n\nnext\n[Chorus2]\nlast", "[A]\n\n\n\n", "[A]\n[A]\n", "[A]\n\n", "[a]\r\nfirst\r\n[B]\nsecond", "[not a header\ntext", "[ 12 ]\n\n \n", "[A １２]\nline", "[A\u00a012]\nline", "[A]\n\u0085\n\uFEFF\nX", "[a]\n\n[a]\n\n[a]\n\n"]
    for (let n = 0; n < 50; n++) values.push(`[Verse ${n}]\n${"\n".repeat(n % 7)}a ${n}\n\nB\n[Chorus${n}]\n${" \n".repeat(n % 5)}c`)
    compare(values.map((s) => fixture("parse", [s])))
})
test("song slides/layouts preserve dedup collisions and later children replacement", () => {
    const cases = [song("plain"), song("[Verse1]\nA\n\nB\n[Verse2]\nA\n\nC"), song("[Verse1]\nA\n\nB\n[Verse2]\nA"), song("[Verse1]\nA\n\nA\n[Verse2]\nA\n\nB"), song("[A]\n\n\n\n[B]\n\n"), song("[ΟΣ]\nA\n[ΟΣΑ]\nB\n[İ]\nC\n[AΣ\u0301]\nD\n[e\u0301]\nE"), song(undefined), song("")]
    cases.push(song("[A]\nX\n\nY\n[B]\nZ\n\nQ"))
    cases.at(-1).uids = Array(20).fill("aaaaaaaaaaa")
    compare(cases)
})
test("source truthiness, interpolation, own undefined, raw values, IEEE fields and order", () => {
    const values = [undefined, null, false, true, 0, -0, NaN, Infinity, -Infinity, 1e-7, 1e21, 9007199254740992, "", "音楽", [], [null, undefined, "x"], { 2: "two", 1: "one", nested: true }]
    const cases = []
    for (const v of values) {
        cases.push(fixture("empty", [v, v, v, v]))
        cases.push(song("line", { 0: { id: v, keySignature: v }, 1: { name: v, freeShowId: v }, 2: { notes: v }, 3: { title: v, artist: v, keySignature: v, seconds: v } }))
    }
    compare(cases)
    compare([fixture("media", [{}, [{}]]), fixture("empty", [])])
})
test("media classification is case-sensitive, ordered and source-short-circuited", () => {
    const files = [{}, { name: "", url: "a.mp4" }, { url: "A.MP4" }, { url: "a.mp4?download=1" }, { url: "/externalVideos/download/1" }, { fileType: "Video", streamUrl: "https://example.invalid/stream" }, { fileType: "video", url: 123, loopVideo: "loop" }, { url: "photo.png", streamUrl: {}, name: { nested: true }, loopVideo: [false] }, { url: false, streamUrl: "video" }]
    compare([fixture("media", [{ itemType: "lessonSection", id: 42, label: "Slides 敬拜", description: "Notes", seconds: 1.25 }, files]), fixture("media", [{ itemType: "lessonAction", id: false, label: 0, description: false, seconds: Infinity }, []]), fixture("media", [{ label: "collision" }, files], { uids: Array(30).fill("aaaaaaaaaaa") })])
})
test("maximum admitted media file/UID sequence preserves every full source record", () => {
    compare([fixture("media", [{ id: "maximum", label: "Quota", ignored: { url: "never scanned" } }, Array.from({ length: 1024 }, (_, i) => ({ url: `file-${i}.mp4`, name: String(i) }))], { uids: uids(2049) })])
})
test("Date construction/TimeClip and fallback match actual source at precise boundaries", () => {
    const dates = [
        undefined,
        null,
        false,
        true,
        0,
        -0,
        0.99,
        -0.99,
        1.99,
        -1.99,
        NaN,
        Infinity,
        8640000000000000,
        -8640000000000000,
        8640000000000001,
        "",
        "invalid",
        "not a date",
        "1970-01-01",
        "1969-12-31T23:59:59.999Z",
        "0000-01-01",
        "2023-02-29",
        "2024-02-30",
        "2024-04-31",
        "2024-13-01",
        "2024-01-32",
        "2024-01-01T24:00Z",
        "2024-01-01T24:00:00.001Z",
        "2024-01-01T12:34Z",
        "2024-01-01T12:34:56.123456Z",
        "2024-01-01T00:00:00+23:59",
        "2024-01-01T00:00:00-23:59",
        "+275760-09-13T00:00:00Z",
        "-271821-04-20T00:00:00Z",
        "+275760-09-13T00:00:00.001Z"
    ]
    compare(dates.map((dateAdded) => song("", { 2: { dateAdded } })))
    compare([fixture("empty", ["id", "title", "notes", 0], { clock: [-0] }), fixture("media", [{ label: "special clock" }, []], { clock: [NaN] })])
})
test("legacy/local-zone/unsafe coercions are explicit unsupported, with source negative controls", () => {
    const unsupported = [song("", { 2: { dateAdded: "01/02/2024" } }), song("", { 2: { dateAdded: "2024-01-01T12:34:56" } }), song("", { 2: { dateAdded: "-000000-01-01" } }), song("", { 2: { dateAdded: [] } }), fixture("empty", [{ toString: "custom" }, "Title", "", 0]), fixture("media", [{}, [{ url: true }]]), fixture("parse", [true])]
    for (let i = 0; i < 4; i++) assert.ok(Number.isFinite(original(unsupported[i]).result.show.timestamps.created))
    for (const c of unsupported.slice(4))
        assert.throws(
            () => original(c),
            (error) => error?.name === "TypeError"
        )
    const result = native(unsupported)
    for (const r of result) assert.equal(r.error.code, "provider_shows_unsupported")
    counts.unsupported += result.length
})
test("bounded inputs reject before intermediate slide/UID/output growth", () => {
    const cases = [fixture("parse", ["x".repeat(1024 * 1024 + 1)]), fixture("parse", ["[A]\n" + "X\n\n".repeat(4097)]), fixture("media", [{}, Array(1025).fill({})]), fixture("empty", ["id", "\n".repeat(2 * 1024 * 1024), "", 0]), song("a".repeat(1024 * 1024), { 3: { title: "x".repeat(1024 * 1024) } })]
    for (const c of cases) {
        const [r] = native([c])
        assert.equal(r.error.code, "provider_shows_limit")
        counts.bounded++
    }
})
test("Unicode lower table remains the actual packaged Electron15.1 provenance, without NFC", () => {
    const oracle = JSON.parse(fs.readFileSync(path.join(root, "crates/freeshow-files/tests/unicode-oracle.json"), "utf8"))
    assert.equal(oracle.runtime.versions.icu, "74.2")
    assert.equal(oracle.runtime.versions.unicode, "15.1")
    assert.equal(oracle.sourceCommit, revision)
    assert.equal(fs.readFileSync(path.join(root, "crates/freeshow-provider-shows/src/case_table.rs"), "utf8"), fs.readFileSync(path.join(root, "crates/freeshow-files/src/case_table.rs"), "utf8"))
    // The table's scalar mappings come from the actual packaged Electron,
    // independent of the standalone Node22 VM's newer ICU tables.
    for (let offset = 0; offset < oracle.mappings.length; offset += 64) {
        const entries = oracle.mappings.slice(offset, offset + 64)
        const cases = entries.map(([cp]) => song(`[${String.fromCodePoint(cp)}]\nline`))
        const rows = native(cases)
        entries.forEach(([cp, lower], i) => {
            const show = rows[i].json.show
            assert.equal(Object.values(show.slides)[0].globalGroup, lower, cp.toString(16))
            counts.electronMappings++
        })
    }
    const contexts = oracle.contexts.filter(([text]) => !text.includes("\n") && text.normalize("NFC") === text && text.trim().replace(/\s*\d+$/, "") === text)
    for (let offset = 0; offset < contexts.length; offset += 64) {
        const entries = contexts.slice(offset, offset + 64),
            rows = native(entries.map(([text]) => song(`[${text}]\nline`)))
        entries.forEach(([text, expected], i) => {
            assert.equal(Object.values(rows[i].json.show.slides)[0].globalGroup, expected, JSON.stringify(text))
            counts.electronContexts++
        })
    }
    const [row] = native([song("[Ᲊ]\nline")])
    assert.equal(Object.values(row.json.show.slides)[0].globalGroup, "Ᲊ")
    assert.notEqual("Ᲊ".toLowerCase(), "Ᲊ", "standalone Node Unicode drift control")
})
