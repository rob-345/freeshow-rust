import assert from "node:assert/strict"
import { exec, execFileSync, spawn } from "node:child_process"
import { createHash } from "node:crypto"
import ts from "typescript"
import { Lines, privateBus, root } from "./spotify-oracle.mjs"
import path from "node:path"

const pin = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
function source(file, hash) {
    const text = execFileSync("git", ["show", `${pin}:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 }).replace(/\r\n/g, "\n")
    assert.equal(createHash("sha256").update(text).digest("hex"), hash, file)
    return text
}
const spotify = source("src/electron/utils/spotify.ts", "75d1ceab68e46c2a21dfc5a61f4e667f1d7d130e7b34b377938b7a97082918fd")
const responses = source("src/electron/IPC/responsesMain.ts", "70372b48b698f74f239b82f92e48dde69880a7c4d78af1a9f0e1723313fb57a3")
const main = source("src/electron/IPC/main.ts", "11afb38cdc1ca36134b48b7457da2cd27078dedd8b6d7a54cf24e0ecc0608609")
const parsed = ts.createSourceFile("responsesMain.ts", responses, ts.ScriptTarget.Latest, true)
let handler
function visit(node) {
    if (ts.isPropertyAssignment(node) && node.name.getText(parsed) === "[Main.SPOTIFY_COMMAND]") handler = node.initializer.getText(parsed)
    ts.forEachChild(node, visit)
}
visit(parsed)
assert.ok(handler)
const parsedMain = ts.createSourceFile("main.ts", main, ts.ScriptTarget.Latest, true)
const receive = parsedMain.statements.find((node) => ts.isFunctionDeclaration(node) && node.name.text === "receiveMain").getText(parsedMain)
const compile = (text) => ts.transpileModule(text, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, esModuleInterop: true } }).outputText
export function commandOracle(execute) {
    const module = { exports: {} }
    const calls = { commands: [], errors: 0, replies: [] }
    // Actual complete Spotify module, actual handler and receiveMain in one realm.
    new Function("require", "module", "exports", "console", "process", compile(spotify))(
        (name) => {
            if (name === "os") return { platform: () => "linux" }
            if (name === "child_process")
                return {
                    exec: (command, cb) => {
                        calls.commands.push(command)
                        return execute(command, cb)
                    }
                }
            throw Error(name)
        },
        module,
        module.exports,
        { error: () => calls.errors++ },
        { env: {} }
    )
    const invoke = new Function("executeSpotifyCommand", "Main", "ToMain", "MAIN", "console", "exports", compile(`const mainResponses={[Main.SPOTIFY_COMMAND]:${handler}};${receive};return receiveMain`))(module.exports.executeSpotifyCommand, { SPOTIFY_COMMAND: "SPOTIFY_COMMAND" }, {}, "MAIN", { error: () => calls.errors++ }, {})
    return { calls, send: (data, listener = "source-listener") => invoke({ reply: (...args) => calls.replies.push(args) }, { channel: "SPOTIFY_COMMAND", data }, listener) }
}
export async function commandBus(profile = "ordinary") {
    const bus = await privateBus()
    const peer = profile === null ? undefined : new Lines(spawn("/usr/bin/python3", [path.join(root, "crates/freeshow-spotify/tests/fixtures/commands.py"), profile], { env: bus.env, stdio: ["pipe", "pipe", "pipe"] }))
    const children = new Set()
    const pending = new Set()
    async function settle() {
        let timer
        try {
            await Promise.race([
                (async () => {
                    while (pending.size) await Promise.all([...pending])
                })(),
                new Promise((_, reject) => {
                    timer = setTimeout(() => reject(Error("source command children did not settle")), 8000)
                })
            ])
        } finally {
            clearTimeout(timer)
        }
    }
    try {
        if (peer) await peer.wait((line) => line === "READY")
        const original = commandOracle((command, callback) => {
            assert.ok(children.size < 8 && pending.size < 8, "bounded source child ownership")
            let done
            const completion = new Promise((resolve) => (done = resolve))
            pending.add(completion)
            const child = exec(command, { env: bus.env, maxBuffer: 1024 * 1024 }, callback)
            children.add(child)
            child.once("close", () => {
                children.delete(child)
                pending.delete(completion)
                done()
            })
            return child
        })
        return {
            env: bus.env,
            peer,
            original,
            events: () => (peer?.lines || []).filter((line) => line.startsWith("{")).map((line) => JSON.parse(line)),
            settle,
            close: async () => {
                const results = await Promise.allSettled([peer?.stop(), bus.close()])
                const failures = results.filter((result) => result.status === "rejected").map((result) => result.reason)
                // Closing the actual private bus settles held source invocations;
                // include children newly created by an already-running seek callback.
                try {
                    await settle()
                } catch (error) {
                    failures.push(error)
                }
                if (children.size) failures.push(Error("source child ownership remains"))
                if (failures.length) throw new AggregateError(failures, "Actual command fixture cleanup failed")
            }
        }
    } catch (error) {
        if (peer) await peer.stop()
        await bus.close()
        throw error
    }
}
