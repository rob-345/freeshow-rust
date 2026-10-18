import assert from "node:assert/strict"
import { execFile, spawn } from "node:child_process"
import { createHash } from "node:crypto"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { createInterface } from "node:readline"
import { test } from "node:test"
import { fileURLToPath } from "node:url"
import { promisify } from "node:util"
import vm from "node:vm"
import ts from "typescript"

const root = fileURLToPath(new URL("../../", import.meta.url))
const binary = process.env.FREESHOW_ENCODERS_BIN || path.join(root, "target/debug/examples", process.platform === "win32" ? "encoders-parity.exe" : "encoders-parity")
const ffmpeg = process.env.FREESHOW_FFMPEG_PATH || "/usr/bin/ffmpeg"
const pause = (ms = 5) => new Promise((resolve) => setTimeout(resolve, ms))
async function until(read, ms = 4000) {
    const end = Date.now() + ms
    while (Date.now() < end) {
        const value = await read()
        if (value) return value
        await pause()
    }
    throw new Error("Encoder condition timed out")
}
function original({ executable = null, sourcePath = executable, mtimeMs = 0, platform = process.platform, arch = process.arch, device = "", cache, disableHardware = false, processMs } = {}) {
    const calls = [],
        writes = []
    let cacheBytes = cache
    const load = (filename, hash, require, injected = {}) => {
        const source = fs.readFileSync(path.join(root, filename), "utf8")
        assert.equal(createHash("sha256").update(source.replace(/\r\n/g, "\n")).digest("hex"), hash, "pinned encoder source changed")
        const context = vm.createContext({ exports: {}, require, process: { platform, arch, env: { FREESHOW_VAAPI_DEVICE: device } }, console: { warn() {}, info() {} }, ...injected })
        new vm.Script(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText, { filename }).runInContext(context, { timeout: 1000 })
        return context.exports
    }
    const profiles = load("src/electron/streaming/encoderProfiles.ts", "26e2923819f19b5f3c40992fbe0dd70a81fe2180e5d23bbf00f5a49ce1ac0787", () => {
        throw new Error("unexpected profile import")
    })
    function tracedExec() {
        throw new Error("promisify custom path expected")
    }
    tracedExec[promisify.custom] = async (bin, args, options) => {
        calls.push({ bin, args: Array.from(args), options: { ...options } })
        return promisify(execFile)(executable || bin, args, { ...options, ...(processMs ? { timeout: processMs } : {}) })
    }
    const detection = load("src/electron/streaming/encoderDetection.ts", "edde3d614ea3774b65ab31555a7be69d26c863f505afa02d8f2ceb20e373cb5b", (name) => {
        if (name === "child_process") return { execFile: tracedExec }
        if (name === "electron") return { app: { getPath: () => "/injected-unused-profile" } }
        if (name === "fs")
            return {
                statSync: () => ({ mtimeMs }),
                readFileSync: () => {
                    if (cacheBytes === undefined) throw new Error("missing cache")
                    return cacheBytes
                },
                writeFileSync: (_path, bytes) => {
                    writes.push(bytes)
                    cacheBytes = bytes
                }
            }
        if (name === "path") return path
        if (name === "util") return { promisify }
        if (name.includes("data/store")) return { config: { get: () => disableHardware } }
        if (name === "./encoderProfiles") return profiles
        if (name === "./ffmpegManager") return { resolveFfmpegPath: async () => sourcePath }
        throw new Error(`unexpected detection import ${name}`)
    })
    // These declarations are evaluated from the source itself for private helpers.
    const source = fs.readFileSync(path.join(root, "src/electron/streaming/encoderDetection.ts"), "utf8")
    const ast = ts.createSourceFile("encoderDetection.ts", source, ts.ScriptTarget.Latest, true)
    const node = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "firstErrorLine")
    const context = vm.createContext({})
    new vm.Script(ts.transpileModule(node.getText(ast) + ";globalThis.error=firstErrorLine", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).runInContext(context, { timeout: 1000 })
    return { profiles, detection, calls, writes, error: context.error }
}
function rust(limits = {}, env = {}) {
    const child = spawn(binary, [], { stdio: ["pipe", "pipe", "pipe"], env: { ...process.env, FREESHOW_ENCODER_LIMITS: JSON.stringify(limits), ...env } })
    const pending = new Map()
    let next = 0,
        stderr = ""
    child.stderr.on("data", (chunk) => (stderr += chunk))
    createInterface({ input: child.stdout }).on("line", (line) => {
        const value = JSON.parse(line),
            entry = pending.get(value.id)
        if (entry) {
            pending.delete(value.id)
            clearTimeout(entry.timer)
            entry.resolve(value)
        }
    })
    child.on("exit", (code) => {
        for (const entry of pending.values()) {
            clearTimeout(entry.timer)
            entry.reject(new Error(`Encoder child exited ${code}: ${stderr}`))
        }
    })
    const request = (input) =>
        new Promise((resolve, reject) => {
            const id = next++,
                timer = setTimeout(() => {
                    pending.delete(id)
                    reject(new Error(`Encoder request timeout ${JSON.stringify(input)} ${stderr}`))
                }, 40000)
            pending.set(id, { resolve, reject, timer })
            child.stdin.write(JSON.stringify({ ...input, id }) + "\n")
        })
    return {
        request,
        async value(input) {
            const response = await request(input)
            assert.equal(response.ok, true, JSON.stringify(response))
            return response.value
        },
        async close() {
            const closed = new Promise((resolve) => child.once("close", resolve))
            child.stdin.end()
            assert.equal(await closed, 0, stderr)
        }
    }
}
const clean = (value) => JSON.parse(JSON.stringify(value))
const baseOptions = { encoderId: "x264", inputWidth: 1920, inputHeight: 1080, outputWidth: 1920, outputHeight: 1080, fps: 30, bitrate: 4000, enableAudio: true }
function fixture(mode) {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-encoder-fixture-")),
        directory = path.join(root, "witness")
    fs.mkdirSync(directory)
    const executable = path.join(root, `encoders-fixture-${mode}${process.platform === "win32" ? ".exe" : ""}`)
    try {
        fs.linkSync(binary, executable)
    } catch {
        fs.copyFileSync(binary, executable)
    }
    return {
        root,
        directory,
        executable,
        input: { executable, sourcePath: "frozen-ffmpeg", mtimeMs: 0, platform: "linux", arch: "x64" },
        env: { FREESHOW_ENCODER_WITNESS_DIR: directory },
        pids: () => fs.readdirSync(directory).filter((name) => /^\d+$/.test(name)),
        release(pid) {
            fs.writeFileSync(path.join(directory, `release-${pid}`), "")
        },
        close: () => fs.rmSync(root, { recursive: true, force: true })
    }
}
test("actual profile order/table/platform filters and exact test/stream/relay commands", async () => {
    const native = rust()
    try {
        for (const device of ["", "/dev/dri/custom été", "literal:device,flags"]) {
            const source = original({ device }),
                ids = source.profiles.ENCODER_IDS
            assert.deepEqual(await native.value({ op: "profiles", device }), clean(Object.values(source.profiles.ENCODER_PROFILES)))
            for (const encoder of [...ids, "unknown"]) {
                for (const platform of ["linux", "win32", "darwin", "freebsd"]) assert.equal(await native.value({ op: "supported", encoder, platform }), source.profiles.isSupportedOnPlatform(encoder, platform))
                assert.deepEqual(await native.value({ op: "test_command", encoder, device }), clean(source.profiles.buildTestEncodeCommand(encoder)))
                for (const [bitrate, gop, mode] of [
                    [4001, 60, undefined],
                    [4001, 60, "cbr"],
                    [4001, 60, "vbr"],
                    [1.5, 0, "vbr"],
                    [-1.5, -0, "vbr"],
                    [1e21, 120, "vbr"]
                ])
                    assert.deepEqual(await native.value({ op: "args", encoder, bitrate, gop, mode }), clean(source.profiles.getProfile(encoder).args(bitrate, gop, mode)))
                for (const options of [
                    { ...baseOptions, encoderId: encoder },
                    { ...baseOptions, encoderId: encoder, enableAudio: false, inputWidth: 3840, inputHeight: 2160, fps: 29.97, rateControl: "vbr" },
                    { ...baseOptions, encoderId: encoder, sampleRate: 0 },
                    { ...baseOptions, encoderId: encoder, sampleRate: 44100 }
                ])
                    assert.deepEqual(await native.value({ op: "command", options, device }), clean(source.profiles.buildEncoderCommand(options)))
            }
        }
        for (const url of ["rtmp://controlled.invalid/app/key", "rtmps://controlled.invalid/a/été?x=1"]) {
            assert.deepEqual(await native.value({ op: "relay", url }), clean(original().profiles.buildRelayCommand(url)))
        }
    } finally {
        await native.close()
    }
})
test("listing parser uses original flags, codec boundaries, ordering and JS whitespace", async () => {
    const source = original(),
        native = rust()
    try {
        for (const stdout of ["", " V....D libx264rgb description\n A....D libx264 audio\n", "\uFEFFV....D h264_vaapi listed\r\n V h264_nvenc listed\n V..... libx264 listed\n V h264_nvenc again", "V libx264", "V libx264\u2028V h264_amf\t listed", "prefix V libx264 listed\n V h264_qsv listed\n V h264_videotoolbox listed"]) {
            assert.deepEqual(await native.value({ op: "parse", stdout }), clean(source.profiles.parseAvailableEncoders(stdout)))
        }
    } finally {
        await native.close()
    }
})
test("firstErrorLine preserves source trim, stderr priority, timeout and UTF16 length", async () => {
    const source = original(),
        native = rust()
    try {
        for (const error of [{ stderr: "\n \uFEFF GPU unavailable été 漢字\r\n second", killed: true, message: "ignored" }, { stderr: " \n", killed: true }, { message: "x".repeat(300) }, {}, { stderr: "🙌".repeat(100) + "tail" }, { stderr: "\u0085not JS whitespace\u0085" }]) assert.equal(await native.value({ op: "error", ...error }), source.error(error))
        const split = { stderr: "x".repeat(199) + "🙌" }
        assert.equal(await native.value({ op: "error", ...split }), source.error(split).replace(/[\ud800-\udbff]$/, "�")) // explicit UTF8 contract gap
    } finally {
        await native.close()
    }
})
test("missing FFmpeg differs from failed listing and supported-platform filtering", async () => {
    const native = rust(),
        files = fixture("listing-failure")
    try {
        for (const platform of ["linux", "win32", "darwin", "freebsd"]) {
            const source = original({ platform })
            const missing = await native.value({ op: "detect", platform })
            assert.deepEqual(missing.detection, clean(await source.detection.detectEncoders()))
            assert.equal(missing.detection.encoders.length, 6)
            assert.equal(missing.cacheIntent, undefined)
        }
        const source = original({ executable: files.executable, sourcePath: "frozen-ffmpeg", platform: "linux" }),
            failed = await native.value({ op: "detect", ...files.input })
        assert.deepEqual(failed.detection, clean(await source.detection.detectEncoders()))
        assert.equal(failed.source, "probe")
        assert.equal(failed.diagnostics[0].code, "process_exit")
        assert.ok(failed.detection.encoders.every((encoder) => encoder.reason === "Not included in this FFmpeg build"))
        assert.deepEqual(JSON.parse(failed.cacheIntent.bytes), JSON.parse(source.writes[0]))
        const unsupported = await native.value({ op: "detect", ...files.input, platform: "freebsd" })
        const unsupportedSource = original({ executable: files.executable, sourcePath: "frozen-ffmpeg", platform: "freebsd" })
        assert.deepEqual(unsupported.detection, clean(await unsupportedSource.detection.detectEncoders()))
        assert.deepEqual(unsupported.detection, { encoders: [], recommended: "x264" })
    } finally {
        await native.close()
        files.close()
    }
})
test("listed encoders require a genuine tiny encode; deterministic failures never mark hardware working", async () => {
    const files = fixture("encode-failure"),
        native = rust(),
        source = original({ executable: files.executable, sourcePath: "frozen-ffmpeg", platform: "linux" })
    try {
        const actual = await native.value({ op: "detect", ...files.input })
        assert.deepEqual(actual.detection, clean(await source.detection.detectEncoders()))
        assert.ok(actual.detection.encoders.every((encoder) => !encoder.available))
        assert.equal(actual.detection.recommended, "x264")
        assert.equal((await native.value({ op: "stats" })).children_started, 5)
        assert.ok(source.calls.slice(1).every((call) => call.args.includes("3") && call.args.slice(-3).join(" ") === "-f null -"))
    } finally {
        await native.close()
        files.close()
    }
})
test("real installed FFmpeg source/native detection executes exact encode commands", async () => {
    assert.ok(fs.existsSync(ffmpeg), `Set FREESHOW_FFMPEG_PATH to a real local FFmpeg; missing ${ffmpeg}`)
    const native = rust(),
        source = original({ executable: ffmpeg, sourcePath: "ffmpeg", platform: process.platform })
    const normalize = (detection) => ({ ...detection, encoders: detection.encoders.map((encoder) => ({ ...encoder, ...(encoder.reason ? { reason: encoder.reason.replace(/0x[0-9a-f]+/gi, "0xPTR") } : {}) })) })
    try {
        const expected = clean(await source.detection.detectEncoders(true)),
            actual = await native.value({ op: "detect", executable: ffmpeg, sourcePath: "ffmpeg", platform: process.platform, force: true })
        assert.deepEqual(normalize(actual.detection), normalize(expected))
        assert.equal(actual.source, "probe")
        assert.equal(actual.detection.encoders.find((encoder) => encoder.id === "x264")?.available, true, "local real software encode must work for this acceptance fixture")
        assert.ok(source.calls.length >= 2)
        assert.equal((await native.value({ op: "stats" })).children_reaped, source.calls.length)
    } finally {
        await native.close()
    }
})
test("cache key/raw detection quirks, invalidation, force and typed persistence intent", async () => {
    const files = fixture("listing-failure"),
        native = rust()
    try {
        for (const detection of [{ recommended: "unknown", encoders: [], future: { text: "漢字", number: 9007199254740992 } }, [], "raw truthy cache", 17, { encoders: null }]) {
            const source = original({ ...files.input, executable: files.executable, sourcePath: "frozen-ffmpeg", cache: JSON.stringify({ version: 1, key: "1|frozen-ffmpeg|0|linux|x64", detection }) })
            const actual = await native.value({ op: "detect", ...files.input, cache: JSON.stringify({ version: 1, key: "1|frozen-ffmpeg|0|linux|x64", detection }) })
            assert.deepEqual(actual.detection, clean(await source.detection.detectEncoders()))
            assert.equal(actual.source, "cache")
            assert.equal(actual.cacheIntent, undefined)
            assert.equal(source.calls.length, 0)
        }
        const exactCache = '{"version":1,"key":"1|frozen-ffmpeg|0|linux|x64","detection":{"recommended":"x264","encoders":[],"n":8.3163996793004788882e-275,"z":-0,"dup":1,"dup":2}}'
        const exact = await native.value({ op: "detect", ...files.input, cache: exactCache })
        assert.deepEqual(exact.detection, JSON.parse(exactCache).detection)
        assert.deepEqual(Object.keys(exact.detection), Object.keys(JSON.parse(exactCache).detection))
        for (const value of [null, false, 0, ""]) {
            const actual = await native.value({ op: "detect", ...files.input, cache: JSON.stringify({ version: 1, key: "1|frozen-ffmpeg|0|linux|x64", detection: value }) })
            assert.equal(actual.source, "probe")
        }
        for (const cache of ["{invalid", JSON.stringify({ version: 2, key: "1|frozen-ffmpeg|0|linux|x64", detection: {} }), JSON.stringify({ version: 1, key: "wrong", detection: {} })]) assert.equal((await native.value({ op: "detect", ...files.input, cache })).source, "probe")
        const cache = JSON.stringify({ version: 1, key: "1|frozen-ffmpeg|0|linux|x64", detection: { custom: "retained" } })
        assert.equal((await native.value({ op: "detect", ...files.input, cache, force: true })).source, "probe")
        for (const mtimeMs of [0, -0, 1.25, 1700000000000.123, 1e21]) assert.equal(await native.value({ op: "key", ...files.input, mtimeMs }), `1|frozen-ffmpeg|${mtimeMs}|linux|x64`)
        const overflow = await native.request({ op: "detect", ...files.input, cache: '{"version":1,"key":"1|frozen-ffmpeg|0|linux|x64","detection":{"n":1e309}}' })
        assert.equal(overflow.error.code, "json_number_overflow")
        const surrogate = await native.request({ op: "detect", ...files.input, cache: '{"version":1,"key":"1|frozen-ffmpeg|0|linux|x64","detection":{"text":"\\ud800"}}' })
        assert.equal(surrogate.error.code, "unsupported_utf16")
    } finally {
        await native.close()
        files.close()
    }
})
test("resolve respects disable-HW only for auto, explicit availability, and malformed cache failures", async () => {
    const files = fixture("listing-failure"),
        native = rust(),
        detection = {
            recommended: "nvenc",
            encoders: [
                { id: "nvenc", available: true },
                { id: "x264", available: false },
                { id: "qsv", available: "yes" }
            ]
        }
    const cache = JSON.stringify({ version: 1, key: "1|frozen-ffmpeg|0|linux|x64", detection })
    try {
        for (const setting of [undefined, "", "auto", "nvenc", "qsv", "x264", "unknown"]) {
            for (const disableHardware of [true, false]) {
                const source = original({ ...files.input, executable: files.executable, sourcePath: "frozen-ffmpeg", cache, disableHardware })
                assert.equal(await native.value({ op: "resolve", ...files.input, cache, setting, disableHardware }), await source.detection.resolveEncoder(setting))
            }
        }
        for (const detection of [{ encoders: null }, "raw", { encoders: [null] }, { recommended: 17, encoders: [] }]) {
            const cache = JSON.stringify({ version: 1, key: "1|frozen-ffmpeg|0|linux|x64", detection })
            const actual = await native.request({ op: "resolve", ...files.input, cache, setting: detection.recommended === 17 ? "auto" : "nvenc" })
            assert.equal(actual.error.code, "invalid_cached_detection")
        }
        const disabled = await native.value({ op: "resolve", setting: "auto", disableHardware: true })
        assert.equal(disabled, "x264")
        assert.equal((await native.value({ op: "stats" })).children_started, 0)
    } finally {
        await native.close()
        files.close()
    }
})
test("ephemeral output settings preserve unknown IDs and provide reconfiguration intent without ACK", async () => {
    const native = rust({ settings: 2 }),
        source = original()
    try {
        assert.equal(await native.value({ op: "get", outputId: "unset" }), "auto")
        for (const [outputId, encoder] of [
            ["a", "nvenc"],
            ["a", "unknown future encoder"],
            ["a", ""],
            ["b", "auto"]
        ]) {
            source.detection.setRtmpEncoderSetting(outputId, encoder)
            const intent = await native.value({ op: "set", outputId, encoder })
            assert.equal(intent.reconfigure_rtmp, true)
            assert.equal(await native.value({ op: "get", outputId }), source.detection.getRtmpEncoderSetting(outputId))
        }
        assert.equal((await native.request({ op: "set", outputId: "c", encoder: "x264" })).error.code, "setting_limit")
        source.detection.setRtmpEncoderSetting("__proto__", "nvenc")
        assert.equal(typeof source.detection.getRtmpEncoderSetting("__proto__"), "object")
        await native.value({ op: "set", outputId: "__proto__", encoder: "nvenc" })
        assert.equal((await native.request({ op: "get", outputId: "__proto__" })).error.code, "unsupported_prototype_setting")
    } finally {
        await native.close()
    }
})
test("nonforce callers share newest matching owned probe; force bounds and old completion cannot clear it", async () => {
    const files = fixture("gate"),
        native = rust({ process_ms: 5000 }, files.env)
    try {
        const first = native.request({ op: "detect", ...files.input })
        await until(() => files.pids().length === 1)
        const firstPid = files.pids()[0]
        const second = native.request({ op: "detect", ...files.input, force: true })
        await until(() => files.pids().length === 2)
        const secondPid = files.pids().find((pid) => pid !== firstPid)
        assert.equal((await native.request({ op: "detect", ...files.input, force: true })).error.code, "probe_limit")
        files.release(firstPid)
        const a = await first
        assert.equal(a.ok, true)
        const shared = native.request({ op: "detect", ...files.input })
        await until(async () => (await native.value({ op: "stats" })).admitted_callers === 2)
        assert.equal(files.pids().length, 2)
        files.release(secondPid)
        const [b, c] = await Promise.all([second, shared])
        assert.equal(b.value.probe_id, c.value.probe_id)
        assert.notEqual(a.value.probe_id, b.value.probe_id)
    } finally {
        await native.close()
        files.close()
    }
})
test("caller admission, dropped reply ownership, shutdown cancellation kill/reap and generation separation", async () => {
    const files = fixture("gate"),
        native = rust({ callers: 2 }, files.env)
    try {
        const first = native.request({ op: "detect", ...files.input, generation: 1 })
        await until(() => files.pids().length === 1)
        const joined = native.request({ op: "detect", ...files.input, generation: 1 })
        await until(async () => (await native.value({ op: "stats" })).admitted_callers === 2)
        assert.equal((await native.request({ op: "detect", ...files.input })).error.code, "caller_limit")
        const stopped = await native.value({ op: "shutdown" })
        assert.equal(stopped.active_children, 0)
        assert.equal(stopped.children_started, stopped.children_reaped)
        assert.equal((await first).error.code, "cancelled")
        assert.equal((await joined).error.code, "cancelled")
        for (const pid of files.pids()) if (process.platform === "linux") assert.equal(fs.existsSync(`/proc/${pid}`), false)
        assert.equal((await native.request({ op: "detect", ...files.input })).error.code, "shutdown")
    } finally {
        await native.close()
        files.close()
    }
    const dropped = fixture("gate"),
        owner = rust({}, dropped.env)
    try {
        const value = await owner.value({ op: "drop_detect", ...dropped.input })
        assert.equal(value.active_children, 1)
        await until(() => dropped.pids().length === 1)
        const unrelated = owner.request({ op: "detect", ...dropped.input, generation: 2 })
        await until(() => dropped.pids().length === 2)
        for (const pid of dropped.pids()) dropped.release(pid)
        assert.equal((await unrelated).ok, true)
        await until(async () => (await owner.value({ op: "stats" })).active_probes === 0)
        assert.equal((await owner.value({ op: "stats" })).children_reaped, 2)
    } finally {
        await owner.close()
        dropped.close()
    }
})
test("timeouts, output limits, missing executable and recovery remain explicit bounded outcomes", async () => {
    for (const mode of ["timeout", "stdout", "stderr", "encode-timeout"]) {
        const files = fixture(mode),
            native = rust({ process_ms: 100, operation_ms: 1000, listing: 4096, encode: 4096 }, files.env)
        try {
            const actual = await native.value({ op: "detect", ...files.input })
            assert.ok(actual.diagnostics.some((value) => value.code === (mode.includes("timeout") ? "process_timeout" : "output_limit")))
            assert.equal(actual.detection.recommended, "x264")
            assert.ok(actual.detection.encoders.every((encoder) => !encoder.available))
            const stats = await native.value({ op: "stats" })
            assert.equal(stats.active_children, 0)
            assert.equal(stats.children_started, stats.children_reaped)
            assert.equal((await native.value({ op: "detect" })).source, "missing")
        } finally {
            await native.close()
            files.close()
        }
    }
    const deadlineFixture = fixture("encode-timeout"),
        deadlineOwner = rust({ process_ms: 200, operation_ms: 300 }, deadlineFixture.env)
    try {
        const result = await deadlineOwner.request({ op: "detect", ...deadlineFixture.input })
        assert.equal(result.error.code, "operation_timeout")
        const stats = await deadlineOwner.value({ op: "stats" })
        assert.equal(stats.active_children, 0)
        assert.equal(stats.children_started, stats.children_reaped)
    } finally {
        await deadlineOwner.close()
        deadlineFixture.close()
    }
    const native = rust()
    try {
        const result = await native.value({ op: "detect", executable: path.resolve(os.tmpdir(), "freeshow-encoder-missing-never-created") })
        assert.equal(result.diagnostics[0].code, "spawn_failed")
        assert.ok(result.detection.encoders.every((encoder) => encoder.reason === "Not included in this FFmpeg build"))
    } finally {
        await native.close()
    }
})
