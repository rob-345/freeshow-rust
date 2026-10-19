import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import vm from "node:vm"
import { createRequire } from "node:module"
import { spawn, spawnSync, execFile } from "node:child_process"
import { promisify } from "node:util"
import { createInterface } from "node:readline"
import { once } from "node:events"
import { test } from "node:test"
import ts from "typescript"
import { clean, root, upstreamFunctions } from "./oracle.mjs"

const binary = process.env.FREESHOW_MEDIA_BIN || path.join(root, "target/debug", process.platform === "win32" ? "freeshow-media.exe" : "freeshow-media")
const require = createRequire(import.meta.url)
const execFileAsync = promisify(execFile)
const ffmpeg = process.env.FREESHOW_TEST_FFMPEG || "ffmpeg"
const ffprobe = process.env.FREESHOW_TEST_FFPROBE || "ffprobe"
const available = [ffmpeg, ffprobe].every((file) => spawnSync(file, ["-version"], { encoding: "utf8", timeout: 10000 }).status === 0)
if (process.env.FREESHOW_REQUIRE_MEDIA_TOOLS === "1" && !available) throw new Error("Required real media parity needs working ffmpeg and ffprobe binaries")
const encoders = available ? spawnSync(ffmpeg, ["-hide_banner", "-encoders"], { encoding: "utf8", timeout: 10000 }).stdout : ""

function sourceVariables(filename, names) {
    const source = fs.readFileSync(path.join(root, filename), "utf8")
    const ast = ts.createSourceFile(filename, source, ts.ScriptTarget.Latest, true)
    return names
        .map((name) => {
            const statement = ast.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((declaration) => declaration.name.getText(ast) === name))
            assert.ok(statement, `missing upstream variable ${name}`)
            return statement.getText(ast).replace(/^export\s+/, "")
        })
        .join("\n")
}

function evaluate(source, injected = {}) {
    const context = vm.createContext(injected)
    new vm.Script(ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText).runInContext(context, { timeout: 1000 })
    return context.result
}

const mimeTypes = evaluate(sourceVariables("src/electron/data/media.ts", ["mimeTypes"]) + "\nglobalThis.result = mimeTypes;")
const oracle = upstreamFunctions("src/electron/utils/files.ts", ["getMediaCodec", "getMimeType", "getMediaTracks", "formatTimestamp", "parseMp4File"], { fs, path, Buffer, TextDecoder, require, mimeTypes }, sourceVariables("src/electron/utils/files.ts", ["CHUNK_SIZE"]))

function ffmpegOracle(injected = {}) {
    const file = "src/electron/streaming/ffmpegManager.ts"
    const upstream = upstreamFunctions(file, ["runsOk", "resolveFfmpegPath"], { process, path, app: { getPath: () => "/nonexistent-media-parity-user-data" }, existsSync: fs.existsSync, execFileAsync, ...injected }, sourceVariables(file, ["IS_WIN", "BIN_NAME", "resolvedPath", "resolvingPromise", "getFfmpegDir", "getFfmpegPathLocal", "SYSTEM_PATHS"]))
    const filename = "src/electron/IPC/responsesMain.ts"
    const ast = ts.createSourceFile(filename, fs.readFileSync(path.join(root, filename), "utf8"), ts.ScriptTarget.Latest, true)
    let handler
    const visit = (node) => {
        if (ts.isPropertyAssignment(node) && node.name.getText(ast) === "[Main.FFMPEG_CHECK]") handler = node.initializer.getText(ast)
        ts.forEachChild(node, visit)
    }
    visit(ast)
    assert.ok(handler)
    return evaluate(`globalThis.result = (${handler});`, { resolveFfmpegPath: upstream.resolveFfmpegPath })
}

function rust(cases, args = [], env = process.env) {
    const result = spawnSync(binary, args, { input: cases.map((value) => JSON.stringify(value)).join("\n") + "\n", encoding: "utf8", timeout: 60000, maxBuffer: 32 * 1024 * 1024, env })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return result.stdout.trim().split("\n").map(JSON.parse)
}
const request = (command, data) => ({ command, data })

