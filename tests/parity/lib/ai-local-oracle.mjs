// Actual immutable TypeScript with genuine Node fs; no model/provider imports.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import { fileURLToPath } from "node:url"
import ts from "typescript"
const root = fileURLToPath(new URL("../../../", import.meta.url))
const pin = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const hash = (text) => createHash("sha256").update(text).digest("hex")
function original(file) {
    return execFileSync("git", ["show", `${pin}:${file}`], { cwd: root, encoding: "utf8", timeout: 5000, maxBuffer: 1024 * 1024 }).replaceAll("\r\n", "\n")
}
const options = { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, esModuleInterop: true }
const compile = (text) => ts.transpileModule(text, { compilerOptions: options }).outputText
const managerText = original("src/electron/ai/setup/LocalModelManager.ts")
assert.equal(hash(managerText), "8f015d524d7f14ac536033530a15407989033556be3ac1c21ca3f0c265b1f80b")
const filesText = original("src/electron/utils/files.ts")
const filesAst = ts.createSourceFile("files.ts", filesText, ts.ScriptTarget.Latest, true)
const folderText = filesAst.statements.find((n) => ts.isFunctionDeclaration(n) && n.name?.text === "getFolderSize").getText(filesAst)
assert.equal(hash(folderText), "5d68a709d96083c42805dd8649af0ea30e03ddb377227224db852ab19db2109a")
const folder = new Function("fs", "join", `${compile(folderText.replace(/^export\s+/, ""))}; return getFolderSize`)(fs, path.join)
const responseText = original("src/electron/IPC/responsesMain.ts")
const responseAst = ts.createSourceFile("responsesMain.ts", responseText, ts.ScriptTarget.Latest, true)
let handler
function visit(node) {
    if (ts.isPropertyAssignment(node) && node.name.getText(responseAst) === "[Main.AI_GET_BIN]") handler = node.getText(responseAst)
    ts.forEachChild(node, visit)
}
visit(responseAst)
assert.equal(hash(handler), "6ae4d5a589fde80716918f6e64a4a29f304730f5a9ed149715a3514e0d773127")
const ipcText = original("src/electron/IPC/main.ts")
const ipcAst = ts.createSourceFile("main.ts", ipcText, ts.ScriptTarget.Latest, true)
const receiveText = ipcAst.statements.find((n) => ts.isFunctionDeclaration(n) && n.name?.text === "receiveMain").getText(ipcAst)
assert.equal(hash(receiveText), "943411675b781c85f088eab5ddfb4bb9294cfc5a5171e07372115ac956bda91b")
export function oracle(userData) {
    assert.equal(process.versions.node, "22.21.1")
    assert.equal(typeof userData, "string")
    assert.ok(Buffer.byteLength(userData) <= 32768)
    const calls = []
    const output = {}
    const require = (name) => {
        if (name === "electron")
            return {
                app: {
                    getPath(name) {
                        assert.equal(name, "userData")
                        calls.push(name)
                        return userData
                    }
                }
            }
        if (name === "fs") return fs
        if (name === "path") return path
        if (name === "../../utils/files")
            return {
                getFolderSize: folder,
                createFolder() {
                    throw Error("Inventory cannot create folders")
                }
            }
        throw Error("Inventory imported a model/backend: " + name)
    }
    new Function("require", "exports", compile(managerText))(require, output)
    const Main = { AI_GET_BIN: "AI_GET_BIN" }
    const mainResponses = new Function("Main", "LocalModelManager", `return ({${handler}})`)(Main, output.LocalModelManager)
    const receiveMain = new Function("Main", "ToMain", "MAIN", "mainResponses", `${compile(receiveText.replace(/^export\s+/, ""))}; return receiveMain`)(Main, {}, "MAIN", mainResponses)
    return {
        calls,
        folder,
        async inventory() {
            return await output.LocalModelManager.getDownloadedBinFiles()
        },
        async dispatch(payload, listenerId) {
            const replies = []
            await receiveMain({ reply: (...args) => replies.push(args) }, { channel: "AI_GET_BIN", data: payload }, listenerId)
            return replies
        }
    }
}
