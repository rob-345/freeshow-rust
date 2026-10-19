// Actual immutable folder/now-playing functions, real private OS mkdir/write
// failures. Only renderer publication, Icecast and shell boundaries are traced.
import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import ts from "typescript"
import { parseFile } from "music-metadata"
import { wav } from "../../crates/freeshow-cover-images/tools/fixtures.mjs"

const root = fileURLToPath(new URL("../../", import.meta.url))
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
function original(file, expected) {
    const result = spawnSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 })
    assert.equal(result.status, 0, result.stderr)
    assert.equal(createHash("sha256").update(result.stdout).digest("hex"), expected)
    return result.stdout
}
const fileSource = original("src/electron/utils/files.ts", "98e563c55026d2dbbdd217ee9b94962ed7fc8b82d893795355bd7dc0f54fc5de")
const playingSource = original("src/electron/audio/nowPlaying.ts", "0a203138d9c283b3e5fa37f0aa46cd33df596d8ee7183ac6cd73d3c59cf3bc7c")
const compile = (source) => ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
const ast = ts.createSourceFile("files.ts", fileSource, ts.ScriptTarget.Latest, true)
const names = ["actionComplete", "doesPathExist", "readFile", "safeBufferToString", "fileContentMatches", "writeFile", "deleteFile", "makeDir", "createFolder", "getDataFolderRoot", "getDataFolderPath", "openInSystem"]
const declarations = ast.statements.filter((statement) => (ts.isFunctionDeclaration(statement) && names.includes(statement.name?.text)) || (ts.isVariableStatement(statement) && statement.declarationList.declarations.some((declaration) => ["dataFolderNames", "MAX_NODE_STRING_LENGTH"].includes(declaration.name.getText(ast)))))
assert.equal(declarations.length, names.length + 2)
const filesCode = compile(declarations.map((declaration) => declaration.getText(ast).replace(/^export /, "")).join("\n") + ";return {getDataFolderPath,writeFile,deleteFile,doesPathExist,openInSystem}")
const playingCode = compile(playingSource)

function fixture(t, action) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-folder-alerts-"))
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }))
    const selected = path.join(directory, "selected 日本語")
    fs.mkdirSync(selected)
    const folder = path.join(selected, "Audio"),
        bytes = Buffer.from("regular file blocks every actual mkdir")
    fs.writeFileSync(folder, bytes)
    const input = path.join(directory, "genuine source.wav")
    fs.writeFileSync(input, wav())
    const trace = [],
        diagnostics = []
    let phase = action === "set" ? "Preparation" : action === "unset" ? "Unset" : "Open"
    const publish = (channel, data) => {
        if (channel === "AUDIO_METADATA") phase = "PublishMetadata"
        trace.push({ kind: channel === "ALERT" ? "alert" : "metadata", channel, data, phase })
    }
    const actualFs = {
        ...fs,
        mkdirSync(...args) {
            trace.push({ kind: "mkdir", path: args[0], phase })
            try {
                return fs.mkdirSync(...args)
            } catch (error) {
                trace.push({ kind: "mkdir_failure", code: error.code, phase })
                throw error
            }
        }
    }
    const files = new Function("fs", "path", "config", "sendToMain", "ToMain", "console", "shell", filesCode)(
        actualFs,
        path,
        {
            get(key) {
                assert.equal(key, "dataPath")
                return selected
            }
        },
        publish,
        { ALERT: "ALERT" },
        { error: (...values) => diagnostics.push(values.map(String)) },
        {
            openPath() {
                throw new Error("Controlled missing path must not open a shell")
            },
            showItemInFolder() {
                throw new Error("Controlled missing path must not reveal a shell")
            }
        }
    )
    const moduleFiles = {
        ...files,
        getDataFolderPath(...args) {
            trace.push({ kind: "folder", phase })
            return files.getDataFolderPath(...args)
        },
        writeFile(...args) {
            const success = files.writeFile(...args)
            trace.push({ kind: "write", path: args[0], success, phase })
            return success
        }
    }
    const modules = {
        electron: {
            nativeImage: {
                createFromBuffer() {
                    throw new Error("Actual no-cover WAV must not create an image")
                }
            }
        },
        "music-metadata": { parseFile },
        path,
        "../../types/IPC/ToMain": { ToMain: { AUDIO_METADATA: "AUDIO_METADATA" } },
        "../IPC/main": { sendToMain: publish },
        "../utils/files": moduleFiles,
        "./IcecastSender": {
            IcecastSender: {
                updateMetadata(title) {
                    phase = action === "set" ? "PostMetadata" : "Unset"
                    trace.push({ kind: "title", title, phase })
                    // A genuine same-realm microtask marks the end of the source's
                    // synchronous post region before its awaited Text continuation.
                    if (action === "set")
                        queueMicrotask(() => {
                            phase = "Text"
                            trace.push({ kind: "yield", phase })
                        })
                }
            }
        }
    }
    const exports = {}
    new Function("require", "exports", "console", playingCode)(
        (name) => {
            assert.ok(Object.hasOwn(modules, name), name)
            return modules[name]
        },
        exports,
        { error: (...values) => diagnostics.push(values.map(String)) }
    )
    return { exports, files, trace, folder, bytes, input, diagnostics }
}
function folderAlerts(f) {
    return f.trace.filter((entry) => entry.kind === "alert" && entry.data === `Error: Could not create folder at: ${f.folder}!`)
}
function unchanged(f) {
    assert.ok(fs.statSync(f.folder).isFile())
    assert.deepEqual(fs.readFileSync(f.folder), f.bytes)
    assert.equal(fs.existsSync(path.join(f.folder, "NowPlaying.txt")), false)
    assert.equal(fs.existsSync(path.join(f.folder, "NowPlayingCover.png")), false)
}

