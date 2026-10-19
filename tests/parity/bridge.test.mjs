import assert from "node:assert/strict"
import { test } from "node:test"
import fs from "node:fs"
import path from "node:path"
import os from "node:os"
import { createRequire } from "node:module"
import { spawnSync } from "node:child_process"
import ts from "typescript"
import { root, upstreamFunctions } from "./oracle.mjs"
import { corpus, generatedCorpus } from "../../crates/freeshow-exif/tools/fixtures.mjs"
import { Driver as SpotifyDriver, sourceOracle as spotifyOracle } from "./spotify-oracle.mjs"

const source = fs.readFileSync(path.join(root, "src/frontend/rust/bridge.ts"), "utf8")
const nativeDrop = ts.transpileModule(fs.readFileSync(path.join(root, "src/frontend/rust/nativeDrop.ts"), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 }
}).outputText
const nativeDropUrl = `data:text/javascript;base64,${Buffer.from(nativeDrop).toString("base64")}`
const fileStats = ts.transpileModule(fs.readFileSync(path.join(root, "src/frontend/rust/fileStats.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } }).outputText
const fileStatsUrl = `data:text/javascript;base64,${Buffer.from(fileStats).toString("base64")}`
const mediaUrl = ts.transpileModule(fs.readFileSync(path.join(root, "src/frontend/rust/mediaUrl.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } }).outputText
const mediaUrlModule = `data:text/javascript;base64,${Buffer.from(mediaUrl).toString("base64")}`
const captureBridge = ts.transpileModule(fs.readFileSync(path.join(root, "src/frontend/rust/captureBridge.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } }).outputText
const captureBridgeModule = `data:text/javascript;base64,${Buffer.from(captureBridge).toString("base64")}`
const exif = ts.transpileModule(fs.readFileSync(path.join(root, "src/frontend/rust/exif.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } }).outputText
const exifModule = `data:text/javascript;base64,${Buffer.from(exif).toString("base64")}`
const midi = ts.transpileModule(fs.readFileSync(path.join(root, "src/frontend/rust/midi.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } }).outputText
const midiModule = `data:text/javascript;base64,${Buffer.from(midi).toString("base64")}`
const audioPort = ts.transpileModule(fs.readFileSync(path.join(root, "src/frontend/rust/audioPortBridge.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } }).outputText
const audioPortModule = `data:text/javascript;base64,${Buffer.from(audioPort).toString("base64")}`
const audioRaw = ts.transpileModule(fs.readFileSync(path.join(root, "src/frontend/rust/audioRaw.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } }).outputText
const audioRawModule = `data:text/javascript;base64,${Buffer.from(audioRaw).toString("base64")}`
const timecodeBridge = ts.transpileModule(fs.readFileSync(path.join(root, "src/frontend/rust/timecodeBridge.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } }).outputText
const timecodeBridgeModule = `data:text/javascript;base64,${Buffer.from(timecodeBridge).toString("base64")}`
const timecodeWire = ts.transpileModule(fs.readFileSync(path.join(root, "src/frontend/rust/timecodeWire.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } }).outputText
const timecodeWireModule = `data:text/javascript;base64,${Buffer.from(timecodeWire).toString("base64")}`
function inlineModule(name, imports = {}) {
    let code = ts.transpileModule(fs.readFileSync(path.join(root, `src/frontend/rust/${name}.ts`), "utf8"), { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } }).outputText
    for (const [dependency, url] of Object.entries(imports)) code = code.replace(`from "./${dependency}"`, `from "${url}"`)
    return `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`
}
const nowPlayingBridgeModule = inlineModule("nowPlayingBridge", { metadata: inlineModule("metadata"), nowPlayingWire: inlineModule("nowPlayingWire") })
const coverBridgeModule = inlineModule("coverBridge")
const spotifyModule = inlineModule("spotify")
const javascript = ts
    .transpileModule(source, {
        compilerOptions: {
            module: ts.ModuleKind.ES2022,
            target: ts.ScriptTarget.ES2022
        }
    })
    .outputText.replace('from "./nativeDrop"', `from "${nativeDropUrl}"`)
    .replace('from "./fileStats"', `from "${fileStatsUrl}"`)
    .replace('from "./mediaUrl"', `from "${mediaUrlModule}"`)
    .replace('from "./captureBridge"', `from "${captureBridgeModule}"`)
    .replace('from "./exif"', `from "${exifModule}"`)
    .replace('from "./midi"', `from "${midiModule}"`)
    .replace('from "./audioPortBridge"', `from "${audioPortModule}"`)
    .replace('from "./audioRaw"', `from "${audioRawModule}"`)
    .replace('from "./timecodeBridge"', `from "${timecodeBridgeModule}"`)
    .replace('from "./timecodeWire"', `from "${timecodeWireModule}"`)
    .replace('from "./nowPlayingBridge"', `from "${nowPlayingBridgeModule}"`)
    .replace('from "./coverBridge"', `from "${coverBridgeModule}"`)
    .replace('from "./spotify"', `from "${spotifyModule}"`)
const { createRustApi, installRustBridge } = await import(`data:text/javascript;base64,${Buffer.from(javascript).toString("base64")}`)
const tick = () => new Promise((resolve) => setImmediate(resolve))

function spotifyEvent(context, listenerId, wire, extra = {}) {
    return {
        channel: "MAIN",
        context,
        listenerId,
        message: { channel: "SPOTIFY_GET_STATE", data: wire.data, autoProfile: false },
        nativeSpotify: { undefinedFields: wire.undefinedFields, specialNumbers: wire.specialNumbers },
        ...extra
    }
}
const emptySpotifyRestoration = () => ({ undefinedFields: [], specialNumbers: [] })
const nullSpotifyWire = () => ({ data: null, ...emptySpotifyRestoration() })
function compareSpotifyReply(message, expected) {
    assert.deepEqual(Object.keys(message), ["channel", "data"])
    assert.equal(message.channel, "SPOTIFY_GET_STATE")
    if (expected === null) return assert.equal(message.data, null)
    assert.deepEqual(Object.keys(message.data), Object.keys(expected))
    for (const key of Object.keys(expected)) {
        assert.ok(Object.hasOwn(message.data, key), key)
        assert.ok(Object.is(message.data[key], expected[key]), key)
    }
}

test("ordinary Spotify send/receive preserves immutable source and native WireState replies without exposing restoration or returning a source ACK", async () => {
    const context = { runtimeId: "spotify-wire", libraryGeneration: 3, dataPath: "/library/音楽" },
        driver = new SpotifyDriver(),
        common = 'string "xesam:title" variant string "音楽 🎵"\nstring "xesam:artist" variant array [ string "Artist" ]\nstring "mpris:length" variant uint64 120000000\n'
    try {
        for (const [index, output] of ["", common + 'variant string "Playing"\nint64 3000000\ndouble 0.5', common + 'string "mpris:artUrl" variant string "https://example.invalid/cover"\nint64 9007199254740993\ndouble 0.00000000000000001', common.replace("120000000", "9".repeat(512)) + `int64 ${"9".repeat(512)}\ndouble .`].entries()) {
            const expected = await spotifyOracle((_, callback) => callback(null, output)).get(),
                { wire } = await driver.value({ op: "parse", text: output }),
                before = structuredClone(wire),
                requests = [],
                messages = [],
                errors = [],
                listenerId = `spotify-${index}-音楽`,
                bridge = createRustApi(
                    async (request) => {
                        requests.push(request)
                        return [spotifyEvent(request.context, request.listenerId, wire)]
                    },
                    (...error) => errors.push(error),
                    context
                ),
                payload = { channel: "SPOTIFY_GET_STATE", data: { ignored: "not provider authority" } }
            assert.equal(bridge.api.send("MAIN", payload, listenerId), undefined)
            assert.deepEqual(requests, [], "the retained receiver may register after source send")
            bridge.api.receive("MAIN", (message, id) => messages.push({ message, id }))
            await tick()
            assert.deepEqual(requests, [{ channel: "MAIN", data: payload, listenerId, context }])
            assert.equal(messages.length, 1)
            assert.equal(messages[0].id, listenerId)
            compareSpotifyReply(messages[0].message, expected)
            assert.deepEqual(wire, before, "native placeholders and private metadata remain unchanged")
            if (expected !== null) {
                assert.notEqual(messages[0].message.data, wire.data)
                assert.deepEqual(Object.keys(messages[0].message.data), ["isPlaying", "title", "artist", "positionSec", "durationSec", "albumArt", "volume", "platform"])
                assert.equal(Object.hasOwn(messages[0].message.data, "albumArt"), true)
            } else assert.deepEqual({ undefinedFields: wire.undefinedFields, specialNumbers: wire.specialNumbers }, emptySpotifyRestoration())
            assert.deepEqual(errors, [])
            assert.deepEqual(bridge.api.getRuntimeContext(), context)
        }
    } finally {
        await driver.finish()
    }
})

test("Spotify snapshots the original command before transport entry and retains that origin while its reply is pending", async () => {
    const context = { runtimeId: "spotify-snapshot", libraryGeneration: 2, dataPath: "/library" }
    for (const phase of ["before-transport", "after-transport"]) {
        const payload = { channel: "SPOTIFY_GET_STATE", data: { ignored: { value: "sent" } } },
            sent = structuredClone(payload),
            messages = [],
            errors = [],
            requests = []
        let finish
        const bridge = createRustApi(
            async (request) => {
                requests.push({ request, serialized: JSON.parse(JSON.stringify(request)) })
                return new Promise((resolve) => {
                    finish = resolve
                })
            },
            (...error) => errors.push(error),
            context
        )
        bridge.api.receive("MAIN", (message, listenerId) => messages.push({ message, listenerId }))
        assert.equal(bridge.api.send("MAIN", payload, phase), undefined)
        if (phase === "after-transport") {
            await tick()
            assert.equal(requests.length, 1, "real asynchronous transport entered before caller reuse")
        }
        payload.channel = "GET_OS"
        payload.data.ignored.value = "reused"
        if (phase === "before-transport") await tick()
        assert.equal(requests.length, 1)
        const { request, serialized } = requests[0]
        assert.notEqual(request.data, payload)
        assert.deepEqual(serialized, { channel: "MAIN", data: sent, listenerId: phase, context })
        assert.deepEqual(request.data, sent, "origin remains frozen after actual transport serialization")
        finish([spotifyEvent(request.context, phase, nullSpotifyWire())])
        await tick()
        assert.deepEqual(messages, [{ message: { channel: "SPOTIFY_GET_STATE", data: null }, listenerId: phase }])
        assert.deepEqual(errors, [])
        assert.deepEqual(bridge.api.getRuntimeContext(), context)
    }
})

test("Spotify pending transport replies are discarded before malformed hydration after a real retained library switch", async () => {
    const old = { runtimeId: "spotify-library", libraryGeneration: 3, dataPath: "/old/音楽" },
        next = { ...old, libraryGeneration: 4, dataPath: "/next/音楽" },
        requests = [],
        messages = [],
        errors = [],
        pending = [],
        malformed = { undefinedFields: ["title"], specialNumbers: [] },
        bridge = createRustApi(
            async (request) => {
                requests.push(request)
                if (request.data.channel === "UPDATE_DATA_PATH") return [{ channel: "MAIN", context: next, message: { channel: "DATA_PATH", data: next.dataPath } }]
                return new Promise((resolve) => pending.push({ request, resolve }))
            },
            (...error) => errors.push(error),
            old
        )
    bridge.api.receive("MAIN", (message, listenerId) => messages.push({ message, listenerId }))
    assert.equal(bridge.api.send("MAIN", { channel: "SPOTIFY_GET_STATE" }, "old-request"), undefined)
    await tick()
    assert.equal(pending.length, 1)
    assert.equal(bridge.api.send("MAIN", { channel: "UPDATE_DATA_PATH", data: next.dataPath }), undefined)
    await tick()
    assert.deepEqual(bridge.api.getRuntimeContext(), next)
    const obsolete = spotifyEvent(pending[0].request.context, "old-request", nullSpotifyWire(), { nativeSpotify: malformed }),
        before = structuredClone(obsolete)
    pending.shift().resolve([obsolete])
    await tick()
    assert.deepEqual(obsolete, before)
    assert.deepEqual(messages, [{ message: { channel: "DATA_PATH", data: next.dataPath }, listenerId: undefined }])
    assert.deepEqual(errors, [], "obsolete malformed restoration must never enter hydration")
    assert.equal(bridge.api.send("MAIN", { channel: "SPOTIFY_GET_STATE" }, "current-request"), undefined)
    await tick()
    const current = pending.shift()
    assert.deepEqual(current.request.context, next)
    current.resolve([spotifyEvent(current.request.context, "current-request", nullSpotifyWire(), { nativeSpotify: malformed })])
    await tick()
    assert.equal(messages.length, 1, "the same malformed restoration is rejected in the current context")
    assert.equal(errors.length, 1)
    assert.equal(errors[0][0].code, "spotify_publication")
    assert.deepEqual(bridge.api.getRuntimeContext(), next)
    assert.equal(requests.length, 3)
})

test("Spotify runtime, generation and exact dataPath guards precede hydration and future replies never select a context", async () => {
    const context = { runtimeId: "spotify-context", libraryGeneration: 8, dataPath: "/selected/音楽" }
    for (const frozen of [undefined, { ...context, runtimeId: "another-runtime" }, { ...context, libraryGeneration: 7 }, { ...context, libraryGeneration: 9 }, { ...context, dataPath: "/other/音楽" }]) {
        const errors = [],
            messages = [],
            malformed = { undefinedFields: [], specialNumbers: [{ field: "volume", kind: "not-a-kind" }] },
            event = spotifyEvent(frozen, "context-request", nullSpotifyWire(), { nativeSpotify: malformed }),
            before = structuredClone(event),
            bridge = createRustApi(
                async () => [event],
                (...error) => errors.push(error),
                context
            )
        bridge.api.receive("MAIN", (...message) => messages.push(message))
        assert.equal(bridge.api.send("MAIN", { channel: "SPOTIFY_GET_STATE" }, "context-request"), undefined)
        await tick()
        assert.deepEqual(messages, [])
        assert.deepEqual(errors, [], "foreign-context malformed restoration must be discarded first")
        assert.deepEqual(event, before)
        assert.deepEqual(bridge.api.getRuntimeContext(), context)
    }
    const future = { ...context, libraryGeneration: 9, dataPath: "/future" },
        messages = [],
        errors = [],
        bridge = createRustApi(
            async () => [spotifyEvent(future, "future-request", nullSpotifyWire())],
            (...error) => errors.push(error),
            context
        )
    bridge.api.receive("MAIN", (...message) => messages.push(message))
    bridge.api.send("MAIN", { channel: "SPOTIFY_GET_STATE" }, "future-request")
    await tick()
    assert.deepEqual(messages, [], "even a valid source-null reply cannot advance the selected context")
    assert.deepEqual(errors, [])
    assert.deepEqual(bridge.api.getRuntimeContext(), context)
})

test("ordinary Spotify transport rejects wrong request origins, listener pairing, reply routes and missing restoration", async () => {
    const context = { runtimeId: "spotify-pairing", libraryGeneration: 2, dataPath: "/library" },
        cases = [{ outer: "REMOTE", command: "SPOTIFY_GET_STATE" }, { outer: "MAIN", command: "GET_OS" }, { event: { channel: "OUTPUT" } }, { event: { message: { channel: "GET_OS", data: null } } }, { event: { listenerId: "another-listener" } }, { event: { listenerId: undefined } }, { event: { nativeSpotify: undefined } }, { event: { nativeSpotify: null } }]
    for (const invalid of cases) {
        const messages = [],
            errors = [],
            bridge = createRustApi(
                async () => [spotifyEvent(context, "actual-listener", nullSpotifyWire(), invalid.event)],
                (...error) => errors.push(error),
                context
            )
        for (const channel of ["MAIN", "OUTPUT", "REMOTE"]) bridge.api.receive(channel, (...message) => messages.push(message))
        assert.equal(bridge.api.send(invalid.outer || "MAIN", { channel: invalid.command || "SPOTIFY_GET_STATE" }, "actual-listener"), undefined)
        await tick()
        assert.deepEqual(messages, [], JSON.stringify(invalid))
        assert.equal(errors.length, 1, JSON.stringify(invalid))
        assert.equal(errors[0][0].code, "spotify_publication")
        assert.deepEqual(bridge.api.getRuntimeContext(), context)
    }
})

test("Spotify asynchronous events cannot impersonate a request or initialize an unknown selected context", async () => {
    const context = { runtimeId: "spotify-unsolicited", libraryGeneration: 1, dataPath: "/library" },
        errors = [],
        messages = [],
        bridge = createRustApi(
            async () => [],
            (...error) => errors.push(error),
            context
        )
    bridge.api.receive("MAIN", (...message) => messages.push(message))
    bridge.dispatch(spotifyEvent(context, undefined, nullSpotifyWire()))
    assert.deepEqual(messages, [])
    assert.equal(errors.length, 1)
    assert.equal(errors[0][0].code, "spotify_publication")
    const unknownErrors = [],
        unknown = createRustApi(
            async () => [spotifyEvent(context, "no-context", nullSpotifyWire())],
            (...error) => unknownErrors.push(error)
        )
    unknown.api.receive("MAIN", (...message) => messages.push(message))
    unknown.api.send("MAIN", { channel: "SPOTIFY_GET_STATE" }, "no-context")
    await tick()
    assert.deepEqual(messages, [])
    assert.deepEqual(unknownErrors, [])
    assert.equal(unknown.api.getRuntimeContext(), undefined)
})

test("installed now-playing retains send-time numeric identity and source void replies through private raw IPC", async () => {
    const context = { runtimeId: "now-playing-raw", libraryGeneration: 3, dataPath: "/library" },
        calls = [],
        errors = [],
        replies = []
    const target = {
        addEventListener() {},
        removeEventListener() {},
        dispatchEvent: (event) => errors.push(event.detail),
        __TAURI__: {
            core: {
                invoke: async (name, args) => {
                    calls.push({ name, args })
                    return name === "runtime_context" ? context : []
                }
            },
            webviewWindow: { getCurrentWebviewWindow: () => ({ label: "main" }) },
            event: { listen: async () => () => {} }
        }
    }
    await installRustBridge(target)
    target.api.receive("MAIN", (...reply) => replies.push(reply))
    const payload = { filePath: "/song.mp3", name: "Original", duration: Infinity, unknownLang: ["Artist", "Title", "Album"] }
    assert.equal(target.api.send("MAIN", { channel: "NOW_PLAYING", data: payload }, "source-void"), undefined)
    payload.name = "Changed"
    payload.duration = 3
    payload.unknownLang[0] = "Changed"
    target.api.send("MAIN", { channel: "NOW_PLAYING_UNSET" })
    target.api.send("MAIN", { channel: "OPEN_NOW_PLAYING" })
    await tick()
    const bodies = calls
        .filter((call) => call.name === "now_playing_command")
        .map((call) => {
            assert.ok(call.args instanceof Uint8Array)
            return JSON.parse(new TextDecoder().decode(call.args))
        })
    assert.equal(bodies.length, 3)
    assert.deepEqual(bodies[0], { version: 1, context, command: { kind: "set", data: { filePath: "/song.mp3", name: "Original", duration: null, unknownLang: ["Artist", "Title", "Album"] }, duration: { kind: "PositiveInfinity" } } })
    assert.deepEqual(
        bodies.slice(1).map((body) => body.command),
        [{ kind: "unset" }, { kind: "open" }]
    )
    assert.equal(
        calls.some((call) => call.name === "ipc_dispatch"),
        false
    )
    assert.deepEqual(replies, [])
    assert.deepEqual(errors, [])
})

test("shared now-playing dispatch restores original metadata types before callbacks and keeps private receipts outside payloads", async () => {
    const context = { runtimeId: "now-playing-events", libraryGeneration: 1, dataPath: "/one" },
        messages = [],
        completions = [],
        errors = []
    const bridge = createRustApi(
        async () => [],
        (error) => errors.push(error),
        context,
        undefined,
        undefined,
        undefined,
        undefined,
        undefined,
        {
            command: async () => {},
            retire: async () => {},
            complete: async (context, id, accepted) => completions.push({ context, id, accepted })
        }
    )
    bridge.api.receive("MAIN", (message, listenerId) => messages.push({ message, listenerId }))
    const event = (sequence, data, hydration, extra = {}) => ({ channel: "MAIN", context, message: { channel: "AUDIO_METADATA", data }, nativeNowPlaying: { id: `native-now-playing-${String(sequence).padStart(32, "0")}`, sequence: String(sequence), hydration }, ...extra })
    const hydration = { undefinedPaths: ["/metadata/absent"], specialNumbers: [{ path: "/metadata/year", kind: "NaN" }], byteBuffers: ["/metadata/picture/0/data"], objectKeys: [{ path: "/metadata", keys: ["absent", "year", "picture"] }] }
    const original = { filePath: "/song.mp3", metadata: { year: null, picture: [{ format: "image/png", data: "AQID" }], absent: null } }
    bridge.dispatch(event(1, original, hydration))
    const replay = event(1, { filePath: "/song.mp3", metadata: {} }, { undefinedPaths: [], specialNumbers: [], byteBuffers: [] })
    bridge.dispatch(replay)
    bridge.dispatch(event(2, { filePath: "/song.mp3", metadata: {} }, { undefinedPaths: [], specialNumbers: [], byteBuffers: [] }, { context: { ...context, dataPath: "/foreign" } }))
    bridge.dispatch(event(3, { filePath: "/song.mp3", metadata: { picture: [{ data: "invalid" }] } }, { undefinedPaths: [], specialNumbers: [], byteBuffers: ["/metadata/picture/0/data"] }))
    await tick()
    assert.equal(messages.length, 1)
    assert.deepEqual(Object.keys(messages[0].message), ["channel", "data"])
    assert.equal(messages[0].listenerId, undefined)
    assert.deepEqual(Object.keys(messages[0].message.data.metadata), ["absent", "year", "picture"])
    assert.ok(Object.hasOwn(messages[0].message.data.metadata, "absent"))
    assert.equal(messages[0].message.data.metadata.absent, undefined)
    assert.ok(Number.isNaN(messages[0].message.data.metadata.year))
    assert.equal(messages[0].message.data.metadata.picture[0].data.constructor, Uint8Array)
    assert.deepEqual([...messages[0].message.data.metadata.picture[0].data], [1, 2, 3])
    assert.deepEqual(
        completions.map(({ accepted }) => accepted),
        [true, false]
    )
    assert.deepEqual(bridge.api.getRuntimeContext(), context)
    assert.equal(errors.length, 1)
})

test("shared now-playing folder and missing-file alerts preserve original payloads while private provenance stays outside callbacks", async () => {
    const context = { runtimeId: "now-playing-alerts", libraryGeneration: 1, dataPath: "/selected/native\\literal" },
        messages = [],
        completions = [],
        errors = []
    const bridge = createRustApi(
        async () => [],
        (error) => errors.push(error),
        context,
        undefined,
        undefined,
        undefined,
        undefined,
        undefined,
        {
            command: async () => {},
            retire: async () => {},
            complete: async (context, id, accepted) => completions.push({ context, id, accepted })
        }
    )
    bridge.api.receive("MAIN", (message, listenerId) => messages.push({ message, listenerId }))
    const folder = context.dataPath + "/Audio",
        text = `Error: Could not create folder at: ${folder}!`
    const event = (sequence, data, privateFields) => ({ channel: "MAIN", context, message: { channel: "ALERT", data }, nativeNowPlaying: { id: `native-now-playing-${String(sequence).padStart(32, "0")}`, sequence: String(sequence), ...privateFields } })
    bridge.dispatch(event(1, text, { alertKind: "folderCreationFailed", alertFolder: folder }))
    bridge.dispatch(event(2, "This does not exist!", { alertKind: "missingText" }))
    bridge.dispatch(event(3, text + " wrong", { alertKind: "folderCreationFailed", alertFolder: folder }))
    await tick()
    assert.deepEqual(messages, [
        { message: { channel: "ALERT", data: text }, listenerId: undefined },
        { message: { channel: "ALERT", data: "This does not exist!" }, listenerId: undefined }
    ])
    assert.deepEqual(
        completions.map(({ accepted }) => accepted),
        [true, true, false]
    )
    assert.ok(completions.every((item) => JSON.stringify(item.context) === JSON.stringify(context)))
    assert.equal(errors.length, 1)
})

test("now-playing library retirement captures the old full context and native CLOSE survives a settled frontend drain failure", async () => {
    const old = { runtimeId: "now-playing-life", libraryGeneration: 1, dataPath: "/one" },
        next = { ...old, libraryGeneration: 2, dataPath: "/two" },
        retirements = [],
        calls = [],
        errors = []
    const bridge = createRustApi(
        async (request) => {
            calls.push(request)
            return request.data.channel === "UPDATE_DATA_PATH" ? [{ channel: "MAIN", context: next, message: { channel: "DATA_PATH", data: next.dataPath } }] : []
        },
        (error) => errors.push(error),
        old,
        undefined,
        undefined,
        undefined,
        undefined,
        undefined,
        {
            command: async () => {},
            complete: async () => {},
            retire: async (context, terminal) => {
                retirements.push({ context, terminal })
                if (terminal) throw new Error("actual retained owner diagnostic")
            }
        }
    )
    bridge.api.send("MAIN", { channel: "UPDATE_DATA_PATH", data: next.dataPath })
    await tick()
    await tick()
    assert.deepEqual(retirements, [{ context: old, terminal: false }])
    bridge.api.send("MAIN", { channel: "CLOSE" })
    await tick()
    await tick()
    assert.deepEqual(retirements[1], { context: next, terminal: true })
    assert.equal(calls.at(-1).data.channel, "CLOSE")
    assert.equal(errors.length, 1)
})

test("installed retained timecode sends typed numbers and exact reused views through private raw IPC without source ACKs", async () => {
    const context = { runtimeId: "timecode-raw", libraryGeneration: 0, dataPath: "/library" },
        calls = [],
        errors = [],
        replies = []
    const target = {
        addEventListener() {},
        removeEventListener() {},
        dispatchEvent: (event) => errors.push(event.detail),
        __TAURI__: {
            core: {
                invoke: async (name, args) => {
                    calls.push({ name, args })
                    return name === "runtime_context" ? context : []
                }
            },
            webviewWindow: { getCurrentWebviewWindow: () => ({ label: "main" }) },
            event: { listen: async () => () => {} }
        }
    }
    await installRustBridge(target)
    target.api.receive("MAIN", (...reply) => replies.push(reply))
    const start = { channel: "TIMECODE_START", data: { type: "send", mode: "LTC", framerate: -0, data: { first: "雪", second: 1 } } }
    target.api.send("MAIN", start, "must-stay-void")
    start.data.data.first = "mutated"
    await tick()
    for (const value of [NaN, Infinity, -Infinity, -0]) {
        target.api.send("MAIN", { channel: "TIMECODE_VALUE", data: value })
        await tick()
    }
    target.api.send("MAIN", { channel: "TIMECODE_STATUS", data: undefined })
    const storage = new Uint8Array([99, 1, 2, 3, 98])
    target.api.send("MAIN", { channel: "TIMECODE_AUDIO_DATA", data: { mode: "LTC", buffer: storage.subarray(1, 4) } })
    storage.fill(77)
    target.api.send("MAIN", { channel: "TIMECODE_AUDIO_DATA", data: new Uint8Array([5, 6]) })
    await tick()
    const commands = calls.filter((call) => call.name === "timecode_command").map((call) => JSON.parse(new TextDecoder().decode(call.args)))
    assert.equal(commands.length, 6)
    assert.deepEqual(commands[0].context, context)
    assert.equal(commands[0].command.framerate.value.kind, "NegativeZero")
    assert.deepEqual(commands[0].command.data, { kind: "json", value: { first: "雪", second: 1 } })
    assert.deepEqual(
        commands.slice(1, 5).map((command) => command.command.value.kind),
        ["NaN", "PositiveInfinity", "NegativeInfinity", "NegativeZero"]
    )
    assert.deepEqual(commands[5].command, { kind: "status", data: { kind: "undefined" } })
    const audio = calls.filter((call) => call.name === "timecode_audio")
    assert.equal(audio.length, 1, "original raw-worklet route remains its no-op")
    const body = audio[0].args,
        prefixLength = new DataView(body.buffer, body.byteOffset, 4).getUint32(0, true)
    assert.deepEqual(JSON.parse(new TextDecoder().decode(body.subarray(4, 4 + prefixLength))), { version: 1, context, mode: "LTC", byteLength: 3 })
    assert.deepEqual([...body.subarray(4 + prefixLength)], [1, 2, 3])
    assert.equal(
        calls.some((call) => call.name === "ipc_dispatch"),
        false
    )
    assert.deepEqual(replies, [])
    assert.deepEqual(errors, [])
})

test("shared runtime timecode restores genuine source callbacks with private completions and rejects foreign/replayed delivery", async () => {
    const context = { runtimeId: "timecode-events", libraryGeneration: 1, dataPath: "/one" },
        messages = [],
        completions = [],
        errors = []
    const bridge = createRustApi(
        async () => [],
        (error) => errors.push(error),
        context,
        undefined,
        undefined,
        undefined,
        undefined,
        {
            command: async () => {},
            audio: async () => {},
            retire: async () => {},
            complete: async (frozen, id, accepted) => completions.push({ frozen, id, accepted })
        }
    )
    bridge.api.receive("MAIN", (message, listenerId) => messages.push({ message, listenerId }))
    const event = (sequence, channel, data, extra = {}) => ({
        channel: "MAIN",
        context,
        message: { channel, data },
        nativeTimecode: {
            hydration: { undefinedPaths: [], numbers: [] },
            delivery: { id: "timecode-00000000-0000-0000-0000-" + String(sequence).padStart(12, "0"), context, run: 1, sequence: String(sequence) },
            ...extra
        }
    })
    bridge.dispatch(event(1, "TIMECODE_VALUE", null, { hydration: { undefinedPaths: [], numbers: [{ path: "", number: { kind: "NaN" } }] } }))
    await tick()
    assert.ok(Number.isNaN(messages[0].message.data))
    bridge.dispatch(event(2, "TIMECODE_AUDIO_DATA", null, { bytes: [1, 128, 255] }))
    await tick()
    assert.ok(messages[1].message.data instanceof Uint8Array)
    assert.deepEqual([...messages[1].message.data], [1, 128, 255])
    assert.ok(messages.every(({ message, listenerId }) => listenerId === undefined && Object.keys(message).join() === "channel,data"))
    bridge.dispatch(event(2, "TIMECODE_AUDIO_DATA", null, { bytes: [1, 128, 255] }))
    const foreign = event(3, "TIMECODE_VALUE", 9)
    foreign.context = { ...context, dataPath: "/foreign" }
    bridge.dispatch(foreign)
    await tick()
    assert.equal(messages.length, 2)
    assert.deepEqual(
        completions.map((item) => item.accepted),
        [true, true, false]
    )
    assert.ok(errors.some((error) => error.code === "timecode_replay"))
    assert.ok(errors.some((error) => error.code === "timecode_publication"))
})

test("shared runtime retires obsolete timecode and still invokes native CLOSE after terminal frontend cleanup fails", async () => {
    const old = { runtimeId: "timecode-lifecycle", libraryGeneration: 1, dataPath: "/one" },
        next = { ...old, libraryGeneration: 2, dataPath: "/two" },
        retirements = [],
        requests = [],
        errors = []
    const bridge = createRustApi(
        async (request) => {
            requests.push(request)
            return request.data.channel === "UPDATE_DATA_PATH" ? [{ channel: "MAIN", context: next, message: { channel: "DATA_PATH", data: next.dataPath } }] : []
        },
        (error) => errors.push(error),
        old,
        undefined,
        undefined,
        undefined,
        undefined,
        {
            command: async () => {},
            audio: async () => {},
            complete: async () => {},
            retire: async (context, terminal) => {
                retirements.push({ context, terminal })
                if (terminal) throw new Error("retained real owner still draining")
            }
        }
    )
    bridge.api.send("MAIN", { channel: "UPDATE_DATA_PATH", data: next.dataPath })
    await tick()
    await tick()
    assert.deepEqual(retirements, [{ context: old, terminal: false }])
    bridge.api.send("MAIN", { channel: "CLOSE" })
    await tick()
    await tick()
    assert.deepEqual(retirements[1], { context: next, terminal: true })
    assert.equal(requests.at(-1).data.channel, "CLOSE")
    assert.ok(errors.length > 0)
})

test("native AUDIO status retains the original fields and owns a private completion through rejection and listener errors", async () => {
    const context = { runtimeId: "audio", libraryGeneration: 1, dataPath: "/one" },
        delivered = [],
        errors = [],
        completed = []
    const bridge = createRustApi(
        async () => [],
        (error) => errors.push(error),
        context,
        undefined,
        undefined,
        undefined,
        {
            postPort() {},
            transport: async () => {},
            disconnect: async () => {},
            closeContext: async () => {},
            completeStatus: async (frozen, id, accepted) => completed.push({ frozen, id, accepted })
        }
    )
    bridge.api.receive("MAIN", (message, id) => delivered.push({ message, id }))
    bridge.api.receive("MAIN", () => {
        throw new Error("retained listener failure")
    })
    let serial = 0
    const event = (changes = {}) => ({ channel: "MAIN", context, message: { channel: "ICECAST_STATUS", data: { connecting: true, connected: false } }, nativeAudio: { id: "native-audio-00000000-0000-0000-0000-" + String(++serial).padStart(12, "0"), run: "18446744073709551615", ownUndefinedError: true }, ...changes })
    const first = event()
    bridge.dispatch(first)
    assert.deepEqual(delivered, [{ message: { channel: "ICECAST_STATUS", data: { connected: false, connecting: true, error: undefined } }, id: undefined }])
    assert.deepEqual(Object.keys(delivered[0].message.data), ["connected", "connecting", "error"])
    assert.equal(Object.hasOwn(first.message.data, "error"), false, "hydration preserves the wire envelope")
    const failed = event({ message: { channel: "ICECAST_STATUS", data: { connected: false, connecting: false, error: "connection refused" } } })
    failed.nativeAudio.ownUndefinedError = false
    bridge.dispatch(failed)
    assert.equal(delivered.at(-1).message.data.error, "connection refused")
    const malformed = event({ message: { channel: "ICECAST_STATUS", data: { connected: true } } })
    bridge.dispatch(malformed)
    const overflow = event()
    overflow.nativeAudio.run = "18446744073709551616"
    bridge.dispatch(overflow)
    bridge.dispatch(event({ context: { ...context, dataPath: "/wrong" } }))
    bridge.dispatch(event({ context: { ...context, libraryGeneration: 2, dataPath: "/future" } }))
    bridge.dispatch(event({ listenerId: "fabricated-source-ack" }))
    await tick()
    assert.equal(delivered.length, 2)
    assert.deepEqual(
        completed.map((item) => item.accepted),
        [true, true, false, false, false, false, false]
    )
    assert.deepEqual(completed[4].frozen, { ...context, dataPath: "/wrong" })
    assert.equal(errors.filter((error) => error.code === "audio_publication").length, 3)
    assert.equal(errors.filter((error) => error.message === "retained listener failure").length, 2)
    assert.deepEqual(bridge.api.getRuntimeContext(), context)
})

test("the installed AUDIO bridge transfers a real port and sends bounded binary snapshots before native close", async (t) => {
    const context = { runtimeId: "audio-installed", libraryGeneration: 1, dataPath: "/音楽" },
        calls = [],
        errors = []
    const channel = new MessageChannel()
    let receivedPort, releaseFrame, releaseClose
    const firstFrame = new Promise((resolve) => {
            releaseFrame = resolve
        }),
        close = new Promise((resolve) => {
            releaseClose = resolve
        })
    t.after(() => {
        receivedPort?.close()
        channel.port1.close()
        channel.port2.close()
        releaseFrame()
        releaseClose()
    })
    const transferred = new Promise((resolve) => {
        channel.port2.onmessage = (event) => {
            assert.deepEqual(event.data, { type: "AUDIO_PORT_RESPONSE", targetId: "icecast" })
            assert.equal(event.ports.length, 1)
            receivedPort = event.ports[0]
            resolve(receivedPort)
        }
    })
    const target = {
        addEventListener() {},
        removeEventListener() {},
        dispatchEvent: (event) => errors.push(event.detail),
        postMessage: (message, origin, ports) => {
            assert.equal(origin, "*")
            channel.port1.postMessage(message, ports)
        },
        __TAURI__: {
            core: {
                invoke: async (name, args) => {
                    calls.push({ name, args })
                    if (name === "runtime_context") return context
                    if (name === "audio_pcm" && calls.filter((call) => call.name === name).length === 1) return firstFrame
                    if (name === "audio_close_context") return close
                    return []
                }
            },
            webviewWindow: { getCurrentWebviewWindow: () => ({ label: "main" }) },
            event: { listen: async () => () => {} }
        }
    }
    await installRustBridge(target)
    const sourceAcks = []
    target.api.receive("AUDIO", (message) => sourceAcks.push(message))
    target.api.send("AUDIO", { channel: "INIT_PORT", data: { id: "icecast" } }, "ignored")
    const port = await Promise.race([
        transferred,
        new Promise((_, reject) => {
            const timer = setTimeout(() => reject(new Error("actual transferred port deadline")), 3000)
            t.after(() => clearTimeout(timer))
        })
    ])
    const storage = new Uint8Array([99, 1, 2, 3, 4, 98])
    const config = Object.defineProperty({ enabled: false }, "__proto__", { value: { "~/": -0 }, enumerable: true })
    port.postMessage({ channel: "AUDIO", payload: { buffer: new DataView(storage.buffer, 1, 4), sampleRate: -0, id: "icecast", icecast: config } })
    storage.fill(88)
    const wait = async (predicate) => {
        const deadline = Date.now() + 3000
        while (!predicate()) {
            assert.ok(Date.now() < deadline, "bounded AUDIO bridge wait")
            await new Promise((resolve) => setTimeout(resolve, 2))
        }
    }
    await wait(() => calls.some((call) => call.name === "audio_pcm"))
    const frame = calls.find((call) => call.name === "audio_pcm").args
    assert.ok(frame instanceof Uint8Array, "actual invoke receives raw binary rather than a numeric JSON array")
    const length = new DataView(frame.buffer, frame.byteOffset, frame.byteLength).getUint32(0, true),
        prefix = JSON.parse(new TextDecoder().decode(frame.subarray(4, 4 + length)))
    assert.deepEqual([...frame.subarray(4 + length)], [1, 2, 3, 4])
    assert.deepEqual(prefix.context, context)
    assert.equal(prefix.origin, "port")
    assert.deepEqual(prefix.negativeZeroPaths, ["/sampleRate", "/icecast/__proto__/~0~1"])
    assert.equal(Object.hasOwn(prefix.metadata.icecast, "__proto__"), true)
    const fallback = new Uint8Array([77, 5, 6, 66])
    target.api.send("AUDIO", { channel: "PCM", data: { buffer: fallback.subarray(1, 3), sampleRate: 48000, id: "icecast" } })
    fallback.fill(55)
    target.api.send("MAIN", { channel: "VERSION" })
    await tick()
    assert.equal(calls.filter((call) => call.name === "audio_pcm").length, 1, "the fixed adapter pump owns entered PCM until actual native completion")
    assert.ok(calls.some((call) => call.name === "ipc_dispatch" && call.args.request.data.channel === "VERSION"))
    releaseFrame()
    await wait(() => calls.filter((call) => call.name === "audio_pcm").length === 2)
    const second = calls.filter((call) => call.name === "audio_pcm")[1].args,
        secondLength = new DataView(second.buffer, second.byteOffset, second.byteLength).getUint32(0, true)
    assert.deepEqual([...second.subarray(4 + secondLength)], [5, 6])
    target.api.send("MAIN", { channel: "CLOSE" })
    await wait(() => calls.some((call) => call.name === "audio_close_context"))
    assert.ok(!calls.some((call) => call.name === "ipc_dispatch" && call.args.request.data.channel === "CLOSE"))
    releaseClose()
    await wait(() => calls.some((call) => call.name === "ipc_dispatch" && call.args.request.data.channel === "CLOSE"))
    assert.deepEqual(calls.find((call) => call.name === "audio_close_context").args, { context })
    assert.deepEqual(sourceAcks, [])
    assert.deepEqual(errors, [])
})

test("accepted library selection retires AUDIO outside the stalled PCM pump and preserves the next full context", async () => {
    const old = { runtimeId: "audio-context", libraryGeneration: 1, dataPath: "/old" },
        next = { ...old, libraryGeneration: 2, dataPath: "/next" },
        frames = [],
        controls = [],
        errors = []
    let releaseFrame, releaseRetirement
    const entered = new Promise((resolve) => {
            releaseFrame = resolve
        }),
        retirement = new Promise((resolve) => {
            releaseRetirement = resolve
        })
    const bridge = createRustApi(
        async () => [],
        (error) => errors.push(error),
        old,
        undefined,
        undefined,
        undefined,
        {
            postPort() {},
            disconnect: async () => {},
            completeStatus: async () => {},
            transport: async (bytes, metadata, context) => {
                frames.push({ bytes: [...bytes], context })
                if (context.libraryGeneration === 1) await entered
            },
            closeContext: async (context) => {
                controls.push(context)
                if (context.libraryGeneration === 1) await retirement
            }
        }
    )
    const packet = (channel, byte) => bridge.api.send("AUDIO", { channel, data: { buffer: new Uint8Array([byte]), sampleRate: 48000 } })
    packet("PCM", 1)
    packet("CAPTURE", 2)
    await tick()
    assert.equal(frames.length, 1)
    bridge.dispatch({ channel: "MAIN", message: { channel: "DATA_PATH", data: next.dataPath }, context: next }, { channel: "MAIN", data: { channel: "UPDATE_DATA_PATH" } })
    await tick()
    assert.deepEqual(controls, [old], "reserved context cleanup enters while actual PCM is still held")
    assert.deepEqual(bridge.api.getRuntimeContext(), next)
    packet("PCM", 3)
    assert.equal(frames.length, 1)
    releaseFrame()
    releaseRetirement()
    await tick()
    await tick()
    packet("PCM", 4)
    await tick()
    assert.deepEqual(frames, [
        { bytes: [1], context: old },
        { bytes: [4], context: next }
    ])
    assert.ok(
        errors.some((error) => error.code === "audio_context_closed"),
        "queued obsolete packet is explicitly canceled"
    )
    assert.ok(
        errors.some((error) => error.code === "audio_closed"),
        "new input waits for real old cleanup"
    )
    bridge.api.send("MAIN", { channel: "CLOSE" })
    await tick()
    await tick()
    assert.deepEqual(controls, [old, next])
})

test("native CLOSE still attempts independent owners after a frontend audio drain fails", async () => {
    const calls = [],
        errors = [],
        context = { runtimeId: "audio-close", libraryGeneration: 1, dataPath: "/one" }
    const bridge = createRustApi(
        async (request) => {
            calls.push(request)
            return []
        },
        (error) => errors.push(error),
        context,
        undefined,
        undefined,
        undefined,
        {
            postPort() {},
            transport: async () => {},
            disconnect: async () => {},
            completeStatus: async () => {},
            closeContext: async () => {
                throw { code: "audio_ingress_drain_timeout", message: "native accepted work remains owned" }
            }
        }
    )
    bridge.api.send("MAIN", { channel: "CLOSE" })
    for (let count = 0; count < 5; count++) await tick()
    assert.equal(calls.length, 1)
    assert.equal(calls[0].data.channel, "CLOSE")
    assert.deepEqual(calls[0].context, context)
    assert.ok(
        errors.some((error) => error.code === "audio_cleanup"),
        "failed frontend drain remains explicit while native cleanup proceeds"
    )
})

test("native MIDI dispatch restores source fields only after exact context/provenance acceptance and completes private receipts", async () => {
    const context = { runtimeId: "midi", libraryGeneration: 1, dataPath: "/one" }
    const deliveries = [],
        errors = [],
        receipts = []
    const bridge = createRustApi(
        async () => [],
        (error) => errors.push(error),
        context,
        undefined,
        undefined,
        async (id, accepted) => receipts.push({ id, accepted })
    )
    bridge.api.receive("MAIN", (message, listenerId) => deliveries.push({ message, listenerId }))
    let serial = 0
    const publication = (changes = {}) => ({ channel: "MAIN", message: { channel: "RECEIVE_MIDI2", data: { values: { channel: 1 }, type: "noteon" } }, context, nativeMidi: { hydration: { undefinedPaths: ["/id", "/values/note", "/values/velocity"], numbers: [] }, delivery: { id: "native-midi-" + (++serial).toString(16).padStart(32, "0"), context, run: "1", listener: "1" } }, ...changes })
    const event = publication()
    bridge.dispatch(event)
    assert.deepEqual(deliveries, [{ message: { channel: "RECEIVE_MIDI2", data: { id: undefined, values: { note: undefined, velocity: undefined, channel: 1 }, type: "noteon" } }, listenerId: undefined }])
    assert.deepEqual(Object.keys(deliveries[0].message.data), ["id", "values", "type"])
    assert.deepEqual(Object.keys(deliveries[0].message.data.values), ["note", "velocity", "channel"])
    assert.equal(Object.hasOwn(event.message.data, "id"), false, "restoration does not mutate a rejected/reused wire envelope")
    const wrongPath = { ...context, dataPath: "/wrong" },
        future = { ...context, libraryGeneration: 2, dataPath: "/next" }
    for (const frozen of [wrongPath, future, { ...context, runtimeId: "foreign" }]) bridge.dispatch(publication({ context: frozen }))
    const malformed = publication()
    malformed.nativeMidi.delivery.run = "0"
    bridge.dispatch(malformed)
    const invalid = publication()
    invalid.nativeMidi.hydration.undefinedPaths.push("/values/__proto__")
    bridge.dispatch(invalid)
    const correlated = publication({ listenerId: "not-source-callback" })
    bridge.dispatch(correlated)
    assert.equal(deliveries.length, 1)
    assert.deepEqual(bridge.api.getRuntimeContext(), context)
    assert.equal(receipts.length, 7)
    assert.equal(receipts[0].accepted, true)
    assert.ok(receipts.slice(1).every((receipt) => receipt.accepted === false))
    assert.equal(errors.length, 3)
    assert.ok(errors.every((error) => error.code === "midi_publication"))
    bridge.dispatch({ channel: "MAIN", message: { channel: "GET_MIDI_INPUTS", data: [{}] }, listenerId: "getter", context, nativeMidi: { hydration: { undefinedPaths: ["/0/name"], numbers: [] } } })
    assert.deepEqual(deliveries.at(-1), { message: { channel: "GET_MIDI_INPUTS", data: [{ name: undefined }] }, listenerId: "getter" })
    assert.equal(Object.hasOwn(deliveries.at(-1).message.data[0], "name"), true)
    assert.equal(receipts.length, 7, "getters introduce no private/source ACK")
    await tick()
})

test("MIDI sends retain send-time snapshots and source void replies without waiting for unrelated mutations", async () => {
    const context = { runtimeId: "midi-send", libraryGeneration: 1, dataPath: "/one" },
        sent = [],
        errors = []
    let release
    const held = new Promise((resolve) => {
        release = resolve
    })
    const bridge = createRustApi(
        async (request) => {
            sent.push(structuredClone(request))
            if (request.data.channel === "SAVE") await held
            return []
        },
        (error) => errors.push(error),
        context
    )
    const replies = []
    bridge.api.receive("MAIN", (message) => replies.push(message))
    bridge.api.send("MAIN", { channel: "SAVE", data: {} })
    const data = { channel: "SEND_MIDI", data: { output: "日本語", values: { note: 60, velocity: 127, channel: 1 } } }
    bridge.api.send("MAIN", data, "void-id")
    bridge.api.send("MAIN", { channel: "RECEIVE_MIDI", data: { id: "action", input: "日本語" } })
    bridge.api.send("MAIN", { channel: "CLOSE_MIDI", data: { id: "action" } })
    data.data.values.note = 99
    await tick()
    assert.equal(sent.length, 4)
    assert.equal(sent[1].data.data.values.note, 60)
    assert.equal(sent[1].listenerId, "void-id")
    assert.deepEqual(sent[1].context, context)
    assert.deepEqual(replies, [])
    release()
    await tick()
    assert.deepEqual(errors, [])
})

test("remote native provenance preserves raw messages and fences exact owned disconnects across a library switch", async () => {
    const context = { runtimeId: "remote", libraryGeneration: 1, dataPath: "/old" },
        next = { ...context, libraryGeneration: 2, dataPath: "/new" }
    const deliveries = [],
        errors = [],
        receipts = []
    const bridge = createRustApi(
        async () => [],
        (error) => errors.push(error),
        context,
        undefined,
        async (id, accepted) => receipts.push({ id, accepted })
    )
    for (const channel of ["REMOTE", "STAGE", "CONTROLLER"]) bridge.api.receive(channel, (message, id) => deliveries.push({ channel, message, id }))
    let serial = 0
    const event = (origin, message, overrides = {}) => ({ channel: "REMOTE", message: { channel: "NATIVE_REMOTE", data: null }, context, nativeRemote: { deliveryId: "native-remote-" + (++serial).toString(16).padStart(32, "0"), run: "a".repeat(32), server: "REMOTE", sourceSocket: "real-socket", origin, message, ...overrides } })
    bridge.dispatch(event("connection", { channel: "CONNECTION", id: "real-socket", data: { name: "Linux" } }))
    const raw = [{ channel: "CONNECTION", id: "spoofed", origin: "connection", data: { any: true } }, { channel: "DISCONNECT", id: 123 }, ["truthy", { channel: "ACTION" }], "primitive", { channel: "QUERY", unknown: 1 }]
    for (const message of raw) bridge.dispatch(event("client", message))
    assert.deepEqual(
        deliveries.slice(1).map((x) => x.message),
        raw,
        "retained source receives unchanged raw JSON including spoofed payload IDs"
    )
    bridge.dispatch({ channel: "MAIN", message: { channel: "DATA_PATH", data: next.dataPath }, context: next }, { channel: "MAIN", data: { channel: "UPDATE_DATA_PATH" } })
    const before = deliveries.length
    for (const [origin, message, metadata, frozen] of [
        ["client", { channel: "ACTION" }, {}, context],
        ["client", { channel: "DISCONNECT", id: "real-socket" }, {}, context],
        ["disconnect", { channel: "DISCONNECT", id: "spoofed" }, { sourceSocket: "spoofed" }, context],
        ["disconnect", { channel: "DISCONNECT", id: "real-socket", data: "extra action" }, {}, context],
        ["disconnect", { channel: "DISCONNECT", id: "real-socket" }, { run: "b".repeat(32) }, context],
        ["disconnect", { channel: "DISCONNECT", id: "real-socket" }, {}, { ...context, dataPath: "/wrong" }],
        ["connection", { channel: "CONNECTION", id: "real-socket" }, {}, context]
    ])
        bridge.dispatch({ ...event(origin, message, metadata), context: frozen })
    assert.equal(deliveries.length, before)
    assert.ok(receipts.slice(-7).every((x) => x.accepted === false))
    bridge.dispatch(event("disconnect", { channel: "DISCONNECT", id: "real-socket" }))
    assert.deepEqual(deliveries.at(-1), { channel: "REMOTE", message: { channel: "DISCONNECT", id: "real-socket" }, id: undefined })
    assert.deepEqual(bridge.api.getRuntimeContext(), next, "cleanup never retags the current library")
    bridge.dispatch(event("disconnect", { channel: "DISCONNECT", id: "real-socket" }))
    assert.equal(deliveries.length, before + 1, "cleanup is one-use")
    assert.equal(receipts.at(-1).accepted, false)
    assert.deepEqual(errors, [])
    await tick()
})

test("remote sends snapshot payloads at call time and preserve lifecycle ordering without socket ACKs", async () => {
    const context = { runtimeId: "remote-queue", libraryGeneration: 1, dataPath: "/one" },
        sent = [],
        errors = []
    let release
    const held = new Promise((resolve) => {
        release = resolve
    })
    const bridge = createRustApi(
        async (request) => {
            sent.push(structuredClone(request))
            if (sent.length === 1) await held
            return []
        },
        (error) => errors.push(error),
        context
    )
    const start = { channel: "START", data: { disabled: { remote: false }, max: 10 } },
        message = { channel: "QUERY", id: "actual", data: { value: 1 } }
    bridge.api.send("MAIN", start)
    bridge.api.send("REMOTE", message)
    bridge.api.send("MAIN", { channel: "SERVER_DATA", data: { remote: { password: "secret" } } })
    message.data.value = 999
    start.data.max = 0
    await tick()
    assert.equal(sent.length, 1)
    assert.equal(sent[0].data.data.max, 10)
    release()
    await tick()
    await tick()
    assert.deepEqual(
        sent.map((x) => x.channel),
        ["MAIN", "REMOTE", "MAIN"]
    )
    assert.equal(sent[1].data.data.value, 1)
    assert.deepEqual(sent[1].context, context)
    const old = { channel: "ACTION", data: { value: 1 } }
    bridge.api.send("CONTROLLER", old)
    const next = { ...context, libraryGeneration: 2, dataPath: "/two" }
    bridge.dispatch({ channel: "MAIN", message: { channel: "DATA_PATH", data: next.dataPath }, context: next }, { channel: "MAIN", data: { channel: "UPDATE_DATA_PATH" } })
    await tick()
    assert.equal(sent.length, 3, "queued old-context outgoing client actions are cancelled")
    assert.deepEqual(errors, [])
})

test("native EXIF callbacks preserve actual source clone values, own undefined and property order", async (t) => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-exif-bridge-"))
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }))
    const ids = [...corpus(), ...generatedCorpus()].map(([name, bytes]) => {
        const file = path.join(directory, name)
        fs.writeFileSync(file, bytes)
        return file
    })
    ids.push(path.join(directory, "missing.jpg"))
    for (const name of ["test1.jpg", "test2.jpg", "test3.jpg", "short-ascii-MM.jpg", "short-ascii-II.jpg", "lens_info.jpg", "right.jpg", "down-mirrored.jpg", "sony-alpha-6000.jpg"]) ids.push(path.join(root, "node_modules/exif/test", name))
    const binary = process.env.FREESHOW_EXIF_PARITY || path.join(root, "target/debug/freeshow-exif-parity" + (process.platform === "win32" ? ".exe" : ""))
    const native = spawnSync(binary, [], { input: ids.map((id) => JSON.stringify({ id })).join("\n") + "\n", encoding: "utf8", timeout: 20000, maxBuffer: 16 * 1024 * 1024 })
    assert.equal(native.status, 0, native.stderr || native.error?.message)
    const outcomes = native.stdout.trim().split("\n").map(JSON.parse)
    const { ExifImage } = createRequire(import.meta.url)("exif")
    const original = upstreamFunctions("src/electron/utils/files.ts", ["readExifData"], { ExifImage, actionComplete: () => {} })
    const context = { runtimeId: "exif-values", libraryGeneration: 1, dataPath: directory }
    const errors = []
    const { api, dispatch } = createRustApi(
        async () => [],
        (error) => errors.push(error),
        context
    )
    const propertyOrder = (actual, expected) => {
        if (!expected || typeof expected !== "object" || ArrayBuffer.isView(expected)) return
        assert.deepEqual(Object.keys(actual), Object.keys(expected))
        for (const key of Object.keys(expected)) propertyOrder(actual[key], expected[key])
    }
    assert.equal(outcomes.length, ids.length)
    for (const [index, id] of ids.entries()) {
        const expected = structuredClone(await original.readExifData({ id }))
        let received
        api.receive(
            "MAIN",
            (message, listenerId) => {
                assert.equal(listenerId, id)
                received = message.data
            },
            "exif-source"
        )
        const value = outcomes[index]
        assert.equal(value.sourceCompatible, true, id)
        dispatch({ channel: "MAIN", message: { channel: "READ_EXIF", data: value.reply }, listenerId: id, context, nativeExif: value.hydration })
        assert.deepEqual(received, expected, id)
        propertyOrder(received, expected)
    }
    assert.deepEqual(errors, [])
})

test("EXIF restoration rejects malformed envelopes atomically and never hydrates stale replies", () => {
    const context = { runtimeId: "exif-validation", libraryGeneration: 2, dataPath: "/new" }
    const errors = [],
        replies = []
    const { api, dispatch } = createRustApi(
        async () => [],
        (error) => errors.push(error),
        context
    )
    api.receive("MAIN", (message) => replies.push(message))
    const data = { id: "file.jpg", exif: { image: { Keep: null }, exif: { Bytes: [0, 256] } } }
    const snapshot = structuredClone(data)
    const metadata = { undefinedPaths: ["/exif/image/Keep"], specialNumbers: [], byteBuffers: ["/exif/exif/Bytes"] }
    dispatch({ channel: "MAIN", message: { channel: "READ_EXIF", data }, context, nativeExif: metadata })
    assert.deepEqual(data, snapshot, "a late validation failure cannot apply earlier undefined writes")
    for (const patch of [{ undefinedPaths: ["/id"] }, { undefinedPaths: ["/exif/__proto__/polluted"] }, { undefinedPaths: ["/exif/missing/child"] }, { undefinedPaths: ["/exif/image/Keep", "/exif/image/Keep"] }, { specialNumbers: [{ path: "/exif/image/Keep", kind: "unknown" }] }, { objectKeys: [{ path: "/exif/image", keys: ["Invented"] }] }])
        dispatch({ channel: "MAIN", message: { channel: "READ_EXIF", data }, context, nativeExif: { undefinedPaths: [], specialNumbers: [], byteBuffers: [], ...patch } })
    assert.deepEqual(replies, [])
    assert.equal(errors.length, 7)
    assert.ok(errors.every((error) => error.code === "exif_publication"))
    const stale = { id: "missing.jpg" }
    dispatch({ channel: "MAIN", message: { channel: "READ_EXIF", data: stale }, context: { ...context, libraryGeneration: 1 }, nativeExif: { undefinedPaths: ["/exif"], specialNumbers: [], byteBuffers: [] } })
    assert.equal(Object.hasOwn(stale, "exif"), false)
    assert.equal(errors.length, 7)
    assert.equal(replies.length, 0)
})

test("retained canvas completion sends a one-use raw byte snapshot without a JSON write request", async () => {
    const context = { runtimeId: "capture", libraryGeneration: 1, dataPath: "/library" }
    const raw = [],
        json = [],
        errors = []
    const { api, dispatch } = createRustApi(
        async (request) => {
            json.push(request)
            return []
        },
        (error) => errors.push(error),
        context,
        async (upload) => {
            raw.push(upload)
        }
    )
    const id = "native-thumbnail-" + "a".repeat(32),
        output = "/private/canvas.png"
    const bytes = new Uint8Array([0, 255, 128])
    api.receive("MAIN", (message) => {
        assert.equal(message.data.id, id)
        api.send("MAIN", { channel: "SAVE_IMAGE", data: { id, path: output, buffer: bytes.buffer } })
        bytes.fill(1)
    })
    dispatch({ channel: "MAIN", message: { channel: "CAPTURE_CANVAS", data: { id, output } }, context, nativeCapture: { id } })
    await tick()
    assert.equal(json.length, 0)
    assert.equal(raw.length, 1)
    assert.equal(raw[0].id, id)
    assert.equal(raw[0].failed, false)
    assert.deepEqual([...new Uint8Array(raw[0].buffer)], [0, 255, 128])
    assert.deepEqual(errors, [])
    api.send("MAIN", { channel: "SAVE_IMAGE", data: { id, path: output, buffer: bytes.buffer } })
    await tick()
    assert.equal(raw.length, 1)
    assert.equal(json.length, 0)
    assert.equal(errors.at(-1).code, "thumbnail_capture")
})

test("private canvas uploads reject mismatched paths, oversized data and obsolete contexts", async () => {
    const context = { runtimeId: "capture", libraryGeneration: 1, dataPath: "/library" }
    const errors = [],
        raw = [],
        json = []
    const { api, dispatch } = createRustApi(
        async (request) => {
            json.push(request)
            return []
        },
        (error) => errors.push(error),
        context,
        async (upload) => {
            raw.push(upload)
        }
    )
    const output = "/private/canvas.png"
    const register = (digit) => {
        const id = "native-thumbnail-" + digit.repeat(32)
        dispatch({ channel: "MAIN", message: { channel: "CAPTURE_CANVAS", data: { id, output } }, context, nativeCapture: { id } })
        return id
    }
    api.send("MAIN", { channel: "SAVE_IMAGE", data: { id: register("1"), path: "/arbitrary/write.png", buffer: new ArrayBuffer(1) } })
    api.send("MAIN", { channel: "SAVE_IMAGE", data: { id: register("2"), path: output, buffer: new ArrayBuffer(8 * 1024 * 1024 + 1) } })
    const obsolete = register("3"),
        next = { ...context, libraryGeneration: 2, dataPath: "/other" }
    dispatch({ channel: "MAIN", message: { channel: "DATA_PATH", data: next.dataPath }, context: next }, { channel: "MAIN", data: { channel: "UPDATE_DATA_PATH" } })
    api.send("MAIN", { channel: "SAVE_IMAGE", data: { id: obsolete } })
    await tick()
    assert.equal(errors.length, 3)
    assert.equal(raw.length, 0)
    assert.equal(json.length, 0)
})

test("retained canvas failure completes only its private request and ordinary image exports stay explicit", async () => {
    const context = { runtimeId: "capture", libraryGeneration: 1, dataPath: "/library" }
    const raw = [],
        json = [],
        errors = []
    const { api, dispatch } = createRustApi(
        async (request) => {
            json.push(request)
            return []
        },
        (error) => errors.push(error),
        context,
        async (upload) => {
            raw.push(upload)
        }
    )
    const id = "native-thumbnail-" + "b".repeat(32)
    dispatch({ channel: "MAIN", message: { channel: "CAPTURE_CANVAS", data: { id, output: "/private/canvas.png" } }, context, nativeCapture: { id } })
    api.send("MAIN", { channel: "SAVE_IMAGE", data: { id } })
    api.send("MAIN", { channel: "SAVE_IMAGE", data: { path: "/export/image.png", base64: "source-export" } })
    await tick()
    assert.equal(raw.length, 1)
    assert.equal(raw[0].failed, true)
    assert.equal(raw[0].buffer.byteLength, 0)
    assert.equal(json.length, 1)
    assert.equal(json[0].data.data.base64, "source-export")
    assert.deepEqual(errors, [])
})

test("native media capabilities publish before retained callbacks and stale library grants never enter the registry", () => {
    const context = { runtimeId: "media", libraryGeneration: 1, dataPath: "/library" }
    const errors = [],
        seen = []
    const bridge = createRustApi(
        async () => [],
        (error) => errors.push(error),
        context
    )
    const grant = (sourcePath, digit, current = context) => ({ id: digit.repeat(32), sourcePath, url: `http://127.0.0.1:43210/${"a".repeat(64)}/${digit.repeat(32)}`, mimeType: "image/png", length: 68, context: current })
    const file = "/日本語/literal%20.png",
        media = grant(file, "1")
    bridge.api.receive("MAIN", (message) => seen.push({ message, url: bridge.api.resolveFilePath(file) }))
    const data = { id: "show", content: ["show", { media: { a: { path: file } }, future: true }] }
    bridge.dispatch({ channel: "MAIN", message: { channel: "SHOW", data }, context, nativeAssets: [media] })
    assert.equal(seen[0].message.data, data)
    assert.equal(seen[0].url, media.url)
    const next = { ...context, libraryGeneration: 2, dataPath: "/other" },
        fresh = grant("/other/media.png", "2", next)
    bridge.dispatch({ channel: "MAIN", message: { channel: "DATA_PATH", data: next.dataPath }, context: next, nativeAssets: [fresh] }, { channel: "MAIN", data: { channel: "UPDATE_DATA_PATH" } })
    assert.equal(bridge.api.resolveFilePath(fresh.sourcePath), fresh.url)
    assert.equal(bridge.api.resolveFilePath(file), "")
    bridge.dispatch({ channel: "MAIN", message: { channel: "SHOW", data }, context, nativeAssets: [media] })
    assert.equal(seen.length, 2)
    assert.equal(bridge.api.resolveFilePath(file), "")
    assert.equal(bridge.api.resolveFilePath(fresh.sourcePath), fresh.url)
    assert.ok(errors.every((error) => error.code === "assets_unregistered"))
})

test("output replay installs existing media before initial state and invalid publication cannot grant a path", () => {
    const context = { runtimeId: "output-media", libraryGeneration: 3, dataPath: "/library" }
    const errors = []
    const { api, dispatch } = createRustApi(
        async () => [],
        (error) => errors.push(error),
        context
    )
    const grant = { id: "1".repeat(32), sourcePath: "/source.png", url: `http://127.0.0.1:43210/${"a".repeat(64)}/${"1".repeat(32)}`, mimeType: "image/png", length: 68, context }
    let observed
    api.receive("OUTPUT", () => (observed = api.resolveFilePath(grant.sourcePath)))
    dispatch({ channel: "OUTPUT", message: { channel: "SHOWS", data: {} }, context, nativeAssets: [grant] })
    assert.equal(observed, grant.url)
    dispatch({ channel: "OUTPUT", message: { channel: "SHOWS", data: {} }, context, nativeAssets: [{ ...grant, sourcePath: "/private", url: "file:///private" }] })
    assert.equal(api.resolveFilePath("/private"), "")
    assert.equal(api.resolveFilePath(grant.sourcePath), grant.url)
    assert.ok(errors.some((error) => error.code === "assets_publication"))
})

test("READ_FOLDER delivery restores own undefined fields only after its library context is accepted", () => {
    const context = { runtimeId: "browse", libraryGeneration: 2, dataPath: "/library" }
    const seen = []
    const { api, dispatch } = createRustApi(
        async () => [],
        (error) => {
            throw error
        },
        context
    )
    api.receive("MAIN", (message) => seen.push(message))
    const data = () => ({ normal: { isFolder: true, noMedia: null, unknown: null }, stub: { isFolder: true }, file: { isFolder: false, noMedia: null }, missing: null })
    const current = data()
    dispatch({ channel: "MAIN", message: { channel: "READ_FOLDER", data: current }, context })
    assert.equal(Object.hasOwn(current.normal, "noMedia"), true)
    assert.equal(current.normal.noMedia, undefined)
    assert.equal(current.normal.unknown, null)
    assert.equal(Object.hasOwn(current.stub, "noMedia"), false)
    assert.equal(current.file.noMedia, null)
    const stale = data()
    dispatch({ channel: "MAIN", message: { channel: "READ_FOLDER", data: stale }, context: { ...context, libraryGeneration: 1 } })
    assert.equal(stale.normal.noMedia, null)
    assert.equal(seen.length, 1)
    const other = data()
    dispatch({ channel: "MAIN", message: { channel: "FILE_INFO", data: other }, context })
    assert.equal(other.normal.noMedia, null)
})

test("interleaved media setting writes and queries retain their send order", async () => {
    let releaseFirst,
        configured = "original"
    const replies = []
    const { api } = createRustApi(
        async (request) => {
            if (request.data.channel === "SET_MEDIA_FOLDER_PATH") {
                configured = request.data.data
                if (configured === "first")
                    await new Promise((resolve) => {
                        releaseFirst = resolve
                    })
                return []
            }
            return [{ channel: "MAIN", message: { channel: "GET_MEDIA_FOLDER_PATH", data: configured }, listenerId: request.listenerId }]
        },
        (error) => {
            throw error
        }
    )
    api.receive("MAIN", (message, id) => replies.push([id, message.data]))
    api.send("MAIN", { channel: "SET_MEDIA_FOLDER_PATH", data: "first" })
    api.send("MAIN", { channel: "GET_MEDIA_FOLDER_PATH" }, "first-query")
    api.send("MAIN", { channel: "SET_MEDIA_FOLDER_PATH", data: "second" })
    api.send("MAIN", { channel: "GET_MEDIA_FOLDER_PATH" }, "second-query")
    await tick()
    releaseFirst()
    await tick()
    assert.deepEqual(replies, [
        ["first-query", "first"],
        ["second-query", "second"]
    ])
})

test("media setting resets order following queries without queuing independent reads behind backups", async () => {
    const calls = [],
        errors = []
    const { api } = createRustApi(
        (request) => new Promise((resolve) => calls.push({ request, resolve })),
        (...error) => errors.push(error)
    )
    api.send("MAIN", { channel: "SAVE", data: { customTriggers: { backup: true } } })
    api.send("MAIN", { channel: "GET_MEDIA_FOLDER_PATH" }, "before-reset")
    api.send("MAIN", { channel: "LOCATE_MEDIA_FILE", data: { filePath: "/existing", folders: [] } }, "during-backup")
    await tick()
    assert.deepEqual(
        calls.map(({ request }) => request.data.channel),
        ["SAVE", "GET_MEDIA_FOLDER_PATH"]
    )
    calls[1].resolve([])
    await tick()
    assert.deepEqual(
        calls.map(({ request }) => request.data.channel),
        ["SAVE", "GET_MEDIA_FOLDER_PATH", "LOCATE_MEDIA_FILE"]
    )
    api.send("MAIN", { channel: "SET_MEDIA_FOLDER_PATH", data: "" })
    api.send("MAIN", { channel: "GET_MEDIA_FOLDER_PATH" }, "after-reset")
    api.send("MAIN", { channel: "LOCATE_MEDIA_FILE", data: { filePath: "/missing", folders: [] } })
    await tick()
    assert.equal(calls.length, 3)
    calls[0].resolve([])
    await tick()
    assert.equal(calls.length, 4)
    assert.equal(calls[3].request.data.channel, "SET_MEDIA_FOLDER_PATH")
    calls[3].resolve([])
    await tick()
    assert.equal(calls[4].request.data.channel, "GET_MEDIA_FOLDER_PATH")
    calls[4].resolve([])
    await tick()
    assert.deepEqual(
        calls.slice(4).map(({ request }) => request.data.channel),
        ["GET_MEDIA_FOLDER_PATH", "LOCATE_MEDIA_FILE"]
    )
    for (const call of calls.slice(1)) call.resolve([])
    await tick()
    assert.deepEqual(errors, [])
})

test("media copy snapshots wait for committed folder settings while independent queries and output continue", async () => {
    const context = { runtimeId: "media-copy", libraryGeneration: 1, dataPath: "/library" },
        calls = [],
        errors = []
    const bridge = createRustApi(
        (request) => new Promise((resolve) => calls.push({ request, resolve })),
        (error) => errors.push(error),
        context
    )
    bridge.api.send("MAIN", { channel: "SET_MEDIA_FOLDER_PATH", data: "/chosen" })
    const paths = { channel: "MEDIA_FOLDER_COPY", data: { paths: ["/original"] } },
        bundle = { channel: "BUNDLE_MEDIA_FILES", data: { outputPath: "/chosen", openFolder: true } }
    bridge.api.send("MAIN", paths, "copy")
    bridge.api.send("MAIN", bundle, "bundle")
    paths.data.paths[0] = "/mutated"
    bundle.data.outputPath = "/mutated"
    bridge.api.send("MAIN", { channel: "VERSION" }, "independent")
    bridge.api.send("OUTPUT", { channel: "SET_VALUE", data: { id: "slide", value: "live" } })
    await tick()
    assert.deepEqual(calls.map(({ request }) => request.data.channel).sort(), ["SET_MEDIA_FOLDER_PATH", "SET_VALUE", "VERSION"])
    calls.find(({ request }) => request.data.channel === "SET_MEDIA_FOLDER_PATH").resolve([])
    await tick()
    assert.deepEqual(
        calls.slice(3).map(({ request }) => [request.data.channel, request.listenerId, request.data.data]),
        [
            ["MEDIA_FOLDER_COPY", "copy", { paths: ["/original"] }],
            ["BUNDLE_MEDIA_FILES", "bundle", { outputPath: "/chosen", openFolder: true }]
        ]
    )
    for (const call of calls) {
        assert.deepEqual(call.request.context, context)
        call.resolve([])
    }
    await tick()
    assert.deepEqual(errors, [])
})

test("native event subscriptions isolate main and output windows", async () => {
    const listeners = []
    const makeWindow = (label) => ({
        addEventListener() {},
        removeEventListener() {},
        __TAURI__: {
            core: { invoke: async () => ({ runtimeId: "process", libraryGeneration: 0, dataPath: "/A" }) },
            webviewWindow: { getCurrentWebviewWindow: () => ({ label }) },
            event: {
                listen: async (name, callback, options) => {
                    listeners.push({ name, callback, target: options?.target })
                    return () => {}
                }
            }
        }
    })
    const main = makeWindow("main"),
        output = makeWindow("output-A")
    await installRustBridge(main)
    await installRustBridge(output)
    const mainMessages = [],
        outputMessages = []
    main.api.receive("OUTPUT", (m) => mainMessages.push(m))
    output.api.receive("OUTPUT", (m) => outputMessages.push(m))
    const emitTo = (label, message) =>
        listeners.forEach((listener) => {
            // Global Any listeners observe addressed events too. Each renderer
            // must subscribe with its own window target to prevent feedback.
            if (listener.name === "freeshow://ipc" && (!listener.target || listener.target.kind === "Any" || listener.target.label === label)) listener.callback({ payload: { channel: "OUTPUT", message } })
        })
    assert.deepEqual(
        listeners.filter((listener) => listener.name === "freeshow://drop").map((listener) => listener.target),
        [{ kind: "WebviewWindow", label: "main" }]
    )
    emitTo("output-A", { channel: "OUTPUTS", data: { A: { out: { slide: { index: 1 } } } } })
    emitTo("main", { channel: "REQUEST_DATA_MAIN" })
    assert.deepEqual(mainMessages, [{ channel: "REQUEST_DATA_MAIN" }])
    assert.deepEqual(outputMessages, [{ channel: "OUTPUTS", data: { A: { out: { slide: { index: 1 } } } } }])
})

test("library switches cancel queued and in-flight old output updates without replay", async () => {
    const calls = [],
        errors = []
    const first = { runtimeId: "process", libraryGeneration: 0, dataPath: "/A" }
    const next = { ...first, libraryGeneration: 1, dataPath: "/B" }
    const { api, dispatch } = createRustApi(
        (request) => new Promise((resolve, reject) => calls.push({ request, resolve, reject })),
        (...error) => errors.push(error),
        first
    )
    api.send("OUTPUT", { channel: "SHOWS", data: { old: true } })
    api.send("OUTPUT", { channel: "OUTPUTS", data: { old: true } })
    await tick()
    dispatch(
        {
            channel: "MAIN",
            message: { channel: "DATA_PATH", data: "/B" },
            context: next
        },
        { channel: "MAIN", data: { channel: "UPDATE_DATA_PATH" } }
    )
    calls[0].reject({ code: "stale_library" })
    await tick()
    assert.equal(calls.length, 1)
    assert.deepEqual(errors, [])
    api.send("OUTPUT", { channel: "SHOWS", data: { current: true } })
    await tick()
    assert.equal(calls.length, 2)
    assert.deepEqual(calls[1].request.context, next)
    calls[1].resolve([])
})

test("output cues keep send-time snapshots and order independently of lifecycle and library writes", async () => {
    const calls = [],
        failures = []
    const { api } = createRustApi(
        (request) => new Promise((resolve, reject) => calls.push({ request, resolve, reject })),
        (...error) => failures.push(error)
    )
    const payload = {
        channel: "OUTPUTS",
        data: { A: { out: { slide: { index: 1 } } } }
    }
    api.send("MAIN", { channel: "SAVE", data: {} })
    api.send("OUTPUT", { channel: "CREATE", data: { id: "B" } })
    api.send("OUTPUT", payload)
    payload.data.A.out.slide.index = 2
    api.send("OUTPUT", payload)
    await tick()
    assert.deepEqual(
        calls.map(({ request }) => request.data.channel),
        ["SAVE", "CREATE", "OUTPUTS"]
    )
    assert.equal(calls[2].request.data.data.A.out.slide.index, 1)
    calls[2].resolve([])
    await tick()
    assert.equal(calls[3].request.data.data.A.out.slide.index, 2)
    calls[3].reject(new Error("output disconnected"))
    api.send("OUTPUT", { channel: "SHOWS", data: { next: true } })
    await tick()
    assert.equal(failures.length, 1)
    assert.equal(calls[4].request.data.channel, "SHOWS")
    calls[4].resolve([])
    calls[1].resolve([])
    calls[0].resolve([])
    await tick()
})

test("bridge installs receiver after send and routes same-command concurrent replies by listener ID", async () => {
    const waiting = []
    const errors = []
    const { api } = createRustApi(
        (request) => new Promise((resolve) => waiting.push({ request, resolve })),
        (...error) => errors.push(error)
    )
    const received = []
    for (const id of ["one", "two"]) {
        api.send("MAIN", { channel: "SHOW", data: { id } }, id)
        api.receive(
            "MAIN",
            (message, listenerId) => {
                if (listenerId === id) {
                    received.push([listenerId, message.data])
                    api.removeListener("MAIN", id)
                }
            },
            id
        )
    }
    await tick()
    for (const { request, resolve } of waiting.reverse())
        resolve([
            {
                channel: "MAIN",
                message: { channel: "SHOW", data: request.data.data.id },
                listenerId: request.listenerId
            }
        ])
    await tick()
    assert.deepEqual(received, [
        ["two", "two"],
        ["one", "one"]
    ])
    assert.deepEqual(api.getListeners(), [])
    assert.deepEqual(errors, [])
})

test("named listener replacement, anonymous receivers, removal and callback isolation", () => {
    const errors = [],
        calls = []
    const { api, dispatch } = createRustApi(
        async () => [],
        (...error) => errors.push(error)
    )
    api.receive("MAIN", () => calls.push("old"), "replace")
    api.receive("MAIN", () => calls.push("new"), "replace")
    api.receive("MAIN", () => {
        throw new Error("receiver exception")
    })
    api.receive("MAIN", () => calls.push("anonymous"))
    dispatch({ channel: "MAIN", message: {} })
    assert.deepEqual(calls, ["new", "anonymous"])
    assert.equal(errors.length, 1)
    api.removeListener("OUTPUT", "replace")
    assert.deepEqual(api.getListeners(), [["MAIN", 3]])
    api.removeListener("MAIN", "replace")
    assert.deepEqual(api.getListeners(), [["MAIN", 2]])
})

test("native event registration completes before window.api is exposed", async () => {
    let receiveEvent, finish, finishDrop
    const target = {
        addEventListener() {},
        removeEventListener() {},
        __TAURI__: {
            webviewWindow: { getCurrentWebviewWindow: () => ({ label: "main" }) },
            core: {
                invoke: async () => ({
                    runtimeId: "test",
                    libraryGeneration: 0,
                    dataPath: "/data"
                })
            },
            event: {
                listen: (name, callback) => {
                    if (name === "freeshow://ipc") receiveEvent = callback
                    return new Promise((resolve) => {
                        if (name === "freeshow://ipc") finish = resolve
                        else {
                            assert.equal(name, "freeshow://drop")
                            finishDrop = resolve
                        }
                    })
                }
            }
        }
    }
    const installing = installRustBridge(target)
    await tick()
    assert.equal(target.api, undefined)
    finish(() => {})
    await tick()
    assert.equal(target.api, undefined, "Native drop subscription is part of the startup barrier")
    finishDrop(() => {})
    await installing
    const received = []
    target.api.receive("OUTPUT", (message) => received.push(message))
    receiveEvent({
        payload: {
            channel: "OUTPUT",
            message: { channel: "SLIDE", data: "lyrics" }
        }
    })
    assert.deepEqual(received, [{ channel: "SLIDE", data: "lyrics" }])
})

test("native rejections are reported without creating a successful reply", async () => {
    const errors = [],
        replies = []
    const error = { code: "unsupported_command", message: "MAIN.NDI_START" }
    const { api } = createRustApi(
        async () => {
            throw error
        },
        (...args) => errors.push(args)
    )
    api.receive("MAIN", (message) => replies.push(message))
    api.send("MAIN", { channel: "NDI_START" })
    await tick()
    assert.deepEqual(replies, [])
    assert.deepEqual(errors[0][0], error)
})

test("browser Files keep their real bytes and return an empty native path", async () => {
    const errors = []
    const { api } = createRustApi(
        async () => [],
        (...error) => errors.push(error)
    )
    assert.equal(api.showFilePath({ name: "test.show", path: "/test.show" }), "/test.show")
    const browser = new File(["real contents"], "test.show")
    assert.equal(api.showFilePath(browser), "")
    assert.equal(await browser.text(), "real contents")
    assert.deepEqual(errors, [])
})

const contextA = {
    runtimeId: "runtime-test",
    libraryGeneration: 0,
    dataPath: "/library-a"
}
const contextB = { ...contextA, libraryGeneration: 1, dataPath: "/library-b" }

test("mutating requests run in order while window/control channels can still be sent", async () => {
    const waiting = [],
        calls = [],
        errors = [],
        received = []
    const { api } = createRustApi(
        (request) => {
            calls.push(request)
            if (request.channel !== "MAIN" || request.data.channel === "MINIMIZE") return Promise.resolve([])
            return new Promise((resolve, reject) => waiting.push({ request, resolve, reject }))
        },
        (...error) => errors.push(error),
        contextA
    )
    api.receive("MAIN", (message) => received.push(message.channel))
    api.send("MAIN", { channel: "SAVE", data: {} })
    api.send("MAIN", {
        channel: "UPDATE_DATA_PATH",
        data: { newPath: "/library-b" }
    })
    api.send("MAIN", { channel: "SAVE", data: { runtimeContext: contextA } })
    api.send("MAIN", { channel: "MINIMIZE" })
    api.send("OUTPUT", { channel: "SLIDE" })
    await tick()
    assert.deepEqual(
        calls.map((request) => request.data.channel),
        ["SAVE", "MINIMIZE", "SLIDE"]
    )
    waiting.shift().resolve([
        {
            channel: "MAIN",
            message: { channel: "SAVE2", data: {} },
            context: contextA
        }
    ])
    await tick()
    assert.equal(received[0], "SAVE2")
    assert.equal(waiting[0].request.data.channel, "UPDATE_DATA_PATH")
    waiting.shift().resolve([
        {
            channel: "MAIN",
            message: { channel: "DATA_PATH", data: "/library-b" },
            context: contextB
        }
    ])
    await tick()
    assert.deepEqual(api.getRuntimeContext(), contextB)
    // The queued old snapshot keeps its own provenance even after the switch.
    assert.deepEqual(waiting[0].request.context, contextA)
    waiting.shift().reject({ code: "stale_library", recoveryPath: "/app/Recovery/save.json" })
    await tick()
    assert.equal(errors[0][0].code, "stale_library")
    api.send("MAIN", { channel: "SET_STORE_VALUE", data: {} })
    await tick()
    assert.deepEqual(waiting[0].request.context, contextB)
    waiting.shift().resolve([])
    await tick()
})

test("late replies from old libraries/sessions cannot replace current state or expose mutable context", () => {
    const calls = []
    const supplied = { ...contextA }
    const { api, dispatch } = createRustApi(
        async () => [],
        () => {},
        supplied
    )
    supplied.dataPath = "/changed-by-the-caller"
    assert.deepEqual(api.getRuntimeContext(), contextA)
    api.receive("MAIN", (message) => calls.push(message.data))
    dispatch({
        channel: "MAIN",
        message: { channel: "PROJECTS", data: "premature" },
        context: contextB
    })
    dispatch(
        {
            channel: "MAIN",
            message: { channel: "DATA_PATH", data: "early path query" },
            context: contextB
        },
        { channel: "MAIN", data: { channel: "DATA_PATH" } }
    )
    assert.deepEqual(api.getRuntimeContext(), contextA)
    dispatch(
        {
            channel: "MAIN",
            message: { channel: "DATA_PATH", data: "new" },
            context: contextB
        },
        { channel: "MAIN", data: { channel: "UPDATE_DATA_PATH" } }
    )
    dispatch({
        channel: "MAIN",
        message: { channel: "SHOWS", data: "stale" },
        context: contextA
    })
    dispatch({
        channel: "MAIN",
        message: { channel: "SHOWS", data: "previous process" },
        context: { ...contextB, runtimeId: "old-process" }
    })
    const copy = api.getRuntimeContext()
    copy.dataPath = "/changed-from-outside"
    assert.deepEqual(api.getRuntimeContext(), contextB)
    assert.deepEqual(calls, ["new"])
})

test("a rejected mutation reports its failure before the next mutation starts", async () => {
    const order = []
    const { api } = createRustApi(
        async (request) => {
            order.push(request.data.channel)
            if (request.data.channel === "SAVE") throw { code: "io_error" }
            return []
        },
        (error) => order.push(error.code),
        contextA
    )
    api.send("MAIN", { channel: "SAVE", data: {} })
    api.send("MAIN", { channel: "UPDATE_DATA_PATH", data: {} })
    await tick()
    assert.deepEqual(order, ["SAVE", "io_error", "UPDATE_DATA_PATH"])
})
