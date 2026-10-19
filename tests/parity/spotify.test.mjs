import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import test from "node:test"
import { cleanup, Driver, privateBus, restore, sourceOracle } from "./spotify-oracle.mjs"

const clean = (value) => JSON.parse(JSON.stringify(value))
const metadata = 'string "xesam:title" variant string "Song"\nstring "xesam:artist" variant array [ string "First" string "Second" ]\nstring "mpris:length" variant uint64 120000000\n'
test("immutable combined source parser matches native including own undefined and IEEE values", async () => {
    const driver = new Driver()
    try {
        for (const text of [
            "",
            metadata,
            metadata + 'variant string "Playing"\nint64 3000000\ndouble 0.5',
            metadata.replace("uint64", "int64") + "int64 3\ndouble -0.5",
            metadata.replace('"Song"', '"Playing title"') + 'string "Paused"\nint64 -3',
            metadata.replace('"Song"', '""'),
            metadata + `int64 ${"9".repeat(512)}\ndouble .`,
            metadata + "double 0.5.9",
            metadata + "double 1e-8",
            metadata.replaceAll(" ", "\u00a0"),
            metadata.replaceAll(" ", "\u0085"),
            metadata + "éint64 71",
            metadata + "_int64 71"
        ]) {
            const source = sourceOracle((_, callback) => callback(null, text))
            const expected = await source.get()
            const actual = restore((await driver.value({ op: "parse", text })).wire)
            assert.deepEqual(clean(actual), clean(expected))
            if (expected) {
                assert.deepEqual(Object.keys(actual), Object.keys(expected))
                for (const key of Object.keys(expected)) assert.ok(Object.is(actual[key], expected[key]), key)
            }
        }
    } finally {
        await driver.finish()
    }
})

test("explicit Linux capability absence never becomes a successful source null", async () => {
    const env = { ...process.env, FREESHOW_SPOTIFY_DBUS_SEND: path.resolve("nonexistent-dbus-send") }
    delete env.DBUS_SESSION_BUS_ADDRESS
    const driver = new Driver(env)
    try {
        const capability = await driver.value({ op: "start" })
        assert.equal(capability.state, "unavailable")
        const { id } = await driver.value({ op: "begin" })
        const result = await driver.request({ op: "finish", id })
        assert.equal(result.ok, false)
        assert.equal(result.error.code, process.platform === "linux" ? "spotify_session_unavailable" : "spotify_unsupported")
        await driver.value({ op: "close" })
        const status = await driver.value({ op: "status" })
        assert.equal(status.joined, true)
        assert.equal(status.spawned, 0)
    } finally {
        await driver.finish()
    }
})

test("real private MPRIS queries preserve four-property source order and all observed quirks", { skip: process.platform !== "linux" ? "Linux-only real Unix D-Bus backend; pure source parser remains covered" : false }, async () => {
    for (const profile of ["ordinary", "signedLength", "negative", "playingTitle", "quoted", "missingTitle", "emptyTitle", "missingArt", "failPlayback"]) {
        const bus = await privateBus(profile),
            driver = new Driver(bus.env)
        try {
            assert.equal((await driver.value({ op: "start" })).state, "available")
            const expected = await bus.original.get()
            const sourceReads = bus.provider.lines.filter((line) => line.startsWith("GET "))
            const { id } = await driver.value({ op: "begin" })
            const result = await driver.value({ op: "finish", id })
            const actual = restore(result.wire)
            assert.deepEqual(clean(actual), clean(expected), profile)
            if (expected) {
                assert.deepEqual(Object.keys(actual), Object.keys(expected), profile)
                assert.equal(Object.hasOwn(actual, "albumArt"), true)
                assert.equal(actual.albumArt, expected.albumArt)
            }
            const nativeReads = bus.provider.lines.filter((line) => line.startsWith("GET ")).slice(sourceReads.length)
            assert.deepEqual(nativeReads, sourceReads, profile)
            assert.deepEqual(nativeReads, profile === "failPlayback" ? ["GET Metadata", "GET PlaybackStatus"] : ["GET Metadata", "GET PlaybackStatus", "GET Position", "GET Volume"])
            if (profile === "signedLength") {
                assert.equal(actual.durationSec, 0)
                assert.equal(actual.positionSec, 120)
            }
            if (profile === "playingTitle") assert.equal(actual.isPlaying, true)
            if (profile === "missingArt") assert.equal(actual.albumArt, undefined)
            if (profile === "failPlayback") assert.equal(result.nullReason, "processFailure")
            await driver.value({ op: "close" })
            const status = await driver.value({ op: "status" })
            assert.equal(status.joined, true)
            assert.equal(status.spawned, status.reaped)
            assert.equal(status.activePid, 0)
            assert.equal(status.retained, 0)
        } finally {
            await cleanup(driver.finish(), bus.close())
        }
    }
})

