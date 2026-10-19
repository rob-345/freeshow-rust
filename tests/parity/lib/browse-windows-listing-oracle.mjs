// Actual pinned functions + real native filesystem. Windows tests require win32.
// The only controlled effect is thumbnail creation, recorded as an unresolved intent.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import ts from "typescript"

const root = fileURLToPath(new URL("../../../", import.meta.url))
const pin = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
assert.equal(process.versions.node, "22.21.1", "use the pinned source Node runtime")
const portable = process.argv[2] === "--portable-source-control"
assert.ok(process.argv.length === 2 || (portable && process.argv.length === 3))
const hash = (text) => createHash("sha256").update(text).digest("hex")
const normalize = (text) => text.replace(/\r\n/g, "\n")
const pins = {
    "src/electron/utils/files.ts": "98e563c55026d2dbbdd217ee9b94962ed7fc8b82d893795355bd7dc0f54fc5de",
    "src/electron/data/media.ts": "ee86974242ab480a834e40bdb6f6e7436f0d4d23d7390526cc9f570d6043d376"
}
function immutable(name) {
    const source = normalize(fs.readFileSync(path.join(root, name), "utf8"))
    const pinned = normalize(execFileSync("git", ["show", `${pin}:${name}`], { cwd: root, timeout: 5000, maxBuffer: 2 * 1024 * 1024 }).toString("utf8"))
    const certify = (working, original) => {
        assert.equal(hash(normalize(working)), hash(normalize(original)), "immutable source changed")
        assert.equal(hash(normalize(working)), pins[name], "normalized pinned source SHA")
    }
    certify(source, pinned)
    certify(source.replace(/\n/g, "\r\n"), pinned)
    certify(source, pinned.replace(/\n/g, "\r\n"))
    assert.throws(() => certify(source + "\n// real mutation\n", pinned), /immutable source changed/)
    return ts.createSourceFile(name, source, ts.ScriptTarget.Latest, true)
}
const files = immutable("src/electron/utils/files.ts")
const media = immutable("src/electron/data/media.ts")
const functions = ["actionComplete", "isPermissionDeniedError", "readFolder", "readFolderAsync", "getFileStatsAsync", "getExtension", "readFolderContent", "isMedia", "getFirstMediaFiles", "getSimularPaths", "getParentFolderName", "similarity", "asyncPool"]
const declarations = functions.map((name) => {
    const declaration = files.statements.find((item) => ts.isFunctionDeclaration(item) && item.name?.text === name)
    assert.ok(declaration, `actual function ${name}`)
    return declaration.getText(files).replace(/^export\s+/, "")
})
for (const name of ["imageExtensions", "videoExtensions"]) {
    const declaration = media.statements.find((item) => ts.isVariableStatement(item) && item.declarationList.declarations.some((variable) => variable.name.getText(media) === name))
    assert.ok(declaration, `actual media table ${name}`)
    declarations.unshift(declaration.getText(media).replace(/^export\s+/, ""))
}
const javascript = ts.transpileModule(declarations.join("\n"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
const diagnostics = []
const thumbnails = []
function diagnostic(...args) {
    assert.ok(diagnostics.length < 128, "bounded fixture diagnostics")
    diagnostics.push(args.map((value) => (value && typeof value === "object" ? { code: value.code } : String(value))))
}
const source = new Function("fs", "path", "join", "parse", "console", "createThumbnail", `${javascript}\nreturn {readFolderContent,getSimularPaths,readFolder}`)(fs, path, path.join, path.parse, { error: diagnostic }, (file) => {
    assert.ok(thumbnails.length < 128, "bounded fixture thumbnail effects")
    thumbnails.push(file)
    return "" // controlled effect only; no claim that a thumbnail was generated
})
let requests
if (portable) {
    requests = [
        { command: "READ_FOLDER", data: { path: [] } },
        { command: "GET_SIMILAR", data: { paths: [] } }
    ]
} else {
    assert.equal(process.platform, "win32", "the listing corpus must execute real Windows fs, not path.win32 simulation")
    const input = Buffer.alloc(256 * 1024 + 1)
    let length = 0
    while (length < input.length) {
        const count = fs.readSync(0, input, length, input.length - length, null)
        if (!count) break
        length += count
    }
    assert.ok(length <= 256 * 1024)
    requests = JSON.parse(input.subarray(0, length))
}
assert.ok(Array.isArray(requests) && requests.length <= 32)
const results = []
for (const request of requests) {
    assert.ok(["READ_FOLDER", "GET_SIMILAR", "READ_DIRECTORY_NAMES"].includes(request.command))
    const paths = request.command === "READ_FOLDER" ? (Array.isArray(request.data.path) ? request.data.path : [request.data.path]) : request.command === "GET_SIMILAR" ? request.data.paths : [request.data]
    assert.ok(Array.isArray(paths) && paths.length <= 32)
    for (const file of paths) assert.ok(typeof file === "string" && Buffer.byteLength(file) <= 32768 && !file.includes("\0"))
    if (request.command === "READ_FOLDER") assert.ok(request.data.depth === undefined || (Number.isFinite(request.data.depth) && request.data.depth <= 16))
    diagnostics.length = 0
    thumbnails.length = 0
    const data = structuredClone(request.data)
    const actual = request.command === "READ_FOLDER" ? await source.readFolderContent(data) : request.command === "GET_SIMILAR" ? source.getSimularPaths(data) : source.readFolder(data)
    const undefinedFields = []
    const statsBits = {}
    if (request.command === "READ_FOLDER") {
        for (const [key, value] of Object.entries(actual)) {
            if (Object.hasOwn(value, "noMedia") && value.noMedia === undefined) undefinedFields.push(key)
            if (value.stats) {
                statsBits[key] = Object.fromEntries(
                    Object.keys(value.stats).map((name) => {
                        const bytes = Buffer.alloc(8)
                        bytes.writeDoubleLE(value.stats[name])
                        return [name, bytes.toString("hex")]
                    })
                )
            }
        }
    }
    results.push({
        // Preserve the own-undefined observation separately from this JSON
        // projection. The native existing noMedia:null placeholder is hydrated.
        data: JSON.parse(JSON.stringify(structuredClone(actual), (_key, value) => (value === undefined ? null : value))),
        undefinedFields,
        statsBits,
        thumbnails: [...thumbnails],
        diagnostics: [...diagnostics],
        inputAfter: data
    })
}
const result = JSON.stringify({ node: process.versions.node, uv: process.versions.uv, platform: process.platform, pin, sourcePins: pins, lineEndingControls: true, controlledThumbnailEffect: true, results })
assert.ok(Buffer.byteLength(result) <= 4 * 1024 * 1024, "bounded source fixture output")
process.stdout.write(result)
