import assert from "node:assert/strict"
import { createHash } from "node:crypto"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import ts from "typescript"

const root = fileURLToPath(new URL("../../", import.meta.url))
const source = fs.readFileSync(path.join(root, "src/frontend/rust/coverBridge.ts"), "utf8")
const compiled = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS }, reportDiagnostics: true })
assert.equal(compiled.diagnostics.length, 0)
function load() {
    // Each load models a new WebView module lifetime. Helpers within one load
    // share the real exported module's quota, sequence, seals and terminal flag.
    const exports = {}
    new Function("exports", "require", compiled.outputText)(exports, (name) => assert.fail(`unexpected runtime import ${name}`))
    return exports
}
const context = (libraryGeneration = 3) => ({ runtimeId: "runtime-音楽", libraryGeneration, dataPath: `/selected/🎵/${libraryGeneration}` })
const id = (n) => `native-cover-${n.toString(16).padStart(32, "0")}`
const tick = () => new Promise((resolve) => setImmediate(resolve))
function deferred() {
    let resolve, reject
    const promise = new Promise((a, b) => {
        resolve = a
        reject = b
    })
    return { promise, resolve, reject }
}
function publication(n = 1, ctx = context(), folder = `${ctx.dataPath}/Audio`) {
    return { event: { channel: "MAIN", message: { channel: "ALERT", data: `Error: Could not create folder at: ${folder}!` }, context: { ...ctx } }, id: id(n), sequence: String(n), folder }
}
function harness({ create = load().createCoverBridge, ref = { value: context() }, timeoutMs = 40, deliver, complete, report } = {}) {
    const delivered = [],
        completions = [],
        errors = [],
        order = []
    const bridge = create({
        getContext: () => ref.value,
        timeoutMs,
        deliver(message, listenerId) {
            order.push("deliver")
            delivered.push({ message, listenerId })
            deliver?.(message, bridge)
        },
        complete(ctx, nonce, accepted) {
            const work = deferred()
            order.push("complete")
            completions.push({ ...work, context: ctx, id: nonce, accepted })
            return complete ? complete(completions.at(-1), bridge) : work.promise
        },
        report(error) {
            errors.push(error)
            report?.(error, bridge)
        }
    })
    return { bridge, ref, delivered, completions, errors, order }
}
async function settle(h) {
    for (const work of h.completions) work.resolve()
    await tick()
}

test("the actual exported helper strictly type-checks against the current RuntimeContext declaration", () => {
    const file = path.join(root, "src/frontend/rust/coverBridge.ts"),
        stub = path.join(root, "src/frontend/rust/cover-bridge-context.d.ts")
    const bridge = ts.createSourceFile("bridge.ts", fs.readFileSync(path.join(root, "src/frontend/rust/bridge.ts"), "utf8"), ts.ScriptTarget.Latest, true)
    const declaration = bridge.statements.find((node) => ts.isTypeAliasDeclaration(node) && node.name.text === "RuntimeContext")
    assert.ok(declaration)
    const options = { strict: true, noEmit: true, skipLibCheck: true, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022, moduleResolution: ts.ModuleResolutionKind.NodeJs, types: [] }
    const host = ts.createCompilerHost(options),
        original = host.getSourceFile.bind(host)
    host.getSourceFile = (name, ...args) => (name === stub ? ts.createSourceFile(stub, declaration.getText(bridge), options.target, true) : original(name, ...args))
    host.resolveModuleNames = (names) => names.map((name) => (name === "./bridge" ? { resolvedFileName: stub, extension: ts.Extension.Dts } : undefined))
    const diagnostics = ts.getPreEmitDiagnostics(ts.createProgram([file], options, host))
    assert.deepEqual(
        diagnostics.map((entry) => ts.flattenDiagnosticMessageText(entry.messageText, "\n")),
        []
    )
})

