import assert from "node:assert/strict"
import fs from "node:fs"
import test from "node:test"
import { commandBus, commandOracle } from "./spotify-command-oracle.mjs"
import { cleanup, Driver } from "./spotify-oracle.mjs"

const linux = { skip: process.platform !== "linux" ? "actual Linux private D-Bus peer required; pure source tests run on all platforms" : false }
const values = (bus) => bus.events()
const waitEvent = (bus, name, after = 0) => bus.peer.wait((line, index) => index >= after && line.startsWith("{") && JSON.parse(line).name === name)
async function observed(driver, predicate) {
    const deadline = Date.now() + 5000
    let status
    do {
        status = await driver.value({ op: "status" })
        if (predicate(status)) return status
        await new Promise((resolve) => setImmediate(resolve))
    } while (Date.now() < deadline)
    assert.fail(`Native ownership observation not reached: ${JSON.stringify(status)}`)
}
const bits = (value) => {
    const bytes = Buffer.alloc(8)
    bytes.writeDoubleBE(value)
    return bytes.toString("hex")
}

test("immutable complete source command + MAIN returns true before callbacks, preserving listener and source no-ops", async () => {
    const callbacks = []
    const source = commandOracle((command, callback) => callbacks.push({ command, callback }))
    for (const [command, value] of [["pause"], ["seek", 1.5], ["unknown"], ["setVolume"], ["seek"], ["setVolume", null], ["setVolume", "0.5"]]) {
        const count = source.calls.replies.length
        const listener = `listener-${count}`
        await source.send({ command, value, ignored: { nested: true } }, listener)
        assert.deepEqual(source.calls.replies[count], ["MAIN", { channel: "SPOTIFY_COMMAND", data: true }, listener])
        assert.deepEqual(Object.keys(source.calls.replies[count][1]), ["channel", "data"])
    }
    assert.equal(callbacks.length, 4)
    assert.equal(source.calls.commands[1].includes("--print-reply"), true)
    assert.equal(source.calls.commands[2].endsWith("variant:double:null"), true, "source coercion outside the native finite-Number subset is documented")
    callbacks[1].callback(null, 'string "mpris:trackid" variant string "/held/id"')
    assert.equal(callbacks.length, 5, "source seek continuation is independent of early MAIN reply")
    assert.equal(source.calls.commands.at(-1).endsWith("SetPosition objpath:/held/id int64:1500000"), true)
    callbacks[0].callback({ message: "ServiceUnknown" })
    assert.equal(source.calls.errors, 0)
    callbacks[2].callback({ message: "controlled failure" })
    assert.equal(source.calls.errors, 1)
})

test("actual source Number formatting/Math.round matches native finite preparation including generated IEEE cases", async () => {
    const driver = new Driver()
    let seed = 0x29d94defn
    const generated = []
    for (let i = 0; i < 160; i++) {
        seed = BigInt.asUintN(64, seed * 6364136223846793005n + 1442695040888963407n)
        const bytes = Buffer.alloc(8)
        bytes.writeBigUInt64BE(seed)
        const value = bytes.readDoubleBE()
        if (Number.isFinite(value)) generated.push(value)
    }
    const samples = [0, -0, 5e-324, -5e-324, 1e-7, 1e-6, 1e20, 1e21, Number.MAX_VALUE, -Number.MAX_VALUE, 0.1, -0.0000005, 0.0000005, -0.0000015, 0.0000015, -0.0000004999999999999999, ...generated]
    try {
        for (const value of samples) {
            const source = commandOracle(() => {})
            await source.send({ command: "setVolume", value })
            const expected = source.calls.commands[0].split("variant:double:")[1]
            const native = await driver.value({ op: "prepareCommand", command: "setVolume", valueBits: bits(value) })
            assert.equal(native.prepared, `PreparedCommand(SetVolume(${JSON.stringify(expected)}))`)
            if (Math.abs(value * 1e6) <= Number.MAX_SAFE_INTEGER) {
                const callbacks = []
                const seek = commandOracle((_, callback) => callbacks.push(callback))
                await seek.send({ command: "seek", value })
                callbacks[0](null, 'string "mpris:trackid" variant string "/fixture/id"')
                const actual = await driver.value({ op: "prepareCommand", command: "seek", valueBits: bits(value) })
                assert.equal(actual.prepared, `PreparedCommand(Seek(${JSON.stringify(seek.calls.commands[1].split("int64:")[1])}))`)
            }
        }
        for (const command of ["play", "unknown", "setVolume", "seek"]) assert.equal((await driver.request({ op: "prepareCommand", command })).error.code, "spotify_command_unsupported")
        for (const value of [Infinity, -Infinity, NaN]) assert.equal((await driver.request({ op: "prepareCommand", command: "setVolume", valueBits: bits(value) })).error.code, "spotify_command_unsupported")
        assert.equal((await driver.request({ op: "prepareCommand", command: "seek", value: 1e20 })).error.code, "spotify_command_unsupported")
        for (const value of [null, "0.5", true, {}, []]) assert.equal((await driver.request({ op: "prepareCommand", command: "setVolume", value })).error.code, "spotify_invalid")
    } finally {
        await driver.finish()
    }
})

