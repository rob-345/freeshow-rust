import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import vm from "node:vm"
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import { createRequire } from "node:module"
import { fileURLToPath, pathToFileURL } from "node:url"
import test from "node:test"
import ts from "typescript"
import upath from "upath"
import { clean, root, upstreamFunctions } from "./oracle.mjs"

// The source/OS boundary runner uses pinned standalone Node. Its Unicode 16
// tables differ from packaged Electron's ICU 74.2/Unicode 15.1, so filename
// folding is separately compared to the actual packaged-app corpus below.
if (process.version !== "v22.21.1" || process.versions.unicode !== "16.0") {
    const node = process.env.FREESHOW_ORACLE_NODE || path.join(os.homedir(), ".local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node")
    assert.ok(fs.existsSync(node), "Set FREESHOW_ORACLE_NODE to Node 22.21.1")
    const result = spawnSync(node, [fileURLToPath(import.meta.url)], { stdio: "inherit", env: process.env })
    process.exit(result.status ?? 1)
}

const binary = process.env.FREESHOW_FILES_PARITY || path.join(root, "target/files/debug/freeshow-files-parity")
assert.ok(fs.existsSync(binary), "Build freeshow-files-parity in target/files first")
function probe(input) {
    const result = spawnSync(binary, [], { input: JSON.stringify(input) + "\n", encoding: "utf8", maxBuffer: 64 * 1024 * 1024, timeout: 120000 })
    assert.equal(result.status, 0, result.stderr || result.error?.message)
    return JSON.parse(result.stdout)
}
function value(input) {
    const result = probe(input)
    assert.equal(result.ok, true, JSON.stringify(result))
    return result.value
}
function batch(cases) {
    return value({ op: "batch", cases }).map((result) => {
        assert.equal(result.ok, true, JSON.stringify(result))
        return result.value
    })
}
function declaration(filename, name) {
    const source = fs.readFileSync(path.join(root, filename), "utf8")
    const ast = ts.createSourceFile(filename, source, ts.ScriptTarget.Latest, true)
    for (const statement of ast.statements) {
        if (!ts.isVariableStatement(statement)) continue
        const found = statement.declarationList.declarations.find((node) => node.name.getText(ast) === name)
        if (found) return `const ${found.getText(ast)};`
    }
    throw new Error(`Missing upstream constant ${name}`)
}
const filesSource = "src/electron/utils/files.ts"
const thumbnailsSource = "src/electron/data/thumbnails.ts"
const sourceSizes = vm.runInNewContext(declaration(thumbnailsSource, "sizes") + "JSON.stringify(sizes)")
const sizes = JSON.parse(sourceSizes)
const hashOracle = upstreamFunctions(thumbnailsSource, ["filePathHashCode"])
const helpers = upstreamFunctions(filesSource, ["normalizeLocalPath", "normalizeFileNameForMatch", "fileNamesMatch", "getFileParentFolderId"], { fileURLToPath, upath, ...hashOracle })

// Execute the actual installed upath module with Node's other platform path
// implementation. This tests Windows lexical behavior without a fake algorithm.
const require = createRequire(import.meta.url)
const upathFile = require.resolve("upath")
function platformUpath(windows) {
    const module = { exports: {} }
    vm.runInNewContext(fs.readFileSync(upathFile, "utf8"), {
        module,
        exports: module.exports,
        require: (name) => (name === "path" ? (windows ? path.win32 : path.posix) : require(name))
    })
    return module.exports
}

