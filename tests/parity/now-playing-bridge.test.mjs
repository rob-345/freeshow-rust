import assert from "node:assert/strict"
import fs from "node:fs"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import ts from "typescript"

// Execute the actual exported TS helper and its actual wire/hydration dependencies.
const modules = new Map()
function load(name) {
    if (modules.has(name)) return modules.get(name)
    const source = fs.readFileSync(new URL(`../../src/frontend/rust/${name}.ts`, import.meta.url), "utf8")
    const compiled = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS }, reportDiagnostics: true })
    assert.equal(compiled.diagnostics.length, 0)
    const exported = {}
    modules.set(name, exported)
    new Function("exports", "require", compiled.outputText)(exported, (dependency) => {
        assert.ok(["./metadata", "./nowPlayingWire"].includes(dependency), `unexpected runtime import ${dependency}`)
        return load(dependency.slice(2))
    })
    return exported
}
const { createNowPlayingBridge } = load("nowPlayingBridge")
const context = (libraryGeneration = 3) => ({ runtimeId: "runtime-音楽", libraryGeneration, dataPath: `/library/🎵/${libraryGeneration}` })
const id = (n) => `native-now-playing-${n.toString(16).padStart(32, "0")}`
const tick = () => new Promise((resolve) => setImmediate(resolve))
function deferred() {
    let resolve, reject
    const promise = new Promise((a, b) => {
        resolve = a
        reject = b
    })
    return { promise, resolve, reject }
}
function harness({ timeoutMs = 100, deliver } = {}) {
    let current = context()
    const commands = [],
        completions = [],
        retirements = [],
        delivered = [],
        errors = [],
        order = []
    const bridge = createNowPlayingBridge({
        timeoutMs,
        getContext: () => current,
        transport: {
            command(body) {
                const pending = deferred()
                commands.push({ ...pending, body, frame: JSON.parse(new TextDecoder().decode(body)) })
                return pending.promise
            },
            complete(ctx, publicationId, accepted) {
                const pending = deferred()
                order.push("complete")
                completions.push({ ...pending, context: ctx, id: publicationId, accepted })
                return pending.promise
            },
            retire(ctx, terminal) {
                const pending = deferred()
                retirements.push({ ...pending, context: ctx, terminal })
                return pending.promise
            }
        },
        deliver(message) {
            order.push("deliver")
            delivered.push(message)
            deliver?.(message, bridge)
        },
        report(error) {
            errors.push(error)
        }
    })
    return {
        bridge,
        commands,
        completions,
        retirements,
        delivered,
        errors,
        order,
        setContext(value) {
            current = value
        },
        getContext: () => current
    }
}
function alert(n, ctx = context()) {
    return { event: { channel: "MAIN", message: { channel: "ALERT", data: "This does not exist!" }, context: { ...ctx } }, id: id(n), sequence: String(n), alertKind: "missingText" }
}
function folderAlert(n, folder, ctx = context()) {
    const publication = alert(n, ctx)
    publication.alertKind = "folderCreationFailed"
    publication.alertFolder = folder
    publication.event.message.data = `Error: Could not create folder at: ${folder}!`
    return publication
}
function metadata(n, ctx = context()) {
    return {
        event: {
            channel: "MAIN",
            context: { ...ctx },
            message: { channel: "AUDIO_METADATA", data: { filePath: "/native/音楽.mp3", metadata: { picture: [{ data: "AP8B", description: "表紙", type: null, format: "image/png" }], genre: [null], year: null, zero: 0, title: "Café" } } }
        },
        id: id(n),
        sequence: String(n),
        hydration: {
            undefinedPaths: ["/metadata/genre/0", "/metadata/picture/0/type"],
            specialNumbers: [
                { path: "/metadata/year", kind: "NaN" },
                { path: "/metadata/zero", kind: "NegativeZero" }
            ],
            byteBuffers: ["/metadata/picture/0/data"],
            objectKeys: [
                { path: "/metadata", keys: ["title", "year", "zero", "genre", "picture"] },
                { path: "/metadata/picture/0", keys: ["format", "type", "description", "data"] }
            ]
        }
    }
}
async function settle(list) {
    for (const work of list) work.resolve()
    await tick()
}

