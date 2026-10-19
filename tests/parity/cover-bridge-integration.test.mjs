import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import ts from "typescript"

const root = process.env.FREESHOW_COVER_BRIDGE_ROOT || fileURLToPath(new URL("../../", import.meta.url))
const tick = () => new Promise((resolve) => setImmediate(resolve))
const context = (libraryGeneration = 3) => ({ runtimeId: "native-cover-integration-音", libraryGeneration, dataPath: `/selected/🎵/${libraryGeneration}` })
const id = (n) => `native-cover-${n.toString(16).padStart(32, "0")}`
function deferred() {
    let resolve, reject
    const promise = new Promise((a, b) => {
        resolve = a
        reject = b
    })
    return { promise, resolve, reject }
}
function timers() {
    let serial = 0
    const pending = new Map()
    return {
        setTimeout(callback, delay) {
            const token = ++serial
            pending.set(token, { callback, delay })
            return token
        },
        clearTimeout(token) {
            pending.delete(token)
        },
        fire() {
            const ready = [...pending.values()]
            pending.clear()
            for (const { callback } of ready) callback()
            return ready.length
        },
        get size() {
            return pending.size
        }
    }
}
function modules(clock = timers()) {
    const loaded = new Map()
    function load(name) {
        if (loaded.has(name)) return loaded.get(name)
        const source = fs.readFileSync(path.join(root, `src/frontend/rust/${name}.ts`), "utf8")
        const result = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS }, reportDiagnostics: true })
        assert.equal(result.diagnostics.length, 0)
        const exported = {}
        loaded.set(name, exported)
        // All local dependencies are actual product modules. Only timers and
        // the native Tauri boundary are controlled; no helper is substituted.
        new Function("exports", "require", "setTimeout", "clearTimeout", result.outputText)(
            exported,
            (dependency) => {
                assert.match(dependency, /^\.\/[A-Za-z][A-Za-z0-9]*$/)
                return load(dependency.slice(2))
            },
            clock.setTimeout,
            clock.clearTimeout
        )
        return exported
    }
    return { ...load("bridge"), loaded, clock }
}
function event(n, ctx = context(), folder = `${ctx.dataPath}/Audio`) {
    return {
        channel: "MAIN",
        context: { ...ctx },
        message: { channel: "ALERT", data: `Error: Could not create folder at: ${folder}!` },
        nativeCover: { id: id(n), sequence: String(n), folder }
    }
}
function shared({ initial = context(), transport } = {}) {
    const module = modules(),
        requests = [],
        completions = [],
        messages = [],
        errors = []
    const bridge = module.createRustApi(
        async (request) => {
            requests.push(structuredClone(request))
            return transport ? transport(request) : []
        },
        (error, request) => errors.push({ error, request }),
        initial,
        undefined,
        undefined,
        undefined,
        undefined,
        undefined,
        undefined,
        (frozen, nonce, accepted) => {
            const work = deferred()
            completions.push({ ...work, context: frozen, id: nonce, accepted })
            return work.promise
        }
    )
    bridge.api.receive("MAIN", (message, listenerId) => messages.push({ message, listenerId }))
    return { ...module, bridge, requests, completions, messages, errors }
}
async function settle(h) {
    for (const completion of h.completions) completion.resolve()
    await tick()
}
function installed(label = "main", { loss = false } = {}) {
    const module = modules(),
        calls = [],
        completions = [],
        errors = [],
        subscriptions = [],
        ctx = context()
    const target = {
        addEventListener() {},
        removeEventListener() {},
        postMessage() {},
        dispatchEvent(event) {
            errors.push(event.detail)
        },
        __TAURI__: {
            core: {
                invoke(name, args) {
                    calls.push({ name, args })
                    if (name === "runtime_context") return Promise.resolve(ctx)
                    if (name === "cover_complete" && loss) {
                        const work = deferred()
                        completions.push({ ...work, args })
                        return work.promise
                    }
                    return Promise.resolve([])
                }
            },
            event: {
                async listen(name, callback, scope) {
                    subscriptions.push({ name, callback, scope })
                    return () => {}
                }
            },
            webviewWindow: { getCurrentWebviewWindow: () => ({ label }) }
        }
    }
    return { ...module, target, calls, completions, errors, subscriptions, ctx }
}

