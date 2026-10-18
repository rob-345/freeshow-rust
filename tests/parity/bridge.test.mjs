import assert from "node:assert/strict"
import { test } from "node:test"
import fs from "node:fs"
import path from "node:path"
import ts from "typescript"
import { root } from "./oracle.mjs"

const source = fs.readFileSync(path.join(root, "src/frontend/rust/bridge.ts"), "utf8")
const javascript = ts.transpileModule(source, {
    compilerOptions: {
        module: ts.ModuleKind.ES2022,
        target: ts.ScriptTarget.ES2022
    }
}).outputText
const { createRustApi, installRustBridge } = await import(`data:text/javascript;base64,${Buffer.from(javascript).toString("base64")}`)
const tick = () => new Promise((resolve) => setImmediate(resolve))

test("native event subscriptions isolate main and output windows", async () => {
    const listeners = []
    const makeWindow = (label) => ({
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
            if (!listener.target || listener.target.kind === "Any" || listener.target.label === label) listener.callback({ payload: { channel: "OUTPUT", message } })
        })
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
    let receiveEvent, finish
    const target = {
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
                listen: (_name, callback) => {
                    receiveEvent = callback
                    return new Promise((resolve) => {
                        finish = resolve
                    })
                }
            }
        }
    }
    const installing = installRustBridge(target)
    await tick()
    assert.equal(target.api, undefined)
    finish(() => {})
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

test("missing native file path reports incomplete support", () => {
    const errors = []
    const { api } = createRustApi(
        async () => [],
        (...error) => errors.push(error)
    )
    assert.equal(api.showFilePath({ name: "test.show", path: "/test.show" }), "/test.show")
    assert.equal(api.showFilePath({ name: "test.show" }), "")
    assert.equal(errors[0][0].code, "unsupported_command")
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
