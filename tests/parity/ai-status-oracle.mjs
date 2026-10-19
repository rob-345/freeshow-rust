// Immutable source execution. Real fs/crypto/stream pipeline; no provider/network.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import crypto from "node:crypto"
import { Readable } from "node:stream"
import { pipeline } from "node:stream/promises"
import { execFileSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import vm from "node:vm"
import ts from "typescript"
const root = fileURLToPath(new URL("../../", import.meta.url))
const pin = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const hash = (s) => crypto.createHash("sha256").update(s).digest("hex")
function original(file, expected) {
    const source = execFileSync("git", ["show", `${pin}:${file}`], { cwd: root, encoding: "utf8", timeout: 5000, maxBuffer: 1024 * 1024 }).replaceAll("\r\n", "\n")
    if (expected) assert.equal(hash(source), expected, file)
    return source
}
const source = {
    status: original("src/electron/ai/setup/status.ts", "6831bd633a87ea383c45ade3b62d13bab81d17d0145b4c232fa7c38852b161b9"),
    manager: original("src/electron/ai/setup/LocalModelManager.ts", "8f015d524d7f14ac536033530a15407989033556be3ac1c21ca3f0c265b1f80b"),
    nemotron: original("src/electron/ai/setup/models/nemotron.ts", "6bfe1a15603986f83f6295a1df9a030de0ee7c017f5bf6e09203ee116db5154c"),
    download: original("src/electron/ai/setup/DownloadManager.ts", "a38ae48d33ac85610383635f36ff6a1da10b0d13615109ec7f65d718925d7245")
}
const options = { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, esModuleInterop: true }
const compile = (text) => ts.transpileModule(text, { compilerOptions: options }).outputText
const responseText = original("src/electron/IPC/responsesMain.ts")
const ast = ts.createSourceFile("responses.ts", responseText, ts.ScriptTarget.Latest, true)
let handler
function visit(node) {
    if (ts.isPropertyAssignment(node) && node.name.getText(ast) === "[Main.AI_GET_STATUS]") handler = node.getText(ast)
    ts.forEachChild(node, visit)
}
visit(ast)
assert.ok(handler)
assert.equal(handler, "[Main.AI_GET_STATUS]: (data) => aiGetModelStatus(data)")
const ipcText = original("src/electron/IPC/main.ts")
const ipcAST = ts.createSourceFile("main.ts", ipcText, ts.ScriptTarget.Latest, true)
const receive = ipcAST.statements.find((n) => ts.isFunctionDeclaration(n) && n.name?.text === "receiveMain").getText(ipcAST)
assert.equal(hash(receive), "943411675b781c85f088eab5ddfb4bb9294cfc5a5171e07372115ac956bda91b")
const bits = (n) => {
    const b = Buffer.alloc(8)
    b.writeDoubleBE(n)
    return b.toString("hex")
}
export const revision = /const MODEL_REVISION = "([a-f0-9]+)"/.exec(source.nemotron)[1]
export const files = [...source.nemotron.matchAll(/file: "([a-z0-9.]+)", sha256: "([a-f0-9]+)"/g)].map((m) => [m[1], m[2]])
files.push([/export const NEMOTRON_VAD_FILE = "([^"]+)"/.exec(source.nemotron)[1], /const NEMOTRON_VAD_SHA256 = "([^"]+)"/.exec(source.nemotron)[1]])
assert.equal(files.length, 5)
export function makeStamp(modelDir) {
    const entries = {}
    for (const [name, sha256] of files) {
        const { size, mtimeMs } = fs.statSync(path.join(modelDir, name))
        entries[name] = { sha256, size, mtimeMs }
    }
    return { revision, files: entries }
}
export function oracle(userData, { intelMac = false, injectedDigests = false } = {}) {
    assert.equal(process.versions.node, "22.21.1")
    const evidence = { hashed: [], stats: [] }
    const calls = [],
        errors = []
    const realFs = Object.create(fs)
    realFs.statSync = (...args) => {
        const stat = fs.statSync(...args)
        evidence.stats.push({ file: path.basename(args[0]), size_bits: bits(stat.size), mtime_bits: bits(stat.mtimeMs) })
        return stat
    }
    const modules = new Map()
    const forbidden = () => {
        throw Error("Explicit Nemotron status attempted unrelated effects")
    }
    const context = vm.createContext({ process: { platform: intelMac ? "darwin" : process.platform, arch: intelMac ? "x64" : process.arch }, console: { error: (...args) => errors.push(String(args[0])) }, Buffer, AbortController })
    const load = (name, text) => {
        const exports = {}
        const require = (id) => {
            if (id === "fs") return realFs
            if (id === "path") return path
            if (id === "crypto") return crypto
            if (id === "stream") return { Readable }
            if (id === "stream/promises") return { pipeline }
            if (id === "electron")
                return {
                    app: {
                        getPath: (key) => {
                            assert.equal(key, "userData")
                            calls.push(key)
                            return userData
                        }
                    },
                    net: { fetch: forbidden }
                }
            if (id === "../../utils/files") return { createFolder: forbidden, getFolderSize: forbidden }
            if (id.endsWith("types/IPC/ToMain")) return { ToMain: {} }
            if (id.endsWith("IPC/main")) return { sendToMain: forbidden }
            if (id === "../llm/llmProviders") return { getLLMProvider: forbidden }
            if (id === "./aiKeys") return { getAiKey: forbidden }
            const key = { "../DownloadManager": "download", "./models/nemotron": "nemotron", "./LocalModelManager": "manager" }[id]
            assert.ok(key, `Unexpected source import ${name} -> ${id}`)
            return modules.get(key) || load(key, source[key])
        }
        modules.set(name, exports)
        vm.runInContext(`(function(require,exports){${compile(text)}\n})`, context, { timeout: 2000 })(require, exports)
        return exports
    }
    const download = load("download", source.download).DownloadManager
    const compute = download.prototype.computeSha256
    download.prototype.computeSha256 = async function (file) {
        evidence.hashed.push(path.basename(file))
        const actual = await compute.call(this, file)
        if (injectedDigests) return files.find(([name]) => name === path.basename(file))[1]
        return actual
    }
    const status = load("status", source.status).aiGetModelStatus
    const Main = { AI_GET_STATUS: "AI_GET_STATUS" }
    const responses = new Function("Main", "aiGetModelStatus", `return ({${handler}})`)(Main, status)
    const receiveMain = new Function("Main", "ToMain", "MAIN", "mainResponses", `${compile(receive.replace(/^export\s+/, ""))};return receiveMain`)(Main, {}, "MAIN", responses)
    return {
        evidence,
        calls,
        errors,
        status: (payload = { engineId: "nemotron" }) => status(payload),
        async dispatch(payload, listener) {
            const replies = []
            await receiveMain({ reply: (...args) => replies.push(args) }, { channel: "AI_GET_STATUS", data: payload }, listener)
            return replies
        }
    }
}