test("actual immutable makeDir failure supplies the exact source message, original void and isolated callback shape", async (t) => {
    const original = spawnSync("git", ["show", "2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/utils/files.ts"], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 })
    assert.equal(original.status, 0, original.stderr)
    assert.equal(createHash("sha256").update(original.stdout).digest("hex"), "98e563c55026d2dbbdd217ee9b94962ed7fc8b82d893795355bd7dc0f54fc5de")
    const ast = ts.createSourceFile("files.ts", original.stdout, ts.ScriptTarget.Latest, true)
    const declaration = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "makeDir")
    const javascript = ts.transpileModule(declaration.getText(ast).replace(/^export /, "") + ";return makeDir", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-cover-source-"))
    t.after(() => fs.rmSync(temp, { recursive: true, force: true }))
    const folder = path.join(temp, "日本語\\literal", "Audio")
    fs.mkdirSync(path.dirname(folder), { recursive: true })
    fs.writeFileSync(folder, "unchanged actual regular-file blocker")
    const sourceEvents = []
    const makeDir = new Function("fs", "sendToMain", "ToMain", "console", javascript)(fs, (channel, data) => sourceEvents.push({ channel, data }), { ALERT: "ALERT" }, { error() {} })
    assert.equal(makeDir(folder), undefined)
    assert.equal(sourceEvents.length, 1)
    const h = harness(),
        input = publication(1, h.ref.value, folder)
    input.event.message = sourceEvents[0]
    input.event.listenerId = undefined
    assert.equal(h.bridge.publish(input), undefined)
    assert.deepEqual(h.order, ["deliver"], "source callback is synchronous; private completion enters later")
    assert.deepEqual(h.delivered, [{ message: sourceEvents[0], listenerId: undefined }])
    assert.notEqual(h.delivered[0].message, sourceEvents[0], "private event objects are not forwarded by reference")
    assert.deepEqual(Object.keys(h.delivered[0].message), ["channel", "data"])
    await tick()
    assert.equal(h.completions.length, 1)
    assert.equal(h.completions[0].accepted, true)
    assert.deepEqual(h.completions[0].context, context())
    assert.equal(Object.isFrozen(h.completions[0].context), true)
    assert.equal(fs.readFileSync(folder, "utf8"), "unchanged actual regular-file blocker")
    assert.equal(h.errors.length, 0)
    await settle(h)
    await h.bridge.close()
})

test("Unicode, literal Linux backslashes, Windows separators and exact UTF8 maximum retain diagnostic spelling without path normalization", async () => {
    const h = harness()
    const maximum = "/" + "🎵".repeat(8191) + "abc"
    assert.equal(Buffer.byteLength(maximum), 32768)
    for (const [index, [ctx, folder]] of [
        [context(), "/literal\\Linux/日本語/Audio"],
        [context(), "C:\\原文\\🎵\\Audio"],
        [context(), "\\\\host\\share\\Audio"],
        [{ ...context(), dataPath: maximum }, maximum + "/Audio"]
    ].entries()) {
        h.ref.value = ctx
        const input = publication(index + 1, ctx, folder)
        assert.equal(h.bridge.publish(input), undefined)
        await tick()
        assert.equal(h.delivered.at(-1).message.data, `Error: Could not create folder at: ${folder}!`)
        assert.equal(h.completions.at(-1).accepted, true)
        assert.equal(Buffer.byteLength(input.folder) <= 32774, true)
        await settle(h)
    }
    assert.equal(h.errors.length, 0)
    await h.bridge.close()
})