function oracle(configured, dataRoot, cacheDir, initialCache = { unrelated: { keep: true }, replacedPaths: { old: "keep" } }, filesystem = fs) {
    const invalidations = []
    const writes = []
    const alerts = []
    const cache = structuredClone(initialCache)
    const boundaries = upstreamFunctions(filesSource, ["doesPathExistAsync", "getFileStatsAsync", "readFolderWithTypesAsync", "isPermissionDeniedError"], { fs: filesystem })
    const thumbnail = upstreamFunctions(
        thumbnailsSource,
        ["deleteThumbnails", "getThumbnailPath", "filePathHashCode"],
        {
            path,
            doesPathExist: fs.existsSync,
            deleteFile: (file) => fs.unlinkSync(file)
        },
        declaration(thumbnailsSource, "sizes") + `let thumbnailFolderPath = ${JSON.stringify(cacheDir)};`
    )
    const exists = upstreamFunctions(thumbnailsSource, ["doesMediaExist"], {
        ...boundaries,
        deleteThumbnails: (filePath) => {
            invalidations.push({ filePath, hash: thumbnail.filePathHashCode(filePath), sizes })
            thumbnail.deleteThumbnails(filePath)
        }
    })
    const functions = upstreamFunctions(
        filesSource,
        ["locateMediaFile", "normalizeLocalPath", "normalizeFileNameForMatch", "fileNamesMatch", "getFileParentFolderId", "asyncPool", "getMediaSyncFolderPath", "getDataFolderPath", "getDataFolderRoot", "createFolder", "makeDir", "doesPathExist"],
        {
            fs,
            path,
            upath,
            fileURLToPath,
            ...boundaries,
            ...exists,
            ...hashOracle,
            config: { get: () => dataRoot },
            getDefaultDataFolderRoot: () => {
                throw new Error("Host must supply the selected data root")
            },
            getStore: (id) => (id === "config" ? { mediaFolderPath: configured } : cache),
            _store: { CACHE_SYNC: "CACHE_SYNC" },
            setStore: (store, content) => writes.push({ store, content: clean(content) }),
            sendToMain: (...args) => alerts.push(args),
            ToMain: { ALERT: "ALERT" },
            actionComplete() {}
        },
        declaration(filesSource, "NESTED_SEARCH") + declaration(filesSource, "dataFolderNames")
    )
    return { ...functions, invalidations, writes, alerts, thumbnail }
}

test("pinned source path normalization, upath, URI decoding, UTF-16 hash and platform joins", () => {
    const cases = [
        "",
        ".",
        "..",
        "/",
        "//",
        "///",
        "//a",
        "//server/share",
        "//server/share/",
        "//server/share/foo",
        "/x//ab/../cd.png",
        "a/b/",
        "a//b",
        "C:",
        "C:/",
        "C:foo",
        "C:/foo",
        "C:\\Folder\\../Music\\Song.png",
        "é/日本語/🙌🏾",
        "file:///tmp/a%20b.png",
        "file://localhost/tmp/a%2520b.png",
        "file://remote/tmp/x",
        "file:///C:/Music/a%20b%2520c.mp4",
        "file://nas/share/a%23b.mp4",
        "file://xn--r8jz45g.xn--zckzah/share/a.mp4",
        "file://[::1]/share/a.mp4",
        "file://C:/Music/a.mp4",
        "file:///tmp/a%2fb",
        "file:///tmp/a%5cb",
        "file:///tmp/a?query#hash",
        "file:///tmp/%zz",
        "file:///tmp/%FF",
        "file://user:pw@host/x",
        "file://localhost:80/x",
        "FILE:///tmp/a%20b",
        "a%20b%23c%2Fc",
        "%3b%3B%3f%3F%40%26%3d%2b%24%2c",
        "%E6%97%A5%E6%9C%AC%E8%AA%9E",
        "bad%20name%zz",
        "%ED%A0%80",
        "%C0%AF",
        "%00",
        "AΣ",
        "AΣA",
        "A\u0345Σ",
        "\u0345Σ",
        "İ.JPG",
        "E\u0301.JPG",
        "😀".repeat(1000)
    ]
    for (const windows of [false, true]) {
        const platform = platformUpath(windows)
        const normalizer = upstreamFunctions(filesSource, ["normalizeLocalPath"], { fileURLToPath: (url) => fileURLToPath(url, { windows }) })
        const actual = batch(cases.map((filePath) => ({ op: "helpers", path: filePath, windows })))
        for (const [index, filePath] of cases.entries()) {
            const dirname = platform.dirname(filePath)
            assert.deepEqual(
                actual[index],
                {
                    normalized: normalizer.normalizeLocalPath(filePath),
                    hash: hashOracle.filePathHashCode(filePath),
                    basename: platform.basename(filePath),
                    dirname,
                    parentId: `${platform.basename(dirname)}_${hashOracle.filePathHashCode(dirname)}`,
                    folded: helpers.normalizeFileNameForMatch(filePath)
                },
                `${windows ? "win32" : "posix"} ${JSON.stringify(filePath)}`
            )
        }
        const parts = [[], [""], ["/", "a"], ["a", "/b"], ["a", "..", "b/"], ["C:", "a"], ["C:/", "../a"], ["//server/share", "../a"], ["//", "server", "share", "x"], ["///server", "share"], ["x\\y", "a"], ["", "a", ""], ["a", "../../b"], ["C:foo", "../"], ["NUL:", "../a"], ["foo", "COM1:", "../a"], ["//server//share", "foo"], ["//?/C:/", "foo", "..", "..", "a"]]
        const joined = batch(parts.map((parts) => ({ op: "join", parts, windows })))
        for (const [index, input] of parts.entries()) assert.equal(joined[index], (windows ? path.win32 : path.posix).join(...input), JSON.stringify({ windows, input }))
    }
})

