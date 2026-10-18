import assert from "node:assert/strict"
import { test } from "node:test"
import fs from "node:fs"
import path from "node:path"
import ts from "typescript"
import { root } from "./oracle.mjs"

const source = fs.readFileSync(path.join(root, "src/frontend/rust/bridge.ts"), "utf8")
const javascript = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } }).outputText
const { createRustApi, installRustBridge } = await import(`data:text/javascript;base64,${Buffer.from(javascript).toString("base64")}`)
const tick = () => new Promise((resolve) => setImmediate(resolve))

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
    for (const { request, resolve } of waiting.reverse()) resolve([{ channel: "MAIN", message: { channel: "SHOW", data: request.data.data.id }, listenerId: request.listenerId }])
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
            core: { invoke: async () => [] },
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
    assert.equal(target.api, undefined)
    finish(() => {})
    await installing
    const received = []
    target.api.receive("OUTPUT", (message) => received.push(message))
    receiveEvent({ payload: { channel: "OUTPUT", message: { channel: "SLIDE", data: "lyrics" } } })
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
