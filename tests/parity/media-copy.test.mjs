import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import vm from "node:vm"
import { spawnSync } from "node:child_process"
import { createRequire } from "node:module"
import { createHash } from "node:crypto"
import { test } from "node:test"
import ts from "typescript"
import { root, clean } from "./oracle.mjs"
const require = createRequire(import.meta.url),
    upath = require("upath")
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const binary = process.env.FREESHOW_MEDIA_COPY_BIN || path.join(root, "target/debug", process.platform === "win32" ? "freeshow-media-copy.exe" : "freeshow-media-copy")
const linux = process.platform === "linux",
    sources = new Map()
function ast(file) {
    if (!sources.has(file)) {
        const result = spawnSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 4 * 1024 * 1024 })
        assert.equal(result.status, 0, result.stderr)
        sources.set(file, ts.createSourceFile(file, result.stdout.replace(/\r\n/g, "\n"), ts.ScriptTarget.Latest, true))
    }
    return sources.get(file)
}
function declaration(file, name, variable = false) {
    const source = ast(file)
    const node = source.statements.find((node) => (variable ? ts.isVariableStatement(node) && node.declarationList.declarations.some((v) => v.name.getText(source) === name) : ts.isFunctionDeclaration(node) && node.name?.text === name))
    assert.ok(node, `Pinned ${name} missing`)
    return node.getText(source).replace(/^export\s+/, "")
}
function handler(name) {
    const source = ast("src/electron/IPC/responsesMain.ts")
    const response = source.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((v) => v.name.getText(source) === "mainResponses"))
    return response.declarationList.declarations
        .find((v) => v.name.getText(source) === "mainResponses")
        .initializer.properties.find((p) => p.name?.getText(source) === `[Main.${name}]`)
        .initializer.getText(source)
}
function pureUpath() {
    // Run the installed pinned upath2 source with its sole path boundary set to
    // Linux POSIX, allowing pure Linux naming tests on unsupported OS hosts.
    const exports = {},
        context = vm.createContext({
            exports,
            require: (id) => {
                assert.equal(id, "path")
                return path.posix
            }
        })
    new vm.Script(fs.readFileSync(require.resolve("upath"), "utf8")).runInContext(context)
    return exports
}
function oracle(item, posix = false, copyFile = fs.copyFile) {
    const copies = [],
        folders = [],
        errors = [],
        alerts = [],
        opened = [],
        candidates = [],
        pending = []
    const boundary = Object.create(fs)
    boundary.copyFile = (source, target, callback) => {
        copies.push({ source, target })
        copyFile(source, target, callback)
    }
    boundary.mkdirSync = (folder, options) => {
        folders.push(folder)
        return fs.mkdirSync(folder, options)
    }
    const file = "src/electron/utils/files.ts"
    const code = [
        ...["MAX_NODE_STRING_LENGTH", "dataFolderNames", "currentlyBundling"].map((n) => declaration(file, n, true)),
        ...["bundleMediaFiles", "addToMediaFolder", "getFileParentFolderId", "readFile", "readFolder", "safeBufferToString", "parseShow", "parseJSON", "doesPathExist", "doesPathExistAsync", "getDataFolderPath", "getMediaSyncFolderPath", "createFolder", "makeDir", "copyFileAsync", "asyncPool", "actionComplete", "isPermissionDeniedError", "openInSystem"].map((n) => declaration(file, n)),
        declaration("src/electron/data/thumbnails.ts", "filePathHashCode"),
        `const sourceCopy=addToMediaFolder; addToMediaFolder=(...args)=>{candidates.push([...args[0]]);const promise=sourceCopy(...args);pending.push(promise);return promise;};globalThis.result={BUNDLE_MEDIA_FILES:(${handler("BUNDLE_MEDIA_FILES")}),MEDIA_FOLDER_COPY:(${handler("MEDIA_FOLDER_COPY")}),getFileParentFolderId,filePathHashCode};`
    ].join("\n")
    const context = vm.createContext({
        fs: boundary,
        path: posix ? path.posix : path,
        upath: posix ? pureUpath() : upath,
        candidates,
        pending,
        getDataFolderRoot: () => item.dataRoot,
        getStore: (id) => ({ PROJECTS: item.stores.projects, OVERLAYS: item.stores.overlays, TEMPLATES: item.stores.templates, config: { mediaFolderPath: item.syncRoot || "" } })[id],
        sendToMain: (channel, data) => alerts.push({ channel, data }),
        ToMain: { ALERT: "ALERT" },
        shell: {
            openPath: async (path) => {
                opened.push(path)
                return ""
            },
            showItemInFolder: (path) => opened.push(path)
        },
        console: { error: (...args) => errors.push(args) }
    })
    new vm.Script(ts.transpileModule(code, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).runInContext(context, { timeout: 1000 })
    return { ...context.result, copies, folders, errors, alerts, opened, candidates, pending }
}
function temporary(t) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-media-copy-"))
    t.after(() => fs.rmSync(dir, { recursive: true, force: true }))
    return dir
}
function write(dir, name, content = "fixture") {
    const file = path.join(dir, name)
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, content)
    return file
}
function request(command, dir, data = {}) {
    return { command, dataRoot: path.join(dir, "data"), syncRoot: path.join(dir, "media"), stores: { projects: { projects: {} }, overlays: {}, templates: {} }, data }
}
function rust(cases, args = []) {
    const result = spawnSync(binary, args, { input: cases.map(JSON.stringify).join("\n") + "\n", encoding: "utf8", timeout: 30000, maxBuffer: 32 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return result.stdout.trim().split("\n").map(JSON.parse)
}
async function compare(item) {
    const [native] = rust([item])
    if (linux && native.plan) for (const staged of native.staged) assert.equal(fs.existsSync(path.join(native.plan.destinationRoot, staged.destination)), false)
    const source = oracle(item)
    const reply = await source[item.command](item.data)
    await Promise.all(source.pending)
    if (!linux) {
        assert.equal(native.error.code, "unsupported_platform")
        return { native, source, reply }
    }
    assert.equal(native.error, undefined, JSON.stringify(native.error))
    const plan = native.plan
    assert.deepEqual(plan.candidates, clean(source.candidates[0] || []))
    if (item.command === "BUNDLE_MEDIA_FILES") {
        assert.equal(reply, undefined)
        assert.equal(plan.deferredReply.kind, "noReply")
    } else {
        assert.equal(plan.deferredReply.kind, "mediaFolderCopy")
        assert.equal(plan.deferredReply.changed_if_effects_resolved, reply)
    }
    for (const staged of native.staged) {
        const target = path.join(plan.destinationRoot, staged.destination)
        const expected = fs.readFileSync(target)
        assert.equal(staged.hex, expected.toString("hex"))
        const install = plan.effects.find((e) => e.kind === "install" && e.file.destination === staged.destination)
        assert.equal(install.file.sha256, createHash("sha256").update(expected).digest("hex"))
    }
    assert.equal(plan.diagnostics.filter((d) => d.source_error_logged).length + plan.omittedDiagnostics, source.errors.length)
    return { native, source, reply }
}

test("actual upath parent IDs and UTF16 hashes include separators, dot segments, UNC, emoji and sign", () => {
    const paths = ["", ".", "..", "/", "//", "///", "file", "./file", "a//b/file/", "/a/../b/file", "C:\\folder\\clip.mp4", "\\\\server\\share\\file", "///server///share/file", "/敬拜/🙌/movie.mp4", "/😀/x", "/\uE000/x", "negative-hash-path-123456", "/a\\b\\c.txt"]
    assert.equal(require("upath/package.json").version, "2.0.1")
    const source = oracle({}, true),
        result = rust(
            paths.map((path) => ({ path })),
            ["--paths"]
        )
    for (const [index, name] of paths.entries()) {
        assert.equal(result[index].parentId, source.getFileParentFolderId(name), name)
        assert.equal(result[index].hash, source.filePathHashCode(name), name)
    }
    assert.ok(result.some((v) => v.hash.startsWith("i")))
})

test("MEDIA_FOLDER_COPY streams opaque real bytes, keeps existing destinations and source prefix-skip/missing/duplicate quirks", async (t) => {
    const dir = temporary(t),
        data = request("MEDIA_FOLDER_COPY", dir)
    const first = write(dir, "敬拜/🙌/clip.bin", Buffer.from([0, 255, 10, 128, 1])),
        second = write(dir, "second/clip.bin", "second"),
        exists = write(dir, "existing/same.bin", "new"),
        prefix = write(dir, "media-sibling/should-skip.bin", "source prefix quirk")
    const source = oracle(data),
        target = write(data.syncRoot, path.join(source.getFileParentFolderId(exists), "same.bin"), "preexisting different bytes")
    data.data.paths = [first, first, second, prefix, path.join(dir, "missing"), "", exists, "https://invalid.local/private", "file://" + first]
    const { native, reply } = await compare(data)
    assert.equal(reply, true)
    assert.equal(fs.readFileSync(target, "utf8"), "preexisting different bytes")
    if (linux) {
        assert.equal(native.staged.length, 2)
        assert.ok(native.plan.observations.some((o) => o.kind === "duplicateDestination"))
        assert.ok(native.plan.observations.some((o) => o.kind === "prefixSkipped"))
        assert.ok(native.plan.effects.some((e) => e.kind === "skipExisting"))
    }
})

test("BUNDLE typed extraction uses Show order, JS integer keys/truthiness, project types, overlays/templates and ordered Set", async (t) => {
    const dir = temporary(t),
        data = request("BUNDLE_MEDIA_FILES", dir, { openFolder: true })
    const files = Array.from({ length: 12 }, (_, index) => write(dir, `sources/${index}.bin`, `opaque ${index}`))
    // Deliberately textual numeric-key insertion order differs from JS Object.values.
    const media = `{"10":{"path":${JSON.stringify(files[1])}},"2":{"path":"","id":${JSON.stringify(files[0])}},"z":{"path":false,"id":${JSON.stringify(files[2])}}}`
    write(data.dataRoot, "Shows/a.SHOW", `["id",{"media":${media},"slides":{"10":{"items":[{"type":"media","src":${JSON.stringify(files[4])}}]},"2":{"items":[null,{"type":"text","src":"ignore"},{"type":"media","src":${JSON.stringify(files[3])}}]}}}]`)
    write(data.dataRoot, "Shows/b.show", JSON.stringify(["id", { media: { a: { path: files[0] }, b: { path: 0, id: files[5] } }, future: { src: "do-not-scan" } }]))
    write(data.dataRoot, "Shows/.show", JSON.stringify(["id", { slides: { one: { items: [{ type: "media", src: files[6] }] } } }]))
    write(data.dataRoot, "Shows/not.show.txt", JSON.stringify(["id", { media: { a: { path: "never" } } }]))
    data.stores.projects.projects = { z: { shows: [null, { id: files[7], type: "image" }, { id: "skip-show" }, { id: "skip-folder", type: "folder" }, { id: files[8], type: "ppt" }, { id: files[7], type: "audio" }, { id: "skip-uppercase", type: "VIDEO" }] } }
    data.stores.overlays = {
        one: {
            items: [
                { type: "media", src: files[9] },
                { type: "media", src: "" }
            ]
        }
    }
    data.stores.templates = {
        one: {
            items: [
                { type: "media", src: files[10] },
                { type: "media", src: files[11] }
            ]
        }
    }
    const { native, source } = await compare(data)
    assert.equal(source.opened.length, 1)
    if (linux) {
        assert.deepEqual(native.plan.candidates, [files[6], files[0], files[1], files[2], files[3], files[4], files[5], ...files.slice(7)])
        assert.equal(native.plan.openFolderIntent, true)
        assert.equal(native.staged.length, 12)
    }
})

test("bundle source JSON repair, BOM/invalid UTF8/ciphertext, empty/default roots and raw local backslashes remain bounded/source-driven", async (t) => {
    const dir = temporary(t),
        data = request("BUNDLE_MEDIA_FILES", dir)
    const content = write(dir, linux ? "sources/C:\\folder\\clip.bin" : "sources/opaque.bin", "opaque protected bytes")
    write(data.dataRoot, "Shows/repaired.show", JSON.stringify(["id", { media: { a: { path: content } } }]) + "trailing junk")
    write(data.dataRoot, "Shows/bom.show", Buffer.from('\uFEFF["id",{}]'))
    write(data.dataRoot, "Shows/encrypted.show", "v1:opaque-ciphertext-with-no-json")
    write(data.dataRoot, "Shows/invalid.show", Buffer.from([0xff, 0xfe, 0x80]))
    write(data.dataRoot, "Shows/empty.show", "")
    const { native } = await compare(data)
    if (linux) {
        assert.deepEqual(native.plan.candidates, [content])
        assert.equal(native.plan.diagnostics.filter((d) => d.code === "source_show_parse_error").length, 3)
    }
    const emptyDir = temporary(t),
        empty = request("BUNDLE_MEDIA_FILES", emptyDir, { openFolder: true })
    fs.mkdirSync(path.join(empty.dataRoot, "Shows"), { recursive: true })
    const { native: none, source } = await compare(empty)
    assert.equal(source.opened.length, 0)
    if (linux) {
        assert.equal(none.plan.openFolderIntent, false)
        assert.equal(none.staged.length, 0)
        assert.equal(
            none.plan.effects.some((e) => e.kind === "ensureDestinationRoot"),
            false
        )
    }
})

test("configured versus default Media roots, approved custom bundle output and early custom-folder open alert follow source", async (t) => {
    for (const custom of [false, true]) {
        const dir = temporary(t),
            file = write(dir, "source/media", "bytes"),
            data = request("BUNDLE_MEDIA_FILES", dir, { openFolder: true })
        write(data.dataRoot, "Shows/test.show", JSON.stringify(["id", { media: { a: { id: file } } }]))
        if (custom) {
            data.data.outputPath = path.join(dir, "approved custom")
            data.approvedRoot = data.data.outputPath
        }
        const { native, source } = await compare(data)
        if (linux) assert.equal(native.plan.destinationRoot, custom ? data.approvedRoot : path.join(data.dataRoot, "Media"))
        assert.equal(source.alerts.length, custom ? 1 : 0)
        if (custom) assert.deepEqual(source.alerts, [{ channel: "ALERT", data: "This does not exist!" }])
    }
    const dir = temporary(t),
        data = request("MEDIA_FOLDER_COPY", dir, { paths: [] })
    delete data.syncRoot
    const { native } = await compare(data)
    if (linux) assert.ok(native.plan.effects.some((e) => e.kind === "ensureDestinationRoot"))
    const rawDir = temporary(t),
        raw = request("MEDIA_FOLDER_COPY", rawDir)
    const inside = write(raw.syncRoot, "inside.bin", "literal prefix semantics")
    raw.syncRoot += "/./"
    raw.data = { paths: [inside] }
    const rawResult = await compare(raw)
    assert.equal(rawResult.reply, true)
    const emptyDir = temporary(t),
        configuredFile = request("MEDIA_FOLDER_COPY", emptyDir, { paths: [] })
    configuredFile.syncRoot = write(emptyDir, "configured-file", "must not be inspected/created for empty input")
    const emptyResult = await compare(configuredFile)
    assert.equal(emptyResult.reply, false)
})

test("ordinary directory/copy-permission failures retain changed-before-copy boolean, with no fake staged install", async (t) => {
    const dir = temporary(t),
        folder = path.join(dir, "source-folder")
    fs.mkdirSync(folder)
    const data = request("MEDIA_FOLDER_COPY", dir, { paths: [folder] })
    const { native, source, reply } = await compare(data)
    assert.equal(reply, true)
    assert.equal(source.copies.length, 1)
    if (linux) {
        assert.equal(native.staged.length, 0)
        assert.ok(native.plan.effects.some((e) => e.kind === "copyAttemptFailure"))
    }
    if (linux && process.getuid() !== 0) {
        const denied = write(dir, "private/denied", "secret")
        fs.chmodSync(denied, 0)
        const other = request("MEDIA_FOLDER_COPY", dir, { paths: [denied] })
        const result = await compare(other)
        assert.equal(result.reply, true)
        assert.equal(result.native.staged.length, 0)
    }
})

test("missing Shows root is a host intent, and context output overrides/resource/unsupported variants do not become successful ACKs", (t) => {
    const dir = temporary(t),
        data = request("BUNDLE_MEDIA_FILES", dir)
    const [missing] = rust([data])
    assert.equal(fs.existsSync(data.dataRoot), false)
    if (linux) {
        assert.equal(missing.plan.deferredReply.kind, "noReply")
        assert.ok(missing.plan.effects.some((e) => e.kind === "ensureShowsRoot"))
        assert.equal(missing.plan.diagnostics[0].source_error_logged, false)
    }
    const file = write(dir, "source", "binary")
    write(data.dataRoot, "Shows/test.show", JSON.stringify(["id", { media: { a: { path: file } } }]))
    data.data.outputPath = path.join(dir, "not frozen")
    const oversized = path.join(dir, "oversized")
    const fd = fs.openSync(oversized, "w")
    fs.ftruncateSync(fd, 32 * 1024 * 1024 + 1)
    fs.closeSync(fd)
    const cases = [data, request("MEDIA_FOLDER_COPY", dir, { paths: [oversized] }), request("MEDIA_FOLDER_COPY", dir, { paths: ["x".repeat(32769)] }), request("MEDIA_FOLDER_COPY", dir, { paths: [3] }), request("OTHER", dir)]
    const result = rust(cases)
    const codes = [linux ? "invalid_context" : "unsupported_platform", linux ? "copy_resource_limit" : "unsupported_platform", "copy_resource_limit", "unsupported_variant", "unsupported_command"]
    for (const [index, code] of codes.entries()) {
        assert.equal(result[index].error.code, code)
        assert.equal(Object.hasOwn(result[index], "plan"), false)
    }
})

test("actual extraction null/items-shape errors stay explicit failures rather than empty successful bundles", (t) => {
    for (const show of [{ media: { bad: null } }, { slides: { bad: { items: {} } } }]) {
        const dir = temporary(t),
            data = request("BUNDLE_MEDIA_FILES", dir)
        write(data.dataRoot, "Shows/malformed.show", JSON.stringify(["id", show]))
        const source = oracle(data)
        assert.throws(() => source.BUNDLE_MEDIA_FILES(data.data), { name: "TypeError" })
        const [native] = rust([data])
        assert.equal(native.error.code, linux ? "source_extraction_error" : "unsupported_platform")
        assert.equal(Object.hasOwn(native, "plan"), false)
    }
})

test("controlled real colliding copy completions prove the source scheduling gap from deterministic native coalescing", { timeout: 15000 }, async (t) => {
    for (const order of [
        [0, 1],
        [1, 0]
    ]) {
        const dir = temporary(t),
            first = write(dir, "Aa/common/same.bin", "first"),
            second = write(dir, "BB/common/same.bin", "second"),
            data = request("MEDIA_FOLDER_COPY", dir, { paths: [first, second] })
        const held = []
        let started
        const bothStarted = new Promise((resolve) => {
            started = resolve
        })
        const source = oracle(data, false, (from, to, callback) => {
            held.push({ from, to, callback })
            if (held.length === 2) started()
        })
        assert.equal(source.getFileParentFolderId(first), source.getFileParentFolderId(second))
        const [native] = rust([data])
        const finished = source.MEDIA_FOLDER_COPY(data.data)
        // Hold only the actual filesystem call boundary until both original
        // copy decisions are made. Complete each real fs.copyFile in a known
        // order; do not assume an unrestricted overwrite race has only two
        // possible byte strings (truncate/write operations can interleave).
        await bothStarted
        assert.equal(held.length, 2)
        assert.equal(held[0].to, held[1].to)
        for (const index of order) {
            const copy = held.find((copy) => copy.from === [first, second][index])
            await new Promise((resolve, reject) =>
                fs.copyFile(copy.from, copy.to, (error) => {
                    copy.callback(error)
                    if (error) reject(error)
                    else resolve()
                })
            )
        }
        assert.equal(await finished, true)
        await Promise.all(source.pending)
        assert.equal(source.copies.length, 2)
        assert.equal(source.errors.length, 0)
        const output = fs.readFileSync(held[0].to, "utf8")
        assert.equal(output, ["first", "second"][order[1]])
        if (linux) {
            assert.equal(native.staged.length, 1)
            assert.equal(Buffer.from(native.staged[0].hex, "hex").toString(), "first")
            assert.ok(native.plan.observations.some((o) => o.kind === "duplicateDestination"))
        } else assert.equal(native.error.code, "unsupported_platform")
    }
})

test("denied Shows folders use actual silent source fallback while frozen project media is still collected", async (t) => {
    if (!linux || process.getuid() === 0) {
        t.skip("Requires unprivileged Linux permission evidence")
        return
    }
    const dir = temporary(t),
        data = request("BUNDLE_MEDIA_FILES", dir),
        file = write(dir, "source/media.bin", "bytes"),
        shows = path.join(data.dataRoot, "Shows")
    fs.mkdirSync(shows, { recursive: true })
    fs.chmodSync(shows, 0)
    data.stores.projects.projects = { p: { shows: [{ type: "audio", id: file }] } }
    try {
        const { native } = await compare(data)
        assert.deepEqual(native.plan.candidates, [file])
        assert.equal(native.plan.diagnostics[0].source_error_logged, false)
    } finally {
        fs.chmodSync(shows, 0o700)
    }
})
