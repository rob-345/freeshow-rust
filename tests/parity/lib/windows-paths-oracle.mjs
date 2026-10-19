import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import ts from "typescript"

assert.equal(process.versions.node, "22.21.1", "use pinned Node for the actual path.win32 oracle")
export const root = fileURLToPath(new URL("../../../", import.meta.url))
const filename = "src/electron/utils/files.ts"
const pin = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const source = fs.readFileSync(path.join(root, filename), "utf8")
const original = execFileSync("git", ["show", `${pin}:${filename}`], { cwd: root, maxBuffer: 2 * 1024 * 1024, timeout: 5000 }).toString("utf8")
export const sourceSha256 = createHash("sha256").update(source).digest("hex")
assert.equal(sourceSha256, createHash("sha256").update(original).digest("hex"))
const ast = ts.createSourceFile(filename, source, ts.ScriptTarget.Latest, true)
const names = ["getParentFolderName", "getExtension"]
const code = names.map((name) => {
    const declaration = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === name)
    assert.ok(declaration, `actual pinned source missing ${name}`)
    return declaration.getText(ast).replace(/^export\s+/, "")
})
const javascript = ts.transpileModule(code.join("\n"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
export const sourceFunctions = new Function("path", "parse", `${javascript}\nreturn {${names.join(",")}}`)(path.win32, path.win32.parse)
export const win32 = path.win32
