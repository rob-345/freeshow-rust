import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { spawnSync } from "node:child_process"
import { createRequire } from "node:module"
import ts from "typescript"
import { root } from "../oracle.mjs"
const require = createRequire(import.meta.url),
    upath = require("upath")
assert.equal(require("upath/package.json").version, "2.0.1")
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const sources = new Map()
function ast(file) {
    if (!sources.has(file)) {
        const result = spawnSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 4 * 1024 * 1024 })
        assert.equal(result.status, 0, result.stderr)
        sources.set(file, ts.createSourceFile(file, result.stdout.replace(/\r\n/g, "\n"), ts.ScriptTarget.Latest, true))
    }
    return sources.get(file)
}
function declaration(file, name, variable = false) {
    const source = ast(file),
        node = source.statements.find((node) => (variable ? ts.isVariableStatement(node) && node.declarationList.declarations.some((v) => v.name.getText(source) === name) : ts.isFunctionDeclaration(node) && node.name?.text === name))
    assert.ok(node, `Pinned ${name} missing`)
    return node.getText(source).replace(/^export\s+/, "")
}
function handler(name) {
    const source = ast("src/electron/IPC/responsesMain.ts"),
        response = source.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((v) => v.name.getText(source) === "mainResponses"))
    return response.declarationList.declarations
        .find((v) => v.name.getText(source) === "mainResponses")
        .initializer.properties.find((p) => p.name?.getText(source) === `[Main.${name}]`)
        .initializer.getText(source)
}
export function mediaCopyOracle(item) {
    const copies = [],
        folders = [],
        errors = [],
        alerts = [],
        opened = [],
        pending = []
    const boundary = Object.create(fs)
    boundary.copyFile = (source, target, callback) => {
        copies.push({ source, target })
        fs.copyFile(source, target, callback)
    }
    boundary.mkdirSync = (folder, options) => {
        folders.push(folder)
        return fs.mkdirSync(folder, options)
    }
    const file = "src/electron/utils/files.ts",
        code = [
            ...["MAX_NODE_STRING_LENGTH", "dataFolderNames", "currentlyBundling"].map((n) => declaration(file, n, true)),
            ...["bundleMediaFiles", "addToMediaFolder", "getFileParentFolderId", "readFile", "readFolder", "safeBufferToString", "parseShow", "parseJSON", "doesPathExist", "doesPathExistAsync", "getDataFolderPath", "getMediaSyncFolderPath", "createFolder", "makeDir", "copyFileAsync", "asyncPool", "actionComplete", "isPermissionDeniedError", "openInSystem"].map((n) => declaration(file, n)),
            declaration("src/electron/data/thumbnails.ts", "filePathHashCode"),
            `const sourceCopy=addToMediaFolder;addToMediaFolder=(...args)=>{const p=sourceCopy(...args);pending.push(p);return p;};globalThis.result={BUNDLE_MEDIA_FILES:(${handler("BUNDLE_MEDIA_FILES")}),MEDIA_FOLDER_COPY:(${handler("MEDIA_FOLDER_COPY")}),getFileParentFolderId};`
        ].join("\n")
    const context = vm.createContext({
        fs: boundary,
        path,
        upath,
        pending,
        getDataFolderRoot: () => item.dataRoot,
        getStore: (id) => ({ PROJECTS: item.stores.projects, OVERLAYS: item.stores.overlays, TEMPLATES: item.stores.templates, config: { mediaFolderPath: item.syncRoot || "" } })[id],
        sendToMain: (channel, data) => alerts.push({ channel, data }),
        ToMain: { ALERT: "ALERT" },
        shell: {
            openPath: async (path) => {
                opened.push(path)
                return ""
            },
            showItemInFolder: (path) => opened.push(path)
        },
        console: { error: (...args) => errors.push(args) }
    })
    new vm.Script(ts.transpileModule(code, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).runInContext(context, { timeout: 1000 })
    return { ...context.result, copies, folders, errors, alerts, opened, pending }
}
