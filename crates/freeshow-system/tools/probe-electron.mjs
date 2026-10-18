import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import { fileURLToPath } from "node:url"
import ts from "typescript"
import { withElectron } from "./electron.mjs"

const root = fileURLToPath(new URL("../../../", import.meta.url))
const binary = process.env.FREESHOW_SYSTEM_PARITY || path.join(root, "target/debug/freeshow-system-parity" + (process.platform === "win32" ? ".exe" : ""))
const sourceFile = path.join(root, "src/electron/IPC/responsesMain.ts")
const source = fs.readFileSync(sourceFile, "utf8")
const ast = ts.createSourceFile(sourceFile, source, ts.ScriptTarget.Latest, true)
const names = ["getOS", "getDeviceName", "checkRamUsage"]
const declarations = names.map((name) => ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === name).getText(ast)).join("\n")
const javascript = ts.transpileModule(declarations, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
const sha = (value) => createHash("sha256").update(value).digest("hex")
function native(env = process.env) {
    const result = spawnSync(binary, [], { input: '{"op":"snapshot"}\n', encoding: "utf8", env, timeout: 10000 })
    assert.equal(result.status, 0, result.stderr || result.error?.message)
    const answer = JSON.parse(result.stdout)
    assert.equal(answer.ok, true, JSON.stringify(answer))
    return answer.value
}
const receipt = await withElectron(async (app, runtime, executablePath) => {
    assert.equal(runtime.versions.uv, "1.51.0")
    const sourceSample = await app.evaluate((_, javascript) => {
        const functions = new Function("os", javascript + ";return {getOS,getDeviceName,checkRamUsage}")(process.getBuiltinModule("os"))
        return { os: functions.getOS(), deviceName: functions.getDeviceName(), ram: functions.checkRamUsage() }
    }, javascript)
    const nativeSample = native()
    assert.deepEqual(nativeSample.os, sourceSample.os)
    assert.equal(nativeSample.deviceName, sourceSample.deviceName)
    assert.equal(nativeSample.ram.total, sourceSample.ram.total)
    const nativeDecision = await app.evaluate(
        (_, { javascript, sample }) => {
            const functions = new Function("os", javascript + ";return {checkRamUsage}")({ totalmem: () => sample.total, freemem: () => sample.free })
            return functions.checkRamUsage()
        },
        { javascript, sample: nativeSample.ram }
    )
    assert.deepEqual(nativeDecision, nativeSample.ram)
    const spoofed = native({ ...process.env, HOSTNAME: "spoofed.local", COMPUTERNAME: "spoofed.local" })
    assert.deepEqual(spoofed.os, sourceSample.os)
    assert.equal(spoofed.deviceName, sourceSample.deviceName)
    // Free is deliberately recorded independently, with no equality/tolerance
    // assertion across these separate system samples.
    return { schemaVersion: 1, kind: "native-system-info-source-parity", sourceCommit: "2d94defcf98175e4f5c9504e7f4fd1d33c35c975", sourceFunctionsSha256: sha(declarations), executableSha256: sha(fs.readFileSync(executablePath)), runtime: { version: runtime.version, packaged: runtime.packaged, versions: runtime.versions }, sourceSample, nativeSample, spoofedSample: spoofed, stableFieldsMatch: true, nativeMathMatchesActualSource: true, freeSamplesComparedForEquality: false }
})
const artifacts = process.env.FREESHOW_SYSTEM_ARTIFACTS || path.join(root, "test-output-rust/system-info")
fs.mkdirSync(artifacts, { recursive: true })
fs.writeFileSync(path.join(artifacts, "electron-system.json"), JSON.stringify(receipt, null, 2) + "\n")
process.stdout.write(JSON.stringify({ stableFieldsMatch: receipt.stableFieldsMatch, nativeMathMatchesActualSource: receipt.nativeMathMatchesActualSource, sourceFree: receipt.sourceSample.ram.free, nativeFree: receipt.nativeSample.ram.free }) + "\n")
