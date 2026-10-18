import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import vm from "node:vm"
import { fileURLToPath } from "node:url"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import ts from "typescript"

const root = fileURLToPath(new URL("../../", import.meta.url))
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const binary = process.env.FREESHOW_SCRIPTURES_PARITY || path.join(root, "target/debug/freeshow-scriptures-parity" + (process.platform === "win32" ? ".exe" : ""))
const linux = process.platform === "linux"
const coverage = { comparisons: 0, generatedVerses: 0 }
test.after(() => console.log("Scripture parity coverage:", JSON.stringify(coverage)))
const sources = new Map()
function original(filename) {
    if (!sources.has(filename)) {
        const result = spawnSync("git", ["show", `${revision}:${filename}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 4 * 1024 * 1024 })
        assert.equal(result.status, 0, result.stderr)
        sources.set(filename, ts.createSourceFile(filename, result.stdout.replaceAll("\r\n", "\n"), ts.ScriptTarget.Latest, true))
    }
    return sources.get(filename)
}
function declaration(filename, name, variable = false) {
    const ast = original(filename)
    const node = ast.statements.find((node) => (variable ? ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText(ast) === name) : ts.isFunctionDeclaration(node) && node.name?.text === name))
    assert.ok(node, "Actual pinned source declaration " + name)
    return node.getText(ast).replace(/^export\s+/, "")
}
function oracle(dataRoot, documentsRoot, options = {}) {
    const errors = [],
        alerts = [],
        mkdir = [],
        loads = [],
        reads = []
    const boundary = Object.create(fs)
    boundary.mkdirSync = (folder, args) => {
        mkdir.push(folder)
        if (options.failMkdir) throw Object.assign(new Error("denied"), { code: "EACCES" })
        return fs.mkdirSync(folder, args)
    }
    boundary.readFileSync = (file, ...args) => {
        const bytes = fs.readFileSync(file, ...args)
        reads.push(file)
        if (reads.length === 1) options.afterFirstRead?.()
        return bytes
    }
    const files = "src/electron/utils/files.ts",
        responses = "src/electron/IPC/responsesMain.ts"
    const ast = original(responses)
    const response = ast.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText(ast) === "mainResponses"))
    const object = response.declarationList.declarations.find((item) => item.name.getText(ast) === "mainResponses").initializer
    const handler = object.properties.find((item) => item.name?.getText(ast) === "[Main.BIBLE]").initializer.getText(ast)
    const code = [
        '"use strict";',
        ...["MAX_NODE_STRING_LENGTH", "dataFolderNames"].map((name) => declaration(files, name, true)),
        ...["getDataFolderPath", "createFolder", "makeDir", "doesPathExist", "readFile", "safeBufferToString", "actionComplete", "loadFile", "parseJSON"].map((name) => declaration(files, name)),
        declaration(responses, "loadScripture"),
        "const originalLoadFile = loadFile; loadFile = (...args) => { loads.push(args[0]); return originalLoadFile(...args) };",
        `globalThis.result = (${handler})`
    ].join("\n")
    const context = vm.createContext({
        fs: boundary,
        Buffer,
        path,
        loads,
        getDataFolderRoot: () => dataRoot,
        app: {
            getPath: (name) => {
                assert.equal(name, "documents")
                return documentsRoot
            }
        },
        ToMain: { ALERT: "ALERT" },
        sendToMain: (channel, data) => alerts.push({ channel, data }),
        console: { error: (...values) => errors.push(values), warn() {} }
    })
    new vm.Script(ts.transpileModule(code, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText).runInContext(context, { timeout: 1000 })
    return { load: context.result, errors, alerts, mkdir, loads, reads }
}
function fixture(t) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-scripture-"))
    t.after(() => fs.rmSync(dir, { recursive: true, force: true }))
    const dataRoot = path.join(dir, "selected 敬拜"),
        documentsRoot = path.join(dir, "Documents é")
    fs.mkdirSync(path.join(dataRoot, "Bibles"), { recursive: true })
    fs.mkdirSync(path.join(documentsRoot, "Bibles"), { recursive: true })
    return { dir, dataRoot, documentsRoot }
}
const request = (roots, name, id = "requested", extra = {}) => ({ dataRoot: roots.dataRoot, documentsRoot: roots.documentsRoot, payload: { name, id }, ...extra })
function write(roots, name, value, legacy = false) {
    const file = path.join(legacy ? roots.documentsRoot : roots.dataRoot, "Bibles", name + ".fsb")
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, Buffer.isBuffer(value) || typeof value === "string" ? value : JSON.stringify(value))
    return file
}
function native(cases) {
    const result = spawnSync(binary, [], { input: cases.map(JSON.stringify).join("\n") + "\n", encoding: "utf8", timeout: 30000, maxBuffer: 64 * 1024 * 1024 })
    assert.equal(result.status, 0, result.stderr || result.error?.message)
    return result.stdout.trim().split("\n").map(JSON.parse)
}
function propertyOrder(actual, expected) {
    if (!expected || typeof expected !== "object") return
    assert.deepEqual(Object.keys(actual), Object.keys(expected))
    for (const key of Object.keys(expected)) propertyOrder(actual[key], expected[key])
}
function compare(cases) {
    const results = native(cases)
    for (const [i, input] of cases.entries()) {
        const source = oracle(input.dataRoot, input.documentsRoot)
        const expected = structuredClone(source.load(input.payload))
        if (!linux) {
            assert.equal(results[i].error.code, "scripture_unsupported")
            continue
        }
        assert.equal(results[i].ok, true, JSON.stringify(results[i]))
        assert.equal(results[i].complete, true)
        assert.deepEqual(results[i].reply.data, expected, JSON.stringify(input))
        propertyOrder(results[i].reply.data, expected)
        assert.deepEqual(results[i].reply.stats.loadPaths, source.loads)
        assert.equal(results[i].reply.diagnostics.filter((item) => item.sourceErrorLogged).length, source.errors.length)
        assert.deepEqual(source.alerts, [])
        coverage.comparisons++
    }
    return results
}
function bible(verses) {
    return ["original", { books: [{ name: "Genesis", chapters: [{ verses }] }], future: { nested: [true, 0, null, "敬拜 e\u0301 🙌"], __protoField: "preserve" } }]
}

test("actual BIBLE handler preserves generated original shapes and exact first-value/falsy-text migration gate", { skip: !linux }, (t) => {
    const roots = fixture(t),
        cases = []
    const values = [undefined, null, false, true, 0, 1, "", "verse 日本語 🙌", [], {}, ["future"], { future: [1, false] }]
    for (const [firstIndex, first] of values.entries())
        for (const [textIndex, text] of values.entries()) {
            const verses = [first === undefined ? {} : { value: first }, { number: 2, text, value: "replacement", future: { keep: true } }, { number: 3, value: 0 }, { number: 4, value: false }, { number: 5 }, { text: "retained", value: "old remains" }]
            const name = `generated-${firstIndex}-${textIndex}`
            write(roots, name, bible(verses))
            cases.push(request(roots, name))
            coverage.generatedVerses += verses.length
        }
    compare(cases)
})

test("Unicode/path spelling, scalar/default IDs and meaningful unknown fields retain source semantics", { skip: !linux }, (t) => {
    const roots = fixture(t),
        cases = []
    for (const name of ["", "敬拜 🙌", "e\u0301", "é", "slash\\name", "nested/name", "nested/../same", "../outside", "/leading", "line\nbreak", 'quote"name']) {
        write(roots, name, bible([{ text: "modern", future: { unknown: [false, null, { x: true }] } }]))
        cases.push(request(roots, name))
    }
    write(roots, "ids", ["file id", { unknown: { __proto__: "data", 10: "ten", 2: "two" } }])
    for (const id of ["", null, false, 0, true, 2, [], {}, ["array id"], { native: "retained JSON id" }]) cases.push(request(roots, "ids", id))
    cases.push({ ...request(roots, "ids"), payload: { name: "ids", unused: "untouched" } })
    cases.push(request(roots, "NUL\0name"))
    compare(cases)
})

test("real UTF-8 bytes, JSON repair, finite binary64 and duplicate keys match immutable source", { skip: !linux }, (t) => {
    const roots = fixture(t),
        cases = []
    const texts = ["", "not json", "null", "false", "0", '""', "[]", "{}", "[]junk", "{}junk", "1\ngarbage", '["id",{"nested":{}}] trailing', '["id",{"text":"}}] inside string"}] trailing', '\uFEFF["id",{}]', '["id",{"number":9007199254740993,"small":8.3163996793004788882e-275,"subnormal":5e-324,"z":-0,"duplicate":1,"other":2,"duplicate":3}]', '["id",{"escaped":"\\ud83d\\ude4c \\u00e9 \\u0000","line":"\\r\\n"}]']
    texts.push(Buffer.from('["id",{"text":"before ' + "after" + '"}]'))
    for (const bytes of [[0xff], [0xed, 0xa0, 0x80], [0xe2, 0x82], [0xe2, 0x28, 0xa1]]) texts.push(Buffer.concat([Buffer.from('["id",{"text":"'), Buffer.from(bytes), Buffer.from('"}]')]))
    for (const [index, text] of texts.entries()) {
        const name = "bytes-" + index
        write(roots, name, text)
        cases.push(request(roots, name, index === 10 ? "" : "id"))
    }
    write(roots, "unknown-proto", '["stored",{"__proto__":{"preserved":true},"future":[{"constructor":"data"}],"metadata":{"unknown":1}}]')
    cases.push(request(roots, "unknown-proto"))
    compare(cases)
})

test("primary/fallback source precedence includes duplicate loads, directory/symlink/permission and absent/corrupt cases", { skip: !linux }, (t) => {
    const roots = fixture(t),
        cases = []
    write(roots, "primary", ["id", { from: "primary" }])
    write(roots, "primary", ["id", { from: "legacy" }], true)
    cases.push(request(roots, "primary"))
    for (const [name, content] of [
        ["missing", undefined],
        ["empty", ""],
        ["corrupt", "not json"],
        ["falsy", "null"]
    ]) {
        if (content !== undefined) write(roots, name, content)
        write(roots, name, ["id", { from: "legacy " + name }], true)
        cases.push(request(roots, name))
    }
    cases.push(request(roots, "both-absent"))
    write(roots, "both-corrupt", "invalid")
    write(roots, "both-corrupt", "invalid", true)
    cases.push(request(roots, "both-corrupt"))
    fs.mkdirSync(path.join(roots.dataRoot, "Bibles/directory.fsb"))
    cases.push(request(roots, "directory"))
    if (linux) {
        const file = write(roots, "real", ["id", { actual: true }])
        for (const [name, target] of [
            ["link", file],
            ["dir-link", roots.dataRoot],
            ["dangling", path.join(roots.dir, "absent")],
            ["loop", path.join(roots.dataRoot, "Bibles/loop.fsb")]
        ]) {
            fs.symlinkSync(target, path.join(roots.dataRoot, "Bibles", name + ".fsb"))
            cases.push(request(roots, name))
        }
        const denied = write(roots, "denied", "private")
        fs.chmodSync(denied, 0)
        t.after(() => {
            if (fs.existsSync(denied)) fs.chmodSync(denied, 0o600)
        })
        if (process.getuid() !== 0) cases.push(request(roots, "denied"))
    }
    const results = compare(cases)
    if (linux) {
        assert.equal(results[0].reply.stats.loadPaths[0], results[0].reply.stats.loadPaths[1])
        assert.notEqual(results[1].reply.stats.loadPaths[0], results[1].reply.stats.loadPaths[1])
    }
})

test("actual source second-read and fallback decisions stay observable under controlled real filesystem changes", { skip: !linux }, (t) => {
    const roots = fixture(t),
        file = write(roots, "changing", '["first",{"from":"first"}]')
    write(roots, "changing", '["legacy",{"from":"legacy"}]', true)
    let source = oracle(roots.dataRoot, roots.documentsRoot, { afterFirstRead: () => fs.writeFileSync(file, '["second",{"from":"second"}]') })
    assert.equal(source.load({ name: "changing", id: "requested" }).content[1].from, "second")
    assert.deepEqual(source.loads, [file, file])
    fs.writeFileSync(file, "invalid")
    source = oracle(roots.dataRoot, roots.documentsRoot, { afterFirstRead: () => fs.writeFileSync(file, '["new",{"from":"new"}]') })
    assert.equal(source.load({ name: "changing", id: "requested" }).content[1].from, "legacy")
    source = oracle(roots.dataRoot, roots.documentsRoot, { afterFirstRead: () => fs.unlinkSync(file) })
    assert.deepEqual(structuredClone(source.load({ name: "changing", id: "requested" })), { error: "not_found", id: "requested" })
    // The matching native replacement/removal barrier runs in the crate test;
    // no mutation/debug observer is exposed in the production service API.
})

test("directory preparation is explicit, read-only and allows fallback after a failed host mkdir", { skip: !linux }, (t) => {
    const roots = fixture(t)
    fs.rmdirSync(path.join(roots.dataRoot, "Bibles"))
    fs.rmdirSync(roots.dataRoot)
    write(roots, "legacy", ["id", { from: "legacy" }], true)
    const input = request(roots, "legacy"),
        [pending] = native([input])
    assert.equal(pending.complete, false)
    assert.equal(pending.reply.data, null)
    assert.equal(pending.reply.folderIntents[0].path, path.join(roots.dataRoot, "Bibles"))
    assert.equal(fs.existsSync(roots.dataRoot), false)
    const [actual] = native([{ ...input, afterDirectoryAttempt: true }])
    const source = oracle(roots.dataRoot, roots.documentsRoot, { failMkdir: true })
    assert.deepEqual(actual.reply.data, structuredClone(source.load(input.payload)))
    assert.equal(source.alerts.length, 1)
    assert.equal(fs.existsSync(roots.dataRoot), false)
    // A successful host attempt must be rerun through the service as well.
    fs.mkdirSync(path.join(roots.dataRoot, "Bibles"), { recursive: true })
    compare([input])
})

test("source throws and unrepresentable values are explicit unsupported outcomes, never not_found success", { skip: !linux }, (t) => {
    const roots = fixture(t),
        cases = []
    for (const [index, text] of ["true", '"primitive"', '["id",{"books":[{"chapters":[{"verses":[{"value":"first"},null]}]}]}]', '["id",{"number":1e309}]', '["id",{"text":"\\ud800"}]'].entries()) {
        const name = "unsupported-" + index
        write(roots, name, text)
        cases.push(request(roots, name))
    }
    const results = native(cases)
    for (const result of results) {
        assert.equal(result.ok, false)
        assert.equal(result.error.code, "scripture_unsupported")
    }
    for (const item of cases.slice(0, 3)) assert.throws(() => oracle(item.dataRoot, item.documentsRoot).load(item.payload))
    assert.equal(oracle(roots.dataRoot, roots.documentsRoot).load(cases[3].payload).content[1].number, Infinity)
    assert.equal(oracle(roots.dataRoot, roots.documentsRoot).load(cases[4].payload).content[1].text.charCodeAt(0), 0xd800)
})

test("full-sized generated 31680-verse local Bible remains bounded and preserves original migration and metadata", { skip: !linux }, (t) => {
    const roots = fixture(t)
    const books = Array.from({ length: 66 }, (_, b) => ({ name: "Book " + b, chapters: Array.from({ length: 20 }, (_, c) => ({ chapter: c + 1, verses: Array.from({ length: 24 }, (_, v) => ({ verse: v + 1, value: `Verse ${b}:${c}:${v} — original UTF-8 敬拜 text and preserved unknown metadata.`, future: { keep: true } })) })) }))
    write(roots, "large", ["stored", { books, metadata: { customName: "Original 日本語", copyright: "generated fixture" }, future: { keep: "unchanged" } }])
    const [result] = compare([request(roots, "large")])
    assert.equal(result.reply.stats.visitedVerses, 31680)
    assert.equal(result.reply.stats.migratedVerses, 31680)
    coverage.generatedVerses += 31680
    assert.ok(result.reply.stats.allocatedBytes <= 128 * 1024 * 1024)
    assert.equal(result.reply.stats.loadPaths.length, 2)
    coverage.fullBible = { readBytes: result.reply.stats.readBytes, allocatedBytes: result.reply.stats.allocatedBytes, parsedNodes: result.reply.stats.parsedNodes, operations: result.reply.stats.operations }
})

test("non-Linux backend reports explicit unsupported instead of claiming filesystem parity", { skip: linux }, () => {
    const [result] = native([{ dataRoot: os.tmpdir(), documentsRoot: os.tmpdir(), payload: { name: "test", id: "id" } }])
    assert.equal(result.ok, false)
    assert.equal(result.error.code, "scripture_unsupported")
})
