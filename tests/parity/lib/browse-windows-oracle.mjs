// Actual immutable FreeShow functions + actual native fs.statSync. No stat model.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import ts from "typescript"

const root = fileURLToPath(new URL("../../../", import.meta.url))
const pin = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const sourcePin = "98e563c55026d2dbbdd217ee9b94962ed7fc8b82d893795355bd7dc0f54fc5de"
const control = process.argv.slice(2)
assert.ok(control.length === 0 || (control.length === 1 && control[0] === "--crlf-source-control"))
const forcedCrlf = control.length === 1
assert.equal(process.versions.node, "22.21.1", "use the pinned source Node runtime")
const input = Buffer.alloc(256 * 1024 + 1)
let length = 0
while (length < input.length) {
    const count = fs.readSync(0, input, length, input.length - length, null)
    if (!count) break
    length += count
}
assert.ok(length <= 256 * 1024)
const paths = JSON.parse(input.subarray(0, length))
assert.ok(Array.isArray(paths) && paths.length <= 32)
for (const value of paths) assert.ok(typeof value === "string" && Buffer.byteLength(value) <= 32768 && !value.includes("\0"))
const name = "src/electron/utils/files.ts"
const checkedOut = fs.readFileSync(path.join(root, name), "utf8")
const gitOutput = execFileSync("git", ["show", `${pin}:${name}`], { cwd: root, timeout: 5000, maxBuffer: 2 * 1024 * 1024 }).toString("utf8")
const hash = (text) => createHash("sha256").update(text).digest("hex")
const normalize = (text) => text.replace(/\r\n/g, "\n")
const asCrlf = (text) => normalize(text).replace(/\n/g, "\r\n")
function certify(working, immutable) {
    const source = normalize(working),
        pinned = normalize(immutable)
    assert.equal(hash(source), hash(pinned), "the real source oracle must remain immutable after CRLF normalization")
    assert.equal(hash(source), sourcePin, "the normalized source must match the immutable pinned SHA")
    return source
}
const source = certify(forcedCrlf ? asCrlf(checkedOut) : checkedOut, forcedCrlf ? asCrlf(gitOutput) : gitOutput)
// Portable controls execute the exact gate used before AST extraction. CRLF
// transport must pass, while a real source edit must still fail that gate.
assert.equal(certify(asCrlf(source), gitOutput), source)
assert.equal(certify(source, asCrlf(gitOutput)), source)
assert.throws(() => certify(source + "\n// non-line-ending mutation control\n", gitOutput), /must remain immutable/)
const ast = ts.createSourceFile(name, source, ts.ScriptTarget.Latest, true)
const declarations = ["getFileStats", "getFileInfo"].map((name) => {
    const item = ast.statements.find((item) => ts.isFunctionDeclaration(item) && item.name?.text === name)
    assert.ok(item, `missing actual source ${name}`)
    return item.getText(ast).replace(/^export\s+/, "")
})
const javascript = ts.transpileModule(declarations.join("\n"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
const diagnostics = []
const original = new Function("fs", "path", "actionComplete", `${javascript}\nreturn {getFileStats,getFileInfo}`)(fs, path, (error, title) => diagnostics.push({ code: error?.code, title }))
const values = paths.map((file) => {
    diagnostics.length = 0
    const value = original.getFileInfo(file)
    const stat = value?.stat
    // The real source's IPC clone loses prototype methods and nonenumerable
    // lazy Dates; structuredClone observes that actual boundary directly.
    return {
        value: structuredClone(value),
        keys: stat ? Object.keys(stat) : [],
        fields: stat ? Object.fromEntries(Object.keys(stat).map((name) => [name, typeof stat[name]])) : {},
        bits: stat
            ? Object.fromEntries(
                  Object.keys(stat).map((name) => {
                      const bytes = Buffer.alloc(8)
                      bytes.writeDoubleLE(stat[name])
                      return [name, bytes.toString("hex")]
                  })
              )
            : {},
        diagnostics: [...diagnostics]
    }
})
const result = JSON.stringify({ node: process.versions.node, platform: process.platform, pin, sourceSha256: hash(source), sourceTransport: forcedCrlf ? "forced-crlf" : "checkout", lineEndingControls: { crlfAccepted: true, sourceMutationRejected: true }, values })
assert.ok(Buffer.byteLength(result) <= 512 * 1024)
process.stdout.write(result)