test("handle returns original void and snapshots raw IEEE duration, nested data and full context before deferred entry", async () => {
    const h = harness()
    const values = [61.25, 0, -0, NaN, Infinity, -Infinity, Number.MIN_VALUE, Number.MAX_VALUE]
    const tags = ["Finite", "Finite", "NegativeZero", "NaN", "PositiveInfinity", "NegativeInfinity", "Finite", "Finite"]
    for (let i = 0; i < values.length; i++) {
        const ctx = context(),
            data = { filePath: "/audio/音楽.wav", name: "Original", duration: values[i], unknownLang: ["Artist", "Title", "Album"], nested: { signed: -0 } }
        h.setContext(ctx)
        assert.equal(h.bridge.handle({ channel: "NOW_PLAYING", data }), undefined)
        data.duration = 7
        data.name = "changed"
        data.unknownLang.fill("changed")
        data.nested.signed = 8
        ctx.runtimeId = "changed"
        ctx.libraryGeneration++
        ctx.dataPath = "/changed"
        await tick()
        const entered = h.commands.at(-1),
            frame = entered.frame
        assert.ok(entered.body instanceof Uint8Array)
        assert.deepEqual(frame.context, context())
        assert.equal(frame.command.kind, "set")
        assert.equal(frame.command.data.name, "Original")
        assert.equal(frame.command.data.unknownLang[0], "Artist")
        assert.equal(frame.command.data.duration, null)
        assert.equal(frame.command.duration.kind, tags[i])
        if (tags[i] === "Finite") assert.ok(Object.is(frame.command.duration.value, values[i]))
        assert.ok(Object.is(frame.command.data.nested.signed, -0))
        entered.resolve()
        await tick()
    }
    for (const [channel, kind] of [
        ["NOW_PLAYING_UNSET", "unset"],
        ["OPEN_NOW_PLAYING", "open"]
    ]) {
        h.setContext(context())
        assert.equal(h.bridge.handle({ channel, data: { ignored: true } }), undefined)
        await tick()
        assert.deepEqual(h.commands.at(-1).frame.command, { kind })
        h.commands.at(-1).resolve()
        await tick()
    }
    assert.equal(h.delivered.length, 0, "native command completion is not an invented source ACK")
    assert.equal(h.errors.length, 0)
})

test("typed metadata restores raw callback values/order before original delivery, then issues a separate private ACK", async () => {
    const h = harness(),
        publication = metadata(1),
        original = publication.event.message
    assert.equal(h.bridge.publish(publication), undefined)
    assert.equal(h.delivered[0], original, "retained listener receives exactly original inner channel/data")
    assert.deepEqual(Object.keys(original), ["channel", "data"])
    const common = original.data.metadata
    assert.ok(Number.isNaN(common.year))
    assert.ok(Object.is(common.zero, -0))
    assert.equal(Object.hasOwn(common.genre, 0), true)
    assert.equal(common.genre[0], undefined)
    assert.equal(Object.hasOwn(common.picture[0], "type"), true)
    assert.equal(common.picture[0].type, undefined)
    assert.deepEqual(Object.keys(common), ["title", "year", "zero", "genre", "picture"])
    assert.deepEqual(Object.keys(common.picture[0]), ["format", "type", "description", "data"])
    assert.equal(common.picture[0].data.constructor, Uint8Array)
    assert.deepEqual([...common.picture[0].data], [0, 255, 1])
    assert.equal(h.completions.length, 0, "private native call enters asynchronously after retained callback")
    publication.event.context.dataPath = "/reused"
    await tick()
    assert.deepEqual(h.order, ["deliver", "complete"])
    assert.deepEqual(h.completions[0].context, context(), "ACK retains accepted full context")
    assert.equal(h.completions[0].id, id(1))
    assert.equal(h.completions[0].accepted, true)
    await settle(h.completions)
    assert.equal(h.delivered.length, 1)
})

