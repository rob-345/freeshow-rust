import assert from "node:assert/strict"
import path from "node:path"
import fs from "node:fs"
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import ts from "typescript"
import { root } from "./oracle.mjs"
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975",
    file = "src/electron/audio/processAudio.ts"
const result = spawnSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 })
assert.equal(result.status, 0, result.stderr)
const original = result.stdout.replace(/\r\n/g, "\n"),
    ast = ts.createSourceFile(file, original, ts.ScriptTarget.Latest, true)
const code = ts.transpileModule(
    ast.statements
        .filter((n) => !ts.isImportDeclaration(n))
        .map((n) => n.getText(ast).replace(/^export\s+/, ""))
        .join("\n"),
    { compilerOptions: { target: ts.ScriptTarget.ES2022 } }
).outputText
const binary = process.env.FREESHOW_OPUS_BIN || path.join(root, "target/debug", process.platform === "win32" ? "freeshow-opus.exe" : "freeshow-opus")
const oracleDir = process.env.FREESHOW_OPUS_ORACLE_DIR
const digest = (bytes) => createHash("sha256").update(bytes).digest("hex")
function call(exe, args, payload) {
    const r = spawnSync(exe, args, { cwd: root, input: JSON.stringify(payload), encoding: "utf8", timeout: 30000, maxBuffer: 8 * 1024 * 1024 })
    assert.equal(r.error, undefined)
    assert.equal(r.status, 0, r.stderr)
    return JSON.parse(r.stdout)
}
function rust(payload) {
    return call(binary, [], payload)
}
function source(payload) {
    return call(process.execPath, [path.join(root, "tests/parity/fixtures/opus/oracle.mjs")], { code, ...payload })
}
function planar(kind, frames = 1, offset = 0) {
    const b = Buffer.alloc(frames * 960 * 2 * 4),
        n = frames * 960
    for (let i = 0; i < n; i++)
        for (let ch = 0; ch < 2; ch++) {
            const t = i + offset
            let x = 0
            switch (kind) {
                case "silence":
                    break
                case "impulse":
                    x = t % 960 === 0 ? (ch ? -0.7 : 0.7) : 0
                    break
                case "tone":
                    x = 0.45 * Math.sin((2 * Math.PI * (ch ? 660 : 440) * t) / 48000)
                    break
                case "alternating":
                    x = t % 2 ? (ch ? 1 : -1) : ch ? -1 : 1
                    break
                case "noise":
                    x = (((Math.imul(t + 1, 1664525) + 1013904223 + ch * 7919) >>> 0) / 4294967296 - 0.5) * 1.6
                    break
                default:
                    throw new Error(kind)
            }
            b.writeFloatLE(x, (ch * n + i) * 4)
        }
    return b.toString("hex")
}
function corpus() {
    return [...Array(4).fill("silence"), "impulse", ...Array(40).fill("tone"), "alternating", ...Array(8).fill("noise")].map((kind, i) => planar(kind, 1, i * 960))
}
function samples(hex) {
    const b = Buffer.from(hex, "hex"),
        a = []
    for (let i = 0; i < b.length; i += 2) a.push(b.readInt16LE(i))
    return a
}
function quality(pcm, decoded, lookahead) {
    const input = pcm.flatMap(samples),
        output = decoded.flatMap(samples)
    let square = 0,
        energy = 0,
        correlation = 0,
        a2 = 0,
        b2 = 0,
        n = 0
    for (let i = 960 * 6 * 2; i < input.length - 960 * 2; i++) {
        const x = input[i],
            y = output[i + lookahead * 2]
        if (y === undefined) break
        square += (x - y) ** 2
        energy += x * x
        correlation += x * y
        a2 += x * x
        b2 += y * y
        n++
    }
    return { count: n, snrDb: 10 * Math.log10(energy / Math.max(square, 1)), correlation: correlation / Math.sqrt(a2 * b2) }
}
function addon(version) {
    return path.join(oracleDir, version, "opus.node")
}
const libraries = () => [
    { name: "bundled", version: "libopus 1.3.1", library: path.join(oracleDir, "libopus.so.0") },
    { name: "system", version: "libopus 1.4", library: process.env.FREESHOW_OPUS_SYSTEM_LIBRARY || "/lib/x86_64-linux-gnu/libopus.so.0" }
]
function requireOracles(t) {
    if (oracleDir) return true
    if (process.env.FREESHOW_REQUIRE_OPUS_ORACLE) assert.fail("FREESHOW_OPUS_ORACLE_DIR required: build private actual @discordjs/opus oracles")
    t.skip("Actual codec comparison unavailable: no private original addon build; source/capability/ownership coverage remains separate")
    return false
}

