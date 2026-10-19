import assert from "node:assert/strict"
import { test } from "node:test"
import { execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import fs from "node:fs"
import vm from "node:vm"
import ts from "typescript"
import { root } from "./oracle.mjs"

const file = "src/frontend/audio/audioPlayer.ts",
    original = execFileSync("git", ["show", `2d94defcf98175e4f5c9504e7f4fd1d33c35c975:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1048576 }).replace(/\r\n/g, "\n"),
    current = fs.readFileSync(new URL("../../src/frontend/audio/audioPlayer.ts", import.meta.url), "utf8")
assert.equal(createHash("sha256").update(original).digest("hex"), "0731f9e0de7a001e9bf71ff650b4b4e19bcd10e59474c54d258719d6d3fda918")

function load(source, native, input, resolved, duration = false) {
    const trace = [],
        instances = [],
        stops = [],
        tree = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true),
        declaration = tree.statements.find((node) => ts.isClassDeclaration(node) && node.name?.text === "AudioPlayer"),
        helpers = tree.statements
            .filter((node) => ts.isFunctionDeclaration(node) && ["createAudioElement", "loadAudioFile"].includes(node.name?.text))
            .map((node) => node.getText(tree).replace(/^export /, ""))
            .join("\n")
    class Audio {
        constructor(...args) {
            trace.push(["construct", ...args])
            this.listeners = new Map()
            this.crossOriginValue = null
            if (args.length) this.srcValue = args[0]
            instances.push(this)
        }
        set crossOrigin(value) {
            trace.push(["cors", value])
            this.crossOriginValue = value
        }
        get crossOrigin() {
            return this.crossOriginValue
        }
        set src(value) {
            trace.push(["src", value])
            this.srcValue = value
        }
        get src() {
            return this.srcValue
        }
        addEventListener(name, callback, options) {
            trace.push(["listen", name, options?.once ?? false])
            this.listeners.set(name, callback)
        }
        removeEventListener(name) {
            trace.push(["remove", name])
            this.listeners.delete(name)
        }
    }
    const context = vm.createContext({
        Audio,
        window: { api: native ? { resolveFilePath() {} } : {} },
        AudioMultichannel: { DEFAULT_CHANNELS: 2, MAX_CHANNELS: 8 },
        encodeFilePath: (path) => {
            assert.equal(path, input)
            return resolved
        },
        console: { error() {} }
    })
    new vm.Script(ts.transpileModule(helpers + "\n" + declaration.getText(tree).replace(/^export /, "") + ";globalThis.Player=AudioPlayer;globalThis.Loader=loadAudioFile", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).runInContext(context, { timeout: 1000 })
    context.Player.stop = (id) => stops.push(id)
    const promise = duration ? context.Loader(input) : context.Player.createAudio(input, input)
    assert.equal(instances.length, 1)
    return { audio: instances[0], promise, trace, stops }
}
const plain = (value) => JSON.parse(JSON.stringify(value))

test("actual AudioPlayer preserves original Electron, direct online and unresolved/non-native constructors and load events", async () => {
    for (const duration of [false, true]) {
        for (const [native, input, source] of [
            [false, "/tmp/original 敬拜.wav", "file:///tmp/original%20%E6%95%AC%E6%8B%9C.wav"],
            [false, "https://example.invalid/a.wav", "https://example.invalid/a.wav"],
            [true, "https://example.invalid/a.wav", "https://example.invalid/a.wav"],
            [true, "data:audio/wav;base64,AAAA", "data:audio/wav;base64,AAAA"],
            [true, "blob:tauri://localhost/fixture", "blob:tauri://localhost/fixture"],
            [true, "http://127.0.0.1:12345/direct.wav", "http://127.0.0.1:12345/direct.wav"],
            [true, "/tmp/not-granted.wav", ""],
            [true, "/tmp/ordinary.wav", "file:///tmp/ordinary.wav"]
        ]) {
            const before = load(original, native, input, source, duration),
                after = load(current, native, input, source, duration)
            assert.deepEqual(after.trace, before.trace)
            before.audio.listeners.get(duration ? "loadedmetadata" : "canplay")()
            after.audio.listeners.get(duration ? "loadedmetadata" : "canplay")()
            assert.equal(await before.promise, before.audio)
            assert.equal(await after.promise, after.audio)
            assert.equal(after.audio.crossOrigin, null)
        }
    }
})

test("native mapped asset gets anonymous CORS before one src assignment, retaining actual source handlers and error behavior", async () => {
    for (const duration of [false, true]) {
        const input = "/tmp/real 敬拜.wav",
            source = "http://127.0.0.1:12345/native-capability/native-asset",
            before = load(original, true, input, source, duration),
            after = load(current, true, input, source, duration)
        assert.deepEqual(after.trace.slice(0, 2), [["construct"], ["cors", "anonymous"]])
        assert.deepEqual(after.trace.at(-1), ["src", source], "all original listeners precede native source assignment")
        assert.equal(after.trace.filter(([kind]) => kind === "src").length, 1, "no second source assignment/fetch introduced")
        assert.deepEqual(
            after.trace.filter(([kind]) => kind === "listen"),
            before.trace.filter(([kind]) => kind === "listen")
        )
        assert.equal(after.audio.crossOrigin, "anonymous")
        before.audio.listeners.get("error")({ type: "error" })
        after.audio.listeners.get("error")({ type: "error" })
        assert.equal(await before.promise, null)
        assert.equal(await after.promise, null)
        assert.deepEqual(plain(after.stops), plain(before.stops))
        const loaded = load(current, true, input, source, duration)
        loaded.audio.listeners.get(duration ? "loadedmetadata" : "canplay")()
        assert.equal(await loaded.promise, loaded.audio)
        if (!duration) {
            loaded.audio._cleanupListeners()
            assert.deepEqual([...loaded.audio.listeners.keys()], ["canplay", "error"])
        }
    }
})
