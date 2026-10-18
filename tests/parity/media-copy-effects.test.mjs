import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import vm from "node:vm"
import { spawnSync } from "node:child_process"
import { createRequire } from "node:module"
import { test } from "node:test"
import ts from "typescript"
import { root, clean } from "./oracle.mjs"
const require = createRequire(import.meta.url),
    upath = require("upath")
assert.equal(require("upath/package.json").version, "2.0.1")
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const binary = process.env.FREESHOW_MEDIA_COPY_EFFECTS_BIN || path.join(root, "target/debug/examples", process.platform === "win32" ? "freeshow-media-copy-effects.exe" : "freeshow-media-copy-effects")
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
    const source = ast(file),
        node = source.statements.find((node) => (variable ? ts.isVariableStatement(node) && node.declarationList.declarations.some((v) => v.name.getText(source) === name) : ts.isFunctionDeclaration(node) && node.name?.text === name))
    assert.ok(node, `Pinned ${name} missing`)
    return node.getText(source).replace(/^export\s+/, "")
}
function handler(name) {
    const source = ast("src/electron/IPC/responsesMain.ts"),
        response = source.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((v) => v.name.getText(source) === "mainResponses"))
    return response.declarationList.declarations
        .find((v) => v.name.getText(source) === "mainResponses")
        .initializer.properties.find((p) => p.name?.getText(source) === `[Main.${name}]`)
        .initializer.getText(source)
}
function oracle(item) {
    const copies = [],
        folders = [],
        errors = [],
        alerts = [],
        opened = [],
        pending = []
    const boundary = Object.create(fs)
    boundary.copyFile = (source, target, callback) => {
        copies.push({ source, target })
        fs.copyFile(source, target, callback)
    }
    boundary.mkdirSync = (folder, options) => {
        folders.push(folder)
        return fs.mkdirSync(folder, options)
    }
    const file = "src/electron/utils/files.ts",
        code = [
            ...["MAX_NODE_STRING_LENGTH", "dataFolderNames", "currentlyBundling"].map((n) => declaration(file, n, true)),
            ...["bundleMediaFiles", "addToMediaFolder", "getFileParentFolderId", "readFile", "readFolder", "safeBufferToString", "parseShow", "parseJSON", "doesPathExist", "doesPathExistAsync", "getDataFolderPath", "getMediaSyncFolderPath", "createFolder", "makeDir", "copyFileAsync", "asyncPool", "actionComplete", "isPermissionDeniedError", "openInSystem"].map((n) => declaration(file, n)),
            declaration("src/electron/data/thumbnails.ts", "filePathHashCode"),
            `const sourceCopy=addToMediaFolder;addToMediaFolder=(...args)=>{const p=sourceCopy(...args);pending.push(p);return p;};globalThis.result={BUNDLE_MEDIA_FILES:(${handler("BUNDLE_MEDIA_FILES")}),MEDIA_FOLDER_COPY:(${handler("MEDIA_FOLDER_COPY")}),getFileParentFolderId};`
        ].join("\n")
    const context = vm.createContext({
        fs: boundary,
        path,
        upath,
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
    return { ...context.result, copies, folders, errors, alerts, opened, pending }
}
function temporary(t) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-media-copy-effects-"))
    t.after(() => fs.rmSync(dir, { recursive: true, force: true }))
    return dir
}
function write(dir, name, content = "opaque fixture") {
    const file = path.join(dir, name)
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, content)
    return file
}
function request(command, dir, data = {}) {
    return { command, dataRoot: path.join(dir, "data"), syncRoot: path.join(dir, "media"), stores: { projects: { projects: {} }, overlays: {}, templates: {} }, data }
}
function rust(item) {
    const result = spawnSync(binary, [], { input: JSON.stringify(item) + "\n", encoding: "utf8", timeout: 30000, maxBuffer: 16 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return JSON.parse(result.stdout)
}
function pair(t, command = "MEDIA_FOLDER_COPY", data = {}) {
    const dir = temporary(t),
        native = request(command, path.join(dir, "native"), data),
        source = request(command, path.join(dir, "source"), data)
    return { dir, native, source }
}
function destination(item) {
    return item.command === "BUNDLE_MEDIA_FILES" ? item.data.outputPath || path.join(item.dataRoot, "Media") : item.syncRoot || path.join(item.dataRoot, "Media")
}
function normalize(value, item) {
    return value.replaceAll(destination(item), "<MEDIA>").replaceAll(item.dataRoot, "<DATA>")
}
async function compare(p) {
    const native = rust(p.native),
        source = oracle(p.source),
        reply = await source[p.source.command](p.source.data)
    await Promise.all(source.pending)
    if (!linux) {
        assert.equal(native.error.code, "unsupported_platform")
        return { native, source, reply }
    }
    assert.equal(native.error, undefined, JSON.stringify(native.error))
    const receipt = native.receipt
    assert.equal(receipt.reply, reply === undefined ? null : reply)
    assert.deepEqual(
        receipt.alerts.map((v) => normalize(v, p.native)),
        clean(source.alerts.map((a) => normalize(a.data, p.source)))
    )
    for (const installed of receipt.installed) {
        const relative = path.relative(destination(p.native), installed.destination),
            expected = path.join(destination(p.source), relative)
        assert.deepEqual(fs.readFileSync(installed.destination), fs.readFileSync(expected), relative)
        assert.equal(fs.statSync(installed.destination).mode & 0o7777, fs.statSync(expected).mode & 0o7777, relative)
        assert.equal(fs.statSync(installed.destination).size, installed.length)
    }
    if (receipt.openFolder?.kind === "openDirectory") assert.deepEqual(clean(source.opened), [destination(p.source)])
    if (receipt.openFolder?.kind === "missing") assert.deepEqual(source.opened, [])
    return { native, source, reply }
}
test("actual MEDIA_FOLDER_COPY installs opaque Unicode/multiple files with source modes and keeps existing bytes", async (t) => {
    const p = pair(t),
        a = write(p.dir, "敬拜/🙌/clip.bin", Buffer.from([0, 255, 128, 3])),
        b = write(p.dir, "other/second.bin", Buffer.alloc(80000, 7)),
        empty = write(p.dir, "empty/zero.bin", Buffer.alloc(0)),
        existing = write(p.dir, "existing/keep.bin", "new")
    if (linux) {
        fs.chmodSync(a, 0o640)
        fs.chmodSync(b, 0o751)
    }
    const paths = [a, a, b, empty, existing, path.join(p.dir, "missing"), "https://invalid.local/literal", "file://" + a]
    p.native.data = { paths }
    p.source.data = { paths }
    const id = oracle(p.source).getFileParentFolderId(existing)
    for (const item of [p.native, p.source]) write(destination(item), path.join(id, "keep.bin"), "preexisting different bytes")
    const { native, reply } = await compare(p)
    assert.equal(reply, true)
    if (linux) {
        assert.equal(native.receipt.installed.length, 3)
        assert.equal(native.receipt.skippedExisting, 1)
        assert.equal(native.receipt.duplicateDestinations, 1)
        assert.equal(fs.readFileSync(path.join(destination(p.native), id, "keep.bin"), "utf8"), "preexisting different bytes")
    }
})
test("actual BUNDLE extraction, Shows/default-root side effects, ordered dedupe and no IPC reply survive final apply", async (t) => {
    const p = pair(t, "BUNDLE_MEDIA_FILES", { openFolder: true }),
        files = Array.from({ length: 7 }, (_, i) => write(p.dir, `sources/${i}.bin`, `file${i}`))
    for (const item of [p.native, p.source]) {
        write(
            item.dataRoot,
            "Shows/Unicode.show",
            JSON.stringify([
                "id",
                {
                    media: { 10: { path: files[1] }, 2: { id: files[0] } },
                    slides: {
                        s: {
                            items: [
                                { type: "media", src: files[2] },
                                { type: "text", src: "not media" }
                            ]
                        }
                    }
                }
            ])
        )
        item.stores = {
            projects: {
                projects: {
                    p: {
                        shows: [
                            { type: "audio", id: files[3] },
                            { type: "folder", id: "ignored" },
                            { type: "show", id: files[6] }
                        ]
                    }
                }
            },
            overlays: { o: { items: [{ type: "media", src: files[4] }] } },
            templates: {
                t: {
                    items: [
                        { type: "media", src: files[5] },
                        { type: "media", src: files[0] }
                    ]
                }
            }
        }
    }
    const { native, reply } = await compare(p)
    assert.equal(reply, undefined)
    if (linux) {
        assert.equal(native.receipt.installed.length, 6)
        assert.equal(native.receipt.reply, null)
        assert.ok(fs.statSync(path.join(p.native.dataRoot, "Shows")).isDirectory())
    }
})
test("custom missing BUNDLE root records early source missing-open ALERT even though copies later create it", async (t) => {
    const p = pair(t, "BUNDLE_MEDIA_FILES", { openFolder: true }),
        file = write(p.dir, "sources/media.bin")
    for (const [name, item] of [
        ["native", p.native],
        ["source", p.source]
    ]) {
        item.data = { openFolder: true, outputPath: path.join(p.dir, name, "selected-output") }
        item.approvedRoot = item.data.outputPath
        item.stores.overlays = { o: { items: [{ type: "media", src: file }] } }
    }
    const { native } = await compare(p)
    if (linux) {
        assert.deepEqual(native.receipt.alerts, ["This does not exist!"])
        assert.equal(native.receipt.openFolder.kind, "missing")
        assert.equal(native.receipt.installed.length, 1)
        assert.ok(fs.statSync(path.join(p.native.dataRoot, "Shows")).isDirectory())
    }
})
test("custom bundle uses raw root existence for early open even when Node join normalizes a missing dot parent", async (t) => {
    const p = pair(t, "BUNDLE_MEDIA_FILES", { openFolder: true }),
        input = write(p.dir, "sources/input.bin")
    for (const [name, item] of [
        ["native", p.native],
        ["source", p.source]
    ]) {
        const selected = path.join(p.dir, name, "selected")
        fs.mkdirSync(selected, { recursive: true })
        item.data = { openFolder: true, outputPath: path.join(p.dir, name) + "/absent/../selected" }
        item.approvedRoot = item.data.outputPath
        item.stores.overlays = { o: { items: [{ type: "media", src: input }] } }
    }
    const { native } = await compare(p)
    if (linux) {
        assert.equal(native.receipt.openFolder.kind, "missing")
        assert.deepEqual(native.receipt.alerts, ["This does not exist!"])
        assert.equal(native.receipt.installed.length, 1)
    }
})
test("literal root-prefix sibling is skipped and raw backslash/control-character filenames copy without reinterpretation", async (t) => {
    const p = pair(t),
        input = write(p.dir, linux ? 'odd/back\\slash\n".bin' : "odd/plain.bin", Buffer.from([0, 255, 10, 13]))
    for (const item of [p.native, p.source]) {
        const skipped = write(item.syncRoot + "-sibling", "skip.bin")
        item.data = { paths: [skipped, input] }
    }
    const { native, reply } = await compare(p)
    assert.equal(reply, true)
    if (linux) {
        assert.equal(native.receipt.installed.length, 1)
        assert.equal(native.receipt.failures.length, 0)
        assert.ok(native.plan.observations.some((o) => o.kind === "prefixSkipped"))
    }
})
test("ordinary directory-source copy failure returns changed=true while no artifact is installed", async (t) => {
    const p = pair(t),
        folder = path.join(p.dir, "source-directory")
    fs.mkdirSync(folder)
    p.native.data = { paths: [folder] }
    p.source.data = { paths: [folder] }
    const { native, source, reply } = await compare(p)
    assert.equal(reply, true)
    assert.equal(source.copies.length, 1)
    if (linux) {
        assert.equal(native.receipt.installed.length, 0)
        assert.equal(native.receipt.failures.length, 1)
        assert.equal(native.receipt.failures[0].osCode, os.constants.errno.EISDIR)
    }
})
test("empty MEDIA request ensures only a default Media root and returns false", async (t) => {
    for (const configured of [false, true]) {
        const p = pair(t)
        p.native.data = { paths: [] }
        p.source.data = { paths: [] }
        if (!configured) {
            delete p.native.syncRoot
            delete p.source.syncRoot
        }
        const { native, reply } = await compare(p)
        assert.equal(reply, false)
        if (linux) {
            assert.equal(native.receipt.reply, false)
            assert.equal(fs.existsSync(destination(p.native)), !configured)
        }
    }
})
test("root/dot aliases preserve source directory-destination skip quirks", async (t) => {
    const p = pair(t),
        directory = path.join(p.dir, "alias-input")
    fs.mkdirSync(directory)
    p.native.data = { paths: ["/", directory + "/.", directory + "/.."] }
    p.source.data = p.native.data
    const { native, reply } = await compare(p)
    assert.equal(reply, false)
    if (linux) {
        assert.equal(native.receipt.installed.length, 0)
        assert.equal(native.receipt.skippedExisting, 3)
    }
})
test("repeated failed same-folder mkdir emits independent source ALERTs and attempted copies still return true", async (t) => {
    const p = pair(t),
        a = write(p.dir, "sources/a.bin"),
        b = write(p.dir, "sources/b.bin")
    p.native.data = { paths: [a, b] }
    p.source.data = { paths: [a, b] }
    if (linux && process.getuid() !== 0) {
        for (const [name, item] of [
            ["native", p.native],
            ["source", p.source]
        ]) {
            const parent = path.join(p.dir, name, "readonly")
            fs.mkdirSync(parent, { recursive: true })
            fs.chmodSync(parent, 0o555)
            t.after(() => {
                if (fs.existsSync(parent)) fs.chmodSync(parent, 0o755)
            })
            item.syncRoot = path.join(parent, "missing-media")
        }
    }
    const { native, source, reply } = await compare(p)
    assert.equal(reply, true)
    if (linux && process.getuid() !== 0) {
        assert.equal(source.alerts.length, 2)
        assert.equal(native.receipt.failures.length, 2)
        assert.equal(native.receipt.installed.length, 0)
        assert.equal(native.receipt.alerts.length, 2)
    }
})
test("failed default-root creation is a source ALERT even with no MEDIA candidates", async (t) => {
    const p = pair(t)
    delete p.native.syncRoot
    delete p.source.syncRoot
    p.native.data = { paths: [] }
    p.source.data = p.native.data
    if (linux && process.getuid() !== 0)
        for (const item of [p.native, p.source]) {
            fs.mkdirSync(item.dataRoot, { recursive: true })
            fs.chmodSync(item.dataRoot, 0o555)
            t.after(() => {
                if (fs.existsSync(item.dataRoot)) fs.chmodSync(item.dataRoot, 0o755)
            })
        }
    const { native, source, reply } = await compare(p)
    assert.equal(reply, false)
    if (linux && process.getuid() !== 0) {
        assert.equal(source.alerts.length, 1)
        assert.equal(native.receipt.alerts.length, 1)
        assert.ok(!fs.existsSync(destination(p.native)))
    }
})
test("existing readonly hash folder gives ordinary copy failure, changed=true and no mkdir ALERT", async (t) => {
    const p = pair(t),
        input = write(p.dir, "sources/source.bin")
    p.native.data = { paths: [input] }
    p.source.data = p.native.data
    if (linux && process.getuid() !== 0)
        for (const item of [p.native, p.source]) {
            const folder = path.join(destination(item), oracle(item).getFileParentFolderId(input))
            fs.mkdirSync(folder, { recursive: true })
            fs.chmodSync(folder, 0o555)
            t.after(() => {
                if (fs.existsSync(folder)) fs.chmodSync(folder, 0o755)
            })
        }
    const { native, source, reply } = await compare(p)
    assert.equal(reply, true)
    if (linux && process.getuid() !== 0) {
        assert.equal(source.alerts.length, 0)
        assert.equal(native.receipt.failures.length, 1)
        assert.equal(native.receipt.installed.length, 0)
        assert.equal(native.receipt.alerts.length, 0)
    }
})
test("failed mkdir makes root/dot aliases actual failed attempts rather than a false preparation bool", async (t) => {
    const p = pair(t),
        directory = path.join(p.dir, "aliases")
    fs.mkdirSync(directory)
    p.native.data = { paths: [directory + "/.", directory + "/.."] }
    p.source.data = p.native.data
    if (linux && process.getuid() !== 0)
        for (const [name, item] of [
            ["native", p.native],
            ["source", p.source]
        ]) {
            const parent = path.join(p.dir, name, "readonly")
            fs.mkdirSync(parent, { recursive: true })
            fs.chmodSync(parent, 0o555)
            item.syncRoot = path.join(parent, "missing")
            t.after(() => {
                if (fs.existsSync(parent)) fs.chmodSync(parent, 0o755)
            })
        }
    const { native, reply } = await compare(p)
    if (linux && process.getuid() !== 0) {
        assert.equal(reply, true)
        assert.equal(native.plan.deferredReply.changed_if_effects_resolved, false)
        assert.equal(native.receipt.reply, true)
        assert.equal(native.receipt.failures.length, 2)
        assert.equal(native.receipt.alerts.length, 2)
    }
})
test("existing live symlink destinations skip without replacing bytes and symlink sources copy target bytes", async (t) => {
    const p = pair(t),
        input = write(p.dir, "sources/source.bin", "opaque target"),
        target = write(p.dir, "preexisting/keep.bin", "keep"),
        link = path.join(p.dir, "link.bin")
    if (!linux) {
        p.native.data = { paths: [input] }
        p.source.data = p.native.data
        await compare(p)
        return
    }
    fs.symlinkSync(input, link)
    p.native.data = { paths: [input, link] }
    p.source.data = p.native.data
    const id = oracle(p.source).getFileParentFolderId(input)
    for (const item of [p.native, p.source]) {
        const folder = path.join(destination(item), id)
        fs.mkdirSync(folder, { recursive: true })
        fs.symlinkSync(target, path.join(folder, "source.bin"))
    }
    const { native } = await compare(p)
    assert.equal(native.receipt.skippedExisting, 1)
    assert.equal(native.receipt.installed.length, 1)
    assert.ok(fs.lstatSync(path.join(destination(p.native), id, "source.bin")).isSymbolicLink())
})
test("empty BUNDLE still creates Shows and sends no successful response or open intent", async (t) => {
    const p = pair(t, "BUNDLE_MEDIA_FILES", { openFolder: true }),
        { native, reply } = await compare(p)
    assert.equal(reply, undefined)
    if (linux) {
        assert.equal(native.receipt.reply, null)
        assert.equal(native.receipt.openFolder, null)
        assert.ok(fs.statSync(path.join(p.native.dataRoot, "Shows")).isDirectory())
        assert.equal(fs.existsSync(destination(p.native)), false)
    }
})
