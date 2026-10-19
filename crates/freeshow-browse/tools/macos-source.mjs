// Original FreeShow handlers + actual pinned Node Stats conversion. --project
// is pure arithmetic evidence; --requests requires real Darwin filesystem IO.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import v8 from "node:v8"
import { types } from "node:util"
import { execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import { fileURLToPath } from "node:url"
import ts from "typescript"

export const root = fileURLToPath(new URL("../../../", import.meta.url)),
    revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975",
    keys = ["dev", "mode", "nlink", "uid", "gid", "rdev", "blksize", "ino", "size", "blocks", "atimeMs", "mtimeMs", "ctimeMs", "birthtimeMs"],
    hash = (bytes) => createHash("sha256").update(bytes).digest("hex")
assert.equal(process.versions.node, "22.21.1", "actual pinned original source runtime required")
function source(file) {
    return execFileSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 }).replace(/\r\n/g, "\n")
}
function functions(text, names) {
    const tree = ts.createSourceFile("source.ts", text, ts.ScriptTarget.Latest, true)
    return names.map((name) => {
        const node = tree.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === name)
        assert.ok(node, name)
        return node.getText(tree).replace(/^export /, "")
    })
}
function variables(text, names) {
    const tree = ts.createSourceFile("source.ts", text, ts.ScriptTarget.Latest, true)
    return names.map((name) => {
        const node = tree.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText(tree) === name))
        assert.ok(node, name)
        return node.getText(tree).replace(/^export /, "")
    })
}
const nodeText = process.binding("natives")["internal/fs/utils"].replace(/\r\n/g, "\n"),
    nodeDeclarations = [...functions(nodeText, ["getStatsFromBinding", "msFromTimeSpec"]), ...variables(nodeText, ["kMsPerSec", "kNsPerMs"])],
    nodeHash = hash(nodeDeclarations.join("\n")),
    fileText = source("src/electron/utils/files.ts"),
    fileHash = hash(fileText)