test("wrong source kinds, extra private fields, listener IDs and malformed folder messages negatively complete without consuming sequence", async () => {
    const h = harness()
    const mutations = [
        (p) => (p.event.channel = "REMOTE"),
        (p) => (p.event.message.channel = "AUDIO_METADATA"),
        (p) => (p.event.message.channel = "FFMPEG_DOWNLOAD"),
        (p) => (p.event.listenerId = "source-listener"),
        (p) => (p.event.listenerId = null),
        (p) => (p.alertKind = "folderCreationFailed"),
        (p) => (p.event.nativeCover = { id: p.id }),
        (p) => (p.event.message.id = p.id),
        (p) => (p.event.message.accepted = true),
        (p) => delete p.event.message.data,
        (p) => (p.event.message.data = "This does not exist!"),
        (p) => (p.event.message.data += "extra"),
        (p) => (p.folder += "/child"),
        (p) => (p.event.message = []),
        (p) => (p.folder = "Audio")
    ]
    for (let index = 0; index < mutations.length; index++) {
        const input = publication(1)
        input.id = id(index + 1)
        mutations[index](input)
        assert.equal(h.bridge.publish(input), undefined)
        await tick()
        assert.equal(h.completions.at(-1).accepted, false)
        assert.equal(h.bridge.status().sequence, "0")
        await settle(h)
    }
    assert.equal(h.delivered.length, 0)
    assert.equal(h.completions.length, mutations.length)
    assert.equal(h.errors.length, mutations.length)
    h.bridge.publish(publication(1))
    await tick()
    assert.equal(h.completions.at(-1).accepted, true, "a corrected native retry can still consume the uncommitted sequence")
    await settle(h)
    await h.bridge.close()
})

test("byte budgets, NUL and lone surrogates reject before source delivery and never manufacture path authority", async () => {
    const h = harness()
    for (const [index, folder] of ["/" + "x".repeat(32769) + "/Audio", "/" + "🎵".repeat(8193) + "/Audio", "/bad\0/Audio", "/bad\ud800/Audio", "/bad\udc00/Audio"].entries()) {
        h.bridge.publish(publication(index + 1, context(), folder))
        await tick()
        assert.equal(h.completions.at(-1).accepted, false)
        await settle(h)
    }
    const oversized = publication(6)
    oversized.event.message.data = "x".repeat(32815)
    h.bridge.publish(oversized)
    await tick()
    assert.equal(h.completions.at(-1).accepted, false)
    assert.equal(h.delivered.length, 0)
    assert.equal(h.bridge.status().sequence, "0")
    await settle(h)
    await h.bridge.close()
})

test("public events, malformed private identity and foreign full contexts never adopt a context or invoke completion", async () => {
    const h = harness()
    for (const input of [
        undefined,
        null,
        { channel: "ALERT", data: "Error: Could not create folder at: /Audio!" },
        { channel: "MAIN", message: { channel: "ALERT", data: "source only" } },
        { ...publication(), id: "native-cover-" + "A".repeat(32) },
        { ...publication(), id: "native-now-playing-" + "0".repeat(32) },
        publication(1, { ...context(), runtimeId: "other-runtime" }),
        publication(1, { ...context(), dataPath: "/same-generation-other-path" }),
        publication(1, context(4)),
        publication(1, { ...context(), libraryGeneration: Number.MAX_SAFE_INTEGER + 1 }),
        publication(1, { ...context(), dataPath: "x".repeat(32769) }),
        publication(1, { ...context(), runtimeId: "x".repeat(257) })
    ])
        h.bridge.publish(input)
    h.ref.value = undefined
    h.bridge.publish(publication())
    await tick()
    assert.equal(h.delivered.length, 0)
    assert.equal(h.completions.length, 0)
    assert.equal(h.bridge.status().completionSlots, 0)
    assert.equal(h.bridge.status().sequence, "0")
    assert.ok(h.errors.length >= 13)
    await h.bridge.close()
})

