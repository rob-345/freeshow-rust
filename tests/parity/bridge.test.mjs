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
const { createRustApi, installRustBridge } = await import(`data:text/javascript;base64,${Buffer.from(javascript).toString("base64")}`)
const tick = () => new Promise((resolve) => setImmediate(resolve))

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
