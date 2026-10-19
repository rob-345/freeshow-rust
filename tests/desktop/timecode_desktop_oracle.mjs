// One actual immutable source VM/addon for the COMPLETE ordered desktop series.
// Explicit sourceOnly is diagnostic reference generation, never native parity.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { createHash } from "node:crypto"
import { createRequire } from "node:module"
import { execFileSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import ts from "typescript"

const root = fileURLToPath(new URL("../../", import.meta.url)),
    revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975",
    require = createRequire(import.meta.url)
const sourceHashes = {
    "src/electron/timecode/timecode.ts": "18506a959deaf9053f5bae84235963e2dbf3779688d5a3d646a4995a65820a79",
    "src/electron/timecode/LTC.ts": "f4cbf2b19e2f6a8e22967c2409c2acca72b03e1f027dcead85dec318f12aaacf",
    "src/electron/timecode/MTC.ts": "d0127f1074686d169e7deeae708a2f4ef281bac2f028f2d75b0bc400a7ebab2c",
    "src/electron/IPC/responsesMain.ts": "70372b48b698f74f239b82f92e48dde69880a7c4d78af1a9f0e1723313fb57a3"
}
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex")
const source = (file) => {
    const text = execFileSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", maxBuffer: 1024 * 1024, timeout: 10000 }).replace(/\r\n/g, "\n")
    assert.equal(sha(text), sourceHashes[file], "actual immutable source " + file)
    return text
}
function bytes(text) {
    assert.equal(typeof text, "string")
    assert.ok(text.length <= Math.ceil(65536 / 3) * 4)
    const result = Buffer.from(text, "base64")
    assert.ok(result.length <= 65536)
    assert.equal(result.toString("base64"), text, "canonical exact byte evidence")
    return result
}
let input = ""
for await (const chunk of process.stdin) {
    input += chunk
    assert.ok(Buffer.byteLength(input) <= 2 * 1024 * 1024, "desktop receipt input bound")
}
const fixture = JSON.parse(input)
assert.ok(fixture.sourceOnly === undefined || typeof fixture.sourceOnly === "boolean", "sourceOnly must be explicit boolean")
assert.equal(process.versions.node, "22.21.1", "pinned original source runtime")
assert.equal(process.platform, "linux", "actual supported original bundled ABI evidence")
assert.equal(process.arch, "x64")
assert.ok(Array.isArray(fixture.phases) && fixture.phases.length >= 1 && fixture.phases.length <= 8)
const addon = path.resolve(fixture.addon || process.env.FREESHOW_LTC_ORACLE_ADDON || path.join(root, "target/ltc-original/ltc-native.node")),
    build = JSON.parse(fs.readFileSync(path.join(path.dirname(addon), "receipt.json"), "utf8"))
assert.equal(build.node, "v22.21.1")
assert.equal(build.platform, "linux")
assert.equal(build.architecture, "x64")
assert.equal(sha(fs.readFileSync(addon)), build.addon, "private actual original NAPI addon hash")
const originals = path.join(root, "node_modules/libltc-wrapper")
const pinnedInputs = {
    "libltc/bin/libltc.so.11": "f02c75f982fe01d16f345875d2b09404b8b326bb847f2b5456616f6d0227d09f",
    "libltc/include/ltc.h": "a2d5666fb300ffd05cbfada622029f2647dc26ba3ff03a834e3d5bd7ff77dff7",
    "src/ltcnative.c": "eda296ab102d1af0676cd593be7b704fb69d68bc4be33ac78b462b20e26e9036",
    "src/ltcnative.h": "2b38910857fb4375d5b396ffa4ebc5c8a0da4b8e8e08f9c97a5f23578dd46b30",
    "dist/binding.js": "11add39f95a974541316c242cb1558ae1863646e1abddeb258282e2e758e4717"
}
assert.deepEqual(build.inputs, pinnedInputs, "receipt is for exact original addon inputs")
for (const [file, hash] of Object.entries(build.inputs)) assert.equal(sha(fs.readFileSync(path.join(originals, file))), hash, "unchanged original wrapper/C/header/library " + file)
const expectedLibrary = "f02c75f982fe01d16f345875d2b09404b8b326bb847f2b5456616f6d0227d09f"
assert.equal(build.inputs["libltc/bin/libltc.so.11"], expectedLibrary)
assert.equal(sha(fs.readFileSync(path.join(path.dirname(addon), "libltc.so.11"))), expectedLibrary)
const native = require(addon),
    module = { exports: {} }
const binding = fs.readFileSync(require.resolve("libltc-wrapper"), "utf8")
assert.equal(sha(binding), "11add39f95a974541316c242cb1558ae1863646e1abddeb258282e2e758e4717")
new vm.Script(`(function(require,exports,module){${binding}})`).runInNewContext()(
    (name) => {
        assert.equal(name, "../build/Release/ltc-native")
        return native
    },
    module.exports,
    module
)
const mapped = [
    ...new Set(
        fs
            .readFileSync("/proc/self/maps", "utf8")
            .split("\n")
            .map((line) => line.match(/\s(\/[^\n]*libltc\.so\.11)$/)?.[1])
            .filter(Boolean)
            .map((file) => fs.realpathSync(file))
    )
]
assert.equal(mapped.length, 1, "one actual mapped original LTC library")
assert.equal(sha(fs.readFileSync(mapped[0])), expectedLibrary)
const events = [],
    constructors = [],
    logs = [],
    loaded = new Map(),
    Main = Object.fromEntries(["TIMECODE_START", "TIMECODE_STOP", "TIMECODE_VALUE", "TIMECODE_STATUS", "TIMECODE_AUDIO_DATA"].map((name) => [name, name]))
let currentPhase
const wrapper = { ...module.exports }
for (const kind of ["LTCEncoder", "LTCDecoder"])
    wrapper[kind] = new Proxy(module.exports[kind], {
        construct(target, args) {
            const result = Reflect.construct(target, args)
            constructors.push({ phase: currentPhase, kind, args })
            return result
        }
    })
const context = vm.createContext({
    Buffer,
    console: {
        info() {},
        error(...data) {
            logs.push({ phase: currentPhase, error: data.map(String) })
        },
        log(value) {
            logs.push({ phase: currentPhase, ...(value === undefined ? { undefined: true } : { data: value }) })
        }
    }
})
function load(file) {
    if (loaded.has(file)) return loaded.get(file).exports
    const module = { exports: {} }
    loaded.set(file, module)
    const code = ts.transpileModule(source(file), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, esModuleInterop: true } }).outputText
    const localRequire = (name) => {
        if (name === "libltc-wrapper") return wrapper
        if (name === "jzz")
            return () => {
                throw new Error("Physical MTC is deliberately outside controlled LTC oracle")
            }
        if (name === "..") return { isWindows: false }
        if (name === "../../types/IPC/Main") return { Main }
        if (name === "../../types/IPC/ToMain") return { ToMain: { ALERT: "ALERT" } }
        if (name === "../IPC/main")
            return {
                sendMain(channel, data) {
                    assert.ok(events.length < 512)
                    events.push({ channel, ...(Buffer.isBuffer(data) ? { base64: data.toString("base64") } : { data }) })
                },
                sendToMain(channel, data) {
                    events.push({ channel, data })
                }
            }
        if (["./timecode", "./LTC", "./MTC"].includes(name)) return load(`src/electron/timecode/${name.slice(2)}.ts`)
        throw new Error("Unexpected immutable source import " + name)
    }
    new vm.Script(`(function(require,exports,module){${code}})`, { filename: file }).runInContext(context)(localRequire, module.exports, module)
    return module.exports
}
const api = load("src/electron/timecode/timecode.ts")
// Execute the exact original five handler properties, including STATUS console
// logging and return values, without evaluating unrelated Electron initialization.
const responseFile = "src/electron/IPC/responsesMain.ts",
    ast = ts.createSourceFile(responseFile, source(responseFile), ts.ScriptTarget.Latest, true)