test("seeded path corpus compares actual upath and native POSIX/Windows join semantics", () => {
    let seed = 0x6d656469
    const next = (max) => {
        seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
        return seed % max
    }
    const components = ["", ".", "..", "foo", "é", "日本語", "C:", "d:", "\\", "/", "//", "///", "a%20b", "file:", " a ", "🌍"]
    const corpus = []
    for (let index = 0; index < 1000; index++) corpus.push(Array.from({ length: next(6) }, () => components[next(components.length)]))
    for (const windows of [false, true]) {
        const platform = platformUpath(windows)
        const normalizer = upstreamFunctions(filesSource, ["normalizeLocalPath"], { fileURLToPath: (url) => fileURLToPath(url, { windows }) })
        const inputs = corpus.map((parts) => parts.join(next(2) ? "/" : "\\"))
        const actual = batch(inputs.map((filePath) => ({ op: "helpers", path: filePath, windows })))
        for (const [index, filePath] of inputs.entries()) {
            assert.equal(actual[index].dirname, platform.dirname(filePath), `dirname ${JSON.stringify({ windows, filePath })}`)
            assert.equal(actual[index].basename, platform.basename(filePath), `basename ${JSON.stringify({ windows, filePath })}`)
            assert.equal(actual[index].normalized, normalizer.normalizeLocalPath(filePath), `normalize ${JSON.stringify({ windows, filePath })}`)
        }
        const joined = batch(corpus.map((parts) => ({ op: "join", parts, windows })))
        for (const [index, parts] of corpus.entries()) assert.equal(joined[index], (windows ? path.win32 : path.posix).join(...parts), `join ${JSON.stringify({ windows, parts })}`)
    }
})

test("NFC and lowercase match actual packaged Electron for every scalar and contextual sigma", () => {
    const corpus = JSON.parse(fs.readFileSync(path.join(root, "crates/freeshow-files/tests/unicode-oracle.json"), "utf8"))
    const source = fs.readFileSync(path.join(root, filesSource), "utf8")
    const ast = ts.createSourceFile(filesSource, source, ts.ScriptTarget.Latest, true)
    const declaration = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "normalizeFileNameForMatch").getText(ast)
    assert.equal(corpus.sourceFunctionSha256, createHash("sha256").update(declaration).digest("hex"))
    assert.equal(corpus.runtime.version, "1.6.7-beta.2")
    assert.equal(corpus.runtime.versions.electron, "37.10.3")
    assert.equal(corpus.runtime.versions.icu, "74.2")
    assert.equal(corpus.runtime.versions.unicode, "15.1")
    assert.equal(corpus.sourceCommit, "2d94defcf98175e4f5c9504e7f4fd1d33c35c975")
    const foldedScalars = new Map(corpus.folded)
    const ranges = []
    for (let start = 0; start < 0x110000; start += 8192) ranges.push({ op: "foldRange", start, end: Math.min(start + 8192, 0x110000) })
    const actual = batch(ranges)
    for (const [index, range] of ranges.entries()) {
        const expected = []
        for (let cp = range.start; cp < range.end; cp++) if (cp < 0xd800 || cp > 0xdfff) expected.push(foldedScalars.get(cp) ?? String.fromCodePoint(cp))
        assert.deepEqual(actual[index], expected, `Unicode scalar range ${range.start.toString(16)}`)
    }
    const folded = batch(corpus.contexts.map(([text]) => ({ op: "helpers", path: text })))
    for (const [index, [text, expected]] of corpus.contexts.entries()) assert.equal(folded[index].folded, expected, JSON.stringify(text))
    // This must remain visibly different: Node's advertised version alone is
    // insufficient to select the application's Unicode behavior.
    assert.notEqual(helpers.normalizeFileNameForMatch(String.fromCodePoint(0x10d50)), foldedScalars.get(0x10d50) ?? String.fromCodePoint(0x10d50), "Garay Unicode 16 casing must not leak into packaged-app parity")
})

