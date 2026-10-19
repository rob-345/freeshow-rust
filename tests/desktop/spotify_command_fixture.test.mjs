// Source-only controls for the actual WebView script; this is not a GUI proof.
import assert from "node:assert/strict"
import fs from "node:fs"
import test from "node:test"
const source = fs.readFileSync(new URL("./spotify_command_probe.js", import.meta.url), "utf8")
const execute = new Function("window", "input", "setTimeout", "clearTimeout", `return (function(){${source}}).call(null,input)`)
const context = { runtimeId: "actual-runtime", libraryGeneration: 7, dataPath: "/tmp/fixture 日本語" }
const bits = (value) => {
    const bytes = Buffer.alloc(8)
    bytes.writeDoubleBE(value)
    return bytes.toString("hex")
}
function fixture(control = {}) {
    const receivers = new Map(),
        runtime = new Set(),
        observed = [],
        routes = []
    let current = structuredClone(context),
        removed = 0
    const window = {
        api: {
            getRuntimeContext: () => structuredClone(current),
            receive: (channel, fn, id) => {
                assert.equal(channel, "MAIN")
                assert.equal(receivers.has(id), false)
                receivers.set(id, fn)
            },
            removeListener: (channel, id) => {
                assert.equal(channel, "MAIN")
                assert.equal(receivers.delete(id), true)
                removed++
            },
            send: (channel, message, listener) => {
                assert.equal(channel, "MAIN")
                assert.equal(message.channel, "SPOTIFY_COMMAND")
                observed.push({ command: message.data.command, ...(Object.hasOwn(message.data, "value") ? { valueBits: bits(message.data.value) } : {}) })
                queueMicrotask(() => {
                    const receiver = receivers.values().next().value
                    // Unknown, null and wrong correlation must not match.
                    receiver(null, undefined)
                    receiver({ channel: "OTHER" }, undefined)
                    receiver({ channel: "SPOTIFY_COMMAND", data: false }, "foreign-listener")
                    if (control.runtimeError) {
                        for (const fn of runtime) fn({ detail: { request: { listenerId: listener }, error: { code: "controlled" } } })
                        return
                    }
                    if (control.lost) return
                    if (control.context) current = { ...current, ...control.context }
                    const value = control.message || { channel: "SPOTIFY_COMMAND", data: true }
                    receiver(...(control.args || [value, listener]))
                    if (control.duplicate) receiver(value, listener)
                })
                return control.nonvoid ? Promise.resolve() : undefined
            }
        },
        __TAURI__: {
            core: {
                invoke: async (route, payload) => {
                    routes.push({ route, payload: structuredClone(payload) })
                    if (control.routeAccept) return []
                    throw { code: control.routeCode || "spotify_raw_body" }
                }
            }
        },
        addEventListener: (name, fn) => {
            assert.equal(name, "freeshow:runtime-error")
            runtime.add(fn)
        },
        removeEventListener: (name, fn) => {
            assert.equal(name, "freeshow:runtime-error")
            assert.equal(runtime.delete(fn), true)
        }
    }
    const run = (input) => execute(window, input, (fn, ms) => setTimeout(fn, ms === 5000 ? 100 : Math.min(ms, 2)), clearTimeout)
    return {
        run,
        observed,
        routes,
        clean: () => {
            assert.equal(receivers.size, 0)
            assert.equal(runtime.size, 0)
            assert.equal(removed, 1)
        }
    }
}

test("actual exported probe preserves all six send-time command snapshots, primitive ULP and original callbacks", async () => {
    for (const command of ["playpause", "next", "prev", "pause", "setVolume", "seek"]) {
        const f = fixture(),
            input = { phase: "ordinary", command, ...(["setVolume", "seek"].includes(command) ? { valueBits: bits(command === "seek" ? 3.0000005 : 0.5000000000000001) } : {}) }
        const actual = await f.run(input)
        assert.deepEqual(f.observed, [{ command, ...(input.valueBits ? { valueBits: input.valueBits } : {}) }])
        assert.deepEqual(actual.sent, f.observed[0])
        assert.equal(actual.sendUndefined, true)
        assert.deepEqual(actual.context, context)
        assert.deepEqual(actual.current, context)
        assert.deepEqual(actual.callbacks, [{ message: { channel: "SPOTIFY_COMMAND", data: true }, messageKeys: ["channel", "data"], args: 2, listener: `spotify-command-probe-ordinary-${command}`, callbackContext: context }])
        f.clean()
    }
})
test("both real route shapes must reject raw-body guard and never synthesize source callbacks", async () => {
    const f = fixture(),
        actual = await f.run({ phase: "ordinary", action: "routes" })
    assert.deepEqual(actual.rejections, [
        { route: "ipc_dispatch", code: "spotify_raw_body" },
        { route: "spotify_command", code: "spotify_raw_body" }
    ])
    assert.deepEqual(actual.callbacks, [])
    assert.deepEqual(
        f.routes.map((r) => r.route),
        ["ipc_dispatch", "spotify_command"]
    )
    for (const route of f.routes) assert.deepEqual(route.payload.request, { channel: "MAIN", context, listenerId: "spotify-command-probe-ordinary-routes", data: { channel: "SPOTIFY_COMMAND", data: { command: "pause" } } })
    f.clean()
    for (const control of [{ routeAccept: true }, { routeCode: "other_error" }]) {
        const negative = fixture(control)
        await assert.rejects(negative.run({ phase: "ordinary", action: "routes" }))
        negative.clean()
    }
})
test("malformed relevant/duplicate/private callbacks, foreign context, command error and lost reply fail with listener cleanup", async () => {
    for (const control of [{ message: { channel: "SPOTIFY_COMMAND", data: false } }, { message: { channel: "SPOTIFY_COMMAND", data: true, nativeLifetime: {} } }, { duplicate: true }, { context: { runtimeId: "foreign-runtime" } }, { context: { libraryGeneration: 8 } }, { context: { dataPath: "/different" } }, { args: [{ channel: "SPOTIFY_COMMAND", data: true }, "spotify-command-probe-ordinary-pause", "private"] }, { runtimeError: true }, { nonvoid: true }, { lost: true }]) {
        const f = fixture(control)
        await assert.rejects(f.run({ phase: "ordinary", command: "pause" }))
        f.clean()
    }
})
test("invalid fixture actions and numeric bits fail before original send", async () => {
    for (const input of [{ action: "other" }, { command: "other" }, { command: "seek", valueBits: "not-bits" }, { command: "setVolume", valueBits: "7ff0000000000000" }]) {
        const f = fixture()
        await assert.rejects(f.run({ phase: "ordinary", ...input }))
        assert.deepEqual(f.observed, [])
    }
})
