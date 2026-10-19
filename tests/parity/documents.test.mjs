import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import vm from "node:vm"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import ts from "typescript"
import { root, clean } from "./oracle.mjs"
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const binary = process.env.FREESHOW_DOCUMENTS_BIN || path.join(root, "target/debug", process.platform === "win32" ? "freeshow-documents.exe" : "freeshow-documents")
const linux = process.platform === "linux"
const sources = new Map()
function original(filename) {
    if (!sources.has(filename)) {
        const result = spawnSync("git", ["show", `${revision}:${filename}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 4 * 1024 * 1024 })
        assert.equal(result.status, 0, result.stderr)
        const text = result.stdout.replace(/\r\n/g, "\n")
        sources.set(filename, ts.createSourceFile(filename, text, ts.ScriptTarget.Latest, true))
    }
    return sources.get(filename)
}
function declaration(filename, name, variable = false) {
    const ast = original(filename)
    const node = ast.statements.find((node) => (variable ? ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText(ast) === name) : ts.isFunctionDeclaration(node) && node.name?.text === name))
    assert.ok(node, `Pinned source missing ${name}`)
    return node.getText(ast).replace(/^export\s+/, "")
}
function handler(name) {
    const ast = original("src/electron/IPC/responsesMain.ts")
    const response = ast.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText(ast) === "mainResponses"))
    const object = response.declarationList.declarations.find((item) => item.name.getText(ast) === "mainResponses").initializer
    return object.properties.find((item) => item.name?.getText(ast) === `[Main.${name}]`).initializer.getText(ast)
}
function oracle(dataRoot, injectedFs = fs) {
    const errors = [],
        alerts = [],
        mkdir = []
    const boundaryFs = Object.create(injectedFs)
    boundaryFs.mkdirSync = (folder, options) => {
        mkdir.push(folder)
        return injectedFs.mkdirSync(folder, options)
    }
    const files = "src/electron/utils/files.ts"
    const code = [
        ...["MAX_NODE_STRING_LENGTH", "dataFolderNames"].map((name) => declaration(files, name, true)),
        ...["readFile", "safeBufferToString", "readFolder", "isPermissionDeniedError", "actionComplete", "getDataFolderPath", "createFolder", "doesPathExist", "makeDir"].map((name) => declaration(files, name)),
        declaration("src/electron/IPC/responsesMain.ts", "readBiblesFolder"),
        `globalThis.result = {READ_FILE: (${handler("READ_FILE")}), READ_BIBLES_FOLDER: (${handler("READ_BIBLES_FOLDER")})}`
    ].join("\n")
    const context = vm.createContext({ fs: boundaryFs, Buffer, path, getDataFolderRoot: () => dataRoot, ToMain: { ALERT: "ALERT" }, sendToMain: (channel, data) => alerts.push({ channel, data }), console: { error: (...values) => errors.push(values), warn() {} } })
    new vm.Script(ts.transpileModule(code, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText).runInContext(context, { timeout: 1000 })
    return { ...context.result, errors, alerts, mkdir }
}
function temporary(t) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-documents-parity-"))
    t.after(() => fs.rmSync(dir, { recursive: true, force: true }))
    return dir
}
function write(dir, name, bytes) {
    const file = path.join(dir, name)
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, bytes)
    return file
}
function request(command, dataRoot, data = null) {
    return { command, dataRoot, data }
}
function rust(cases) {
    const result = spawnSync(binary, [], { input: cases.map(JSON.stringify).join("\n") + "\n", encoding: "utf8", timeout: 30000, maxBuffer: 48 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return result.stdout.trim().split("\n").map(JSON.parse)
}
function compare(cases) {
    const actual = rust(cases)
    for (const [index, item] of cases.entries()) {
        const source = oracle(item.dataRoot)
        const expected = clean(source[item.command](item.data))
        if (!linux) {
            assert.equal(actual[index].error.code, "unsupported_platform")
            continue
        }
        assert.equal(actual[index].error, undefined, JSON.stringify(actual[index].error))
        assert.equal(actual[index].complete, true)
        assert.deepEqual(actual[index].folderIntents, [])
        assert.deepEqual(actual[index].data, expected, JSON.stringify(item))
        assert.equal(actual[index].diagnostics.filter((item) => item.sourceErrorLogged).length, source.errors.length)
    }
    return actual
}

test("READ_FILE real UTF-8, BOM, NUL, newline and malformed bytes match the immutable actual handler", (t) => {
    const dir = temporary(t)
    const cases = [Buffer.alloc(0), Buffer.from("\uFEFFline one\r\n敬拜 🙌\rline two\0end"), Buffer.from('{"retain":"raw json"}'), Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from([0xe0, 0x80, 0xaf]), Buffer.from([0xed, 0xa0, 0x80]), Buffer.from([0xf0, 0x80, 0x80, 0x80]), Buffer.from([0xf4, 0x90, 0x80, 0x80]), Buffer.from([0xe2, 0x82]), Buffer.from([0xf0, 0x9f, 0x92]), Buffer.from([0xe2, 0x28, 0xa1])]
    for (let byte = 0; byte < 256; byte++) cases.push(Buffer.from([byte]))
    let state = 0x12345678
    for (let index = 0; index < 32; index++) {
        const bytes = Buffer.alloc(2048 + index)
        for (let at = 0; at < bytes.length; at++) {
            state ^= state << 13
            state ^= state >>> 17
            state ^= state << 5
            bytes[at] = state & 255
        }
        cases.push(bytes)
    }
    const results = compare(cases.map((bytes, index) => request("READ_FILE", dir, { path: write(dir, `${index} 音楽.txt`, bytes), ignored: true })))
    if (linux) {
        assert.equal(results[1].data.content.charCodeAt(0), 0xfeff)
        assert.ok(results.at(-1).data.content.includes("\uFFFD"))
    }
})

test("READ_FILE missing/empty/NUL/directory and symlink following preserve original empty/error distinctions", (t) => {
    const dir = temporary(t),
        file = write(dir, "actual.txt", "content")
    const paths = ["", "\0", path.join(dir, "missing"), dir, file, file + "/", path.join(dir, "not-created/../actual.txt")]
    if (linux) {
        for (const [name, target] of [
            ["file-link", file],
            ["directory-link", dir],
            ["dangling", path.join(dir, "missing")],
            ["self-loop", path.join(dir, "self-loop")]
        ]) {
            const link = path.join(dir, name)
            fs.symlinkSync(target, link)
            paths.push(link)
        }
        const denied = write(dir, "denied", "private")
        fs.chmodSync(denied, 0)
        t.after(() => {
            if (fs.existsSync(denied)) fs.chmodSync(denied, 0o600)
        })
        if (process.getuid() !== 0) paths.push(denied)
    }
    paths.push(path.relative(process.cwd(), file), path.join(dir, ".") + "/./actual.txt")
    const results = compare(paths.map((file) => request("READ_FILE", dir, { path: file })))
    if (linux) {
        assert.deepEqual(results[3].diagnostics, [])
        assert.equal(results[8].diagnostics[0].code, "source_read_error")
        assert.equal(results[8].diagnostics[0].sourceErrorLogged, true)
        assert.deepEqual(results[9].diagnostics, [])
    }
})

test("Bibles keeps all entry types/raw names, terminal case-insensitive extension stripping, byte order and duplicate display names", (t) => {
    const dir = temporary(t),
        bibles = path.join(dir, "Bibles")
    fs.mkdirSync(bibles)
    const names = ["Z.fsb", "A.fsb", "A.FSB", "A", ".fsb", "book.fsb.fsb", "book.fsb.txt", "敬拜.FsB", "🙌.fsb", "\uE000.fsb", "e\u0301.fsb", "é.fsb", "no-extension", "data.json", "empty.FSB", "FſB.fsb"]
    if (linux) names.push("book.fsb\n", "book.fsb\u2028", "line\nbreak.fsb", "slash\\name.fsb", 'quote".FSB')
    for (const name of names.reverse()) fs.writeFileSync(path.join(bibles, name), "invalid bible data is not parsed")
    fs.mkdirSync(path.join(bibles, "directory.FSB"))
    write(bibles, "directory.FSB/nested.fsb", "not recursively listed")
    if (linux) {
        fs.symlinkSync(path.join(dir, "missing"), path.join(bibles, "dangling.fsb"))
        fs.symlinkSync(bibles, path.join(bibles, "cycle.fsb"))
    }
    const [actual] = compare([request("READ_BIBLES_FOLDER", dir, { path: path.join(dir, "attacker-selected-folder"), names: ["ignored"] })])
    if (linux) {
        assert.equal(actual.data.filter((row) => row.name === "A").length, 3)
        assert.ok(actual.data.some((row) => row.name === ""))
        assert.ok(actual.data.some((row) => row.name === "directory"))
        assert.equal(
            actual.data.some((row) => row.name === "nested"),
            false
        )
        assert.ok(actual.data.some((row) => row.name === "book.fsb\n"))
    }
})

test("Bibles paths use frozen per-request roots and original normalized join, including an existing directory symlink", (t) => {
    const dir = temporary(t),
        first = path.join(dir, "first"),
        second = path.join(dir, "second")
    write(first, "Bibles/first.fsb", "first")
    write(second, "Bibles/second.FSB", "second")
    fs.mkdirSync(path.join(first, "unused"))
    const cases = [request("READ_BIBLES_FOLDER", first, { path: second }), request("READ_BIBLES_FOLDER", second, { path: first }), request("READ_BIBLES_FOLDER", first + "/unused/.././", null), request("READ_FILE", first, { path: path.join(second, "Bibles/second.FSB") })]
    if (linux) {
        const link = path.join(dir, "symlink-root")
        fs.mkdirSync(link)
        fs.symlinkSync(path.join(first, "Bibles"), path.join(link, "Bibles"))
        cases.push(request("READ_BIBLES_FOLDER", link))
    }
    compare(cases)
})

test("missing Bibles root returns an incomplete ensure intent and performs no writes; source mkdir creates it", (t) => {
    const dir = temporary(t),
        dataRoot = path.join(dir, "not-created", "FreeShow"),
        bibles = path.join(dataRoot, "Bibles")
    const [actual] = rust([request("READ_BIBLES_FOLDER", dataRoot)])
    assert.equal(fs.existsSync(dataRoot), false)
    if (!linux) {
        assert.equal(actual.error.code, "unsupported_platform")
        return
    }
    assert.equal(actual.complete, false)
    assert.deepEqual(actual.data, [])
    assert.deepEqual(actual.folderIntents, [{ kind: "ensureBiblesDirectory", path: bibles }])
    assert.equal(actual.diagnostics[0].code, "bibles_directory_preparation_required")
    const source = oracle(dataRoot)
    assert.deepEqual(clean(source.READ_BIBLES_FOLDER()), [])
    assert.deepEqual(source.mkdir, [bibles])
    assert.deepEqual(source.alerts, [])
    assert.ok(fs.statSync(bibles).isDirectory())
    compare([request("READ_BIBLES_FOLDER", dataRoot)])
})

test("non-directory Bibles root defers source mkdir failure/ALERT to host instead of claiming an empty completed listing", (t) => {
    const dir = temporary(t),
        bibles = write(dir, "Bibles", "preexisting")
    const [actual] = rust([request("READ_BIBLES_FOLDER", dir)])
    const source = oracle(dir)
    assert.deepEqual(clean(source.READ_BIBLES_FOLDER()), [])
    assert.deepEqual(source.alerts, [{ channel: "ALERT", data: `Error: Could not create folder at: ${bibles}!` }])
    assert.equal(source.errors.length, 2)
    assert.equal(fs.readFileSync(bibles, "utf8"), "preexisting")
    if (!linux) {
        assert.equal(actual.error.code, "unsupported_platform")
        return
    }
    assert.equal(actual.complete, false)
    assert.deepEqual(actual.folderIntents, [{ kind: "ensureBiblesDirectory", path: bibles }])
})

test("Linux denied-folder source errors are suppressed while empty reply remains a completed source fallback", (t) => {
    if (!linux || process.getuid() === 0) {
        t.skip("Requires Linux unprivileged permission evidence")
        return
    }
    const dir = temporary(t),
        bibles = path.join(dir, "Bibles")
    fs.mkdirSync(bibles)
    fs.chmodSync(bibles, 0)
    t.after(() => {
        if (fs.existsSync(bibles)) fs.chmodSync(bibles, 0o700)
    })
    const [actual] = compare([request("READ_BIBLES_FOLDER", dir)])
    assert.equal(actual.diagnostics[0].code, "source_folder_error")
    assert.equal(actual.diagnostics[0].sourceErrorLogged, false)
})

test("special inputs, non-Unicode filenames, invalid requests and read caps are explicit failures", (t) => {
    const dir = temporary(t),
        bibles = path.join(dir, "Bibles")
    const oversized = path.join(dir, "oversized.txt")
    const fd = fs.openSync(oversized, "w")
    fs.ftruncateSync(fd, 8 * 1024 * 1024 + 1)
    fs.closeSync(fd)
    const cases = [request("READ_FILE", dir, { path: oversized }), request("READ_FILE", dir, { path: "a".repeat(32769) }), request("READ_FILE", dir, {}), request("NOT_A_COMMAND", dir)]
    if (linux) {
        fs.mkdirSync(bibles)
        fs.writeFileSync(Buffer.concat([Buffer.from(bibles + "/"), Buffer.from([0xff])]), "unsupported")
        cases.push(request("READ_BIBLES_FOLDER", dir), request("READ_FILE", dir, { path: "/dev/null" }))
    }
    const actual = rust(cases)
    assert.equal(actual[0].error.code, linux ? "document_resource_limit" : "unsupported_platform")
    assert.equal(actual[1].error.code, "document_resource_limit")
    assert.equal(actual[2].error.code, "invalid_request")
    assert.equal(actual[3].error.code, "unsupported_command")
    if (linux) {
        assert.equal(actual[4].error.code, "unsupported_path_encoding")
        assert.equal(clean(oracle(dir).READ_BIBLES_FOLDER())[0].name, "\uFFFD")
        assert.equal(actual[5].error.code, "unsupported_file_type")
        assert.deepEqual(clean(oracle(dir).READ_FILE({ path: "/dev/null" })), { content: "" })
    }
    for (const result of actual) assert.equal(Object.hasOwn(result, "data"), false)
})
