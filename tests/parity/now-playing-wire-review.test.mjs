// Independent descriptor-edge regressions: actual MessagePort structured clone
// plus the immutable source formatter. No app, native IO or parser is simulated.
import assert from "node:assert/strict"
import { createHash } from "node:crypto"
import fs from "node:fs"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import { MessageChannel } from "node:worker_threads"
import { test } from "node:test"
import ts from "typescript"

const root = process.env.FREESHOW_NOW_PLAYING_REVIEW_ROOT || fileURLToPath(new URL("../../", import.meta.url))
const text = fs.readFileSync(path.join(root, "src/frontend/rust/nowPlayingWire.ts"), "utf8")
const js = ts.transpileModule(text, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText
const { encodeNowPlayingCommand: encode } = await import(`data:text/javascript;base64,${Buffer.from(js).toString("base64")}`)
const loaded = spawnSync("git", ["show", "2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/audio/nowPlaying.ts"], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 })
assert.equal(loaded.status, 0, loaded.stderr)
assert.equal(createHash("sha256").update(loaded.stdout).digest("hex"), "0a203138d9c283b3e5fa37f0aa46cd33df596d8ee7183ac6cd73d3c59cf3bc7c")
const ast = ts.createSourceFile("nowPlaying.ts", loaded.stdout, ts.ScriptTarget.Latest, true)
const declarations = ast.statements.filter((statement) => (ts.isFunctionDeclaration(statement) && ["convertDynamicValues", "getArtist"].includes(statement.name?.text)) || (ts.isVariableStatement(statement) && statement.declarationList.declarations.some((declaration) => declaration.name.getText(ast) === "dynamicValues")))
assert.equal(declarations.length, 3)
const formatter = new Function("getNowPlayingMediaPath", ts.transpileModule(declarations.map((declaration) => declaration.getText(ast)).join("\n") + ";return convertDynamicValues", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText)(() => "/fixture/Audio/NowPlayingCover.png")
const context = { runtimeId: "native-音", libraryGeneration: 3, dataPath: "/selected/日本語/🎹" }
const data = () => ({ filePath: "/source.wav", name: "Name", unknownLang: ["Artist", "Title", "Album"], format: "{duration_s}" })
const decode = (message) => JSON.parse(new TextDecoder().decode(encode(message, context))).command

async function cloned(value) {
    const { port1, port2 } = new MessageChannel()
    let timer
    try {
        return await new Promise((resolve, reject) => {
            timer = setTimeout(() => reject(new Error("Actual MessagePort clone timed out")), 2000)
            port1.once("message", resolve)
            port1.once("messageerror", reject)
            port2.postMessage(value)
        })
    } finally {
        clearTimeout(timer)
        port1.close()
        port2.close()
    }
}

test("non-enumerable own numeric duration follows actual MessagePort omission and emits no orphan Number tag", async () => {
    for (const value of [61.25, -0, NaN, Infinity, -Infinity]) {
        const payload = data()
        Object.defineProperty(payload, "duration", { value, enumerable: false })
        const original = await cloned({ channel: "NOW_PLAYING", data: payload })
        const command = decode({ channel: "NOW_PLAYING", data: payload })
        assert.equal(Object.hasOwn(original.data, "duration"), false)
        assert.equal(Object.hasOwn(command.data, "duration"), false)
        assert.equal(Object.hasOwn(command, "duration"), false, "native input requires an own-null placeholder for any Number tag")
        // Actual source duration_s reads undefined.toString(). Omission must not
        // silently become a finite private duration or a successful null value.
        await assert.rejects(formatter(original.data, null, undefined), TypeError)
        await assert.rejects(formatter(command.data, null, undefined), TypeError)
    }
})

test("non-enumerable duration getter is skipped without evaluation by both actual clone and wire snapshot", async () => {
    let calls = 0
    const payload = data()
    Object.defineProperty(payload, "duration", {
        enumerable: false,
        get() {
            calls++
            throw new Error("Hidden getter must not execute")
        }
    })
    const original = await cloned({ channel: "NOW_PLAYING", data: payload })
    const command = decode({ channel: "NOW_PLAYING", data: payload })
    assert.equal(calls, 0)
    assert.equal(Object.hasOwn(original.data, "duration"), false)
    assert.equal(Object.hasOwn(command.data, "duration"), false)
    assert.equal(Object.hasOwn(command, "duration"), false)
})

test("hidden array indices become holes in real IPC clone and are explicitly unsupported rather than silently restored", async () => {
    let calls = 0
    for (const accessor of [false, true]) {
        const duration = [61]
        Object.defineProperty(
            duration,
            "0",
            accessor
                ? {
                      enumerable: false,
                      get() {
                          calls++
                          return 61
                      }
                  }
                : { enumerable: false, value: 61 }
        )
        const payload = { ...data(), duration }
        const original = await cloned({ channel: "NOW_PLAYING", data: payload })
        assert.equal(original.data.duration.length, 1)
        assert.equal(Object.hasOwn(original.data.duration, "0"), false)
        assert.equal(await formatter(original.data, null, undefined), "")
        assert.equal(await formatter({ ...data(), duration: [61] }, null, undefined), "61", "serializing a hidden value would change actual source text")
        assert.throws(() => decode({ channel: "NOW_PLAYING", data: payload }), { code: "now_playing_wire_shape" })
    }
    assert.equal(calls, 0)
})

test("supported dense arrays keep real clone/source formatting and finite negative zero unchanged", async () => {
    for (const duration of [[-0, 61.25], [Number.MIN_VALUE], [[]], [true, null, "音"]]) {
        const original = await cloned({ channel: "NOW_PLAYING", data: { ...data(), duration } })
        const command = decode({ channel: "NOW_PLAYING", data: { ...data(), duration } })
        assert.equal(Object.hasOwn(command, "duration"), false)
        assert.deepEqual(command.data.duration, original.data.duration)
        assert.equal(await formatter(command.data, null, undefined), await formatter(original.data, null, undefined))
    }
})