test("source locator on controlled real trees preserves search precedence, invalidations and cache writes", async (t) => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-locate-"))
    const unreadable = []
    t.after(() => {
        for (const folder of unreadable) fs.chmodSync(folder, 0o700)
        fs.rmSync(directory, { recursive: true, force: true })
    })
    let caseId = 0
    async function check(name, setup) {
        const base = path.join(directory, String(caseId++))
        fs.mkdirSync(base)
        const media = path.join(base, "media")
        const cache = path.join(base, "thumbs")
        fs.mkdirSync(media)
        fs.mkdirSync(cache)
        const dataRoot = path.join(base, "data")
        const request = { filePath: path.join(base, "missing", "Origin", "Song.mp4"), folders: [] }
        const state = { base, media, cache, dataRoot, configured: media, request, cacheState: undefined }
        const write = (file) => {
            fs.mkdirSync(path.dirname(file), { recursive: true })
            fs.writeFileSync(file, "real fixture bytes")
            return file
        }
        await setup(state, write)
        const source = oracle(state.configured, dataRoot, cache, state.cacheState)
        const normalized = helpers.normalizeLocalPath(request.filePath)
        const sync = path.join(state.configured || path.join(dataRoot, "Media"), helpers.getFileParentFolderId(normalized), upath.basename(normalized))
        for (const file of [normalized, sync]) for (const size of sizes) write(source.thumbnail.getThumbnailPath(file, size))
        const sourceReply = clean(await source.locateMediaFile(request))
        for (const invalidation of source.invalidations) for (const size of invalidation.sizes) assert.equal(fs.existsSync(source.thumbnail.getThumbnailPath(invalidation.filePath, size)), false)
        // The native service returns instructions; application of the cache
        // effects belongs to the context-validated ordered library owner.
        for (const file of [normalized, sync]) for (const size of sizes) write(source.thumbnail.getThumbnailPath(file, size))
        const actual = value({ op: "locate", configured: state.configured, dataRoot, request })
        assert.deepEqual(actual.reply, sourceReply, name)
        assert.deepEqual(actual.thumbnailInvalidations, source.invalidations, name + " invalidations")
        assert.ok(source.writes.length <= 1)
        if (source.writes.length) {
            const [write] = source.writes
            assert.equal(write.store, "CACHE_SYNC")
            assert.deepEqual(actual.replacement, { cacheKey: `${helpers.getFileParentFolderId(normalized)}_${upath.basename(normalized)}`, path: sourceReply.path }, name + " replacement")
            assert.deepEqual(write.content.unrelated, { keep: true })
            assert.equal(write.content.replacedPaths.old, "keep")
        } else assert.equal(actual.replacement, null, name + " no replacement")
        for (const file of [normalized, sync]) for (const size of sizes) assert.equal(fs.existsSync(source.thumbnail.getThumbnailPath(file, size)), true, "native service must not delete thumbnail files")
        assert.equal(actual.diagnostics.length, source.alerts.length)
        return { actual, state }
    }
    await check("existing original skips lazy mkdir", (s, write) => {
        s.configured = ""
        write(s.request.filePath)
    }).then(({ state }) => assert.equal(fs.existsSync(path.join(state.dataRoot, "Media")), false))
    await check("file URL and second URI decode", (s, write) => {
        const file = write(path.join(s.base, "日本語 é", "a b.mp4"))
        s.request.filePath = pathToFileURL(file.replace("a b", "a%20b")).href
    })
    await check("sync hashed folder has no cache replacement", (s, write) => {
        write(path.join(s.media, helpers.getFileParentFolderId(s.request.filePath), "Song.mp4"))
    })
    await check("media root before requested roots", (s, write) => {
        write(path.join(s.media, "song.MP4"))
        s.request.folders = [path.join(s.base, "later")]
        write(path.join(s.request.folders[0], "Song.mp4"))
    })
    await check("requested roots in order", (s, write) => {
        s.request.folders = [path.join(s.base, "first"), path.join(s.base, "second")]
        for (const folder of s.request.folders) write(path.join(folder, "Song.mp4"))
    })
    await check("same parent shortcut before current directory", (s, write) => {
        write(path.join(s.media, "Origin", "Song.mp4"))
        write(path.join(s.media, "Song.mp4"))
    })
    await check("current directory before nested", (s, write) => {
        write(path.join(s.media, "song.mp4"))
        write(path.join(s.media, "nested", "Song.mp4"))
    })
    await check("NFC and case matching", (s, write) => {
        s.request.filePath = path.join(s.base, "absent", "É日本語🙌🏾.MP4")
        write(path.join(s.media, "e\u0301日本語🙌🏾.mp4"))
    })
    await check("Greek contextual final sigma", (s, write) => {
        s.request.filePath = path.join(s.base, "absent", "ΟΣ.mp4")
        write(path.join(s.media, "ος.MP4"))
    })
    await check("missing retains both invalidations", () => {})
    await check("fallback mkdir failure still searches later root", (s, write) => {
        s.configured = ""
        write(s.dataRoot)
        s.request.folders = [path.join(s.base, "later")]
        write(path.join(s.request.folders[0], "Song.mp4"))
    })
    await check("disabled cached replacement ignored", (s, write) => {
        const cached = write(path.join(s.base, "cache-hit", "Song.mp4"))
        s.cacheState = { unrelated: { keep: true }, replacedPaths: { old: "keep", [`${helpers.getFileParentFolderId(s.request.filePath)}_Song.mp4`]: cached } }
    })
    await check("depth eight included", (s, write) => {
        write(path.join(s.media, ...Array(7).fill("nested"), "Song.mp4"))
    })
    await check("depth nine excluded", (s, write) => {
        write(path.join(s.media, ...Array(8).fill("nested"), "Song.mp4"))
    })
    await check("level eight parent shortcut reaches child", (s, write) => {
        write(path.join(s.media, ...Array(7).fill("nested"), "Origin", "Song.mp4"))
    })
    await check("directory itself satisfies initial access", (s) => {
        fs.mkdirSync(s.request.filePath, { recursive: true })
    })
    await check("directory itself satisfies parent shortcut", (s) => {
        fs.mkdirSync(path.join(s.media, "Origin", "Song.mp4"), { recursive: true })
    })
    if (process.platform !== "win32") {
        await check("symlink directory loop not recursed", (s) => {
            fs.symlinkSync(s.media, path.join(s.media, "loop"))
        })
        await check("broken named symlink follows source entry matching", (s) => {
            fs.symlinkSync(path.join(s.base, "nonexistent"), path.join(s.media, "song.mp4"))
        })
        await check("explicit parent candidate follows symlink", (s, write) => {
            const other = path.join(s.base, "other")
            write(path.join(other, "Song.mp4"))
            fs.symlinkSync(other, path.join(s.media, "Origin"))
        })
        if (process.getuid?.() !== 0) {
            await check("unreadable root skipped for next root", (s, write) => {
                const denied = path.join(s.media, "denied")
                write(path.join(denied, "Song.mp4"))
                fs.chmodSync(denied, 0)
                unreadable.push(denied)
                s.request.folders = [path.join(s.base, "readable")]
                write(path.join(s.request.folders[0], "Song.mp4"))
            })
        }
    }
})