test("actual SET publishes metadata, title then three post-folder ALERTs and one text-folder ALERT despite helper false writes", async (t) => {
    const f = fixture(t, "set")
    assert.equal(await f.exports.setPlayingState({ filePath: f.input, name: "Source name", unknownLang: [], format: "{title}", duration: 1 }), undefined)
    const alerts = folderAlerts(f)
    assert.equal(alerts.length, 4)
    assert.deepEqual(
        alerts.map((entry) => entry.phase),
        ["PostMetadata", "PostMetadata", "PostMetadata", "Text"]
    )
    assert.deepEqual(
        f.trace.filter((entry) => ["metadata", "title", "alert", "yield"].includes(entry.kind)).map((entry) => entry.kind),
        ["metadata", "title", "alert", "alert", "alert", "yield", "alert"]
    )
    assert.equal(f.trace.filter((entry) => entry.kind === "mkdir_failure").length, 4)
    assert.deepEqual(
        f.trace.filter((entry) => entry.kind === "write").map((entry) => [path.basename(entry.path), entry.success, entry.phase]),
        [["NowPlaying.txt", false, "Text"]]
    )
    assert.ok(f.diagnostics.length >= 5, "Actual mkdir and write failures are caught/logged separately")
    unchanged(f)
})

test("actual UNSET has two repeated folder ALERTs and a caught false text write without a metadata event or source ACK", (t) => {
    const f = fixture(t, "unset")
    assert.equal(f.exports.unsetPlayingAudio(), undefined)
    assert.equal(folderAlerts(f).length, 2)
    assert.deepEqual(
        f.trace.filter((entry) => ["metadata", "title", "alert"].includes(entry.kind)).map((entry) => [entry.kind, entry.phase]),
        [
            ["title", "Unset"],
            ["alert", "Unset"],
            ["alert", "Unset"]
        ]
    )
    assert.equal(f.trace.filter((entry) => entry.kind === "mkdir_failure").length, 2)
    assert.deepEqual(
        f.trace.filter((entry) => entry.kind === "write").map((entry) => entry.success),
        [false]
    )
    unchanged(f)
})

test("actual OPEN reports the folder-creation ALERT before the separate missing-path ALERT and never opens a shell", (t) => {
    const f = fixture(t, "open")
    assert.equal(f.exports.openNowPlaying(), undefined)
    assert.deepEqual(
        f.trace.filter((entry) => entry.kind === "alert").map((entry) => entry.data),
        [`Error: Could not create folder at: ${f.folder}!`, "This does not exist!"]
    )
    assert.equal(f.trace.filter((entry) => entry.kind === "mkdir_failure").length, 1)
    assert.equal(
        f.trace.some((entry) => entry.kind === "write" || entry.kind === "metadata"),
        false
    )
    unchanged(f)
})

test("actual path helper returns the trusted folder spelling after every failed mkdir without deduplicating ALERTs", (t) => {
    const f = fixture(t, "open")
    for (let index = 0; index < 4; index++) assert.equal(f.files.getDataFolderPath("audio"), f.folder)
    assert.equal(folderAlerts(f).length, 4)
    assert.equal(f.trace.filter((entry) => entry.kind === "mkdir_failure").length, 4)
    unchanged(f)
})
