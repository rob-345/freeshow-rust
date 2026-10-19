import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import vm from "node:vm"
import v8 from "node:v8"
import ts from "typescript"
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import { root, upstreamFunctions, clean } from "./oracle.mjs"
const binary = process.env.FREESHOW_BROWSE_BIN || path.join(root, "target/debug", process.platform === "win32" ? "freeshow-browse.exe" : "freeshow-browse")
const { hydrateFileBrowse } = upstreamFunctions("src/frontend/rust/fileStats.ts", ["hydrateFileBrowse"])
const keys = ["dev", "mode", "nlink", "uid", "gid", "rdev", "blksize", "ino", "size", "blocks", "atimeMs", "mtimeMs", "ctimeMs", "birthtimeMs"]
function variables(filename, names) {
    const ast = ts.createSourceFile(filename, fs.readFileSync(path.join(root, filename), "utf8"), ts.ScriptTarget.Latest, true)
    return names
        .map((name) =>
            ast.statements
                .find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText(ast) === name))
                .getText(ast)
                .replace(/^export\s+/, "")
        )
        .join("\n")
}
function constants() {
    const context = vm.createContext({})
    new vm.Script(ts.transpileModule(variables("src/electron/data/media.ts", ["imageExtensions", "videoExtensions"]) + "\nglobalThis.result={imageExtensions,videoExtensions};", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).runInContext(context)
    return context.result
}
function oracle() {
    const calls = []
    const functions = upstreamFunctions("src/electron/utils/files.ts", ["readFolderContent", "getFileStatsAsync", "readFolderAsync", "asyncPool", "getFileInfo", "getFileStats", "getExtension", "isMedia", "getFirstMediaFiles", "getSimularPaths", "getParentFolderName", "similarity", "readFolder", "isPermissionDeniedError"], {
        fs,
        path,
        join: path.join,
        parse: path.parse,
        ...constants(),
        actionComplete() {},
        createThumbnail(input, size = 250) {
            calls.push({ input, size })
            return `oracle-thumbnail:${input}`
        }
    })
    return { ...functions, calls }
}
const request = (command, data) => ({ command, data })
function rust(cases, args = []) {
    const result = spawnSync(binary, args, { input: cases.map((item) => JSON.stringify(item)).join("\n") + "\n", encoding: "utf8", timeout: 30000, maxBuffer: 32 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return result.stdout.trim().split("\n").map(JSON.parse)
}
function temporary(t) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-browse-parity-"))
    t.after(() => fs.rmSync(dir, { recursive: true, force: true }))
    return dir
}
function write(dir, name, content = "fixture") {
    const file = path.join(dir, name)
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, content)
    return file
}
function freezeDirectoryAccess(folder) {
    for (const entry of fs.readdirSync(folder, { withFileTypes: true })) if (entry.isDirectory()) freezeDirectoryAccess(path.join(folder, entry.name))
    const stat = fs.statSync(folder)
    fs.utimesSync(folder, new Date("2035-01-01T00:00:00Z"), stat.mtime)
}
function tree(t) {
    const dir = temporary(t)
    const file = write(dir, "a/音楽 🙌.MP3")
    write(dir, "a/nested/one.wav")
    write(dir, "a/nested/deep/two.flac")
    write(dir, "a/nested/deep/more/three.ogg")
    write(dir, "a/readme.txt")
    write(dir, "a/plain/no-media.txt")
    fs.mkdirSync(path.join(dir, "a/empty"))
    write(dir, "b/2.txt")
    write(dir, "b/1.txt")
    freezeDirectoryAccess(dir)
    return { dir, file, a: path.join(dir, "a"), b: path.join(dir, "b") }
}
function sorted(value) {
    if (Array.isArray(value)) return value.map(sorted)
    if (value && typeof value === "object")
        return Object.fromEntries(
            Object.entries(value)
                .sort(([a], [b]) => a.localeCompare(b))
                .map(([key, value]) => [key, sorted(value)])
        )
    return value
}
function shape(data) {
    return Object.fromEntries(
        Object.entries(data)
            .filter(([, entry]) => entry.isFolder)
            .map(([key, entry]) => [key, { ownNoMedia: Object.hasOwn(entry, "noMedia"), noMediaType: typeof entry.noMedia }])
    )
}
async function compare(cases) {
    const actual = rust(cases)
    for (const [index, item] of cases.entries()) {
        const original = oracle()
        const handler = { FILE_INFO: original.getFileInfo, READ_FOLDER: original.readFolderContent, GET_SIMILAR: original.getSimularPaths }[item.command]
        const expected = v8.deserialize(v8.serialize(await handler(structuredClone(item.data))))
        assert.equal(actual[index].error, undefined, JSON.stringify(actual[index].error))
        const hydrated = hydrateFileBrowse(item.command, actual[index].data)
        assert.deepEqual(sorted(clean(hydrated)), sorted(clean(expected)), JSON.stringify(item))
        if (item.command === "READ_FOLDER") assert.deepEqual(shape(hydrated), shape(expected))
    }
    return actual
}
const linux = process.platform === "linux"

test("real folder trees preserve source depths, raw paths, files arrays and noMedia/capture ternary semantics", { skip: !linux && "Linux filesystem contract" }, async (t) => {
    const { dir, a, b } = tree(t)
    const cases = [undefined, -1, 0, 0.5, 1, 2, 3].flatMap((depth) => [false, true].map((captureFolderContent) => request("READ_FOLDER", { path: a, ...(depth === undefined ? {} : { depth }), captureFolderContent })))
    cases.push(request("READ_FOLDER", { path: [a, b, a], depth: 0 }), request("READ_FOLDER", { path: [path.join(dir, "missing"), "", path.join(a, "readme.txt")] }), request("READ_FOLDER", { path: [] }), request("READ_FOLDER", { path: a + "/../a/", depth: 0 }))
    const result = await compare(cases)
    assert.ok(Object.values(result[0].data).some((entry) => entry.isFolder && entry.files.length))
    const captured = result[cases.findIndex((item) => item.data.depth === 1 && item.data.captureFolderContent)].data
    assert.equal(Object.hasOwn(captured[path.join(a, "nested/deep")], "noMedia"), false)
    assert.deepEqual(captured[path.join(a, "nested/deep")].files, [])
    assert.equal(captured[path.join(a, "plain")].noMedia, true)
})

test("FILE_INFO numeric fields, negative and fractional timestamps, directories, special files and symlink following match stat IPC", { skip: !linux && "Linux statx contract" }, async (t) => {
    const { dir, a, file } = tree(t)
    fs.utimesSync(file, new Date(-100123), new Date(-50654))
    fs.chmodSync(file, 0o640)
    const fractional = write(dir, "fractional.wav")
    fs.utimesSync(fractional, 123.123456, 456.654321)
    const link = path.join(dir, "link.mp3"),
        folderLink = path.join(dir, "folder-link"),
        dangling = path.join(dir, "dangling")
    fs.symlinkSync(file, link)
    fs.symlinkSync(a, folderLink)
    fs.symlinkSync(path.join(dir, "missing"), dangling)
    const fifo = path.join(dir, "fifo")
    const made = spawnSync("mkfifo", [fifo], { encoding: "utf8", timeout: 10000 })
    assert.equal(made.status, 0, made.stderr)
    const result = await compare([file, a, link, folderLink, dangling, fifo, "", path.join(dir, "missing"), fractional, "/dev/null"].map((file) => request("FILE_INFO", file)))
    for (const item of result.filter((item) => item.data)) {
        assert.deepEqual(Object.keys(item.data.stat), keys)
        for (const key of keys) assert.equal(typeof item.data.stat[key], "number")
        for (const key of ["atime", "mtime", "ctime", "birthtime", "isFile"]) assert.equal(Object.hasOwn(item.data.stat, key), false)
    }
    assert.equal(result[0].data.stat.mtimeMs, -50654)
    assert.notEqual(result[0].data.stat.ctimeMs, result[0].data.stat.mtimeMs)
    assert.equal(result[3].data.folder, true)
    assert.equal(result[4].data, null)
    assert.notEqual(result[8].data.stat.mtimeMs, Math.trunc(result[8].data.stat.mtimeMs))
    await compare([request("READ_FOLDER", { path: [folderLink], depth: 1 })])
})

test("GET_SIMILAR preserves UTF-16 scores, candidate/parent order, asymmetric duplicate suppression, directories and stable top10", { skip: !linux && "Linux path ordering contract" }, async (t) => {
    const dir = temporary(t),
        first = path.join(dir, "first"),
        second = path.join(dir, "second")
    fs.mkdirSync(first)
    fs.mkdirSync(second)
    for (const name of ["Song-1.jpg", "Song-10.png", "Song-1.png", "Song.jpg", "song-1.png", "Song-2.png", "Song-alt.png", "Song-11.jpg", "Song-12.jpg", "Song-13.jpg", "Song-14.jpg", "Song-15.jpg", "Song-16.jpg", "Song-17.jpg", "Song-18.jpg", "Song-19.jpg", "🙌.jpg", "😀.png", "test.jpg", "test\uE000a.png", "test😀.png"]) write(first, name)
    fs.mkdirSync(path.join(first, "Song-3"))
    fs.mkdirSync(path.join(second, "Song-3"))
    for (const name of ["Song-1.png", "Song-4.jpg", "Song-2.jpg"]) write(second, name)
    const cases = [[path.join(first, "Song-1.jpg")], [path.join(first, "🙌.jpg")], [path.join(first, "test.jpg")], [path.join(first, "Song-1.jpg"), path.join(second, "Song-4.jpg")], [path.join(second, "Song-4.jpg"), path.join(first, "Song-1.jpg")], [path.join(first, "missing.jpg")], [path.join(second, "Song-1.png")], [], [""]].map((paths) => request("GET_SIMILAR", { paths }))
    const result = await compare(cases)
    assert.ok(result[6].data.some((item) => item.path === path.join(second, "Song-3")))
    assert.equal(result[0].data.length, 10)
    assert.ok(result[1].data.some((item) => item.name === "😀"))
    assert.deepEqual(
        result[2].data.map((item) => item.name),
        ["test\uE000a", "test😀"]
    )
})

test("thumbnail options expose exact original request intents without pretending generation succeeded", { skip: !linux && "Linux listing contract" }, async (t) => {
    const dir = temporary(t)
    const all = [...constants().imageExtensions, ...constants().videoExtensions]
    for (const [index, extension] of all.entries()) write(dir, `${index}.${extension.toUpperCase()}`)
    const original = oracle()
    await original.readFolderContent({ path: dir, generateThumbnails: true })
    const actual = rust([request("READ_FOLDER", { path: dir, generateThumbnails: true })])[0]
    assert.equal(actual.error.code, "thumbnail_unavailable")
    assert.equal(actual.data, undefined)
    assert.deepEqual(
        actual.error.thumbnailIntents.map(({ input, size }) => ({ input, size })).sort((a, b) => a.input.localeCompare(b.input)),
        original.calls.sort((a, b) => a.input.localeCompare(b.input))
    )
    const child = path.join(dir, "child")
    fs.mkdirSync(child)
    for (const name of ["1.png", "2.mp4", "3.jpg", "4.png", "5.png", "6.mp3"]) write(child, name)
    const captured = oracle()
    await captured.readFolderContent({ path: dir, captureFolderContent: true, depth: 0 })
    const rejected = rust([request("READ_FOLDER", { path: dir, captureFolderContent: true })])[0]
    assert.equal(rejected.error.code, "thumbnail_unavailable")
    assert.equal(rejected.error.thumbnailIntents.length, 4)
    assert.deepEqual(rejected.error.thumbnailIntents.map((item) => item.input).sort(), captured.calls.map((item) => item.input).sort())
    await compare([request("READ_FOLDER", { path: path.join(dir, "empty"), generateThumbnails: true }), request("READ_FOLDER", { path: child, depth: -1, generateThumbnails: true })])
})

test("hydration restores only explicit generated folder noMedia null placeholders, preserving unknown fields and stats", () => {
    const stat = { ...Object.fromEntries(keys.map((key) => [key, 1])), future: null }
    const data = { normal: { isFolder: true, files: [], noMedia: null, future: null }, empty: { isFolder: true, files: [] }, populated: { isFolder: true, noMedia: true }, file: { isFolder: false, noMedia: null, stats: stat }, unknown: null }
    assert.equal(hydrateFileBrowse("FILE_INFO", data), data)
    assert.equal(data.normal.noMedia, null)
    assert.equal(hydrateFileBrowse("READ_FOLDER", data), data)
    assert.equal(Object.hasOwn(data.normal, "noMedia"), true)
    assert.equal(data.normal.noMedia, undefined)
    assert.equal(Object.hasOwn(data.empty, "noMedia"), false)
    assert.equal(data.file.noMedia, null)
    assert.equal(data.file.stats, stat)
    assert.equal(stat.future, null)
    assert.equal(data.normal.future, null)
})

test("permission denied and invalid request/resource variants stay explicit", { skip: !linux && "Linux permission contract" }, async (t) => {
    const dir = temporary(t),
        denied = path.join(dir, "denied")
    fs.mkdirSync(denied)
    write(denied, "secret.txt")
    fs.chmodSync(denied, 0)
    try {
        if (process.getuid?.() !== 0) await compare([request("READ_FOLDER", { path: denied }), request("FILE_INFO", path.join(denied, "secret.txt")), request("GET_SIMILAR", { paths: [path.join(denied, "secret.txt")] })])
    } finally {
        fs.chmodSync(denied, 0o700)
    }
    const result = rust([request("OTHER", {}), request("READ_FOLDER", { path: dir, depth: 17 }), request("FILE_INFO", "a".repeat(32769)), request("FILE_INFO", "bad\0path"), request("GET_SIMILAR", { paths: Array(65).fill("") }), request("READ_FOLDER", { path: [1] }), request("READ_FOLDER", { path: dir, depth: "2" })])
    assert.deepEqual(
        result.map((item) => item.error.code),
        ["unsupported_command", "browse_resource_limit", "browse_resource_limit", "browse_resource_limit", "browse_resource_limit", "invalid_request", "invalid_request"]
    )
})

const electronBin = process.env.FREESHOW_ELECTRON_BIN || path.join(os.homedir(), ".local/share/freeshow-electron-baseline/v1.6.7-beta.2/extracted/opt/FreeShow/freeshow")
const packaged = linux && fs.existsSync(electronBin)
if (process.env.FREESHOW_TEST_PACKAGED_BROWSE === "1")
    test("actual packaged Electron IPC confirms numeric Stats, own undefined noMedia, symlinks and real-tree replies", async (t) => {
        assert.ok(packaged, "Required packaged Electron oracle unavailable")
        const { dir, a, file } = tree(t)
        fs.utimesSync(file, new Date(-100123), new Date(-50654))
        fs.chmodSync(file, 0o640)
        fs.symlinkSync(file, path.join(a, "linked.mp3"))
        write(a, "音楽 😀.mp3")
        const cases = [request("FILE_INFO", file), request("FILE_INFO", a), request("FILE_INFO", path.join(dir, "missing")), request("READ_FOLDER", { path: a }), request("READ_FOLDER", { path: a, depth: 1, captureFolderContent: true }), request("GET_SIMILAR", { paths: [file] })]
        const result = spawnSync("xvfb-run", ["-a", "dbus-run-session", "--", process.execPath, path.join(root, "tests/parity/file-browse-electron.mjs")], { env: { ...process.env, FREESHOW_ELECTRON_BIN: electronBin }, input: JSON.stringify(cases), encoding: "utf8", timeout: 90000, maxBuffer: 8 * 1024 * 1024 })
        assert.equal(result.error, undefined)
        assert.equal(result.status, 0, result.stderr)
        const receipt = JSON.parse(result.stdout.split("\n").find((line) => line.startsWith('{"executablePath"')))
        assert.equal(receipt.temporaryRemoved, true)
        assert.equal(receipt.runtime.versions.node, "22.21.1")
        assert.equal(receipt.runtime.versions.electron, "37.10.3")
        assert.ok(receipt.results.at(-1).data.length > 0, "Packaged GET_SIMILAR must exercise nonempty candidates")
        const actual = rust(cases)
        for (const [index, expected] of receipt.results.entries()) {
            assert.equal(actual[index].error, undefined)
            const hydrated = hydrateFileBrowse(cases[index].command, actual[index].data)
            assert.deepEqual(sorted(clean(hydrated)), sorted(clean(expected.data)))
            if (cases[index].command === "READ_FOLDER") assert.deepEqual(shape(hydrated), expected.folderShapes)
            for (const stat of expected.statShapes) {
                assert.deepEqual(stat.keys, keys)
                assert.equal(stat.prototype, "Object")
                assert.equal(stat.methods, "undefined")
                assert.ok(stat.dates.every((item) => !item.own && item.type === "undefined" && !item.date))
            }
        }
        if (process.env.FREESHOW_BROWSE_RECEIPT) {
            fs.mkdirSync(path.dirname(process.env.FREESHOW_BROWSE_RECEIPT), { recursive: true })
            fs.writeFileSync(process.env.FREESHOW_BROWSE_RECEIPT, JSON.stringify(receipt, null, 2) + "\n")
        }
    })

test("pure source similarity preserves UTF-16 arithmetic on every supported build platform", () => {
    const source = upstreamFunctions("src/electron/utils/files.ts", ["similarity"]).similarity
    const words = ["", "a", "A", "ab", "ba", "abc", "Song-1", "song-1", "🙌", "😀", "a🙌", "a😀", "敬拜", "敬拜 🙌", "café", "café"]
    const cases = words.flatMap((left) => words.map((right) => ({ left, right })))
    const actual = rust(cases, ["--similarity"])
    for (const [index, item] of cases.entries()) assert.deepEqual(actual[index].data, clean(source(item.left, item.right)))
})

if (!["linux", "darwin", "win32"].includes(process.platform))
    test("unverified filesystem platforms return explicit unsupported status", () => {
        const results = rust([request("FILE_INFO", "missing"), request("READ_FOLDER", { path: [] }), request("GET_SIMILAR", { paths: [] })])
        assert.ok(results.every((item) => item.error.code === "unsupported_platform"))
    })

if (process.platform === "win32")
    test("Windows file information and empty folder/similarity requests preserve actual source results", async () => {
        const [info, folders, similar] = await compare([request("FILE_INFO", ""), request("READ_FOLDER", { path: [] }), request("GET_SIMILAR", { paths: [] })])
        assert.equal(info.data, null)
        assert.deepEqual(folders.data, {})
        assert.deepEqual(similar.data, [])
    })

test("committed packaged IPC evidence records immutable source declarations with normalized line endings", () => {
    const fixture = JSON.parse(fs.readFileSync(path.join(root, "tests/parity/fixtures/file-browse-ipc.json"), "utf8"))
    const filename = "src/electron/utils/files.ts"
    const ast = ts.createSourceFile(filename, fs.readFileSync(path.join(root, filename), "utf8"), ts.ScriptTarget.Latest, true)
    for (const [name, expected] of Object.entries(fixture.sourceSha256)) {
        const declaration = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === name)
        assert.ok(declaration)
        assert.equal(createHash("sha256").update(declaration.getText(ast).replace(/\r\n/g, "\n")).digest("hex"), expected)
    }
    assert.equal(fixture.profileIsolation, true)
    assert.equal(fixture.temporaryProfileRemoved, true)
    assert.equal(fixture.runtime.electron, "37.10.3")
    assert.equal(fixture.runtime.node, "22.21.1")
    assert.deepEqual(fixture.nonemptyReplies, { FILE_INFO: true, READ_FOLDER: true, GET_SIMILAR: true })
    assert.deepEqual(fixture.statShape.keys, keys)
    assert.ok(fixture.statShape.dates.every((item) => !item.own && !item.date && item.type === "undefined"))
})

test("Linux invalid-byte names, dangling links and bounded directory symlink cycles match real source trees", { skip: !linux && "Linux filename contract" }, async (t) => {
    const dir = temporary(t)
    write(dir, "visible.txt")
    const invalid = Buffer.concat([Buffer.from(dir + "/"), Buffer.from([0xff]), Buffer.from(".txt")])
    fs.writeFileSync(invalid, "invalid filename bytes")
    fs.symlinkSync(path.join(dir, "missing"), path.join(dir, "broken.PNG"))
    fs.symlinkSync(dir, path.join(dir, "cycle"))
    freezeDirectoryAccess(dir)
    await compare([request("READ_FOLDER", { path: dir, depth: 1 }), request("READ_FOLDER", { path: dir, depth: 1, captureFolderContent: true }), request("GET_SIMILAR", { paths: [path.join(dir, "�.tx")] })])
})