test("real missing service is source-null while a missing executable is unavailable", { skip: process.platform !== "linux" }, async () => {
    const bus = await privateBus(),
        driver = new Driver(bus.env)
    try {
        assert.equal(await bus.original.get(), null)
        assert.equal(bus.original.calls.errors, 0)
        await driver.value({ op: "start" })
        const { id } = await driver.value({ op: "begin" })
        const result = await driver.value({ op: "finish", id })
        assert.equal(result.wire.data, null)
        assert.equal(result.nullReason, "serviceUnknown")
        assert.equal((await driver.value({ op: "status" })).spawned, 2, "help plus only first failed property")
    } finally {
        await cleanup(driver.finish(), bus.close())
    }
    const missing = new Driver({ ...bus.env, FREESHOW_SPOTIFY_DBUS_SEND: "/nonexistent-dbus-send" })
    try {
        assert.equal((await missing.value({ op: "start" })).state, "unavailable")
        const { id } = await missing.value({ op: "begin" })
        assert.equal((await missing.request({ op: "finish", id })).error.code, "spotify_binary_unavailable")
    } finally {
        await missing.finish()
    }
})

test("actual held property witnesses bounded queue, caller drop, cancellation and child waits", { skip: process.platform !== "linux" }, async () => {
    const bus = await privateBus("holdMetadata"),
        driver = new Driver(bus.env)
    try {
        await driver.value({ op: "start", timeoutMs: 5000 })
        const ids = []
        ids.push((await driver.value({ op: "begin" })).id)
        await bus.provider.wait((line) => line === "GET Metadata")
        const active = await driver.value({ op: "status" })
        assert.notEqual(active.activePid, 0)
        assert.ok(fs.existsSync(`/proc/${active.activePid}`))
        for (let index = 0; index < 3; index++) ids.push((await driver.value({ op: "begin" })).id)
        assert.equal((await driver.request({ op: "begin" })).error.code, "spotify_busy")
        await driver.value({ op: "drop", id: ids[1] })
        assert.equal((await driver.value({ op: "status" })).retained, 4, "queued drop remains charged until actual owner settles it")
        bus.provider.child.stdin.write("release\n")
        for (const id of [ids[0], ids[2], ids[3]]) assert.ok(restore((await driver.value({ op: "finish", id })).wire))
        const idle = await driver.value({ op: "status" })
        assert.equal(idle.completed, 4)
        assert.equal(idle.spawned, idle.reaped)
        assert.equal(idle.retained, 0)
        assert.equal(bus.provider.lines.filter((line) => line === "GET Metadata").length, 3, "dropped queued job never entered DBus")
    } finally {
        await cleanup(driver.finish(), bus.close())
    }
    for (const action of ["cancel", "drop", "deadline", "close"]) {
        const bus = await privateBus("holdPosition"),
            driver = new Driver(bus.env)
        try {
            await driver.value({ op: "start", timeoutMs: 800 })
            const { id } = await driver.value({ op: "begin" })
            await bus.provider.wait((line) => line === "GET Position")
            const active = await driver.value({ op: "status" })
            assert.ok(fs.existsSync(`/proc/${active.activePid}`))
            if (action === "close") await driver.value({ op: "close" })
            else if (action !== "deadline") await driver.value({ op: action, id })
            if (action !== "drop") {
                const result = await driver.request({ op: "finish", id })
                assert.equal(result.ok, false)
                assert.equal(result.error.code, action === "close" ? "spotify_shutdown" : action === "deadline" ? "spotify_timeout" : "spotify_cancelled")
            }
            await driver.value({ op: "close" })
            const done = await driver.value({ op: "status" })
            assert.equal(done.joined, true)
            assert.equal(done.spawned, done.reaped)
            assert.equal(done.activePid, 0)
            assert.equal(done.retained, 0)
            assert.equal(fs.existsSync(`/proc/${active.activePid}`), false, "actual child PID disappeared after real wait")
        } finally {
            await cleanup(driver.finish(), bus.close())
        }
    }
})

test("actual native executable disappearance and restoration retry only on a new request", { skip: process.platform !== "linux" }, async () => {
    const bus = await privateBus("ordinary")
    const folder = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-spotify-tool-"))
    const executable = path.join(folder, "dbus-send-日本語")
    fs.symlinkSync("/usr/bin/dbus-send", executable)
    const driver = new Driver({ ...bus.env, FREESHOW_SPOTIFY_DBUS_SEND: executable })
    try {
        assert.equal((await driver.value({ op: "start" })).state, "available")
        fs.unlinkSync(executable)
        const first = await driver.value({ op: "begin" })
        assert.equal((await driver.request({ op: "finish", id: first.id })).error.code, "spotify_binary_unavailable")
        const failed = await driver.value({ op: "status" })
        assert.equal(failed.capability.state, "unavailable")
        assert.equal(failed.spawned, 1, "failed spawn added no live process")
        assert.equal(bus.provider.lines.filter((line) => line.startsWith("GET ")).length, 0)
        fs.symlinkSync("/usr/bin/dbus-send", executable)
        const next = await driver.value({ op: "begin" })
        assert.ok(restore((await driver.value({ op: "finish", id: next.id })).wire))
        const done = await driver.value({ op: "status" })
        assert.equal(done.capability.state, "available")
        assert.equal(done.spawned, 6, "initial help, request-driven reprobe, four actual queries")
        assert.equal(done.spawned, done.reaped)
    } finally {
        await cleanup(driver.finish(), bus.close())
        fs.rmSync(folder, { recursive: true, force: true })
    }
})