test("stale full contexts and replayed sequences never hydrate, deliver or acknowledge again", async () => {
    const h = harness()
    for (const ctx of [
        { ...context(), runtimeId: "other" },
        { ...context(), libraryGeneration: 4 },
        { ...context(), dataPath: "/other" }
    ]) {
        const stale = metadata(1, ctx)
        h.bridge.publish(stale)
        assert.equal(stale.event.message.data.metadata.year, null)
    }
    const missing = metadata(1)
    delete missing.event.context
    h.bridge.publish(missing)
    h.setContext(undefined)
    h.bridge.publish(metadata(1))
    h.setContext(context())
    h.bridge.publish(alert(2))
    const replay = metadata(2)
    h.bridge.publish(replay)
    const older = metadata(1)
    h.bridge.publish(older)
    await tick()
    assert.equal(h.delivered.length, 1)
    assert.equal(h.completions.length, 1)
    assert.equal(replay.event.message.data.metadata.year, null)
    assert.equal(older.event.message.data.metadata.year, null)
    assert.equal(h.errors.length, 0)
    await settle(h.completions)
})

test("malformed callback/hydration rejects with private negative receipt and atomic metadata, without consuming sequence", async () => {
    const h = harness(),
        bad = metadata(1),
        before = structuredClone(bad.event.message.data),
        root = bad.event.message.data.metadata
    bad.hydration.byteBuffers.push("/metadata/picture/0/data")
    h.bridge.publish(bad)
    assert.deepEqual(bad.event.message.data, before)
    assert.equal(bad.event.message.data.metadata, root)
    assert.equal(h.delivered.length, 0)
    await tick()
    assert.equal(h.completions[0].accepted, false)
    assert.equal(h.completions[0].id, id(1))
    await settle(h.completions)
    h.bridge.publish(alert(1))
    await tick()
    assert.equal(h.delivered.length, 1, "failed restoration did not commit replay sequence")
    assert.equal(h.completions[1].accepted, true)
    await settle(h.completions)
    const unexpected = alert(2)
    unexpected.event.message.channel = "NOW_PLAYING_SUCCESS"
    h.bridge.publish(unexpected)
    await tick()
    assert.equal(h.completions[2].accepted, false)
    assert.equal(h.delivered.length, 1)
    await settle(h.completions)
    for (const modify of [(p) => (p.id = "renderer-selected-id"), (p) => (p.sequence = "0"), (p) => (p.sequence = "18446744073709551616"), (p) => (p.event.listenerId = "private-target"), (p) => (p.event.channel = "OUTPUT"), (p) => (p.event.message.extra = true)]) {
        const invalid = alert(3)
        modify(invalid)
        h.bridge.publish(invalid)
    }
    await tick()
    assert.equal(h.completions.length, 3, "invalid authority/schema never enters an ACK")
})

test("four command leases remain occupied through real settlement, independently of twenty completion leases", async () => {
    const h = harness()
    for (let n = 1; n <= 20; n++) {
        if (n <= 4) h.bridge.handle({ channel: "NOW_PLAYING_UNSET" })
        h.bridge.publish(alert(n))
    }
    h.bridge.handle({ channel: "OPEN_NOW_PLAYING" })
    h.bridge.publish(alert(21))
    await tick()
    assert.equal(h.commands.length, 4)
    assert.equal(h.completions.length, 20)
    assert.equal(h.delivered.length, 20)
    assert.deepEqual(
        h.errors.map((e) => e.code),
        ["now_playing_busy", "now_playing_completion_busy"]
    )
    h.commands[0].resolve()
    h.completions[0].resolve()
    h.bridge.handle({ channel: "OPEN_NOW_PLAYING" })
    h.bridge.publish(alert(21))
    assert.equal(h.delivered.length, 20, "merely resolving a promise does not synchronously release its lease")
    await tick()
    h.bridge.handle({ channel: "OPEN_NOW_PLAYING" })
    h.bridge.publish(alert(21))
    await tick()
    assert.equal(h.commands.length, 5)
    assert.equal(h.completions.length, 21)
    assert.equal(h.delivered.length, 21)
    await settle(h.commands)
    await settle(h.completions)
    assert.equal(h.bridge.status().commands, 0)
    assert.equal(h.bridge.status().completionSlots, 0)
})

