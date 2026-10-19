// Source-only desktop expectation: actual pinned helpers and actual session bus.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { exec, execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import { fileURLToPath } from "node:url"
import ts from "typescript"
import { oracle as aiOracle } from "../parity/lib/ai-local-oracle.mjs"
import { sourceOracle as spotifyOracle } from "../parity/spotify-oracle.mjs"

assert.equal(process.versions.node, "22.21.1", "Pinned source oracle requires Node22.21.1")
assert.equal(process.platform, "linux", "This desktop null-state corpus requires actual Linux D-Bus")
const [userData, phase] = process.argv.slice(2)
assert.ok(userData && path.isAbsolute(userData) && Buffer.byteLength(userData) <= 32768)
assert.ok(["normal", "restarted", "switched", "source-check"].includes(phase), "Unknown desktop fixture phase")
assert.ok(process.env.DBUS_SESSION_BUS_ADDRESS?.startsWith("unix:") && !process.env.DBUS_SESSION_BUS_ADDRESS.includes(";"), "Existing private single Unix session bus required")
const dbus = "/usr/bin/dbus-send"
const name = "org.mpris.MediaPlayer2.spotify"
const env = { PATH: "/usr/bin:/bin" }
for (const key of ["DBUS_SESSION_BUS_ADDRESS", "LANG", "LC_ALL", "LC_CTYPE", "HOME"]) if (process.env[key] !== undefined) env[key] = process.env[key]

function busCall(method, ...args) {
    return execFileSync(dbus, ["--session", "--print-reply", "--dest=org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus." + method, ...args], { env, encoding: "utf8", timeout: 3000, maxBuffer: 64 * 1024 })
}
function absentService() {
    const owner = busCall("NameHasOwner", "string:" + name)
    assert.match(owner, /\bboolean false\b/, "Private fixture unexpectedly has a Spotify owner")
    const activatable = busCall("ListActivatableNames")
    assert.ok(!activatable.includes('string "' + name + '"'), "Private fixture must not activate an installed Spotify service")
    return { nameHasOwner: false, activatable: false }
}

const before = absentService()
let child
let closed
let callbackCount = 0
let serviceUnknown = false
const original = spotifyOracle((command, callback) => {
    assert.equal(child, undefined, "Only one actual immutable source state query is expected")
    child = exec(command, { env, timeout: 2500, maxBuffer: 1024 * 1024 }, (error, stdout) => {
        callbackCount++
        serviceUnknown = !!error?.message?.includes("ServiceUnknown")
        callback(error, stdout)
    })
    closed = new Promise((resolve, reject) => {
        child.once("error", reject)
        child.once("close", (code, signal) => resolve({ code, signal }))
    })
    closed.catch(() => {})
})
const sourceCommit = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const root = fileURLToPath(new URL("../../", import.meta.url))
const source = (file) => execFileSync("git", ["show", `${sourceCommit}:${file}`], { cwd: root, encoding: "utf8", timeout: 5000, maxBuffer: 1024 * 1024 }).replaceAll("\r\n", "\n")
const responsesAst = ts.createSourceFile("responsesMain.ts", source("src/electron/IPC/responsesMain.ts"), ts.ScriptTarget.Latest, true)
let handler
function visit(node) {
    if (ts.isPropertyAssignment(node) && node.name.getText(responsesAst) === "[Main.SPOTIFY_GET_STATE]") handler = node.getText(responsesAst)
    ts.forEachChild(node, visit)
}
visit(responsesAst)
assert.equal(createHash("sha256").update(handler).digest("hex"), "8d655e635c64c162bbf128b08089ec4f48a0f9a13af8979c4874e5f5f6069944")
const ipcAst = ts.createSourceFile("main.ts", source("src/electron/IPC/main.ts"), ts.ScriptTarget.Latest, true)
const receiveText = ipcAst.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "receiveMain").getText(ipcAst)
assert.equal(createHash("sha256").update(receiveText).digest("hex"), "943411675b781c85f088eab5ddfb4bb9294cfc5a5171e07372115ac956bda91b")
const Main = { SPOTIFY_GET_STATE: "SPOTIFY_GET_STATE" }
const handlers = new Function("Main", "getSpotifyState", `return ({${handler}})`)(Main, original.get)
const compiledReceive = ts.transpileModule(receiveText.replace(/^export\s+/, ""), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
const receiveMain = new Function("Main", "ToMain", "MAIN", "mainResponses", compiledReceive + ";return receiveMain")(Main, {}, "MAIN", handlers)
const spotifyListener = "ai-spotify-probe-" + phase + "-SPOTIFY_GET_STATE"
const spotifyReplies = []
let state, processExit
try {
    await receiveMain({ reply: (...args) => spotifyReplies.push(args) }, { channel: "SPOTIFY_GET_STATE", data: { ignored: true, title: "not Spotify" } }, spotifyListener)
    assert.deepEqual(spotifyReplies, [["MAIN", { channel: "SPOTIFY_GET_STATE", data: null }, spotifyListener]])
    state = spotifyReplies[0][1].data
    processExit = await closed
} finally {
    // Retain actual source shell ownership on assertion/helper failure too.
    if (child && child.exitCode === null && child.signalCode === null) child.kill("SIGTERM")
    if (closed) await closed
}
assert.equal(state, null)
assert.equal(callbackCount, 1)
assert.equal(serviceUnknown, true, "Only genuine ServiceUnknown certifies this source-null case")
assert.equal(original.calls.errors, 0, "Original source suppresses its expected missing-service console error")
assert.equal(original.calls.commands.length, 1)
assert.ok(Number.isInteger(processExit.code) && processExit.code !== 0 && processExit.signal === null, "Actual original shell must fail normally and be waited")
const after = absentService()

const ai = aiOracle(userData)
const rows = await ai.inventory()
assert.equal(rows.length, 1, "The private desktop fixture must be genuinely nonempty")
assert.equal(rows[0].path, path.join(userData, "bin/nemotron"))
assert.ok(Number.isSafeInteger(rows[0].size) && rows[0].size > 0)
const listener = "ai-spotify-probe-" + phase + "-AI_GET_BIN"
const replies = await ai.dispatch({ path: "/ignored-renderer-root", engine: "ignored", userData: "/ignored-renderer-root" }, listener)
assert.deepEqual(replies, [["MAIN", { channel: "AI_GET_BIN", data: rows }, listener]])
assert.deepEqual(ai.calls, ["userData"], "Actual manager captures only its native userData authority once")
process.stdout.write(
    JSON.stringify({
        phase,
        ai: { data: rows, rowKeys: rows.map(Object.keys), messageKeys: Object.keys(replies[0][1]), outer: replies[0][0], listener },
        spotify: { data: state, messageKeys: Object.keys(spotifyReplies[0][1]), outer: spotifyReplies[0][0], listener: spotifyListener, sourceNullReason: "ServiceUnknown", callbackCount, sourceConsoleErrors: original.calls.errors, sourceShellWaited: true, processExit, before, after },
        provenance: {
            sourceCommit,
            node: process.versions.node,
            managerSha256: "8f015d524d7f14ac536033530a15407989033556be3ac1c21ca3f0c265b1f80b",
            folderFunctionSha256: "5d68a709d96083c42805dd8649af0ea30e03ddb377227224db852ab19db2109a",
            spotifyHelperSha256: "75d1ceab68e46c2a21dfc5a61f4e667f1d7d130e7b34b377938b7a97082918fd",
            dbusSendSha256: createHash("sha256").update(fs.readFileSync(dbus)).digest("hex")
        },
        rawIdentityArchived: false
    }) + "\n"
)