test("real native six-command effects match actual source private GIO observations", linux, async () => {
    const bus = await commandBus(),
        driver = new Driver(bus.env)
    try {
        assert.equal((await driver.value({ op: "start" })).state, "available")
        for (const [command, value, name] of [
            ["playpause", undefined, "PlayPause"],
            ["next", undefined, "Next"],
            ["prev", undefined, "Previous"],
            ["pause", { ignored: [null, "not coerced"] }, "Pause"],
            ["setVolume", 0.375, "Set"],
            ["seek", -0.0000015, "SetPosition"]
        ]) {
            const sourceStart = bus.peer.lines.length
            await bus.original.send({ command, value }, "exact-listener")
            await waitEvent(bus, name, sourceStart)
            await bus.settle()
            const expected = values(bus).slice(-(command === "seek" ? 2 : 1))
            const nativeStart = bus.peer.lines.length
            const accepted = await driver.value({ op: "command", command, ...(value === undefined ? {} : { value }) })
            assert.equal(accepted.reply, true)
            const finished = await driver.value({ op: "finishCommand", id: accepted.id })
            assert.equal(finished, "sent", "no-print-reply exit is tool send/exit, not server ACK")
            await waitEvent(bus, name, nativeStart)
            assert.deepEqual(values(bus).slice(-expected.length), expected, command)
        }
        const before = await observed(driver, (s) => s.commandsCompleted === 6 && s.retained === 0)
        assert.equal(before.commandPids.length, 0)
        assert.equal(before.spawned, before.reaped)
        await driver.value({ op: "close" })
        assert.equal((await driver.value({ op: "status" })).drained, true)
    } finally {
        await cleanup(driver.finish(), bus.close())
    }
})

test("a real held seek replies early and cannot block later pause/volume; source and native continuation order agree", linux, async () => {
    for (const lane of ["source", "native"]) {
        const bus = await commandBus("hold"),
            driver = new Driver(bus.env)
        try {
            if (lane === "native") await driver.value({ op: "start", timeoutMs: 5000 })
            const seek = lane === "source" ? await bus.original.send({ command: "seek", value: 3.0000005 }, "early") : await driver.value({ op: "command", command: "seek", value: 3.0000005 })
            if (lane === "source") assert.deepEqual(bus.original.calls.replies[0], ["MAIN", { channel: "SPOTIFY_COMMAND", data: true }, "early"])
            else assert.equal(seek.reply, true)
            await waitEvent(bus, "Get")
            for (const [command, value, name] of [
                ["pause", undefined, "Pause"],
                ["setVolume", 0.625, "Set"]
            ]) {
                if (lane === "source") await bus.original.send({ command, value })
                else {
                    const item = await driver.value({ op: "command", command, ...(value === undefined ? {} : { value }) })
                    assert.equal(await driver.value({ op: "finishCommand", id: item.id }), "sent")
                }
                await waitEvent(bus, name)
            }
            assert.deepEqual(
                values(bus).map((e) => e.name),
                ["Get", "Pause", "Set"],
                "a held metadata reply must not serialize independent commands"
            )
            if (lane === "native") {
                const active = await driver.value({ op: "status" })
                assert.equal(active.commandPids.length, 1)
                assert.ok(fs.existsSync(`/proc/${active.commandPids[0]}`))
            }
            bus.peer.child.stdin.write("release\n")
            await waitEvent(bus, "SetPosition")
            if (lane === "source") await bus.settle()
            else assert.equal(await driver.value({ op: "finishCommand", id: seek.id }), "sent")
            assert.deepEqual(values(bus).at(-1), { name: "SetPosition", args: ["/fixture/track_01", Math.round(3.0000005 * 1e6)] })
        } finally {
            await cleanup(driver.finish(), bus.close())
        }
    }
})

test("source seek string-only extraction, missing metadata, errors and unsafe-path capability boundary are explicit", linux, async () => {
    for (const [profile, expected] of [
        ["objectPath", "missingTrackId"],
        ["missing", "missingTrackId"],
        ["fail", { sourceProcessFailure: { serviceUnknown: false } }],
        ["unsafe", "spotify_command_unsupported"]
    ]) {
        const bus = await commandBus(profile),
            driver = new Driver(bus.env)
        try {
            await driver.value({ op: "start" })
            await bus.original.send({ command: "seek", value: 2 })
            await waitEvent(bus, "Get")
            await bus.settle()
            if (profile !== "unsafe") assert.equal(bus.original.calls.commands.length, 1, "actual source does not run SetPosition")
            const after = bus.peer.lines.length
            const item = await driver.value({ op: "command", command: "seek", value: 2 })
            await waitEvent(bus, "Get", after)
            const finished = await driver.request({ op: "finishCommand", id: item.id })
            if (profile === "unsafe") assert.equal(finished.error.code, expected)
            else assert.deepEqual(finished.value, expected)
            assert.equal(values(bus).filter((e) => e.name === "SetPosition").length, 0)
        } finally {
            await cleanup(driver.finish(), bus.close())
        }
    }
})