test("reentrant callbacks reserve completion capacity and commit replay identity before invoking retained code", async () => {
    let h
    h = harness({
        deliver(_message, bridge) {
            const n = h.delivered.length
            bridge.publish(alert(n)) // replay from within the original callback
            if (n <= 20) bridge.publish(alert(n + 1))
        }
    })
    h.bridge.publish(alert(1))
    await tick()
    assert.equal(h.delivered.length, 20)
    assert.equal(h.completions.length, 20)
    assert.deepEqual(
        h.completions.map((v) => v.id),
        Array.from({ length: 20 }, (_, i) => id(20 - i))
    )
    assert.ok(h.completions.every((v) => v.accepted))
    assert.equal(h.errors[0].code, "now_playing_completion_busy")
    await settle(h.completions)
})

test("a throwing retained listener gets one negative private receipt and cannot re-deliver a committed sequence", async () => {
    const failure = new Error("retained listener failed"),
        h = harness({
            deliver() {
                throw failure
            }
        })
    h.bridge.publish(alert(1))
    h.bridge.publish(alert(1))
    await tick()
    assert.equal(h.delivered.length, 1)
    assert.equal(h.errors[0], failure)
    assert.equal(h.completions.length, 1)
    assert.equal(h.completions[0].accepted, false)
    await settle(h.completions)
})

test("retirement timeout/retry retains the exact entered native promise and rejects conflicting control", async () => {
    const h = harness(),
        old = context()
    h.setContext(context(4))
    const first = h.bridge.contextChanged(old),
        same = h.bridge.contextChanged(old)
    assert.equal(first, same, "concurrent observer shares one owned retirement")
    await assert.rejects(first, { code: "now_playing_retirement_timeout" })
    assert.equal(h.retirements.length, 1)
    await assert.rejects(h.bridge.contextChanged(context(4)), { code: "now_playing_control_busy" })
    const retry = h.bridge.contextChanged(old)
    await tick()
    assert.equal(h.retirements.length, 1, "timeout did not cancel or replace entered native work")
    h.retirements[0].resolve()
    await retry
    assert.equal(h.bridge.status().control, false)
})

test("retirement native receipt alone cannot drain entered command/completion promises; old-context scope excludes new work", async () => {
    const h = harness(),
        old = context(),
        next = context(4)
    h.bridge.handle({ channel: "NOW_PLAYING_UNSET" })
    h.bridge.publish(alert(1, old))
    await tick()
    h.setContext(next)
    h.bridge.handle({ channel: "OPEN_NOW_PLAYING" })
    h.bridge.publish(alert(2, next))
    await tick()
    const first = h.bridge.contextChanged(old)
    await tick()
    h.retirements[0].resolve()
    await assert.rejects(first, { code: "now_playing_retirement_timeout" })
    assert.equal(h.bridge.status().commands, 2)
    assert.equal(h.bridge.status().completions, 2)
    const retry = h.bridge.contextChanged(old)
    h.commands[0].resolve()
    h.completions[0].resolve()
    await retry
    assert.equal(h.retirements.length, 1)
    assert.equal(h.bridge.status().commands, 1)
    assert.equal(h.bridge.status().completions, 1)
    await settle(h.commands)
    await settle(h.completions)
})

test("settled native retirement failure can retry after actual error receipt", async () => {
    const h = harness(),
        failure = new Error("native cleanup failed")
    const first = h.bridge.contextChanged(context())
    await tick()
    h.retirements[0].reject(failure)
    await assert.rejects(first, (error) => error === failure)
    await tick()
    const retry = h.bridge.contextChanged(context())
    await tick()
    assert.equal(h.retirements.length, 2)
    h.retirements[1].resolve()
    await retry
    assert.equal(h.bridge.status().control, false)
})