test("shared dispatch calls the actual cover helper before ordinary handling and delivers only the original ALERT with undefined listener", async () => {
    const h = shared(),
        input = event(1)
    assert.ok(h.loaded.has("coverBridge"), "the shared bridge must use the actual cover helper")
    input.listenerId = undefined
    assert.equal(h.bridge.dispatch(input), undefined)
    assert.deepEqual(h.messages, [{ message: { channel: "ALERT", data: input.message.data }, listenerId: undefined }])
    assert.deepEqual(Object.keys(h.messages[0].message), ["channel", "data"])
    assert.notEqual(h.messages[0].message, input.message)
    assert.equal(h.completions.length, 0, "source callback entry precedes the deferred native completion")
    await tick()
    assert.equal(h.completions.length, 1)
    assert.deepEqual(h.completions[0].context, context())
    assert.equal(h.completions[0].id, id(1))
    assert.equal(h.completions[0].accepted, true)
    assert.equal(h.requests.length, 0, "a private ALERT is neither a source command nor a source success ACK")
    assert.equal(h.errors.length, 0)
    await settle(h)
})

test("foreign runtime/path/generation and malformed private payloads cannot enter generic context adoption or leak native metadata", async () => {
    const h = shared()
    for (const ctx of [{ ...context(), runtimeId: "foreign" }, { ...context(), dataPath: "/same-generation-other-path" }, context(2), context(4)]) {
        h.bridge.dispatch(event(1, ctx))
        assert.deepEqual(h.bridge.api.getRuntimeContext(), context())
    }
    const wrong = event(1)
    wrong.listenerId = "original-listener-must-not-exist"
    h.bridge.dispatch(wrong)
    await tick()
    assert.equal(h.messages.length, 0)
    assert.equal(h.completions.length, 1, "only the known current-context malformed event gets a negative completion")
    assert.equal(h.completions[0].accepted, false)
    await settle(h)
    const corrected = event(1)
    h.bridge.dispatch(corrected)
    await tick()
    assert.deepEqual(h.messages, [{ message: corrected.message, listenerId: undefined }])
    assert.deepEqual(Object.keys(h.messages[0].message), ["channel", "data"])
    assert.equal(h.completions.at(-1).accepted, true)
    assert.deepEqual(h.bridge.api.getRuntimeContext(), context())
    await settle(h)
})

test("actual shared bridge retains four lost cover completions, rejects the fifth, and retries only after real promise settlement", async () => {
    const h = shared()
    for (let n = 1; n <= 4; n++) h.bridge.dispatch(event(n))
    h.bridge.dispatch(event(5))
    await tick()
    assert.equal(h.messages.length, 4)
    assert.equal(h.completions.length, 4)
    assert.equal(h.errors.at(-1).error.code, "cover_completion_busy")
    assert.equal(h.requests.length, 0)
    h.completions[0].resolve()
    await tick()
    h.bridge.dispatch(event(5))
    await tick()
    assert.equal(h.messages.length, 5)
    assert.equal(h.completions.length, 5)
    assert.equal(h.completions.at(-1).id, id(5))
    assert.ok(h.completions.every((entry) => entry.accepted))
    await settle(h)
})

test("shared UPDATE_DATA_PATH retirement observes old completions without resetting quota, sequence or the new current context", async () => {
    const old = context(),
        next = context(4)
    const h = shared({ transport: (request) => (request.data?.channel === "UPDATE_DATA_PATH" ? [{ channel: "MAIN", context: next, message: { channel: "DATA_PATH", data: next.dataPath } }] : []) })
    for (let n = 1; n <= 4; n++) h.bridge.dispatch(event(n, old))
    await tick()
    assert.equal(h.bridge.api.send("MAIN", { channel: "UPDATE_DATA_PATH", data: next.dataPath }), undefined)
    await tick()
    assert.deepEqual(h.bridge.api.getRuntimeContext(), next)
    assert.ok(h.clock.size > 0, "the old-context completion observation actually entered")
    h.bridge.dispatch(event(5, next))
    await tick()
    assert.equal(h.completions.length, 4)
    assert.equal(h.errors.at(-1).error.code, "cover_completion_busy")
    assert.ok(h.clock.fire() > 0)
    await tick()
    assert.ok(h.errors.some((entry) => entry.error.code === "cover_retirement_timeout"))
    h.bridge.dispatch(event(5, next))
    await tick()
    assert.equal(h.completions.length, 4, "observer timeout did not release any old native promise")
    h.completions[0].resolve()
    await tick()
    h.bridge.dispatch(event(5, next))
    await tick()
    assert.equal(h.completions.length, 5)
    assert.deepEqual(h.completions.at(-1).context, next)
    h.bridge.dispatch(event(6, old))
    await tick()
    assert.equal(h.completions.length, 5, "retired full context cannot be adopted after root selection")
    await settle(h)
    assert.equal(h.clock.size, 0)
})

