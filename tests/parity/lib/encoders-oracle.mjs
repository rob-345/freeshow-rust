// Pinned source oracle shared by controlled-process and real desktop checks.
import assert from "node:assert/strict"
import { execFile } from "node:child_process"
import { createHash } from "node:crypto"
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { promisify } from "node:util"
import vm from "node:vm"
import ts from "typescript"
const root = fileURLToPath(new URL("../../../", import.meta.url))
export function original({ executable = null, sourcePath = executable, mtimeMs = 0, platform = process.platform, arch = process.arch, device = "", cache, disableHardware = false, processMs } = {}) {
    const calls = [],
        writes = []
    let cacheBytes = cache
    const load = (filename, hash, require, injected = {}) => {
        const source = fs.readFileSync(path.join(root, filename), "utf8")
        assert.equal(createHash("sha256").update(source.replace(/\r\n/g, "\n")).digest("hex"), hash, "pinned encoder source changed")
        const context = vm.createContext({
            exports: {},
            require,
            process: { platform, arch, env: { FREESHOW_VAAPI_DEVICE: device } },
            console: { warn() {}, info() {} },
            ...injected
        })
        new vm.Script(
            ts.transpileModule(source, {
                compilerOptions: {
                    module: ts.ModuleKind.CommonJS,
                    target: ts.ScriptTarget.ES2022,
                    esModuleInterop: true
                }
            }).outputText,
            { filename }
        ).runInContext(context, { timeout: 1000 })
        return context.exports
    }
    const profiles = load("src/electron/streaming/encoderProfiles.ts", "26e2923819f19b5f3c40992fbe0dd70a81fe2180e5d23bbf00f5a49ce1ac0787", () => {
        throw new Error("unexpected profile import")
    })
    function tracedExec() {
        throw new Error("promisify custom path expected")
    }
    tracedExec[promisify.custom] = async (bin, args, options) => {
        calls.push({ bin, args: Array.from(args), options: { ...options } })
        return promisify(execFile)(executable || bin, args, {
            ...options,
            ...(processMs ? { timeout: processMs } : {})
        })
    }
    const detection = load("src/electron/streaming/encoderDetection.ts", "edde3d614ea3774b65ab31555a7be69d26c863f505afa02d8f2ceb20e373cb5b", (name) => {
        if (name === "child_process") return { execFile: tracedExec }
        if (name === "electron") return { app: { getPath: () => "/injected-unused-profile" } }
        if (name === "fs")
            return {
                statSync: () => ({ mtimeMs }),
                readFileSync: () => {
                    if (cacheBytes === undefined) throw new Error("missing cache")
                    return cacheBytes
                },
                writeFileSync: (_path, bytes) => {
                    writes.push(bytes)
                    cacheBytes = bytes
                }
            }
        if (name === "path") return path
        if (name === "util") return { promisify }
        if (name.includes("data/store")) return { config: { get: () => disableHardware } }
        if (name === "./encoderProfiles") return profiles
        if (name === "./ffmpegManager") return { resolveFfmpegPath: async () => sourcePath }
        throw new Error(`unexpected detection import ${name}`)
    })
    // These declarations are evaluated from the source itself for private helpers.
    const source = fs.readFileSync(path.join(root, "src/electron/streaming/encoderDetection.ts"), "utf8")
    const ast = ts.createSourceFile("encoderDetection.ts", source, ts.ScriptTarget.Latest, true)
    const node = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "firstErrorLine")
    const context = vm.createContext({})
    new vm.Script(
        ts.transpileModule(node.getText(ast) + ";globalThis.error=firstErrorLine", {
            compilerOptions: { target: ts.ScriptTarget.ES2022 }
        }).outputText
    ).runInContext(context, { timeout: 1000 })
    return { profiles, detection, calls, writes, error: context.error }
}