test("close stops admission and retains every context's entered promises through timeout and terminal retry", async () => {
    const h = harness()
    h.bridge.handle({ channel: "NOW_PLAYING_UNSET" })
    h.bridge.publish(alert(1))
    await tick()
    h.setContext(context(4))
    h.bridge.handle({ channel: "OPEN_NOW_PLAYING" })
    await tick()
    const close = h.bridge.close()
    await tick()
    assert.equal(h.retirements[0].terminal, true)
    assert.deepEqual(h.retirements[0].context, context(4))
    h.bridge.handle({ channel: "NOW_PLAYING_UNSET" })
    h.bridge.publish(alert(2, context(4)))
    assert.equal(h.errors.at(-1).code, "now_playing_closed")
    h.retirements[0].resolve()
    await assert.rejects(close, { code: "now_playing_retirement_timeout" })
    assert.equal(h.bridge.status().commands, 2)
    assert.equal(h.bridge.status().completions, 1)
    const retry = h.bridge.close()
    h.commands[0].reject(new Error("old command canceled"))
    h.commands[1].resolve()
    h.completions[0].reject(new Error("old ACK canceled"))
    await retry
    assert.equal(h.retirements.length, 1)
    assert.equal(h.bridge.status().control, false)
    assert.equal(h.bridge.status().commands, 0)
    assert.equal(h.bridge.status().completionSlots, 0)
    assert.equal(h.delivered.length, 1)
    assert.deepEqual(
        h.errors.map((e) => e.code),
        ["now_playing_closed"],
        "closed owner suppresses late context-bound errors"
    )
})

test("transport command/completion errors release only settled leases and report only to the current live context", async () => {
    const h = harness(),
        commandError = new Error("command"),
        ackError = new Error("ACK")
    h.bridge.handle({ channel: "NOW_PLAYING_UNSET" })
    h.bridge.publish(alert(1))
    await tick()
    h.commands[0].reject(commandError)
    h.completions[0].reject(ackError)
    await tick()
    assert.deepEqual(h.errors, [commandError, ackError])
    h.bridge.handle({ channel: "OPEN_NOW_PLAYING" })
    h.bridge.publish(alert(2))
    await tick()
    h.setContext(context(4))
    h.commands[1].reject(new Error("stale"))
    h.completions[1].reject(new Error("stale ACK"))
    await tick()
    assert.equal(h.errors.length, 2)
    assert.equal(h.bridge.status().commands, 0)
    assert.equal(h.bridge.status().completionSlots, 0)
})

test("missing live context cannot make close report drained while previously entered native work remains pending", async () => {
    for (const origin of ["command", "publication"]) {
        const h = harness()
        if (origin === "command") h.bridge.handle({ channel: "NOW_PLAYING_UNSET" })
        else h.bridge.publish(metadata(1))
        await tick()
        h.setContext(undefined)
        let closed = false
        const close = h.bridge.close().then(() => {
            closed = true
        })
        await tick()
        try {
            assert.equal(h.retirements.length, 1, "terminal native retirement still uses captured authority")
            assert.deepEqual(h.retirements[0].context, context())
            assert.equal(h.retirements[0].terminal, true)
            h.retirements[0].resolve()
            await tick()
            assert.equal(closed, false, "absence of current context is not a receipt for previously entered work")
        } finally {
            // Always settle real controlled work, including when this regression fails.
            for (const pending of [...h.commands, ...h.completions, ...h.retirements]) pending.resolve()
            await close
        }
        assert.equal(h.bridge.status().commands, 0)
        assert.equal(h.bridge.status().completionSlots, 0)
    }
})