async function runningHarness(t, args = [], env = process.env) {
    const child = spawn(binary, args, { stdio: ["pipe", "pipe", "inherit"], env })
    const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]()
    t.after(async () => {
        if (child.exitCode === null && child.signalCode === null) {
            const exit = once(child, "exit")
            child.stdin.end()
            await exit
        }
    })
    return async (command, data) => {
        child.stdin.write(JSON.stringify(request(command, data)) + "\n")
        const { value, done } = await lines.next()
        assert.equal(done, false)
        return JSON.parse(value)
    }
}

function temporary(t) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-media-parity-"))
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }))
    return directory
}

function encode(directory, name, args) {
    const output = path.join(directory, name)
    const result = spawnSync(ffmpeg, ["-hide_banner", "-loglevel", "error", "-nostdin", ...args, "-y", output], { encoding: "utf8", timeout: 15000 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return output
}

function fixtures(t) {
    const directory = temporary(t)
    const audio = encode(directory, "audio.m4a", ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=8000", "-t", "2", "-c:a", "aac", "-threads", "1", "-fflags", "+bitexact", "-flags:a", "+bitexact"])
    const video = encode(directory, "video.mp4", ["-f", "lavfi", "-i", "color=c=blue:s=32x24:r=2", "-i", audio, "-t", "2", "-c:v", "libx264", "-threads", "1", "-pix_fmt", "yuv420p", "-c:a", "copy", "-fflags", "+bitexact", "-flags:v", "+bitexact"])
    const srt = path.join(directory, "captions.srt")
    const japanese = path.join(directory, "japanese.srt")
    fs.writeFileSync(srt, "1\n00:00:00,250 --> 00:00:00,750\nHello world\n\n2\n00:00:01,000 --> 00:00:01,500\n日本語 café <b>bold</b>\n\n3\n00:00:01,650 --> 00:00:01,900\n" + "A".repeat(65) + "\n")
    fs.writeFileSync(japanese, "1\n00:00:00,125 --> 00:00:00,625\n日本語\n")
    const subtitles = encode(directory, "subtitles.mp4", ["-i", video, "-i", srt, "-i", japanese, "-map", "0", "-map", "1", "-map", "2", "-c", "copy", "-c:s", "mov_text", "-metadata:s:s:0", "language=eng"])
    const defaultLanguage = encode(directory, "default-language.mp4", ["-i", video, "-i", srt, "-map", "0", "-map", "1", "-c", "copy", "-c:s", "mov_text"])
    const multiple = encode(directory, "multiple.mp4", ["-i", video, "-i", srt, "-i", srt, "-map", "0", "-map", "1", "-map", "2", "-c", "copy", "-c:s", "mov_text", "-metadata:s:s:0", "language=nor", "-metadata:s:s:1", "language=eng"])
    const wav = encode(directory, "audio.wav", ["-i", audio, "-c:a", "pcm_s16le"])
    const mpeg4 = encode(directory, "mpeg4.mp4", ["-f", "lavfi", "-i", "color=s=32x24:r=2", "-t", "1", "-c:v", "mpeg4", "-threads", "1"])
    const upper = path.join(directory, "UPPER.MP4")
    const unknown = path.join(directory, "video.unknown")
    const dash = path.join(directory, "-")
    for (const file of [upper, unknown, dash]) fs.copyFileSync(video, file)
    const corrupt = path.join(directory, "corrupt.mp4")
    const truncated = path.join(directory, "truncated.mp4")
    fs.writeFileSync(corrupt, Buffer.from([0, 0, 0, 16, ...Buffer.from("ftypisom"), 0, 0, 0, 0]))
    fs.writeFileSync(truncated, fs.readFileSync(video).subarray(0, 64))
    const late = path.join(directory, "late-ftyp.mp4")
    fs.writeFileSync(late, Buffer.concat([Buffer.alloc(256 * 1024), fs.readFileSync(video)]))
    return { directory, audio, video, subtitles, multiple, defaultLanguage, wav, mpeg4, upper, unknown, dash, corrupt, truncated, late, missing: path.join(directory, "missing.mp4") }
}

// These tests run without ffmpeg/ffprobe. The original MP4Box code is invoked directly.
test("media fallback/default payloads match the actual TypeScript functions without binaries", async (t) => {
    const directory = temporary(t)
    const corrupt = path.join(directory, "corrupt.mp4")
    fs.writeFileSync(corrupt, "not media")
    const payloads = [{}, { path: "" }, { path: null }, { path: false }, { path: 123 }, { path: directory, future: { retained: true } }, { path: corrupt }, { path: path.join(directory, "missing.MP4"), codecs: ["stale"], mimeType: "override", mimeCodec: "override", tracks: ["stale"] }]
    for (const [command, upstream] of [
        ["MEDIA_CODEC", oracle.getMediaCodec],
        ["MEDIA_TRACKS", oracle.getMediaTracks]
    ]) {
        const responses = rust(
            payloads.map((data) => request(command, data)),
            ["--no-discover"]
        )
        for (const [index, payload] of payloads.entries()) {
            assert.equal(responses[index].ok, true)
            assert.deepEqual(responses[index].value, clean(await upstream(payload)))
            assert.deepEqual(responses[index].diagnostics, [])
        }
    }
})

test("FFMPEG_CHECK unavailable result matches the actual resolver/handler and omits path", async () => {
    const upstream = ffmpegOracle({
        existsSync: () => false,
        execFileAsync: async () => {
            throw new Error("missing binary")
        }
    })
    const [response] = rust([request("FFMPEG_CHECK", { ignored: true })], ["--no-discover"])
    assert.equal(response.ok, true)
    assert.deepEqual(response.value, clean(await upstream()))
    assert.deepEqual(response.value, { installed: false })
    assert.equal(response.diagnostics[0].code, "media_unavailable")
})

test("media command boundary rejects unsupported commands and non-object metadata payloads", () => {
    const responses = rust([request("UNKNOWN", {}), request("MEDIA_CODEC", null), request("MEDIA_TRACKS", [])], ["--no-discover"])
    assert.deepEqual(
        responses.map((response) => response.error.code),
        ["unsupported_command", "invalid_request", "invalid_request"]
    )
})

test("real ffprobe: generated audio/video/subtitles, payload spread, corrupt/missing inputs match MP4Box", { skip: available ? false : "REAL MEDIA PARITY NOT EXECUTED: ffmpeg/ffprobe unavailable" }, async (t) => {
    const media = fixtures(t)
    const payloads = Object.entries(media)
        .filter(([key]) => key !== "directory")
        .map(([name, file]) => ({ path: file, fixture: name, future: { retained: true } }))
    for (const [command, upstream] of [
        ["MEDIA_CODEC", oracle.getMediaCodec],
        ["MEDIA_TRACKS", oracle.getMediaTracks]
    ]) {
        const responses = rust(
            payloads.map((data) => request(command, data)),
            ["--ffprobe", ffprobe, "--no-discover"]
        )
        for (const [index, payload] of payloads.entries()) {
            assert.equal(responses[index].ok, true)
            assert.deepEqual(responses[index].diagnostics, [], payload.fixture)
            assert.deepEqual(responses[index].value, clean(await upstream(payload)), `${command}: ${payload.fixture}`)
        }
        if (command === "MEDIA_CODEC") {
            assert.equal(responses[0].value.mimeType, "application/octet-stream", "original has no m4a MIME entry")
            assert.ok(
                responses[1].value.codecs.some((codec) => codec.startsWith("avc1.")),
                "valid video comparison must not be empty"
            )
        } else {
            const subtitles = responses.find((response) => response.value.fixture === "subtitles").value.tracks
            assert.equal(subtitles.length, 1, "Japanese-only raw sample becomes empty under the original ASCII filter")
            assert.match(subtitles[0].vtt, /caf boldstyl/)
            assert.match(subtitles[0].vtt, /A{66}/, "printable mov_text length prefix survives the original raw-byte cleanup")
            assert.equal(responses.find((response) => response.value.fixture === "multiple").value.tracks.length, 2)
            const defaultTrack = responses.find((response) => response.value.fixture === "defaultLanguage").value.tracks[0]
            assert.equal(defaultTrack.name, "und")
            assert.equal(defaultTrack.lang, "un")
        }
    }
    t.diagnostic(`Compared ${payloads.length * 2} actual original handler replies using real ${ffprobe}; fixture encoders used single-threaded 32x24 video and 8kHz audio.`)
})

test("real ffmpeg: installed handler result matches the original resolver", { skip: available ? false : "REAL FFMPEG CHECK NOT EXECUTED: binaries unavailable" }, async () => {
    const upstream = ffmpegOracle()
    const [response] = rust([request("FFMPEG_CHECK", null)])
    assert.equal(response.ok, true)
    assert.deepEqual(response.diagnostics, [])
    assert.deepEqual(response.value, clean(await upstream()))
})

test("valid MP4 reports unavailable capability distinctly from missing/corrupt parser fallbacks", { skip: available ? false : "REAL MEDIA INPUT CHECK NOT EXECUTED: binaries unavailable" }, async (t) => {
    const media = fixtures(t)
    const responses = rust([request("MEDIA_CODEC", { path: media.video }), request("MEDIA_TRACKS", { path: media.subtitles })], ["--no-discover"])
    assert.ok(responses.every((response) => response.ok && response.diagnostics[0]?.code === "media_unavailable"))
    assert.deepEqual(responses[0].value.codecs, [])
    assert.deepEqual(responses[1].value.tracks, [])
})

test("real ffmpeg: cached successful PATH result survives executable removal like the original", { skip: !available || process.platform === "win32" ? "Requires available real binaries and POSIX symlink support" : false }, async (t) => {
    const directory = temporary(t)
    const resolved = await execFileAsync(ffmpeg, ["-version"])
    assert.match(resolved.stdout, /ffmpeg version/)
    const location = spawnSync("sh", ["-c", 'command -v "$1"', "media-parity", ffmpeg], { encoding: "utf8" })
    assert.equal(location.status, 0)
    const link = path.join(directory, "ffmpeg")
    fs.symlinkSync(path.resolve(location.stdout.trim()), link)
    const env = { ...process.env, PATH: directory }
    const upstream = ffmpegOracle({ execFileAsync: (file, args, options) => execFileAsync(file, args, { ...options, env }) })
    const call = await runningHarness(t, [], env)
    assert.deepEqual((await call("FFMPEG_CHECK")).value, clean(await upstream()))
    fs.unlinkSync(link)
    assert.deepEqual((await call("FFMPEG_CHECK")).value, clean(await upstream()))
    assert.deepEqual((await call("FFMPEG_CHECK")).value, { installed: true, path: "ffmpeg" })
})

for (const { name, encoder, args } of [
    { name: "HEVC", encoder: "libx265", args: ["-c:v", "libx265", "-threads", "1", "-x265-params", "pools=1:frame-threads=1:wpp=0:log-level=error", "-tag:v", "hvc1"] },
    { name: "AV1", encoder: "libaom-av1", args: ["-c:v", "libaom-av1", "-cpu-used", "8", "-threads", "1", "-row-mt", "0"] }
]) {
    test(`real ${name} MP4 codec configuration matches MP4Box`, { skip: available && encoders.includes(encoder) ? false : `REAL ${name} CODEC PARITY NOT EXECUTED: ${encoder}/ffprobe unavailable` }, async (t) => {
        const directory = temporary(t)
        const file = encode(directory, `${name}.mp4`, ["-f", "lavfi", "-i", "color=s=32x32:r=1", "-t", "1", ...args])
        const [response] = rust([request("MEDIA_CODEC", { path: file })], ["--ffprobe", ffprobe, "--no-discover"])
        assert.equal(response.ok, true)
        assert.deepEqual(response.diagnostics, [])
        assert.deepEqual(response.value, clean(await oracle.getMediaCodec({ path: file })))
        assert.equal(response.value.codecs.length, 1)
    })
}

test("unsupported VP9 configuration reports an explicit gap instead of invented codec metadata", { skip: available && encoders.includes("libvpx-vp9") ? false : "REAL UNSUPPORTED-VARIANT CHECK NOT EXECUTED: libvpx-vp9/ffprobe unavailable" }, async (t) => {
    const directory = temporary(t)
    const file = encode(directory, "vp9.mp4", ["-f", "lavfi", "-i", "color=s=32x32:r=1", "-t", "1", "-c:v", "libvpx-vp9", "-threads", "1"])
    const original = clean(await oracle.getMediaCodec({ path: file }))
    assert.match(original.codecs[0], /^vp09\./)
    const [response] = rust([request("MEDIA_CODEC", { path: file })], ["--ffprobe", ffprobe, "--no-discover"])
    assert.equal(response.ok, true)
    assert.deepEqual(response.value.codecs, [])
    assert.equal(response.diagnostics[0].code, "media_unsupported_variant")
})