test("concurrent duplicate nested source winners depend on OS completion order", async (t) => {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-duplicate-"))
    t.after(() => fs.rmSync(base, { recursive: true, force: true }))
    const media = path.join(base, "media")
    for (const folder of ["A", "B"]) {
        fs.mkdirSync(path.join(media, folder), { recursive: true })
        fs.writeFileSync(path.join(media, folder, "Song.mp4"), "fixture")
    }
    const request = { filePath: path.join(base, "missing", "Origin", "Song.mp4"), folders: [] }
    for (const slow of ["A", "B"]) {
        // Only delay an OS callback, retaining real files/Dirents and the
        // untouched source's asyncPool. Both children have already started.
        const filesystem = {
            ...fs,
            readdir(folder, options, callback) {
                fs.readdir(folder, options, (error, entries) => {
                    if (folder === path.join(media, slow)) setTimeout(() => callback(error, entries), 50)
                    else callback(error, entries)
                })
            }
        }
        const source = oracle(media, base, base, undefined, filesystem)
        assert.equal((await source.locateMediaFile(request)).path, path.join(media, slow, "Song.mp4"), "later completion overwrites source found")
    }
    const actual = value({ op: "locate", configured: media, dataRoot: base, request })
    assert.equal(actual.reply.path, path.join(media, "A", "Song.mp4"), "native sequential policy is deterministic; no universal source duplicate-winner claim")
})