test("source remote STOP does not retire or close frontend cover notification admission", async () => {
    const h = shared()
    h.bridge.dispatch(event(1))
    await tick()
    assert.equal(h.bridge.api.send("REMOTE", { channel: "STOP" }), undefined)
    await tick()
    assert.equal(h.requests.length, 1)
    assert.equal(h.requests[0].channel, "REMOTE")
    assert.equal(h.requests[0].data.channel, "STOP")
    h.bridge.dispatch(event(2))
    await tick()
    assert.equal(h.completions.length, 2)
    assert.equal(h.messages.length, 2)
    assert.equal(h.clock.size, 0, "source STOP must not create a frontend cover retirement observer")
    await settle(h)
})

test("terminal CLOSE still attempts actual native transport after UI observer timeout while lost completion promises remain retained", async () => {
    const h = shared()
    for (let n = 1; n <= 4; n++) h.bridge.dispatch(event(n))
    await tick()
    assert.equal(h.bridge.api.send("MAIN", { channel: "CLOSE" }), undefined)
    await tick()
    assert.equal(h.requests.length, 0, "native CLOSE waits the bounded frontend observation first")
    assert.ok(h.clock.size > 0)
    h.bridge.dispatch(event(5))
    assert.equal(h.errors.at(-1).error.code, "cover_closed")
    h.clock.fire()
    await tick()
    assert.equal(h.requests.length, 1)
    assert.equal(h.requests[0].data.channel, "CLOSE")
    assert.deepEqual(h.requests[0].context, context())
    assert.ok(h.errors.some((entry) => entry.error.code === "cover_retirement_timeout"))
    assert.equal(h.completions.length, 4)
    h.bridge.api.send("MAIN", { channel: "CLOSE" })
    await tick()
    assert.equal(h.requests.length, 1)
    assert.ok(h.clock.size > 0, "terminal retry still observes the same four actual lost responses")
    h.clock.fire()
    await tick()
    assert.equal(h.requests.length, 2)
    assert.equal(h.requests[1].data.channel, "CLOSE")
    await settle(h)
    h.bridge.dispatch(event(6))
    await tick()
    assert.equal(h.completions.length, 4)
    assert.equal(h.messages.length, 4)
    assert.equal(h.errors.at(-1).error.code, "cover_closed")
})

test("main install invokes cover_complete with captured frozen native context and strips private metadata from original callbacks", async () => {
    const h = installed("main", { loss: true })
    await h.installRustBridge(h.target)
    const ipc = h.subscriptions.find((entry) => entry.name === "freeshow://ipc")
    assert.ok(ipc)
    assert.deepEqual(ipc.scope, { target: { kind: "WebviewWindow", label: "main" } })
    const messages = []
    h.target.api.receive("MAIN", (message, listenerId) => messages.push({ message, listenerId }))
    const input = event(1)
    ipc.callback({ payload: input })
    input.context.dataPath = "/mutated-after-dispatch"
    input.nativeCover.id = id(2)
    input.nativeCover.folder = "/mutated/Audio"
    await tick()
    const calls = h.calls.filter((entry) => entry.name === "cover_complete")
    assert.equal(calls.length, 1)
    assert.deepEqual(calls[0].args, { context: context(), id: id(1), accepted: true })
    assert.equal(Object.isFrozen(calls[0].args.context), true)
    assert.deepEqual(messages, [{ message: { channel: "ALERT", data: `Error: Could not create folder at: ${context().dataPath}/Audio!` }, listenerId: undefined }])
    assert.deepEqual(Object.keys(messages[0].message), ["channel", "data"])
    assert.equal(
        h.calls.some((entry) => entry.name === "ipc_dispatch"),
        false
    )
    assert.equal(h.errors.length, 0)
    h.completions[0].resolve()
    await tick()
})

test("an output installer never gains native cover completion capability or exposes a private publication as a generic MAIN event", async () => {
    const h = installed("output-1")
    await h.installRustBridge(h.target)
    const ipc = h.subscriptions.find((entry) => entry.name === "freeshow://ipc")
    assert.deepEqual(ipc.scope, { target: { kind: "WebviewWindow", label: "output-1" } })
    const messages = []
    h.target.api.receive("MAIN", (...args) => messages.push(args))
    const originalConsole = console.error
    console.error = () => {}
    try {
        ipc.callback({ payload: event(1) })
        await tick()
    } finally {
        console.error = originalConsole
    }
    assert.equal(messages.length, 0)
    assert.equal(
        h.calls.some((entry) => entry.name === "cover_complete"),
        false
    )
    assert.deepEqual(h.target.api.getRuntimeContext(), context())
    assert.equal(h.errors.length, 1)
    assert.equal(h.errors[0].error.code, "cover_unavailable")
})
