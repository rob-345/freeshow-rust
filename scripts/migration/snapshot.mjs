import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { createHash } from "node:crypto"
import { fileURLToPath } from "node:url"
import ts from "typescript"

export const root = fileURLToPath(new URL("../../", import.meta.url))
const upstreamCommit = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const read = (name) => fs.readFileSync(path.join(root, name), "utf8")
const parse = (name) => ts.createSourceFile(name, read(name), ts.ScriptTarget.Latest, true)

function enumValues(name, enumName) {
    const node = parse(name).statements.find((node) => ts.isEnumDeclaration(node) && node.name.text === enumName)
    if (!node) throw new Error(`Missing ${enumName} in ${name}`)
    return node.members.map((member) => {
        if (!member.initializer || !ts.isStringLiteral(member.initializer)) throw new Error("Expected a string enum")
        return member.initializer.text
    })
}

function listFiles(directory) {
    return fs
        .readdirSync(path.join(root, directory), { withFileTypes: true })
        .flatMap((entry) => {
            const name = `${directory}/${entry.name}`
            return entry.isDirectory() ? listFiles(name) : [name]
        })
        .sort()
}

// Execute only the upstream constants module, with type imports erased by TypeScript.
// No Electron imports, application startup, user files, or network are involved.
const exports = {}
const javascript = ts.transpileModule(read("src/electron/data/defaults.ts"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 }
}).outputText
vm.runInNewContext(javascript, { exports }, { timeout: 1000 })
const defaults = Object.fromEntries(["defaultConfig", "defaultSettings", "defaultSyncedSettings"].map((name) => [name, exports[name]]))

const channelsFile = parse("src/types/Channels.ts")
const channels = channelsFile.statements.filter(ts.isVariableStatement).flatMap((statement) => statement.declarationList.declarations.filter((node) => ts.isStringLiteral(node.initializer)).map((node) => node.initializer.text))
const sourceFiles = ["src/electron", "src/server"].flatMap(listFiles).filter((name) => /\.(ts|svelte)$/.test(name) && !/\.test\.ts$/.test(name))
const inventory = {
    schemaVersion: 1,
    upstream: { repository: "https://github.com/ChurchApps/FreeShow", commit: upstreamCommit, version: JSON.parse(read("package.json")).version },
    channels,
    mainCommands: enumValues("src/types/IPC/Main.ts", "Main"),
    rendererCommands: enumValues("src/types/IPC/ToMain.ts", "ToMain"),
    sourceFiles: sourceFiles.map((name) => ({ path: name, lines: read(name).split("\n").length, sha256: createHash("sha256").update(read(name).replaceAll("\r\n", "\n")).digest("hex") }))
}

const artifacts = {
    "docs/migration/upstream-inventory.json": inventory,
    "tests/parity/fixtures/defaults.json": defaults
}
let changed = false
for (const [name, value] of Object.entries(artifacts)) {
    const content = JSON.stringify(value, null, 2) + "\n"
    if (process.argv.includes("--check")) {
        if (!fs.existsSync(path.join(root, name)) || read(name) !== content) {
            console.error(`Upstream snapshot drift: ${name}. Review the pinned source before regenerating.`)
            changed = true
        }
    } else {
        fs.mkdirSync(path.dirname(path.join(root, name)), { recursive: true })
        fs.writeFileSync(path.join(root, name), content)
    }
}
if (changed) process.exitCode = 1
else console.log(`Upstream: ${inventory.mainCommands.length} MAIN commands, ${channels.length} channels, ${sourceFiles.length} runtime files`)