test("GET_MEDIA_FOLDER_PATH uses configured path as-is and source fallback mkdir behavior", async (t) => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-folder-"))
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }))
    for (const configured of ["", "relative/é/../folder", path.join(directory, "configured-missing")]) {
        const dataRoot = path.join(directory, configured ? "unused" : "default")
        const source = oracle(configured, dataRoot, directory)
        const expected = source.getMediaSyncFolderPath()
        const actual = value({ op: "folder", configured, dataRoot })
        assert.equal(actual.reply, expected)
        if (configured) assert.equal(fs.existsSync(configured), false)
        else assert.equal(fs.statSync(expected).isDirectory(), true)
    }
    const blocked = path.join(directory, "blocked")
    fs.writeFileSync(blocked, "file")
    const source = oracle("", blocked, directory)
    const expected = source.getMediaSyncFolderPath()
    const actual = value({ op: "folder", configured: "", dataRoot: blocked })
    assert.equal(actual.reply, expected)
    assert.equal(source.alerts.length, 1)
    assert.equal(actual.diagnostics.length, 1)
})

test("invalid input, cancellation, deadline and explicit resource limits never produce synthetic hits", (t) => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-bounds-"))
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }))
    for (let index = 0; index < 10; index++) fs.writeFileSync(path.join(directory, String(index)), "fixture")
    const base = { op: "locate", root: { path: directory, createIfMissing: false }, request: { filePath: "/missing/Origin/Song.mp4", folders: [] } }
    for (const [extra, code] of [
        [{ cancelled: true }, "files_cancelled"],
        [{ deadlineMs: 0 }, "files_deadline"],
        [{ limits: { entries: 1 } }, "files_limit"],
        [{ limits: { workBytes: 1 } }, "files_limit"],
        [{ limits: { operations: 1 } }, "files_limit"],
        [{ request: { filePath: "a\u0000b", folders: [] } }, "files_invalid"],
        [{ request: { filePath: 42, folders: [] } }, "files_invalid"],
        [{ request: { filePath: "a", folders: [null] } }, "files_invalid"]
    ]) {
        const actual = probe({ ...base, ...extra })
        assert.equal(actual.ok, false)
        assert.equal(actual.error.code, code)
        if (extra.limits?.entries) assert.equal(actual.error.thumbnailInvalidations.length, 2, "completed missing probes survive budget failure")
    }
})