test("pinned source constructs one48000/stereo encoder; absent dependency disables actual source capability", () => {
    const o = source({ unavailable: true, planar: [planar("tone")] })
    assert.equal(o.enabled, false)
    assert.deepEqual(o.pcm, [])
    assert.equal(o.logs[0][0], "OPUS not found!")
    assert.match(original, /new OpusEncoder\(sampleRate2, channelCount2\)/)
    assert.match(original, /const frameByteSize = 960 \* 2 \* 2/)
    assert.doesNotMatch(original, /\.setBitrate\(|applyEncoderCTL|OPUS_RESET_STATE/)
})
test("native missing trusted library and unsupported frame receipts never claim codec success", () => {
    const absent = path.join(root, "target", "deliberately-absent-opus-d946df64.so"),
        r = rust({ library: absent, frames: [Buffer.alloc(3840).toString("hex")] })
    assert.equal(r.error, undefined)
    assert.equal(r.ok.capability.Unavailable.code, "library_unavailable")
    assert.equal(r.ok.packets[0].error.code, "library_unavailable")
    assert.equal(r.ok.shutdown, null)
    const invalid = rust({ library: absent, frames: ["", "00", Buffer.alloc(3839).toString("hex"), Buffer.alloc(3841).toString("hex")] })
    for (const p of invalid.ok.packets) assert.equal(p.error.code, "unsupported_frame")
})
test("default actual native capability reports verified version or explicit unavailable", () => {
    const r = rust({ frames: [Buffer.alloc(3840).toString("hex")] })
    assert.equal(r.error, undefined)
    if (r.ok.capability.Available) {
        assert.match(r.ok.capability.Available.version, /^libopus 1\.(3\.1|4)$/)
        assert.equal(r.ok.packets[0].error, undefined)
        assert.equal(r.ok.capability.Available.frame_samples, 960)
    } else {
        assert.ok(r.ok.capability.Unavailable)
        assert.ok(r.ok.packets[0].error)
        if (process.env.FREESHOW_REQUIRE_OPUS) assert.fail(JSON.stringify(r.ok.capability))
    }
})
test("actual original N-API failure branches retain encoder after type error; constructor native init is lazy", (t) => {
    if (!requireOracles(t)) return
    for (const backend of libraries()) {
        const o = source({ addon: addon(backend.name), mode: "errors" })
        assert.deepEqual(o.errors, [
            { name: "TypeError", message: "Provided input needs to be a buffer" },
            { name: "Error", message: "Could not create encoder. Check the encoder parameters" }
        ])
        assert.equal(o.bitrateBefore, 120000)
        assert.equal(o.bitrateAfter, 99000)
        const r = rust({ library: backend.library, frames: [Buffer.alloc(3840).toString("hex")] })
        assert.equal(r.ok.packets[0].hex, o.after)
    }
})
test("actual processAudio→original N-API and native encode exact steady-sequence packets within each verified library", (t) => {
    if (!requireOracles(t)) return
    const records = []
    for (const backend of libraries()) {
        const o = source({ addon: addon(backend.name), planar: corpus() })
        assert.equal(o.enabled, true)
        assert.deepEqual(o.logs, [])
        assert.deepEqual(o.constructors, [{ rate: 48000, channels: 2 }])
        assert.equal(o.pcm.length, 54)
        const r = rust({ library: backend.library, frames: o.pcm })
        assert.equal(r.error, undefined)
        assert.equal(r.ok.capability.Available.version, backend.version)
        assert.equal(r.ok.capability.Available.max_packet_bytes, 3828)
        assert.equal(r.ok.capability.Available.default_bitrate, 120000)
        assert.equal(r.ok.shutdown, null)
        assert.deepEqual(
            r.ok.packets.map((p) => p.hex),
            o.encoded
        )
        assert.deepEqual(
            r.ok.packets.map((p) => p.sequence),
            o.pcm.map((_, i) => i + 1)
        )
        assert.ok(o.encoded.every((p) => p.length > 0 && p.length <= 3828 * 2))
        const nativeDecoded = rust({ mode: "decode", library: backend.library, packets: o.encoded }).ok
        const originalDecoded = source({ mode: "decode", addon: addon(backend.name), packets: o.encoded })
        assert.deepEqual(nativeDecoded.decoded, originalDecoded.decoded)
        assert.ok(nativeDecoded.decoded.every((h) => h.length === 3840 * 2))
        const q = quality(o.pcm.slice(5, 45), nativeDecoded.decoded.slice(5, 45), r.ok.capability.Available.lookahead)
        assert.ok(q.snrDb > 20, JSON.stringify(q))
        assert.ok(q.correlation > 0.99, JSON.stringify(q))
        records.push({ backend, pcm: o.pcm, packets: o.encoded, decoded: nativeDecoded.decoded, info: r.ok.capability.Available, quality: q })
    }
    const cross = []
    for (const [from, to] of [
        [records[0], records[1]],
        [records[1], records[0]]
    ]) {
        const native = rust({ mode: "decode", library: to.backend.library, packets: from.packets }).ok,
            original = source({ mode: "decode", addon: addon(to.backend.name), packets: from.packets })
        assert.deepEqual(native.decoded, original.decoded)
        assert.ok(native.decoded.every((h) => h.length === 3840 * 2))
        const q = quality(from.pcm.slice(5, 45), native.decoded.slice(5, 45), from.info.lookahead)
        assert.ok(q.snrDb > 20, JSON.stringify(q))
        cross.push({ from: from.backend.version, to: to.backend.version, quality: q, decodedSha256: digest(Buffer.from(native.decoded.join(""), "hex")) })
    }
    // Cross-version byte equivalence is an observation, never assumed from a version string.
    const different = records[0].packets.filter((p, i) => p !== records[1].packets[i]).length
    if (process.env.FREESHOW_OPUS_RECEIPT) {
        const build = JSON.parse(fs.readFileSync(path.join(oracleDir, "build-receipt.json"), "utf8")),
            keys = ["package.json", "src/node-opus.cc", "src/node-opus.h", "deps/binding.gyp", "deps/config/opus/linux/x64/config.h", "deps/opus/include/opus.h", "deps/opus/include/opus_defines.h"]
        fs.writeFileSync(
            process.env.FREESHOW_OPUS_RECEIPT,
            JSON.stringify(
                {
                    revision,
                    sourceSha256: digest(original),
                    node: process.version,
                    platform: process.platform,
                    arch: process.arch,
                    addonVersion: build.package,
                    sourceHashes: Object.fromEntries(keys.map((key) => [key, build.hashes[key]])),
                    allBuildSourceHashesSha256: digest(JSON.stringify(build.hashes)),
                    buildOutputs: build.outputs,
                    systemLibrarySha256: digest(fs.readFileSync(libraries()[1].library)),
                    backends: records.map((r) => ({ version: r.backend.version, info: r.info, quality: r.quality, pcmSha256: digest(Buffer.from(r.pcm.join(""), "hex")), packetsSha256: digest(Buffer.from(r.packets.join(""), "hex")), decodedSha256: digest(Buffer.from(r.decoded.join(""), "hex")), firstPackets: r.packets.slice(0, 6) })),
                    crossVersion: { differentPackets: different, totalPackets: records[0].packets.length, crossDecode: cross }
                },
                null,
                2
            ) + "\n"
        )
    }
})
test("actual source fast/accumulating paths share continuous codec state and native matches emitted frames", (t) => {
    if (!requireOracles(t)) return
    const full = planar("tone", 1, 0),
        bytes = Buffer.from(full, "hex"),
        half = Buffer.alloc(960 * 4)
    bytes.copy(half, 0, 0, 480 * 4)
    bytes.copy(half, 480 * 4, 960 * 4, 1440 * 4)
    const half2 = Buffer.alloc(960 * 4)
    bytes.copy(half2, 0, 480 * 4, 960 * 4)
    bytes.copy(half2, 480 * 4, 1440 * 4, 1920 * 4)
    const o = source({ addon: addon("bundled"), planar: [half.toString("hex"), half2.toString("hex"), full, full] })
    assert.equal(o.pcm.length, 3)
    const r = rust({ library: libraries()[0].library, frames: o.pcm })
    assert.deepEqual(
        r.ok.packets.map((p) => p.hex),
        o.encoded
    )
    const fresh = rust({ library: libraries()[0].library, frames: [o.pcm[2]] })
    assert.notEqual(r.ok.packets[2].hex, fresh.ok.packets[0].hex, "codec must not reset per frame")
})
test("actual accepted codec state survives dropped caller and subsequent packet matches original continuation", (t) => {
    if (!requireOracles(t)) return
    for (const backend of libraries()) {
        const o = source({ addon: addon(backend.name), planar: [planar("tone", 1, 0), planar("tone", 1, 960), planar("tone", 1, 1920)] })
        const r = rust({ library: backend.library, frames: o.pcm, discard_first: true })
        assert.deepEqual(r.ok.packets[0], { discarded: true })
        assert.equal(r.ok.packets[1].sequence, 2)
        assert.deepEqual(
            r.ok.packets.slice(1).map((p) => p.hex),
            o.encoded.slice(1)
        )
        assert.equal(r.ok.shutdown, null)
    }
})
test("committed actual-codec receipt identifies source/native versions and separate cross-version evidence", () => {
    const receipt = JSON.parse(fs.readFileSync(new URL("./fixtures/opus/receipt.json", import.meta.url), "utf8"))
    assert.equal(receipt.revision, revision)
    assert.equal(receipt.sourceSha256, digest(original))
    assert.equal(receipt.addonVersion, "0.10.0")
    for (const [name, expected] of Object.entries(receipt.sourceHashes))
        assert.equal(
            digest(
                fs
                    .readFileSync(path.join(root, "node_modules/@discordjs/opus", name))
                    .toString("utf8")
                    .replace(/\r\n/g, "\n")
            ),
            expected
        )
    assert.deepEqual(
        receipt.backends.map((b) => b.version),
        ["libopus 1.3.1", "libopus 1.4"]
    )
    assert.equal(receipt.crossVersion.totalPackets, 54)
    for (const b of receipt.backends) {
        assert.ok(b.quality.snrDb > 20)
        assert.equal(b.firstPackets.length, 6)
    }
})