test("accessors, prototypes, non-enumerable fields and symbols are rejected without invoking getters", async () => {
    const h = harness()
    let getters = 0
    for (let n = 1; n <= 5; n++) {
        const input = publication(n)
        if (n === 1) Object.defineProperty(input, "sequence", { enumerable: true, get: () => ++getters })
        if (n === 2) Object.defineProperty(input.event.message, "data", { enumerable: true, get: () => ++getters })
        if (n === 3) Object.setPrototypeOf(input.event.message, { untrusted: true })
        if (n === 4) Object.defineProperty(input, "folder", { value: input.folder, enumerable: false })
        if (n === 5) input.event.message[Symbol("private")] = true
        h.bridge.publish(input)
        await tick()
        assert.equal(h.completions.at(-1).accepted, false)
        await settle(h)
    }
    assert.equal(getters, 0)
    assert.equal(h.delivered.length, 0)
    assert.equal(h.bridge.status().sequence, "0")
    await h.bridge.close()
})

test("positive canonical u64 ordering survives context changes while replay and retained ID duplication never send a second completion", async () => {
    const h = harness()
    for (const sequence of ["0", "01", "-1", "+1", "1.0", "18446744073709551616", "1".repeat(1000), 1, null]) {
        const input = publication(1)
        input.sequence = sequence
        h.bridge.publish(input)
        await tick()
        assert.equal(h.completions.at(-1).accepted, false)
        assert.equal(h.bridge.status().sequence, "0")
        await settle(h)
    }
    const first = publication(10)
    h.bridge.publish(first)
    const duplicateId = publication(11)
    duplicateId.id = first.id
    h.bridge.publish(duplicateId)
    h.bridge.publish(publication(9))
    await tick()
    const count = h.completions.length
    assert.equal(h.delivered.length, 1)
    assert.equal(h.bridge.status().sequence, "10")
    await settle(h)
    h.ref.value = context(4)
    h.bridge.publish(publication(10, context(4)))
    await tick()
    assert.equal(h.completions.length, count, "retirement/context change cannot reset the sequence")
    const last = publication(12, context(4))
    last.sequence = "18446744073709551615"
    h.bridge.publish(last)
    await tick()
    assert.equal(h.bridge.status().sequence, "18446744073709551615")
    assert.equal(h.delivered.length, 2)
    await settle(h)
    await h.bridge.close()
})

test("callback failure consumes attempted source delivery but negatively completes; actual native completion rejection is reported and settles its lease", async () => {
    const callbackError = new Error("source callback threw after partial effects"),
        completeError = new Error("real native completion failed")
    const h = harness({
        deliver: () => {
            throw callbackError
        }
    })
    const input = publication(1)
    h.bridge.publish(input)
    h.bridge.publish(input)
    await tick()
    assert.equal(h.delivered.length, 1)
    assert.equal(h.completions.length, 1)
    assert.equal(h.completions[0].accepted, false)
    assert.equal(h.errors.filter((error) => error === callbackError).length, 1)
    h.completions[0].reject(completeError)
    await tick()
    assert.equal(h.errors.filter((error) => error === completeError).length, 1)
    assert.equal(h.bridge.status().completionSlots, 0)
    await h.bridge.close() // JS promise settlement only, not native producer success.
})

test("frozen completion context and payload survive callback mutations with no private fields or listener leaking to source", async () => {
    const original = context()
    const h = harness({
        deliver(message) {
            original.runtimeId = "mutated"
            original.dataPath = "/mutated"
            message.data = "source receiver changed its copy"
        }
    })
    h.ref.value = original
    const input = publication(1, original)
    h.bridge.publish(input)
    input.id = id(2)
    input.event.context.dataPath = "/also-mutated"
    input.folder = "/changed/Audio"
    await tick()
    assert.deepEqual(h.completions[0].context, context())
    assert.equal(h.completions[0].id, id(1))
    assert.equal(h.completions[0].accepted, true)
    assert.equal(input.event.message.data, `Error: Could not create folder at: ${context().dataPath}/Audio!`)
    assert.deepEqual(Object.keys(h.delivered[0].message), ["channel", "data"])
    assert.equal(h.delivered[0].listenerId, undefined)
    await settle(h)
    await h.bridge.close()
})

