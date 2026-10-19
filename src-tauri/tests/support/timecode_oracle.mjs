// Actual immutable source modules, actual private original LTC addon, and a
// controlled JZZ port boundary. No hardware, audio device or network destination.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { createHash } from "node:crypto"
import { createRequire } from "node:module"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import ts from "typescript"
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../.."),
    revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
let input = ""
for await (const chunk of process.stdin) {
    input += chunk
    assert.ok(input.length <= 1024 * 1024)
}
const request = JSON.parse(input),
    events = [],
    trace = [],
    logs = [],
    sourceHashes = {},
    loaded = new Map()
assert.ok(request.actions.length <= 128)
const require = createRequire(import.meta.url)
let wrapper
if (request.addon) {
    assert.ok(path.isAbsolute(request.addon))
    const addon = require(request.addon),
        module = { exports: {} }
    new vm.Script(`(function(require,exports,module){${fs.readFileSync(require.resolve("libltc-wrapper"), "utf8")}})`).runInNewContext()(
        (id) => {
            assert.equal(id, "../build/Release/ltc-native")
            return addon
        },
        module.exports,
        module
    )
    wrapper = module.exports
}
const ports = []
function jzz() {
    return {
        openMidiIn(selector) {
            return open("in", selector)
        },
        openMidiOut(selector) {
            return open("out", selector)
        }
    }
}
function open(direction, selector) {
    const id = ports.length
    const port = {
        direction,
        id,
        callback: null,
        name() {
            return direction === "in" ? "Input 日本語" : "Output 🎹"
        },
        connect(callback) {
            port.callback = callback
            return Promise.resolve()
        },
        close() {
            trace.push(["close", direction, port.name()])
        },
        send(bytes) {
            trace.push(["send", port.name(), bytes])
        }
    }
    ports.push(port)
    trace.push(["open", direction, port.name()])
    return {
        or(message) {
            assert.equal(message, direction === "in" ? "Cannot open MIDI In" : "Cannot open MIDI Out")
            return Promise.resolve(port)
        }
    }
}
const clock = { now: 1000 }
class Clock extends Date {
    static now() {
        return clock.now
    }
}
function load(file) {
    if (loaded.has(file)) return loaded.get(file).exports
    const result = spawnSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 })
    assert.equal(result.status, 0, result.stderr)
    const source = result.stdout.replace(/\r\n/g, "\n")
    sourceHashes[file] = createHash("sha256").update(source).digest("hex")
    const module = { exports: {} }
    loaded.set(file, module)
    const code = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, esModuleInterop: true } }).outputText
    const localRequire = (id) => {
        if (id === "jzz") return jzz
        if (id === "libltc-wrapper") {
            if (!wrapper) throw new Error("actual original addon unavailable")
            return wrapper
        }
        if (id === "../../types/IPC/Main") return { Main: { TIMECODE_VALUE: "TIMECODE_VALUE", TIMECODE_STATUS: "TIMECODE_STATUS", TIMECODE_AUDIO_DATA: "TIMECODE_AUDIO_DATA" } }
        if (id === "../../types/IPC/ToMain") return { ToMain: { ALERT: "ALERT" } }
        if (id === "../IPC/main")
            return {
                sendMain(channel, data) {
                    events.push({ channel, ...(Buffer.isBuffer(data) ? { hex: data.toString("hex") } : { data }) })
                },
                sendToMain(channel, data) {
                    events.push({ channel, data })
                }
            }
        if (id === "..") return { isWindows: false }
        if (["./MTC", "./LTC", "./timecode"].includes(id)) return load(`src/electron/timecode/${id.slice(2)}.ts`)
        throw new Error(`unexpected original import ${id}`)
    }
    new vm.Script(`(function(require,exports,module){${code}})`).runInNewContext({
        Buffer,
        Date: Clock,
        console: {
            info() {},
            log(data) {
                logs.push(data === undefined ? { undefined: true } : data)
            },
            error(...data) {
                logs.push({ error: data.map(String) })
            }
        }
    })(localRequire, module.exports, module)
    return module.exports
}
const api = load("src/electron/timecode/timecode.ts")
const number = (n) => (n?.kind === "NaN" ? NaN : n?.kind === "PositiveInfinity" ? Infinity : n?.kind === "NegativeInfinity" ? -Infinity : n?.kind === "NegativeZero" ? -0 : n?.kind === "Finite" ? n.value : n)
for (const a of request.actions) {
    switch (a.op) {
        case "start":
            api.timecodeStart(a.data)
            break
        case "value":
            api.updateTimecodeValue(number(a.value))
            break
        case "audio": {
            const bytes = Buffer.from(a.hex, "hex")
            assert.ok(bytes.length <= 65536)
            api.processAudioData({ mode: "LTC", buffer: bytes })
            break
        }
        case "feedLast": {
            const event = [...events].reverse().find((e) => e.hex)
            assert.ok(event)
            api.processAudioData({ mode: "LTC", buffer: Buffer.from(event.hex, "hex") })
            break
        }
        case "status":
            logs.push(a.undefined ? { undefined: true } : a.data)
            break
        case "emit":
            clock.now = a.now || 1000
            ports[a.port || 0].callback(a.bytes)
            break
        case "stop":
            api.timecodeStop()
            break
        default:
            assert.fail(a.op)
    }
    await new Promise((r) => setImmediate(r))
}
api.timecodeStop()
assert.ok(events.length <= 512)
process.stdout.write(JSON.stringify({ revision, sourceHashes, node: process.version, events, trace, logs }))