assert.equal(nodeHash, "56779a34e11fa086c3a0f6d9719962542c6b4e654350de81f1ac0b96df5e4a94")
assert.equal(fileHash, "98e563c55026d2dbbdd217ee9b94962ed7fc8b82d893795355bd7dc0f54fc5de")
const nodeStats = new Function("Stats", "isBigInt64Array", nodeDeclarations.join("\n") + ";return getStatsFromBinding")(fs.Stats, types.isBigInt64Array)
function clone(value) {
    const copied = v8.deserialize(v8.serialize(value))
    function stats(value) {
        if (!value || typeof value !== "object") return
        if (Object.hasOwn(value, "mtimeMs")) {
            assert.deepEqual(Object.keys(value), keys, "actual numeric own Stats IPC shape")
            for (const key of keys) assert.equal(typeof value[key], "number")
        }
        for (const item of Object.values(value)) stats(item)
    }
    stats(copied)
    return copied
}
// Capture the source Number before JSON's decimal transport. Consumers must
// compare these bits, not a decimal parsed back into another binary64 value.
export function numberBits(value) {
    const bits = Object.create(null),
        bytes = Buffer.alloc(8)
    let count = 0
    function visit(value, pointer) {
        if (typeof value === "number") {
            assert.ok(++count <= 65536)
            bytes.writeDoubleBE(value)
            bits[pointer] = bytes.toString("hex")
        } else if (value && typeof value === "object") {
            for (const [key, item] of Object.entries(value)) visit(item, `${pointer}/${key.replace(/~/g, "~0").replace(/\//g, "~1")}`)
        }
    }
    visit(value, "")
    return bits
}
export function project(rows) {
    assert.ok(Array.isArray(rows) && rows.length <= 32)
    const results = rows.map((row) => {
        assert.ok(Array.isArray(row) && row.length === 18)
        const values = row.map((value, index) => {
            assert.ok(typeof value === "string" && /^-?\d{1,20}$/.test(value))
            const integer = BigInt(value)
            assert.ok(integer >= -(1n << 63n) && integer < 1n << 64n)
            // Source-backed libuv C assignment into uint64_t, then the actual
            // Node binary64 conversion. No expected timestamp formula is copied.
            return Number(index < 10 ? BigInt.asUintN(64, integer) : integer)
        })
        const data = clone(nodeStats(new Float64Array(values)))
        return { data, numberBits: numberBits(data) }
    })
    return { revision, fileHash, nodeHash, mode: "pure-node-projection", actualFilesystemObserved: false, results }
}
export async function sourceResults(cases) {
    assert.ok(Array.isArray(cases) && cases.length <= 64)
    const names = ["readFolderContent", "getFileStatsAsync", "readFolderAsync", "asyncPool", "getFileInfo", "getFileStats", "getExtension", "isMedia", "getFirstMediaFiles", "getSimularPaths", "getParentFolderName", "similarity", "readFolder", "isPermissionDeniedError"],
        errors = [],
        thumbnailAttempts = [],
        declarations = [...functions(fileText, names), ...variables(source("src/electron/data/media.ts"), ["imageExtensions", "videoExtensions"])],
        javascript = ts.transpileModule(declarations.join("\n") + ";return {FILE_INFO:getFileInfo,READ_FOLDER:readFolderContent,GET_SIMILAR:getSimularPaths}", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText,
        handlers = new Function("fs", "path", "join", "parse", "actionComplete", "console", "createThumbnail", javascript)(
            fs,
            path.posix,
            path.posix.join,
            path.posix.parse,
            (error, action) => {
                if (error) errors.push({ code: error.code, action })
            },
            { error() {}, warn() {} },
            (input) => {
                thumbnailAttempts.push(input)
                throw new Error("Thumbnail generation is outside this stat oracle")
            }
        ),
        results = []
    for (const item of cases) {
        assert.ok(item && Object.hasOwn(handlers, item.command))
        const roots = item.command === "FILE_INFO" ? [item.data] : item.command === "GET_SIMILAR" ? item.data.paths : Array.isArray(item.data.path) ? item.data.path : [item.data.path]
        assert.ok(Array.isArray(roots) && roots.length <= 8)
        assert.ok(roots.every((root) => typeof root === "string" && Buffer.byteLength(root) <= 32768))
        if (item.command === "READ_FOLDER") {
            assert.ok(item.data.depth === undefined || (typeof item.data.depth === "number" && Number.isFinite(item.data.depth) && item.data.depth >= -1 && item.data.depth <= 3))
            assert.ok(item.data.generateThumbnails === undefined || item.data.generateThumbnails === false)
        }
        const data = clone(await handlers[item.command](structuredClone(item.data))),
            undefinedNoMedia =
                item.command === "READ_FOLDER"
                    ? Object.entries(data)
                          .filter(([, value]) => value.isFolder && Object.hasOwn(value, "noMedia") && value.noMedia === undefined)
                          .map(([key]) => key)
                          .sort()
                    : []
        results.push({ data, numberBits: numberBits(data), undefinedNoMedia })
    }
    assert.deepEqual(thumbnailAttempts, [], "this stat corpus must not silently substitute thumbnail generation")
    return { platform: process.platform, errors, results }
}
export async function requests(cases) {
    assert.equal(process.platform, "darwin", "real macOS source evidence requires macOS execution")
    return { revision, fileHash, nodeHash, mode: "real-darwin-source", actualFilesystemObserved: true, ...(await sourceResults(cases)) }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    let input = ""
    for await (const piece of process.stdin) {
        input += piece
        assert.ok(Buffer.byteLength(input) <= 65536)
    }
    const parsed = JSON.parse(input),
        mode = process.argv[2],
        output = JSON.stringify(mode === "--project" ? project(parsed) : mode === "--requests" ? await requests(parsed) : assert.fail("Use --project or --requests"))
    assert.ok(Buffer.byteLength(output) <= 1024 * 1024)
    process.stdout.write(output + "\n")
}