test("four lost responses remain globally charged across contexts, a fifth invokes nothing, and retry enters only after real settlement", async () => {
    const h = harness()
    for (let n = 1; n <= 4; n++) h.bridge.publish(publication(n))
    assert.equal(h.bridge.status().completionSlots, 4)
    assert.equal(h.bridge.status().enteredCompletions, 0)
    h.ref.value = context(4)
    h.bridge.publish(publication(5, context(4)))
    assert.equal(h.errors.at(-1).code, "cover_completion_busy")
    await tick()
    assert.equal(h.completions.length, 4)
    assert.equal(h.delivered.length, 4)
    h.completions[0].resolve()
    await tick()
    assert.equal(h.bridge.status().completionSlots, 3)
    h.bridge.publish(publication(5, context(4)))
    await tick()
    assert.equal(h.completions.length, 5)
    assert.equal(h.delivered.length, 5)
    assert.equal(h.bridge.status().completionSlots, 4)
    await settle(h)
    await h.bridge.close()
})

test("reentrant source delivery reserves all four slots before callbacks and rejects the fifth without completing it; released retry is valid", async () => {
    let next = 2
    const h = harness({
        deliver(_message, bridge) {
            if (next <= 5) bridge.publish(publication(next++))
        }
    })
    h.bridge.publish(publication(1))
    assert.equal(h.delivered.length, 4)
    assert.equal(h.bridge.status().completionSlots, 4)
    assert.equal(h.bridge.status().sequence, "4")
    assert.equal(h.errors.at(-1).code, "cover_completion_busy")
    await tick()
    assert.deepEqual(
        h.completions.map((entry) => entry.id),
        [1, 2, 3, 4].map(id)
    )
    assert.ok(h.completions.every((entry) => entry.accepted))
    h.completions[0].resolve()
    await tick()
    h.bridge.publish(publication(5))
    await tick()
    assert.equal(h.delivered.length, 5)
    assert.equal(h.completions.at(-1).id, id(5))
    await settle(h)
    await h.bridge.close()
})

test("retirement seals current admission synchronously; timeout retries keep actual old work while new context completions stay outside the scope", async () => {
    const h = harness({ timeoutMs: 20 }),
        old = context()
    for (let n = 1; n <= 3; n++) h.bridge.publish(publication(n))
    const first = h.bridge.retire(old)
    assert.equal(first, h.bridge.retire(old), "one matching observer is reused")
    h.bridge.publish(publication(4))
    assert.equal(h.errors.at(-1).code, "cover_closed")
    await tick()
    assert.equal(h.completions.length, 3)
    await assert.rejects(first, { code: "cover_retirement_timeout" })
    assert.equal(h.bridge.status().completionSlots, 3)
    assert.equal(h.bridge.status().sealedCurrent, true)
    h.ref.value = context(4)
    h.bridge.publish(publication(4, context(4)))
    await tick()
    const retry = h.bridge.retire(old)
    for (const entry of h.completions.slice(0, 3)) entry.resolve()
    await retry
    assert.equal(h.bridge.status().completionSlots, 1)
    assert.equal(h.bridge.status().sealedCurrent, false, "old-context retirement did not seal or adopt the new current context")
    h.bridge.publish(publication(5, old))
    await tick()
    assert.equal(h.completions.length, 4, "stale context never creates a new completion")
    await settle(h)
    await h.bridge.close()
})

test("terminal close invoked inside source delivery retains the not-yet-entered completion through timeout and bounded retry", async () => {
    let closing
    const h = harness({
        timeoutMs: 20,
        deliver(_message, bridge) {
            closing = bridge.close()
        }
    })
    h.bridge.publish(publication(1))
    assert.equal(h.bridge.status().closed, true)
    assert.equal(h.bridge.status().completionSlots, 1)
    assert.equal(h.bridge.status().enteredCompletions, 0)
    h.ref.value = undefined
    await tick()
    assert.equal(h.completions[0].accepted, true)
    await assert.rejects(closing, { code: "cover_retirement_timeout" })
    assert.equal(h.bridge.status().completionSlots, 1)
    const retry = h.bridge.close()
    assert.equal(retry, h.bridge.close())
    h.bridge.publish(publication(2))
    assert.equal(h.errors.at(-1).code, "cover_closed")
    h.completions[0].resolve()
    await retry
    assert.equal(h.bridge.status().completionSlots, 0)
    assert.equal(h.bridge.status().terminalWait, false)
})

