// Reuse the measured desktop oracle without changing any source algorithm.
// This private harness adapter broadens only its controlled PCM shape to the
// actual retained worklet's observed44100/48000Hz20ms frames and64-frame cap.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
const root = fileURLToPath(new URL("../../../", import.meta.url)),
    sha = (value) => createHash("sha256").update(value).digest("hex"),
    receiptPath = path.resolve(process.argv[2] || "test-output-rust/audio-context-fullfork/before-close.json")
assert.ok(fs.statSync(receiptPath).isFile() && fs.statSync(receiptPath).size <= 2 * 1024 * 1024)
const raw = fs.readFileSync(receiptPath)
assert.ok(raw.length <= 2 * 1024 * 1024)
const archiveVariant = process.argv[3],
    parsedReceipt = JSON.parse(raw)
if (archiveVariant !== undefined) assert.ok(["lazy", "eager"].includes(archiveVariant), "explicit archived app variant")
const receipt = archiveVariant ? parsedReceipt.ordinary[archiveVariant] : parsedReceipt,
    packets = receipt.audio.packets
assert.ok(packets.length >= 4 && packets.length <= 64)
for (const [index, packet] of packets.entries()) {
    assert.equal(packet.state, "completed")
    assert.equal(packet.origin, "port")
    assert.ok([44100, 48000].includes(packet.metadata.sampleRate))
    assert.equal(packet.metadata.id, "icecast")
    assert.equal(packet.receipt.lastReceipt.failure, null)
    assert.deepEqual(packet.receipt.lastReceipt.diagnostics, [])
    const effects = packet.receipt.lastReceipt.effects,
        encoded = effects.filter((effect) => effect.Encoded),
        sent = effects.filter((effect) => effect.Icecast)
    assert.equal(encoded.length, 1, "measured20ms source/native frame corpus")
    assert.equal(sent.length, 1)
    assert.equal(sent[0].Icecast.disposition, index === 0 ? "ConnectingDropped" : "Written", "actual local native disposition; no delivery inferred from queue acceptance")
}
const script = {
    phase: "actual-full-editor-worklet",
    title: "",
    config: packets[0].metadata.icecast,
    packets: packets.map((packet, index) => ({ origin: "port", sampleRate: packet.metadata.sampleRate, id: packet.metadata.id, planarBase64: packet.planarBase64, connectDropped: index === 0 }))
}
let template = fs.readFileSync(path.join(root, "tests/desktop/audio_stream_oracle.mjs"), "utf8").replace(/\r\n/g, "\n")
assert.equal(sha(template), "8a52055af57ae43e2036b840a0fad47d748fe48d26ab3f7f27c2f75ee83e095c", "reviewed immutable desktop oracle harness")
for (const [before, after] of [
    ['const root = fileURLToPath(new URL("../../", import.meta.url))', `const root = ${JSON.stringify(root)}`],
    ["script.packets.length <= 16", "script.packets.length <= 64"],
    ["assert.equal(packet.sampleRate, 48000)", "assert.ok([44100,48000].includes(packet.sampleRate))"],
    ['assert.equal(bytes.length, 7680, "actual controlled planar stereo960 bytes")', 'assert.equal(bytes.length, Math.max(128,Math.round(packet.sampleRate*0.02))*8, "actual retained planar stereo20ms bytes")']
]) {
    assert.equal(template.split(before).length, 2, "one reviewed shape-only harness substitution")
    template = template.replace(before, after)
}
const directory = archiveVariant ? path.join(root, "test-output-rust/audio-context-fullfork/archive-" + archiveVariant) : path.dirname(receiptPath),
    generated = path.join(directory, "worklet-source-replay.mjs")
fs.mkdirSync(directory, { recursive: true })
fs.writeFileSync(generated, template)
const result = spawnSync(process.execPath, [generated], {
    cwd: root,
    input: JSON.stringify({ phases: [{ script, wire: receipt.wire, capability: receipt.capability }], oraclesDir: process.env.FREESHOW_OPUS_ORACLE_DIR }),
    encoding: "utf8",
    timeout: 20000,
    maxBuffer: 4 * 1024 * 1024
})
assert.equal(result.status, 0, result.stderr || String(result.error))
const source = JSON.parse(result.stdout)
assert.equal(source.globalEncodedPackets, packets.length)
assert.equal(source.constructors.length, 1)
assert.equal(source.phases[0].nativeCompared, true)
const output = { source, observedSampleRates: [...new Set(packets.map((packet) => packet.metadata.sampleRate))], packetCount: packets.length, desktopOracleSha256: sha(fs.readFileSync(path.join(root, "tests/desktop/audio_stream_oracle.mjs"))), harnessAdapterSha256: sha(fs.readFileSync(fileURLToPath(import.meta.url))), nativeRawCrcsVerified: receipt.wire.rawCrcsVerified, sourceAlgorithmsAltered: false }
fs.writeFileSync(path.join(directory, "source-oracle.json"), JSON.stringify(output, null, 2) + "\n")
process.stdout.write(JSON.stringify({ compared: true, packets: packets.length, rates: output.observedSampleRates, globalConstructors: source.constructors.length }) + "\n")
