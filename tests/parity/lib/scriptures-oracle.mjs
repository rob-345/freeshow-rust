// Immutable Electron MAIN.BIBLE oracle shared by source and desktop fixtures.
// No node:test registrations and no filesystem effects until load(payload).
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { fileURLToPath } from "node:url"
import { spawnSync } from "node:child_process"
import ts from "typescript"

const root = fileURLToPath(new URL("../../../", import.meta.url))
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
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
export function scriptureOracle(dataRoot, documentsRoot, options = {}) {
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