test("actual pinned makeDir error messages preserve native folder spelling across platforms and exact UTF8 boundary", async () => {
    const source = spawnSync("git", ["show", "2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/utils/files.ts"], { cwd: new URL("../../", import.meta.url), encoding: "utf8", timeout: 10000 })
    assert.equal(source.status, 0, source.stderr)
    const ast = ts.createSourceFile("files.ts", source.stdout, ts.ScriptTarget.Latest, true)
    const declaration = ast.statements.find((s) => ts.isFunctionDeclaration(s) && s.name?.text === "makeDir").getText(ast)
    const compiled = ts.transpileModule(declaration + ";return makeDir", { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
    const cases = [
        ["/library/音楽 🎵", "/library/音楽 🎵/Audio"],
        ["/literal\\Linux-name", "/literal\\Linux-name/Audio"],
        ["/names/with!bang\nline", "/names/with!bang\nline/Audio"],
        ["/", "/Audio"],
        ["C:\\Library\\音楽", "C:\\Library\\音楽\\Audio"],
        ["C:/Library", "C:/Library\\Audio"],
        ["\\\\server\\share\\Library", "\\\\server\\share\\Library\\Audio"],
        ["\\\\?\\C:\\Library", "\\\\?\\C:\\Library\\Audio"],
        ["/" + "é".repeat(16383) + "x", "/" + "é".repeat(16383) + "x/Audio"]
    ]
    assert.equal(Buffer.byteLength(cases.at(-1)[0]), 32768)
    assert.equal(Buffer.byteLength(cases.at(-1)[1]), 32774)
    for (const [dataPath, folder] of cases) {
        const ctx = { ...context(), dataPath },
            h = harness(),
            events = []
        h.setContext(ctx)
        const makeDir = new Function("exports", "fs", "sendToMain", "ToMain", "console", compiled)(
            {},
            {
                mkdirSync(path, options) {
                    assert.equal(path, folder)
                    assert.deepEqual(options, { recursive: true })
                    throw new Error("controlled filesystem failure")
                }
            },
            (channel, data) => events.push({ channel, data }),
            { ALERT: "ALERT" },
            { error() {} }
        )
        makeDir(folder)
        assert.equal(events.length, 1)
        const publication = folderAlert(1, folder, ctx)
        assert.deepEqual(publication.event.message, events[0], "message comes from actual source, not a copied reference algorithm")
        h.bridge.publish(publication)
        await tick()
        assert.equal(h.delivered.length, 1)
        assert.equal(h.delivered[0], publication.event.message)
        assert.equal(h.completions[0].accepted, true)
        assert.equal(h.errors.length, 0)
        await settle(h.completions)
    }
})

test("typed alerts reject wrong kinds, malformed native folder receipts and source message substitutions before delivery", async () => {
    const invalid = [
        (p) => delete p.alertKind,
        (p) => (p.alertKind = "other"),
        (p) => {
            p.alertKind = "missingText"
            delete p.alertFolder
        },
        (p) => delete p.alertFolder,
        (p) => (p.alertFolder = 42),
        (p) => {
            p.alertFolder = "/bad/Other"
            p.event.message.data = `Error: Could not create folder at: ${p.alertFolder}!`
        },
        (p) => {
            p.alertFolder = "/bad/Audio/"
            p.event.message.data = `Error: Could not create folder at: ${p.alertFolder}!`
        },
        ...["/bad\u0000/Audio", "/bad\ud800/Audio", "/bad\udc00/Audio", "/" + "x".repeat(32768) + "/Audio", "/" + "é".repeat(16384) + "/Audio"].map((folder) => (p) => {
            p.alertFolder = folder
            p.event.message.data = `Error: Could not create folder at: ${folder}!`
        }),
        (p) => (p.event.message.data = `Error: Could not create folder at: /another/Audio!`),
        (p) => (p.event.message.data += "extra"),
        (p) => (p.event.message.data = p.event.message.data.slice(0, -1)),
        (p) => (p.event.message.data = "x".repeat(32815)),
        (p) => (p.hydration = undefined)
    ]
    const h = harness()
    for (const modify of invalid) {
        const publication = folderAlert(1, "/library/Audio")
        modify(publication)
        h.bridge.publish(publication)
        await tick()
        assert.equal(h.delivered.length, 0)
        assert.equal(h.completions.at(-1).accepted, false)
        assert.equal(h.errors.at(-1).code, "now_playing_publication")
        await settle(h.completions)
    }
    for (const privateFields of [{ alertKind: undefined }, { alertFolder: undefined }, { alertKind: "folderCreationFailed", alertFolder: "/library/Audio" }]) {
        const publication = Object.assign(metadata(1), privateFields)
        h.bridge.publish(publication)
        await tick()
        assert.equal(publication.event.message.data.metadata.year, null, "forbidden alert fields reject before hydration")
        assert.equal(h.completions.at(-1).accepted, false)
        await settle(h.completions)
    }
    const missing = alert(1)
    missing.alertFolder = undefined
    h.bridge.publish(missing)
    await tick()
    assert.equal(h.completions.at(-1).accepted, false)
    await settle(h.completions)
    h.bridge.publish(alert(1))
    await tick()
    assert.equal(h.delivered.length, 1, "rejections did not commit source sequence")
    assert.equal(h.completions.at(-1).accepted, true)
    await settle(h.completions)
})

test("four pending source commands each deliver metadata plus four folder alerts in source order without leaking private fields or awaiting ACK", async () => {
    const h = harness(),
        expected = []
    for (let operation = 0; operation < 4; operation++) {
        h.bridge.handle({ channel: "NOW_PLAYING", data: { filePath: `/audio/${operation}.mp3`, name: "Title", unknownLang: ["Artist", "Title", "Album"], duration: 1 } })
        const first = operation * 5 + 1
        const publications = [metadata(first), ...Array.from({ length: 4 }, (_, i) => folderAlert(first + i + 1, `${context().dataPath}/Audio`))]
        for (const publication of publications) {
            expected.push(publication.event.message)
            h.bridge.publish(publication)
        }
    }
    await tick()
    assert.equal(h.commands.length, 4)
    assert.equal(h.completions.length, 20)
    assert.equal(h.bridge.status().commands, 4, "source effects did not require command promise completion")
    assert.equal(h.bridge.status().completionSlots, 20, "metadata ACK remains pending while following source alerts deliver")
    assert.deepEqual(
        h.delivered.map((message) => message.channel),
        Array.from({ length: 4 }, () => ["AUDIO_METADATA", "ALERT", "ALERT", "ALERT", "ALERT"]).flat()
    )
    for (let n = 0; n < 20; n++) {
        assert.equal(h.delivered[n], expected[n])
        assert.deepEqual(Object.keys(h.delivered[n]), ["channel", "data"])
        assert.deepEqual(Object.keys(h.delivered[n].data).includes("alertFolder"), false)
        assert.equal(h.completions[n].id, id(n + 1))
        assert.equal(h.completions[n].accepted, true)
    }
    h.bridge.publish(folderAlert(21, `${context().dataPath}/Audio`))
    await tick()
    assert.equal(h.delivered.length, 20)
    assert.equal(h.completions.length, 20)
    assert.equal(h.errors.at(-1).code, "now_playing_completion_busy")
    await settle(h.commands)
    await settle(h.completions)
})

test("twenty lost completion responses remain charged across context retirement and terminal timeout/retry until real settlement", async () => {
    const h = harness(),
        old = context(),
        next = context(4)
    for (let n = 1; n <= 20; n++) {
        if (n <= 4) h.bridge.handle({ channel: "NOW_PLAYING_UNSET" })
        h.bridge.publish(n % 5 === 1 ? metadata(n, old) : folderAlert(n, `${old.dataPath}/Audio`, old))
    }
    await tick()
    h.setContext(next)
    h.bridge.publish(alert(21, next))
    await tick()
    assert.equal(h.delivered.length, 20)
    assert.equal(h.completions.length, 20)
    const first = h.bridge.contextChanged(old)
    await tick()
    h.retirements[0].resolve()
    await assert.rejects(first, { code: "now_playing_retirement_timeout" })
    assert.equal(h.bridge.status().completionSlots, 20)
    const retry = h.bridge.contextChanged(old)
    for (const pending of h.commands) pending.resolve()
    h.completions[0].resolve()
    await tick()
    h.bridge.publish(alert(21, next))
    await tick()
    assert.equal(h.completions.length, 21)
    for (const pending of h.completions.slice(1, 20)) pending.resolve()
    await retry
    assert.equal(h.retirements.length, 1, "old lost native retirement was not replaced")
    assert.equal(h.bridge.status().completionSlots, 1, "new context ACK is outside old retirement scope")
    for (let n = 22; n <= 40; n++) h.bridge.publish(folderAlert(n, `${next.dataPath}/Audio`, next))
    await tick()
    assert.equal(h.bridge.status().completionSlots, 20)
    const close = h.bridge.close()
    await tick()
    assert.equal(h.retirements[1].terminal, true)
    h.bridge.publish(alert(41, next))
    h.bridge.handle({ channel: "OPEN_NOW_PLAYING" })
    assert.equal(h.errors.at(-1).code, "now_playing_closed")
    h.retirements[1].resolve()
    await assert.rejects(close, { code: "now_playing_retirement_timeout" })
    assert.equal(h.bridge.status().completionSlots, 20)
    const terminalRetry = h.bridge.close()
    for (const pending of h.completions) pending.resolve()
    await terminalRetry
    assert.equal(h.retirements.length, 2)
    assert.equal(h.delivered.length, 40)
    assert.equal(h.bridge.status().completionSlots, 0)
    assert.equal(h.bridge.status().control, false)
})