const declaration = ast.statements.flatMap((node) => (ts.isVariableStatement(node) ? [...node.declarationList.declarations] : [])).find((node) => node.name.getText(ast) === "mainResponses")
assert.ok(declaration && ts.isObjectLiteralExpression(declaration.initializer))
const properties = declaration.initializer.properties.filter((node) => Object.keys(Main).some((name) => node.name?.getText(ast) === `[Main.${name}]`)).map((node) => node.getText(ast))
assert.equal(properties.length, 5)
Object.assign(context, { Main, timecodeStart: api.timecodeStart, timecodeStop: api.timecodeStop, updateTimecodeValue: api.updateTimecodeValue, processAudioData: api.processAudioData })
const handlers = new vm.Script(ts.transpileModule(`globalThis.handlers={${properties.join(",")}}`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).runInContext(context)
const results = []
let nativeRuntimeId
for (const phase of fixture.phases) {
    const script = phase.script || phase
    assert.equal(typeof script.phase, "string")
    assert.ok(script.phase.length > 0 && script.phase.length <= 64)
    assert.ok(Array.isArray(script.actions) && script.actions.length >= 1 && script.actions.length <= 64)
    assert.equal(script.actions[0].op, "stop")
    assert.equal(script.actions.at(-1).op, "stop", "only actual explicit source STOP defines a phase boundary")
    currentPhase = script.phase
    const begin = events.length,
        constructorBegin = constructors.length,
        logBegin = logs.length
    let fedBytes = 0
    for (const action of script.actions) {
        let name, data
        switch (action.op) {
            case "start":
                assert.ok(["send", "receive"].includes(action.data?.type))
                assert.equal(action.data.mode, "LTC")
                assert.ok([25, 29.97].includes(action.data.framerate))
                name = "TIMECODE_START"
                data = action.data
                break
            case "stop":
                name = "TIMECODE_STOP"
                break
            case "value":
                assert.ok(Number.isFinite(action.value) && action.value >= 0 && action.value < 86400000)
                name = "TIMECODE_VALUE"
                data = action.value
                break
            case "audio": {
                const buffer = bytes(action.base64)
                fedBytes += buffer.length
                name = "TIMECODE_AUDIO_DATA"
                data = { mode: "LTC", buffer }
                break
            }
            case "rawNoop":
                name = "TIMECODE_AUDIO_DATA"
                data = new Uint8Array(bytes(action.base64))
                break
            case "status":
                assert.equal(action.ownUndefined, true)
                name = "TIMECODE_STATUS"
                break
            default:
                assert.fail("Unknown desktop timecode source action " + action.op)
        }
        assert.equal(handlers[name](data), undefined, "all original timecode commands remain void")
        await new Promise((resolve) => setImmediate(resolve))
    }
    const expected = events.slice(begin),
        actual = phase.events
    assert.ok(expected.length > 0 && expected.length <= 64)
    if (!fixture.sourceOnly) {
        assert.equal(typeof phase.context?.runtimeId, "string")
        assert.ok(phase.context.runtimeId.length > 0 && phase.context.runtimeId.length <= 256)
        assert.ok(Number.isSafeInteger(phase.context.libraryGeneration) && phase.context.libraryGeneration >= 0)
        assert.equal(typeof phase.context.dataPath, "string")
        assert.ok(phase.context.dataPath.length > 0 && phase.context.dataPath.length <= 32768)
        nativeRuntimeId ??= phase.context.runtimeId
        assert.equal(phase.context.runtimeId, nativeRuntimeId, "all phases model one actual native process")
        assert.deepEqual(phase.context, phase.current, "one frozen context per native phase")
        assert.equal(phase.phase, script.phase)
        assert.deepEqual(phase.sourceAcks, [])
        assert.deepEqual(phase.errors, [])
        assert.equal(phase.omittedErrors, 0)
        assert.ok(Array.isArray(actual) && actual.length === expected.length, "same actual source event count")
        for (const [index, event] of actual.entries()) {
            assert.equal(event.listenerIdUndefined, true)
            assert.equal(event.dataOwn, true)
            assert.deepEqual(event.messageKeys, ["channel", "data"])
            if (event.base64 !== undefined) {
                bytes(event.base64)
                assert.equal(event.bytes?.uint8Array, true)
                assert.equal(event.bytes.constructor, "Uint8Array")
                assert.equal(event.bytes.byteOffset, 0)
                assert.equal(event.bytes.backingByteLength, event.bytes.byteLength)
                assert.equal(bytes(event.base64).length, event.bytes.byteLength)
            }
            assert.deepEqual({ channel: event.channel, ...(event.base64 !== undefined ? { base64: event.base64 } : { data: event.data }) }, expected[index], "exact original callback bytes/value/order " + index)
        }
        assert.ok(phase.feedEvidence.length > 0 && phase.feedEvidence.length <= 16)
        assert.equal(
            phase.feedEvidence.reduce((sum, item) => sum + item.byteLength, 0),
            fedBytes
        )
        for (const item of phase.feedEvidence) {
            assert.equal(item.byteOffset, 7)
            assert.equal(item.backingByteLength, item.byteLength + 14)
            assert.equal(item.poisonedPrefix, true)
            assert.equal(item.reusedAfterSend, true)
            assert.equal(bytes(item.expectedBase64).length, item.byteLength)
        }
        for (const state of [phase.stopCheckpoint, phase.after]) {
            assert.equal(state.closing, false)
            assert.equal(state.terminal, null)
            assert.equal(state.retiring, false)
            assert.equal(state.portOwners, 0)
            assert.equal(state.retained, 0)
            assert.equal(state.pendingDelivery, false)
            assert.equal(state.queuedPublications, 0)
            assert.equal(state.asyncEffects, 0)
            assert.equal(state.syncEffect, false)
            assert.equal(state.router.currentMode, "LTC")
            assert.equal(state.router.encoder, false)
            assert.equal(state.router.decoder, false)
            assert.equal(state.router.ownedListeners, 0)
            assert.deepEqual(state.router.receivers, [])
            assert.equal(state.ltc.loaded, true)
            assert.equal(state.ltc.admitted, 0)
            assert.equal(state.ltc.encoder, false)
            assert.equal(state.ltc.decoder, false)
            assert.equal(state.ltc.encoderCreated, state.ltc.encoderFreed)
            assert.equal(state.ltc.decoderCreated, state.ltc.decoderFreed)
            assert.equal(state.ltc.workerFinished, false)
            assert.equal(state.ltc.closing, false)
        }
        assert.equal(phase.after.ltc.encoderCreated - (phase.before.ltc?.encoderCreated || 0), constructors.slice(constructorBegin).filter((item) => item.kind === "LTCEncoder").length, "actual encoder construction/reset count")
        assert.equal(phase.after.ltc.decoderCreated - (phase.before.ltc?.decoderCreated || 0), constructors.slice(constructorBegin).filter((item) => item.kind === "LTCDecoder").length, "actual decoder construction count")
        assert.equal(phase.microphoneCaptureVerified, false)
        assert.equal(phase.playbackVerified, false)
        assert.equal(phase.terminalShutdownVerified, false)
        assert.equal(phase.mtc.exercised, false)
    }
    assert.ok(!expected.some((event) => event.channel === "ALERT" || event.channel === "TIMECODE_STATUS"), "no false constructor availability/status success")
    results.push({ phase: currentPhase, nativeCompared: !fixture.sourceOnly, sourceEvents: expected, fedBytes, constructors: constructors.slice(constructorBegin), logs: logs.slice(logBegin), rawAudioBytesMatched: !fixture.sourceOnly, physicalCaptureVerified: false, playbackVerified: false })
}
const output = { revision, sourceHashes, node: process.version, addonSha256: build.addon, mappedLibrarySha256: expectedLibrary, oneSourceVm: true, phases: results, constructorCount: constructors.length, sourceOnly: fixture.sourceOnly === true }
process.stdout.write(JSON.stringify(output) + "\n")
