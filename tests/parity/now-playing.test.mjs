import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import vm from "node:vm"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import ts from "typescript"
import { parseFile } from "music-metadata"
const root = fileURLToPath(new URL("../../", import.meta.url)),
    revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const binary = process.env.FREESHOW_NOW_PLAYING_PARITY || path.join(root, "target/debug/freeshow-now-playing-parity" + (process.platform === "win32" ? ".exe" : ""))
function original(file) {
    const r = spawnSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", timeout: 10000 })
    assert.equal(r.status, 0, r.stderr)
    return r.stdout
}
const source = original("src/electron/audio/nowPlaying.ts"),
    fileSource = original("src/electron/utils/files.ts")
const fileAst = ts.createSourceFile("files.ts", fileSource, ts.ScriptTarget.Latest, true)
const openInSystem = fileAst.statements
    .find((n) => ts.isFunctionDeclaration(n) && n.name.text === "openInSystem")
    .getText(fileAst)
    .replace("export ", "")
const coverage = { cases: 0, realFiles: 0, pictures: 0, specialDurations: 0, sourceExceptions: 0, unsupportedMetadata: 0 }
test.after(() => console.log("Now-playing source coverage:", JSON.stringify(coverage)))
const normal = (v) => (v === undefined ? null : ArrayBuffer.isView(v) ? [...v] : Array.isArray(v) ? v.map(normal) : v && typeof v === "object" ? Object.fromEntries(Object.entries(v).map(([k, v]) => [k, normal(v)])) : v)
async function reference(c) {
    const trace = [],
        exports = {},
        data = structuredClone(c.data),
        folder = c.folder || "/fixture/Audio"
    if (c.specialDuration) data.duration = { NaN, Infinity, "-Infinity": -Infinity, "-0": -0 }[c.specialDuration]
    let metadata = structuredClone(c.metadata ?? null)
    if (metadata?.picture) for (const p of metadata.picture) p.data = Buffer.from(p.data)
    const image = {
        toPNG() {
            trace.push(["image.png"])
            if (c.fail === "png") throw Error("controlled image failure")
            return Buffer.from(c.image?.png || [])
        },
        getSize() {
            trace.push(["image.size"])
            return { width: c.image?.width || 0, height: c.image?.height || 0 }
        },
        resize(size) {
            trace.push(["image.resize", normal(size)])
            return {
                toJPEG(quality) {
                    trace.push(["image.jpeg", quality])
                    if (c.fail === "jpeg") throw Error("controlled JPEG failure")
                    return Buffer.from(c.image?.jpeg || [])
                }
            }
        }
    }
    const exists = (p) => {
        trace.push(["exists", p])
        return !!c.exists
    }
    const files = {
        getDataFolderPath: (id) => {
            trace.push(["folder", id])
            return folder
        },
        doesPathExist: exists,
        deleteFile: (p) => {
            trace.push(["delete", p])
            return !c.fsFalse
        },
        writeFile: (p, value) => {
            trace.push(["write", p, normal(value)])
            return !c.fsFalse
        },
        openInSystem: null
    }
    const sendToMain = (channel, value) => trace.push(channel === "AUDIO_METADATA" ? ["metadata", normal(value)] : ["alert", value])
    const injected = { doesPathExist: exists, sendToMain, ToMain: { ALERT: "ALERT" }, shell: { showItemInFolder: (p) => trace.push(["reveal", p]) } }
    files.openInSystem = new vm.Script(ts.transpileModule(`(function(){${openInSystem};return openInSystem})()`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).runInNewContext(injected)
    const modules = {
        electron: {
            nativeImage: {
                createFromBuffer(bytes) {
                    trace.push(["image.create", [...bytes]])
                    if (c.fail === "create") throw Error("controlled missing native image")
                    return image
                }
            }
        },
        "music-metadata": {
            parseFile: async (file) => {
                trace.push(["parse", file])
                return c.useAudio ? parseFile(file) : metadata === null ? Promise.reject(Error("controlled parser error")) : { common: metadata }
            }
        },
        path: c.platform === "win32" ? path.win32 : path.posix,
        "../../types/IPC/ToMain": { ToMain: { AUDIO_METADATA: "AUDIO_METADATA" } },
        "../IPC/main": { sendToMain },
        "../utils/files": files,
        "./IcecastSender": { IcecastSender: { updateMetadata: (title) => trace.push(["icecast", title]) } }
    }
    const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText
    new vm.Script(`(function(require,exports){${js}})`).runInNewContext({ Buffer, console: { error() {} } })((id) => {
        assert.ok(Object.hasOwn(modules, id), id)
        return modules[id]
    }, exports)
    let mediaPath = null,
        threw = false
    try {
        if (!c.command || c.command === "set") await exports.setPlayingState(data)
        else if (c.command === "unset") exports.unsetPlayingAudio()
        else if (c.command === "open") exports.openNowPlaying()
        else mediaPath = exports.getNowPlayingMediaPath()
    } catch {
        threw = true
        coverage.sourceExceptions++
    }
    return { trace, format: data?.format ?? null, formatOwn: !!data && Object.hasOwn(data, "format"), mediaPath, threw }
}
async function compare(cases) {
    const expected = []
    for (const c of cases) expected.push(await reference(c))
    const r = spawnSync(binary, [], { cwd: root, input: JSON.stringify(cases), encoding: "utf8", timeout: 30000, maxBuffer: 24 * 1024 * 1024 })
    assert.equal(r.status, 0, r.stderr)
    const actual = JSON.parse(r.stdout)
    actual.forEach((a, i) => {
        coverage.cases++
        assert.deepEqual({ trace: a.trace, format: a.format, formatOwn: a.formatOwn, mediaPath: a.mediaPath, threw: a.threw }, expected[i], JSON.stringify(cases[i]))
        if (cases[i].metadata?.picture) coverage.pictures++
    })
    return actual
}
const data = (fields = {}) => ({ filePath: "/trusted/音楽 🎵.wav", name: "filename", unknownLang: ["Unbekannt", "Inconnu", "未知"], format: "{artist}|{title}|{album}|{year}|{artwork_path}|{artwork_base64}|{duration}|{duration_s}", duration: 123.987, ...fields })
test("immutable source artist fallback, trimming, case-sensitive dedup, unknown fields and input mutation", async () => {
    await compare([
        { data: data(), metadata: { artists: [" A ", "A", "a", "\ufeffB\u00a0", " ", "A"], title: "T", album: "Album", year: 2025, unknown: { keep: [1, 2] } } },
        ...[{}, { artist: "   ", albumartist: "Ignored" }, { albumartist: "Album artist" }, { originalartist: "Original" }, { artists: [], artist: "fallback" }, { artists: ["", null], artist: "ignored" }].map((metadata) => ({ data: data({ format: "", name: "" }), metadata })),
        { data: data({ unknownLang: [], name: "", format: null }), metadata: null },
        { data: data({ unknownLang: ["", "", ""] }), metadata: { year: 0 } },
        { data: data({ format: false }), metadata: { year: false, title: 42, album: ["A", "B"] } }
    ])
})
test("ordered replaceAll insertion and actual JS dollar substitution syntax remain exact", async () => {
    const cases = []
    for (const value of ["{title} {album} {year}", "{artist}", "$$", "$&", "$`", "$'", "$1 $<name>", "Unicode日本語{duration}"]) {
        cases.push({ data: data({ format: "prefix:{artist}/{artist}:suffix", duration: 61 }), metadata: { artist: value, title: "{artist} & title", album: "{year} $$", year: 2030 } })
    }
    await compare(cases)
})
test("duration floor/remainder/pad/math and nonfinite structured-clone numbers", async () => {
    const cases = [0, -0, 0.1, 59.99, 60, 3600, -0.1, -1, -59, -60, -61, 1e21].map((duration) => ({ data: data({ duration }), metadata: {} }))
    for (const specialDuration of ["NaN", "Infinity", "-Infinity", "-0"]) {
        coverage.specialDurations++
        cases.push({ data: data(), metadata: {}, specialDuration })
    }
    await compare(cases)
    await compare(["0x10", "0b11", "0o17", "+Infinity", "inf", "NaN", "bad", " ", ".5", "-1e2", true, false, [], [1], {}, null].map((duration) => ({ data: data({ duration }), metadata: {} })))
})
test("cover presence, empty buffers, PNG-before-size, bounds/resize rounding and deliberately JPEG mime", async () => {
    const cases = []
    for (const [width, height] of [
        [0, 0],
        [640, 480],
        [641, 1],
        [640, 481],
        [2000, 1]
    ]) {
        cases.push({
            data: data(),
            metadata: {
                picture: [
                    { format: "image/png", description: "cover", data: [137, 80, 78, 71] },
                    { format: "image/jpeg", data: [9, 8] }
                ]
            },
            image: { width, height, png: [1, 2, 3], jpeg: [255, 216, 255] }
        })
    }
    cases.push({ data: data(), metadata: { picture: [{ format: "image/png", data: [] }] }, image: { width: 0, height: 0, png: [] } })
    cases.push({ data: data({ format: "unchanged literal" }), metadata: {}, exists: true, fsFalse: true })
    const actual = await compare(cases)
    assert.ok(actual[0].format.includes("data:image/jpeg;base64,iVBORw=="))
    assert.ok(actual[5].format.includes("data:image/jpeg;base64,"))
})
test("real source partial effects survive image failure and malformed late metadata/data errors", async () => {
    await compare([
        ...["create", "png", "jpeg"].map((fail) => ({ data: data(), metadata: { picture: [{ data: [1] }] }, image: { width: 1000, height: 1000, png: [1], jpeg: [2] }, fail })),
        { data: data({ unknownLang: null }), metadata: {} },
        { data: data({ unknownLang: null, format: "literal" }), metadata: { artist: "A", title: "T", album: "B" } },
        { data: data({ duration: null }), metadata: {} },
        { data: data({ format: true }), metadata: {} },
        { data: data(), metadata: { artists: [5] } },
        { data: data({ filePath: "" }), metadata: { artist: "never parsed" }, exists: true }
    ])
})
test("UNSET, OPEN actual reveal-vs-alert helper, internal media path and original IPC names", async () => {
    const cases = []
    for (const platform of ["linux", "win32"]) for (const command of ["unset", "open", "path"]) for (const exists of [false, true]) cases.push({ command, platform, folder: platform === "win32" ? "C:\\基盤\\Audio" : "/基盤/Audio", exists, fsFalse: true })
    await compare(cases)
    await compare([...["/", "/a/../音/./Audio/", "/path\\"].map((folder) => ({ command: "path", folder })), ...["C:/a/../音/Audio/", "\\\\server\\share\\Audio\\.."].map((folder) => ({ command: "path", platform: "win32", folder }))])
    const handlers = original("src/electron/IPC/responsesMain.ts")
    assert.match(handlers, /\[Main\.NOW_PLAYING\]: \(data\) => setPlayingState\(data\)/)
    assert.match(handlers, /\[Main\.NOW_PLAYING_UNSET\]: \(\) => unsetPlayingAudio\(\)/)
    assert.match(handlers, /\[Main\.OPEN_NOW_PLAYING\]: \(\) => openNowPlaying\(\)/)
    assert.match(original("src/electron/servers.ts"), /app\.get\("\/nowplayingcover"/)
})
test("own indexed language values and missing duration preserve source fallback/error timing", async () => {
    const missing = data()
    delete missing.duration
    await compare([...[[], [false, 0, null], [[1], {}, []], { 0: "A", 1: "T", 2: "B" }, "ABC", false].map((unknownLang) => ({ data: data({ name: "", unknownLang }), metadata: {} })), { data: missing, metadata: {} }, { data: data(), metadata: { artists: { length: 2 } } }])
})
test("actual music-metadata WAV INFO/no-cover fixtures flow through existing native audio service", async (t) => {
    const folder = fs.mkdtempSync(path.join(os.tmpdir(), "now-playing-source-"))
    t.after(() => fs.rmSync(folder, { recursive: true, force: true }))
    function chunk(id, bytes) {
        const size = Buffer.alloc(4)
        size.writeUInt32LE(bytes.length)
        return Buffer.concat([Buffer.from(id), size, bytes, bytes.length % 2 ? Buffer.alloc(1) : Buffer.alloc(0)])
    }
    const format = Buffer.alloc(16)
    format.writeUInt16LE(1)
    format.writeUInt16LE(1, 2)
    format.writeUInt32LE(48000, 4)
    format.writeUInt32LE(96000, 8)
    format.writeUInt16LE(2, 12)
    format.writeUInt16LE(16, 14)
    const cases = []
    for (const [name, info] of [
        ["空.wav", []],
        [
            "タグ 🎵.wav",
            [
                ["IART", "Artist"],
                ["INAM", "Title"],
                ["IPRD", "Album"],
                ["ICRD", "2025"]
            ]
        ]
    ]) {
        const body = Buffer.concat([Buffer.from("WAVE"), chunk("fmt ", format), ...(info.length ? [chunk("LIST", Buffer.concat([Buffer.from("INFO"), ...info.map(([k, v]) => chunk(k, Buffer.from(v + "\0")))]))] : []), chunk("data", Buffer.alloc(960))]),
            n = Buffer.alloc(4)
        n.writeUInt32LE(body.length)
        const file = path.join(folder, name)
        fs.writeFileSync(file, Buffer.concat([Buffer.from("RIFF"), n, body]))
        const parsed = await parseFile(file)
        assert.equal(parsed.common.picture, undefined)
        coverage.realFiles++
        cases.push({ data: data({ filePath: file }), useAudio: true })
    }
    const missing = path.join(folder, "missing.wav"),
        corrupt = path.join(folder, "corrupt.wav")
    fs.writeFileSync(corrupt, "not audio")
    cases.push({ data: data({ filePath: missing }), useAudio: true }, { data: data({ filePath: corrupt }), useAudio: true })
    await compare(cases)
    const info = chunk("LIST", Buffer.concat([Buffer.from("INFO"), chunk("YEAR", Buffer.from("invalid\0"))]))
    const body = Buffer.concat([Buffer.from("WAVE"), chunk("fmt ", format), info, chunk("data", Buffer.alloc(960))]),
        size = Buffer.alloc(4)
    size.writeUInt32LE(body.length)
    const invalid = path.join(folder, "nonfinite-year.wav")
    fs.writeFileSync(invalid, Buffer.concat([Buffer.from("RIFF"), size, body]))
    assert.ok(Number.isNaN((await parseFile(invalid)).common.year))
    const fixture = { data: data({ filePath: invalid }), useAudio: true },
        sourceResult = await reference(fixture)
    assert.equal(sourceResult.threw, false)
    assert.ok(sourceResult.format.includes("NaN"))
    const rust = spawnSync(binary, [], { cwd: root, input: JSON.stringify([fixture]), encoding: "utf8", timeout: 30000, maxBuffer: 4 * 1024 * 1024 })
    assert.equal(rust.status, 0, rust.stderr)
    assert.equal(JSON.parse(rust.stdout)[0].error.code, "now_playing_ambiguous_year")
    coverage.unsupportedMetadata++
})
test("actual source write-file comparison distinguishes UTF8 strings from Buffer payloads", () => {
    const declaration = fileAst.statements
        .find((n) => ts.isFunctionDeclaration(n) && n.name.text === "fileContentMatches")
        .getText(fileAst)
        .replace("export ", "")
    const factory = new vm.Script(ts.transpileModule(`(function(){${declaration};return fileContentMatches})()`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText)
    for (const existing of ["", "same", "Unicode日本語"]) {
        const fn = factory.runInNewContext({ doesPathExist: () => true, readFile: () => existing })
        assert.equal(fn(existing, "native-path"), true)
        assert.equal(fn(Buffer.from(existing), "native-path"), false)
    }
    const absent = factory.runInNewContext({ doesPathExist: () => false, readFile: () => assert.fail("absent read") })
    assert.equal(absent("", "native-path"), false)
})
test("actual source file helpers preserve malformed UTF8 skips, truncation and caught filesystem failures", (t) => {
    const folder = fs.mkdtempSync(path.join(os.tmpdir(), "now-playing-files-source-"))
    t.after(() => fs.rmSync(folder, { recursive: true, force: true }))
    const names = ["doesPathExist", "readFile", "safeBufferToString", "fileContentMatches", "writeFile", "deleteFile"]
    const declarations = names
        .map((name) =>
            fileAst.statements
                .find((node) => ts.isFunctionDeclaration(node) && node.name.text === name)
                .getText(fileAst)
                .replace(/^export\s+/, "")
        )
        .join("\n")
    const code = ts.transpileModule(`const MAX_NODE_STRING_LENGTH=0x1fffffe8;\n${declarations}\nreturn {readFile,writeFile,deleteFile};`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
    let writes = 0
    const errors = [],
        nativeFs = {
            ...fs,
            writeFileSync(...args) {
                writes++
                return fs.writeFileSync(...args)
            }
        }
    const helper = new Function("fs", "actionComplete", "Buffer", "console", code)(nativeFs, (...args) => errors.push(args), Buffer, {
        error(...args) {
            errors.push(args)
        }
    })
    const file = path.join(folder, "NowPlaying.txt")
    const corpus = [
        [[0xef, 0xbb, 0xbf, 65], "\uFEFFA"],
        [[0xff, 0xfe, 65], "\uFFFD\uFFFDA"],
        [[0xe2, 0x82], "\uFFFD"],
        [[0xed, 0xa0, 0x80], "\uFFFD\uFFFD\uFFFD"],
        [[0xf0, 0x9f, 0x92, 65], "\uFFFDA"],
        [[0xc0, 0xaf], "\uFFFD\uFFFD"],
        [[65, 0, 66], "A\0B"]
    ]
    for (const [bytes, text] of corpus) {
        fs.writeFileSync(file, Buffer.from(bytes))
        assert.equal(helper.readFile(file), text)
        assert.equal(helper.writeFile(file, text), false)
        assert.deepEqual([...fs.readFileSync(file)], bytes)
    }
    assert.equal(writes, 0)
    const cover = path.join(folder, "NowPlayingCover.png")
    assert.equal(helper.writeFile(cover, Buffer.from([1, 2, 3, 4])), true)
    const descriptor = fs.openSync(cover, "r")
    try {
        assert.equal(helper.writeFile(cover, Buffer.from([8, 9])), true)
        assert.deepEqual([...fs.readFileSync(descriptor)], [8, 9])
        assert.equal(helper.writeFile(cover, Buffer.from([8, 9])), true)
    } finally {
        fs.closeSync(descriptor)
    }
    assert.equal(writes, 3)
    const directory = path.join(folder, "directory")
    fs.mkdirSync(directory)
    assert.equal(helper.writeFile(directory, ""), false)
    assert.equal(writes, 3, "existing directory read is empty, so empty text skips")
    assert.equal(helper.writeFile(directory, "nonempty"), false)
    assert.equal(helper.deleteFile(directory), false)
    assert.ok(errors.length >= 2)
})
