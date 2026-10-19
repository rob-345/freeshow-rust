import assert from "node:assert/strict"
import { test } from "node:test"
import { execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import fs from "node:fs"
import vm from "node:vm"
import ts from "typescript"
import { root } from "./oracle.mjs"
const file = "src/frontend/components/media/video/videoPlayer.ts",
    original = execFileSync("git", ["show", `2d94defcf98175e4f5c9504e7f4fd1d33c35c975:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1048576 }).replace(/\r\n/g, "\n"),
    current = fs.readFileSync(new URL("../../src/frontend/components/media/video/videoPlayer.ts", import.meta.url), "utf8"),
    url = "http://127.0.0.1:12345/" + "a".repeat(64) + "/" + "b".repeat(32),
    alternative = "http://127.0.0.1:12345/" + "a".repeat(64) + "/" + "c".repeat(32),
    context = { runtimeId: "cors-test", libraryGeneration: 1, dataPath: "/fixture" }
assert.equal(createHash("sha256").update(original).digest("hex"), "915987dd58d721b8a890a0ed7dee282daeac4ca9d18c3ceac27041da3afcc0b5")
const compile = (source) => ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
const registrySource = fs.readFileSync(new URL("../../src/frontend/rust/mediaUrl.ts", import.meta.url), "utf8")
const registryRealm = vm.createContext({ TextEncoder, URL, exports: {} })
new vm.Script(compile(registrySource)).runInContext(registryRealm)
function registry(path, state = "granted") {
    let captured = { ...context }
    const errors = [],
        registry = registryRealm.exports.createMediaUrlRegistry(
            () => captured,
            (error) => errors.push(error)
        )
    if (state !== "missing") assert.equal(registry.publish(captured, [{ id: "b".repeat(32), sourcePath: path, url, mimeType: "video/webm", length: 123, context: captured }]), true)
    if (state === "stale") captured = { ...context, libraryGeneration: 2 }
    return { registry, errors }
}
function load(source, { native = false, input = "/fixture/video.webm", resolved = url, kind = "playback", state, online } = {}) {
    const trace = [],
        instances = [],
        stops = [],
        lookups = [],
        fetches = [],
        active = registry(input, state)
    const resolver = state ? (path) => active.registry.resolve(path) : () => resolved
    class Audio {
        constructor(...args) {
            trace.push(["construct", ...args])
            this.listeners = new Map()
            this.mode = null
            this.source = ""
            this.duration = 12
            this.currentTime = 0
            this.volume = 1
            this.paused = true
            instances.push(this)
            if (args.length) this.resource(args[0])
        }
        resource(value) {
            this.source = value
            fetches.push({ source: value, mode: this.mode })
            trace.push(["fetch", value, this.mode])
        }
        set src(value) {
            trace.push(["src", value])
            this.resource(value)
        }
        get src() {
            return this.source
        }
        set crossOrigin(value) {
            trace.push(["cors", value])
            this.mode = value
        }
        get crossOrigin() {
            return this.mode
        }
        addEventListener(name, handler, options) {
            trace.push(["listen", name, options?.once ?? false])
            this.listeners.set(name, handler)
        }
        removeEventListener(name) {
            trace.push(["removeListener", name])
            this.listeners.delete(name)
        }
        setAttribute(name, value) {
            trace.push(["attribute", name, value])
            if (name === "src") this.resource(value)
        }
        removeAttribute(name) {
            trace.push(["removeAttribute", name])
            if (name === "src") this.source = ""
        }
        remove() {
            trace.push(["remove"])
        }
        load() {
            trace.push(["load"])
        }
        pause() {
            trace.push(["pause"])
            this.paused = true
        }
        play() {
            trace.push(["play"])
            this.paused = false
            return Promise.resolve()
        }
    }
    const ast = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true),
        declaration = ast.statements.find((node) => ts.isClassDeclaration(node) && node.name?.text === "VideoPlayer"),
        helpers = ast.statements
            .filter((node) => (ts.isFunctionDeclaration(node) && ["isNativeVideoSource", "createVideoAudioElement"].includes(node.name?.text)) || (ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText(ast) === "nativeVideoAudioSources")))
            .map((node) => node.getText(ast).replace(/^export /, ""))
            .join("\n")
    const values = { media: {}, playing: [], providers: online ? { [input]: { id: "provider", type: online } } : {} }
    const realm = vm.createContext({
        Audio,
        HTMLAudioElement: Audio,
        window: { api: native ? { resolveFilePath: resolver } : {} },
        document: {
            createElement(name) {
                trace.push(["createElement", name])
                return new Audio()
            }
        },
        encodeFilePath(path) {
            lookups.push(path)
            return resolver(path)
        },
        get: (value) => value,
        media: values.media,
        playingVideos: values.playing,
        playerVideos: values.providers,
        getYouTubeData: async () => ({ duration: 19 }),
        getVimeoData: async () => ({ duration: 23 }),
        TimeInterpolator: class {},
        AudioAnalyser: {
            updateSource(...args) {
                trace.push(["updateSource", args[0], args[1].src, args[1].crossOrigin, args[2]])
            },
            setSourceVolume(...args) {
                trace.push(["gain", ...args])
            }
        },
        setTimeout(_callback, delay) {
            trace.push(["timeout", delay])
        },
        console: { error() {} }
    })
    new vm.Script(compile(helpers + "\n" + declaration.getText(ast).replace(/^export /, "") + ";globalThis.Player=VideoPlayer")).runInContext(realm, { timeout: 1000 })
    const player = realm.Player
    player.stop = (path) => stops.push(path)
    player.getAudio = () => null
    player.getVolume = () => 1
    player.isOutputMuted = () => false
    player.updateVolume = (...args) => trace.push(["updateVolume", ...args])
    const promise = kind === "metadata" ? player.getDuration(input) : player.createAudio(input, input, ["output"], !!online)
    return { player, audio: instances[0], instances, promise, trace, lookups, stops, fetches, errors: active.errors, input, kind }
}
async function loaded(value) {
    value.audio.listeners.get(value.kind === "metadata" ? "loadedmetadata" : "canplay")()
    return await value.promise
}
async function clone(value, mode = "preserve", resource) {
    await loaded(value)
    value.audio.paused = false
    value.audio.currentTime = 7.95
    if (mode !== "preserve") value.audio.crossOrigin = mode
    if (resource !== undefined) value.audio.src = resource
    const video = { path: value.input, audio: value.audio, linkedOutputIds: ["output"], fromTime: 0.4, toTime: 8 }
    const result = value.player.handleSoftLoop(video, value.audio, 1.2, true)
    await Promise.resolve()
    await Promise.resolve()
    return { result, video, audio: video.audio }
}
const plain = (value) => JSON.parse(JSON.stringify(value))

test("actual pinned creators retain complete Electron/direct/unmapped load and cleanup traces", async () => {
    for (const kind of ["playback", "metadata"])
        for (const [native, input, resolved] of [
            [false, "/tmp/原本.webm", "file:///tmp/%E5%8E%9F%E6%9C%AC.webm"],
            [true, "https://example.invalid/a.webm", "https://example.invalid/a.webm"],
            [true, url, url],
            [false, url, url],
            [true, "data:video/webm;base64,AAAA", "data:video/webm;base64,AAAA"],
            [true, "blob:tauri://localhost/fixture", "blob:tauri://localhost/fixture"],
            [true, "bundled.webm", "bundled.webm"],
            [true, "/unregistered.webm", ""],
            [true, "/ordinary.webm", "file:///ordinary.webm"]
        ]) {
            const before = load(original, { native, input, resolved, kind }),
                after = load(current, { native, input, resolved, kind })
            assert.deepEqual(after.trace, before.trace)
            assert.equal(await loaded(after), kind === "metadata" ? 12 : after.audio)
            await loaded(before)
            assert.deepEqual(after.trace, before.trace)
            assert.equal(after.audio.crossOrigin, null)
            assert.deepEqual(after.lookups, [input], "one encoding/resolution per creator")
        }
})

test("real provider virtual branches do not create/fetch an Audio element", async () => {
    for (const online of ["youtube", "vimeo", "unknown"]) {
        const before = load(original, { online, native: true }),
            after = load(current, { online, native: true })
        assert.deepEqual(plain(await after.promise), plain(await before.promise))
        assert.deepEqual(after.trace, before.trace)
        assert.equal(after.instances.length, 0)
        assert.equal(after.lookups.length, 0)
    }
})

test("both native mapped creators configure anonymous before their first fetch and retain source events/errors", async () => {
    for (const kind of ["playback", "metadata"]) {
        const input = "/fixture/音楽 🙌.webm",
            after = load(current, { native: true, input, kind, state: "granted" }),
            before = load(original, { native: true, input, kind, state: "granted" })
        assert.deepEqual(after.fetches, [{ source: url, mode: "anonymous" }])
        assert.deepEqual(before.fetches, [{ source: url, mode: null }])
        assert.deepEqual(after.lookups, [input])
        const fetchIndex = after.trace.findIndex(([kind]) => kind === "fetch")
        assert.ok(after.trace.slice(0, fetchIndex).filter(([kind]) => kind === "listen").length >= 2, "source listeners exist before the native fetch")
        assert.deepEqual(
            after.trace.filter(([kind]) => kind === "listen"),
            before.trace.filter(([kind]) => kind === "listen")
        )
        after.audio.listeners.get("error")({ type: "error" })
        before.audio.listeners.get("error")({ type: "error" })
        assert.equal(await after.promise, kind === "metadata" ? 0 : null)
        assert.equal(await before.promise, kind === "metadata" ? 0 : null)
        assert.deepEqual(plain(after.stops), plain(before.stops))
        assert.deepEqual(
            after.trace.filter(([kind]) => ["pause", "remove", "load", "removeAttribute"].includes(kind)),
            before.trace.filter(([kind]) => ["pause", "remove", "load", "removeAttribute"].includes(kind))
        )
    }
})

test("soft-loop preserves mode before fetching an unchanged resolved grant and actual source swap/gain order", async () => {
    const before = load(original, { native: true, state: "granted" }),
        after = load(current, { native: true, state: "granted" })
    const old = await clone(before, "anonymous"),
        next = await clone(after)
    assert.equal(old.audio.crossOrigin, null, "real original clone loses the corrected parent mode")
    assert.equal(next.audio.crossOrigin, "anonymous")
    assert.deepEqual(after.fetches.at(-1), { source: url, mode: "anonymous" })
    assert.deepEqual(before.fetches.at(-1), { source: url, mode: null })
    assert.deepEqual(after.lookups, [after.input], "clone never asks the registry to reauthorize/re-resolve its URL")
    assert.equal(next.video.crossfadeAudio, after.audio)
    assert.equal(next.video.audio, next.audio)
    assert.equal(next.result, old.result)
    const effects = (value) => value.trace.filter(([kind]) => ["gain", "pause", "play", "updateVolume", "timeout"].includes(kind))
    assert.deepEqual(effects(after), effects(before), "the actual original soft-loop effects remain ordered")
    assert.equal(after.trace.find(([kind]) => kind === "updateSource")[3], "anonymous")
    // An already-created crossfade is reused by the actual next source tick;
    // it must not fetch another resource or ask the registry for another grant.
    const reusable = load(current, { native: true, state: "granted" })
    await loaded(reusable)
    reusable.audio.paused = false
    reusable.audio.currentTime = 7
    const video = { path: reusable.input, audio: reusable.audio, linkedOutputIds: ["output"], fromTime: 0.4, toTime: 8 }
    reusable.player.handleSoftLoop(video, reusable.audio, 1.2, true)
    const held = video.crossfadeAudio,
        count = reusable.instances.length
    assert.equal(held.crossOrigin, "anonymous")
    reusable.audio.currentTime = 7.95
    reusable.player.handleSoftLoop(video, reusable.audio, 1.2, true)
    assert.equal(video.audio, held)
    assert.equal(reusable.instances.length, count)
    assert.equal(reusable.fetches.length, 2)
    assert.deepEqual(reusable.lookups, [reusable.input])
})

test("native clone preserves every non-null mode; resource replacements and direct URLs retain source clone semantics", async () => {
    for (const mode of ["anonymous", "", "use-credentials"]) {
        const value = load(current, { native: true, state: "granted" }),
            next = await clone(value, mode)
        assert.deepEqual(value.fetches.at(-1), { source: url, mode })
        assert.equal(next.audio.crossOrigin, mode, "mode preservation is not server support for credentials")
    }
    const cleared = load(current, { native: true, state: "granted" }),
        withoutMode = await clone(cleared, null)
    assert.equal(withoutMode.audio.crossOrigin, null)
    assert.deepEqual(cleared.fetches.at(-1), { source: url, mode: null })
    for (const config of [{ native: false }, { native: true, input: url, resolved: url }, { native: true, state: "granted" }]) {
        for (const replacement of [alternative, "https://example.invalid/external.webm"]) {
            const before = load(original, config),
                after = load(current, config)
            const old = await clone(before, "anonymous", replacement),
                next = await clone(after, "anonymous", replacement)
            assert.equal(next.audio.crossOrigin, old.audio.crossOrigin)
            assert.deepEqual(after.fetches.at(-1), { source: replacement, mode: null })
            assert.equal(after.lookups.length, 1)
        }
    }
})

test("missing and stale actual registry grants keep original empty-source error behavior without fallback", async () => {
    for (const kind of ["metadata", "playback"])
        for (const state of ["missing", "stale"]) {
            const before = load(original, { native: true, kind, state }),
                after = load(current, { native: true, kind, state })
            assert.deepEqual(after.trace, before.trace)
            assert.equal(after.audio.src, "")
            assert.ok(after.errors.some((error) => error.code === "assets_unregistered"))
            assert.equal(
                after.fetches.some(({ source }) => source.startsWith("file:") || source === after.input),
                false
            )
            after.audio.listeners.get("error")({ type: "error" })
            before.audio.listeners.get("error")({ type: "error" })
            assert.equal(await after.promise, kind === "metadata" ? 0 : null)
            assert.deepEqual(plain(after.stops), plain(before.stops))
        }
})