test("four real retained commands share admission, dropped receipts keep effects and explicit cancel/close actually reap", linux, async () => {
    for (const action of ["release", "cancel", "close", "deadline"]) {
        const bus = await commandBus("hold"),
            driver = new Driver(bus.env)
        try {
            await driver.value({ op: "start", timeoutMs: action === "deadline" ? 2000 : 5000 })
            const ids = []
            for (let n = 0; n < 4; n++) ids.push((await driver.value({ op: "command", command: "seek", value: n + 1 })).id)
            const active = await observed(driver, (s) => s.commandPids.length === 4)
            await bus.peer.wait(() => values(bus).length === 4)
            assert.equal((await driver.request({ op: "command", command: "pause" })).error.code, "spotify_busy")
            assert.equal((await driver.request({ op: "begin" })).error.code, "spotify_busy")
            await driver.value({ op: "dropCommand", id: ids[0] })
            assert.equal((await driver.value({ op: "status" })).retained, 4)
            if (action === "release") bus.peer.child.stdin.write("release\n")
            if (action === "cancel") {
                for (const id of ids.slice(1)) await driver.value({ op: "cancelCommand", id })
                // Dropped public receipt did not cancel the first genuine command.
                await observed(driver, (s) => s.commandPids.length === 1)
                bus.peer.child.stdin.write("release\n")
            }
            if (action === "close") await driver.value({ op: "close" })
            for (const id of ids.slice(1)) {
                const finished = await driver.request({ op: "finishCommand", id })
                if (action === "release") assert.equal(finished.value, "sent")
                else assert.equal(finished.error.code, action === "deadline" ? "spotify_timeout" : "spotify_cancelled")
            }
            const idle = await observed(driver, (s) => s.retained === 0 && s.commandPids.length === 0 && s.commandsCompleted === 4)
            assert.equal(idle.spawned, idle.reaped)
            for (const pid of active.commandPids) assert.equal(fs.existsSync(`/proc/${pid}`), false)
            if (action === "release") {
                await bus.peer.wait(() => values(bus).filter((e) => e.name === "SetPosition").length === 4)
                assert.equal(values(bus).filter((e) => e.name === "SetPosition").length, 4, "dropped receipt's accepted effect still completes")
            }
            if (action === "cancel") assert.equal(values(bus).filter((e) => e.name === "SetPosition").length, 1)
            await driver.value({ op: "close" })
            assert.equal((await driver.value({ op: "status" })).drained, true)
        } finally {
            await cleanup(driver.finish(), bus.close())
        }
    }
})

test("known unavailable native executable/session cannot return an accepted true; actual missing service stays a completion outcome", linux, async () => {
    const bus = await commandBus(null)
    try {
        const driver = new Driver(bus.env)
        try {
            await driver.value({ op: "start" })
            await bus.original.send({ command: "seek", value: 1 }, "missing-service")
            await bus.settle()
            assert.equal(bus.original.calls.errors, 0, "actual source suppresses ServiceUnknown")
            assert.deepEqual(bus.original.calls.replies, [["MAIN", { channel: "SPOTIFY_COMMAND", data: true }, "missing-service"]])
            const accepted = await driver.value({ op: "command", command: "seek", value: 1 })
            assert.equal(accepted.reply, true, "known tool/session is available; future remote absence is not early success")
            assert.deepEqual(await driver.value({ op: "finishCommand", id: accepted.id }), { sourceProcessFailure: { serviceUnknown: true } })
            const completed = await observed(driver, (s) => s.retained === 0)
            assert.equal(completed.commandFailures, 1)
            assert.equal(completed.lastCommandError.code, "spotify_service_unknown")
        } finally {
            await driver.finish()
        }
        for (const mode of ["binary", "session"]) {
            const env = { ...bus.env }
            if (mode === "binary") env.FREESHOW_SPOTIFY_DBUS_SEND = "/nonexistent-freeshow-command-binary"
            else delete env.DBUS_SESSION_BUS_ADDRESS
            const absent = new Driver(env)
            try {
                assert.equal((await absent.value({ op: "start" })).state, "unavailable")
                const result = await absent.request({ op: "command", command: "pause" })
                assert.equal(result.ok, false)
                assert.equal(result.error.code, mode === "binary" ? "spotify_binary_unavailable" : "spotify_session_unavailable")
                assert.equal((await absent.value({ op: "status" })).commands, 0)
            } finally {
                await absent.finish()
            }
        }
    } finally {
        await bus.close()
    }
})