test("discarding a timed-out observer does not abandon completions or produce an unhandled rejection", async () => {
    const h = harness({ timeoutMs: 10 })
    h.bridge.publish(publication(1))
    void h.bridge.retire(context())
    await new Promise((resolve) => setTimeout(resolve, 20))
    assert.equal(h.bridge.status().scopedWait, false)
    assert.equal(h.bridge.status().completionSlots, 1)
    assert.equal(h.bridge.status().sealedCurrent, true)
    const retry = h.bridge.retire(context())
    h.completions[0].resolve()
    await retry
    assert.equal(h.bridge.status().completionSlots, 0)
    await h.bridge.close()
})

test("helper replacement cannot bypass shared quota, sequence, current seal or terminal close", async () => {
    const module = load(),
        ref = { value: context() }
    const a = harness({ create: module.createCoverBridge, ref }),
        b = harness({ create: module.createCoverBridge, ref })
    for (let n = 1; n <= 4; n++) a.bridge.publish(publication(n))
    b.bridge.publish(publication(5))
    await tick()
    assert.equal(a.completions.length, 4)
    assert.equal(b.completions.length, 0)
    assert.equal(b.errors.at(-1).code, "cover_completion_busy")
    const old = context()
    const retired = a.bridge.retire(old)
    b.bridge.publish(publication(5))
    assert.equal(b.errors.at(-1).code, "cover_closed")
    await settle(a)
    await retired
    b.bridge.publish(publication(5))
    assert.equal(b.errors.at(-1).code, "cover_closed")
    ref.value = context(4)
    b.bridge.publish(publication(4, context(4)))
    assert.equal(b.errors.at(-1).code, "cover_replay")
    b.bridge.publish(publication(5, context(4)))
    await tick()
    assert.equal(b.completions.length, 1)
    const closed = a.bridge.close()
    b.bridge.publish(publication(6, context(4)))
    assert.equal(b.errors.at(-1).code, "cover_closed")
    await settle(b)
    await closed
    const replacement = harness({ create: module.createCoverBridge, ref })
    replacement.bridge.publish(publication(7, context(4)))
    assert.equal(replacement.errors.at(-1).code, "cover_closed")
    assert.equal(replacement.bridge.status().sequence, "5")
})

test("foreign retire never seals current admission, overlapping scoped waits are bounded, and completion reentrancy remains charged", async () => {
    let reenter = true
    const h = harness({
        complete(entry, bridge) {
            if (reenter) {
                reenter = false
                bridge.publish(publication(2, context(4)))
            }
            return entry.promise
        }
    })
    h.bridge.publish(publication(1))
    h.ref.value = context(4)
    await tick()
    assert.equal(h.completions.length, 2)
    const oldWait = h.bridge.retire(context())
    await assert.rejects(h.bridge.retire(context(5)), { code: "cover_wait_busy" })
    h.bridge.publish(publication(3, context(4)))
    await tick()
    assert.equal(h.completions.length, 3)
    h.completions[0].resolve()
    await oldWait
    assert.equal(h.bridge.status().completionSlots, 2)
    await settle(h)
    await h.bridge.close()
})

test("invalid observer deadlines cannot create unbounded timers", () => {
    const { createCoverBridge } = load()
    for (const timeoutMs of [0, -1, 0.5, NaN, Infinity, 60001]) assert.throws(() => createCoverBridge({ timeoutMs, getContext: () => context(), deliver() {}, report() {}, complete: async () => {} }), { code: "cover_timeout" })
})
