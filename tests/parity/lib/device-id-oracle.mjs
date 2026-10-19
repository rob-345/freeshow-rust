// Immutable source/dependency oracle; no product imports and no real ID output.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import crypto from "node:crypto"
import { execFileSync, execSync } from "node:child_process"
import { createRequire } from "node:module"
import { fileURLToPath } from "node:url"
import ts from "typescript"

export const root = fileURLToPath(new URL("../../../", import.meta.url))
const pin = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const require = createRequire(import.meta.url)
const entry = require.resolve("node-machine-id")
const packageInfo = JSON.parse(fs.readFileSync(path.join(path.dirname(entry), "../package.json"), "utf8"))
assert.equal(packageInfo.version, "1.1.12")
const dependency = fs.readFileSync(entry, "utf8")
assert.equal(crypto.createHash("sha256").update(dependency).digest("hex"), "fbc7e17e8465a0bcedf37f2beb9a24b907a9b801b9783f14968e937b7cd85bf2")
const source = execFileSync("git", ["show", `${pin}:src/electron/utils/helpers.ts`], { cwd: root, encoding: "utf8", timeout: 5000, maxBuffer: 1024 * 1024 })
const ast = ts.createSourceFile("helpers.ts", source, ts.ScriptTarget.Latest, true)
const declaration = ast.statements.find((item) => ts.isFunctionDeclaration(item) && item.name?.text === "getMachineId").getText(ast)
assert.equal(crypto.createHash("sha256").update(declaration).digest("hex"), "6c64f4e19a91a77879fea6b7f6a2a4dac4b22d5d439bee5bd4ca26f0c28b766b")
const javascript = ts.transpileModule(declaration.replace(/^export\s+/, ""), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
const createSource = new Function("require", "crypto", "console", `${javascript}\nreturn getMachineId`)
const responses = execFileSync("git", ["show", `${pin}:src/electron/IPC/responsesMain.ts`], { cwd: root, encoding: "utf8", timeout: 5000, maxBuffer: 1024 * 1024 })
const responseAst = ts.createSourceFile("responsesMain.ts", responses, ts.ScriptTarget.Latest, true)
const table = responseAst.statements.find((item) => ts.isVariableStatement(item) && item.declarationList.declarations.some((item) => item.name.getText(responseAst) === "mainResponses")).declarationList.declarations[0].initializer
const handler = table.properties.find((item) => item.name.getText(responseAst) === "[Main.DEVICE_ID]").getText(responseAst)
assert.equal(crypto.createHash("sha256").update(handler).digest("hex"), "1d03de88e24c56efcaeb3863dac3ed3e2545489ce4aa207e4abc35cb2e802d80")
const createHandler = new Function("Main", "getMachineId", `return ({${handler}})[Main.DEVICE_ID]`)
export const fixtureUuid = "00010203-0405-4607-8809-0a0b0c0d0e0f"
export function sourcePlan(input) {
    const calls = []
    const writes = []
    const stored = input.value
    const config = input.configAbsent ? undefined : {}
    if (config && !input.getterAbsent)
        config.get = () => {
            calls.push("get")
            if (input.readFailed) throw Error("fixture getter failure")
            return stored
        }
    if (config && !input.setterAbsent)
        config.set = (key, value) => {
            calls.push("set")
            writes.push({ key, value })
            if (input.writeFailed) throw Error("fixture setter failure")
        }
    const get = createSource(
        (name) => {
            if (name === "../data/store") {
                if (input.loadFailed) throw Error("fixture module failure")
                return { config }
            }
            assert.equal(name, "node-machine-id")
            return {
                machineIdSync() {
                    calls.push("legacy")
                    if (input.legacyFailed) throw Error("fixture legacy failure")
                    return input.legacy ?? "fixture-legacy-id"
                }
            }
        },
        {
            randomUUID() {
                calls.push("random")
                if (input.randomFailed) throw Error("fixture entropy failure")
                return fixtureUuid
            }
        },
        { warn() {} }
    )
    return { reply: createHandler({ DEVICE_ID: "DEVICE_ID" }, get)({ ignored: true }), writes, calls }
}
function loadedDependency(platform, exec) {
    const module = { exports: {} }
    const context = vm.createContext({
        module,
        exports: module.exports,
        process: { platform, arch: process.arch, env: { ...process.env } },
        require(name) {
            if (name === "crypto") return crypto
            assert.equal(name, "child_process")
            return {
                execSync: exec,
                exec() {
                    throw Error("Async oracle boundary not requested")
                }
            }
        }
    })
    new vm.Script(dependency, { filename: "node-machine-id-1.1.12.js" }).runInContext(context, { timeout: 1000 })
    return module.exports
}
export function sourceExpose(platform, output) {
    try {
        return { ok: true, hash: loadedDependency(platform, () => Buffer.from(output)).machineIdSync() }
    } catch {
        return { ok: false }
    }
}
function limitedExec(command) {
    return execSync(command, { timeout: 2000, maxBuffer: 32768, stdio: ["ignore", "pipe", "pipe"] })
}
export function sourceNativeSalted(salt) {
    assert.match(salt, /^[0-9a-f]{64}$/)
    try {
        const id = loadedDependency(process.platform, limitedExec).machineIdSync()
        return { ok: true, digest: crypto.createHash("sha256").update(Buffer.from(salt, "hex")).update(id).digest("hex") }
    } catch {
        return { ok: false, error: "actual_legacy_provider_failed" }
    }
}
function shellQuote(text) {
    return "'" + text.replaceAll("'", "'\\''") + "'"
}
export function sourceLinuxFiles(paths, hostname) {
    assert.equal(process.platform, "linux")
    assert.equal(paths.length, 2)
    const machine = loadedDependency("linux", (command) => {
        assert.equal(command, "( cat /var/lib/dbus/machine-id /etc/machine-id 2> /dev/null || hostname ) | head -n 1 || :")
        command = command.replace("/var/lib/dbus/machine-id /etc/machine-id", `${shellQuote(paths[0])} ${shellQuote(paths[1])}`).replace(" || hostname )", ` || printf '%s\\n' ${shellQuote(hostname)} )`)
        return limitedExec(command)
    })
    return { hash: machine.machineIdSync() }
}
export function execute(input) {
    assert.equal(process.versions.node, "22.21.1", "Run the immutable oracle under the pinned source Node")
    switch (input.mode) {
        case "plans":
            assert.ok(Array.isArray(input.cases) && input.cases.length <= 512)
            return input.cases.map(sourcePlan)
        case "expose":
            assert.ok(Array.isArray(input.cases) && input.cases.length <= 512)
            return input.cases.map((item) => sourceExpose(item.platform, Buffer.from(item.output, "base64")))
        case "exposeBytes":
            assert.ok(Array.isArray(input.cases) && input.cases.length <= 512)
            return input.cases.map((item) => sourceExpose(item.platform, Buffer.from(item.bytes)))
        case "nativeSalted":
            return sourceNativeSalted(input.salt)
        case "linuxFiles":
            return sourceLinuxFiles(input.paths, input.hostname)
        default:
            throw Error("Unknown source-only oracle mode")
    }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    const maximum = 4 * 1024 * 1024
    const bytes = Buffer.alloc(maximum + 1)
    let length = 0
    while (length < bytes.length) {
        const count = fs.readSync(0, bytes, length, bytes.length - length, null)
        if (!count) break
        length += count
    }
    assert.ok(length <= maximum)
    const output = JSON.stringify(execute(JSON.parse(bytes.subarray(0, length).toString("utf8"))))
    assert.ok(Buffer.byteLength(output) <= 32768)
    process.stdout.write(output)
}
