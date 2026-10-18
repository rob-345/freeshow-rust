import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { createRequire } from "node:module"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import { parseFile } from "music-metadata"
import { root, upstreamFunctions, clean } from "./oracle.mjs"

const require = createRequire(import.meta.url)
const binary = process.env.FREESHOW_AUDIO_BIN || path.join(root, "crates/freeshow-audio/target/debug", process.platform === "win32" ? "freeshow-audio.exe" : "freeshow-audio")
const ffmpeg = process.env.FREESHOW_TEST_FFMPEG || "ffmpeg"
const available = spawnSync(ffmpeg, ["-version"], { encoding: "utf8", timeout: 10000 }).status === 0
const { getAudioMetadata } = upstreamFunctions("src/electron/audio/audio.ts", ["getAudioMetadata"], { parseFile })
const projectOracle = (metadata) => upstreamFunctions("src/electron/audio/audio.ts", ["getAudioMetadata"], { parseFile: async () => metadata }).getAudioMetadata("injected-parser-boundary")
function rust(cases) {
    const result = spawnSync(binary, [], { input: cases.map((item) => JSON.stringify(item)).join("\n") + "\n", encoding: "utf8", timeout: 30000, maxBuffer: 16 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return result.stdout.trim().split("\n").map(JSON.parse)
}
const request = (filePath) => ({ command: "READ_AUDIO_METADATA", data: { filePath } })
function compare(actual, expected, location = "reply") {
    if (typeof expected === "number") {
        assert.equal(typeof actual, "number", location)
        assert.ok(Math.abs(actual - expected) <= 2e-14 * Math.max(1, Math.abs(expected)), `${location}: ${actual} != ${expected}`)
    } else if (expected && typeof expected === "object") {
        assert.equal(Array.isArray(actual), Array.isArray(expected), location)
        assert.deepEqual(Object.keys(actual), Object.keys(expected), `${location} keys/insertion order`)
        for (const key of Object.keys(expected)) compare(actual[key], expected[key], `${location}.${key}`)
    } else assert.deepEqual(actual, expected, location)
}
function temporary(t) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-audio-parity-"))
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }))
    return directory
}
function encode(directory, extension, codec, metadata = {}) {
    fs.mkdirSync(directory, { recursive: true })
    const output = path.join(directory, `base.${extension}`)
    const args = ["-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000", "-t", "0.2", "-c:a", codec, "-threads", "1", "-fflags", "+bitexact", "-flags:a", "+bitexact"]
    for (const [key, value] of Object.entries(metadata)) args.push("-metadata", `${key}=${value}`)
    const result = spawnSync(ffmpeg, [...args, "-y", output], { encoding: "utf8", timeout: 10000 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return output
}
const u32 = (number, little = false) => {
    const buffer = Buffer.alloc(4)
    little ? buffer.writeUInt32LE(number) : buffer.writeUInt32BE(number)
    return buffer
}
function synchsafe(number) {
    return Buffer.from([(number >> 21) & 127, (number >> 14) & 127, (number >> 7) & 127, number & 127])
}
function id3(frames, version = 4) {
    const data = Buffer.concat(
        frames.map(([id, text]) => {
            const bytes = Buffer.concat([Buffer.from([3]), Buffer.from(text)])
            return Buffer.concat([Buffer.from(id), version === 4 ? synchsafe(bytes.length) : u32(bytes.length), Buffer.alloc(2), bytes])
        })
    )
    return Buffer.concat([Buffer.from([73, 68, 51, version, 0, 0]), synchsafe(data.length), data])
}
function withId3(file, frames, suffix, version = 4) {
    let data = fs.readFileSync(file)
    if (data.subarray(0, 3).toString() === "ID3") {
        let size = 0
        for (const byte of data.subarray(6, 10)) size = (size << 7) | byte
        data = data.subarray(10 + size)
    }
    const output = file.replace(/\.mp3$/, `-${suffix}.mp3`)
    fs.writeFileSync(output, Buffer.concat([id3(frames, version), data]))
    return output
}
function comments(items) {
    const vendor = Buffer.from("freeshow deterministic fixture")
    return Buffer.concat([
        u32(vendor.length, true),
        vendor,
        u32(items.length, true),
        ...items.flatMap(([key, value]) => {
            const bytes = Buffer.from(`${key}=${value}`)
            return [u32(bytes.length, true), bytes]
        })
    ])
}
function withFlacComments(file, items) {
    const data = fs.readFileSync(file)
    const pieces = [data.subarray(0, 4)]
    let position = 4
    while (position < data.length) {
        const type = data[position],
            length = data.readUIntBE(position + 1, 3),
            end = position + 4 + length
        if ((type & 127) === 4) {
            const body = comments(items),
                header = Buffer.alloc(4)
            header[0] = type
            header.writeUIntBE(body.length, 1, 3)
            pieces.push(header, body)
        } else pieces.push(data.subarray(position, end))
        position = end
        if (type & 128) break
    }
    pieces.push(data.subarray(position))
    const output = file.replace(/\.flac$/, "-multi.flac")
    fs.writeFileSync(output, Buffer.concat(pieces))
    return output
}
function crcOgg(data) {
    let crc = 0
    for (const byte of data) {
        crc ^= byte << 24
        for (let bit = 0; bit < 8; bit++) crc = ((crc << 1) ^ (crc & 0x80000000 ? 0x04c11db7 : 0)) >>> 0
    }
    return crc
}
function withOggComments(file, items) {
    const data = fs.readFileSync(file)
    const pages = []
    let position = 0,
        replaced = false
    while (position < data.length) {
        const segments = [...data.subarray(position + 27, position + 27 + data[position + 26])],
            header = Buffer.from(data.subarray(position, position + 27))
        let offset = position + 27 + segments.length
        const end = offset + segments.reduce((a, b) => a + b, 0)
        const packets = []
        let chunks = []
        for (const length of segments) {
            chunks.push(data.subarray(offset, offset + length))
            offset += length
            if (length < 255) {
                packets.push(Buffer.concat(chunks))
                chunks = []
            }
        }
        assert.equal(chunks.length, 0, "generated fixture must have complete page-local packets")
        for (let index = 0; index < packets.length; index++) {
            const packet = packets[index]
            if (packet.subarray(0, 8).toString() === "OpusTags") {
                packets[index] = Buffer.concat([Buffer.from("OpusTags"), comments(items)])
                replaced = true
            } else if (packet[0] === 3 && packet.subarray(1, 7).toString() === "vorbis") {
                packets[index] = Buffer.concat([packet.subarray(0, 7), comments(items), Buffer.from([1])])
                replaced = true
            }
        }
        const lace = packets.flatMap((packet) => [...Array(Math.floor(packet.length / 255)).fill(255), packet.length % 255])
        assert.ok(lace.length <= 255)
        header[26] = lace.length
        header.fill(0, 22, 26)
        const page = Buffer.concat([header, Buffer.from(lace), ...packets])
        page.writeUInt32LE(crcOgg(page), 22)
        pages.push(page)
        position = end
    }
    assert.ok(replaced)
    const output = file.replace(/\.(ogg|opus)$/, "-multi.$1")
    fs.writeFileSync(output, Buffer.concat(pages))
    return output
}
function box(name, ...parts) {
    const body = Buffer.concat(parts)
    return Buffer.concat([u32(8 + body.length), Buffer.isBuffer(name) ? name : Buffer.from(name, "latin1"), body])
}
function withMp4Tags(file, tags) {
    const data = fs.readFileSync(file)
    const atoms = tags.map(([key, value]) => box("----", box("mean", Buffer.alloc(4), Buffer.from("com.apple.iTunes")), box("name", Buffer.alloc(4), Buffer.from(key)), box("data", u32(1), Buffer.alloc(4), Buffer.from(value))))
    const rewrite = (body, prefix = 0) => {
        const result = [body.subarray(0, prefix)]
        for (let position = prefix; position < body.length; ) {
            const length = body.readUInt32BE(position),
                name = body.subarray(position + 4, position + 8).toString("latin1"),
                content = body.subarray(position + 8, position + length)
            assert.ok(length >= 8)
            result.push(name === "ilst" ? box("ilst", content, ...atoms) : ["moov", "udta", "meta"].includes(name) ? box(name, rewrite(content, name === "meta" ? 4 : 0)) : body.subarray(position, position + length))
            position += length
        }
        return Buffer.concat(result)
    }
    const output = file.replace(/\.m4a$/, "-gain.m4a")
    fs.writeFileSync(output, rewrite(data))
    return output
}

const tagItems = [
    ["TITLE", "敬拜 — café 🙌"],
    ["ARTIST", "Alpha"],
    ["ARTIST", "Beta"],
    ["ARTIST", "Alpha"],
    ["ARTISTS", "Gamma"],
    ["GENRE", "Rock"],
    ["GENRE", "Rock"],
    ["GENRE", "Jazz"],
    ["COMMENT", "first"],
    ["COMMENT", "first"],
    ["TRACKNUMBER", "0/12"],
    ["TOTALTRACKS", "13"],
    ["DISCNUMBER", "2/3"],
    ["REPLAYGAIN_ALBUM_GAIN", "+9.0 dB"],
    ["REPLAYGAIN_TRACK_GAIN", "-6.0 dB"],
    ["REPLAYGAIN_TRACK_PEAK", "0.8"],
    ["REPLAYGAIN_ALBUM_PEAK", "2"],
    ["RATING:tester@example.test", "0.7"],
    ["RATING", "50"],
    ["ISRC", "CODE123"],
    ["ISRC", "CODE123"],
    ["FUTURE_UNKNOWN", "preserve only if the original does"]
]

test("native common schema data matches original music-metadata 7.14.0 mappings", () => {
    const schema = JSON.parse(fs.readFileSync(path.join(root, "crates/freeshow-audio/src/schema.json")))
    assert.equal(schema.version, "7.14.0")
    for (const [id, filename, name] of [
        ["id3", "id3v2/ID3v24TagMapper", "ID3v24TagMapper"],
        ["vorbis", "ogg/vorbis/VorbisTagMapper", "VorbisTagMapper"],
        ["mp4", "mp4/MP4TagMapper", "MP4TagMapper"],
        ["riff", "riff/RiffInfoTagMap", "RiffInfoTagMapper"]
    ])
        assert.deepEqual(schema.maps[id], new (require(`music-metadata/lib/${filename}`)[name])().tagMap)
    assert.deepEqual(schema.common, require("music-metadata/lib/common/GenericTagTypes").commonTags)
})

test("ReplayGain projection matches actual handler for order, recursion, coercion, missing fields and unknown common tags", async () => {
    const values = [-12, 0, 6, "gain +6.0 dB", "1e3", "1.2.3", "no gain", null, false, [], {}, { dB: -6, ratio: 0.8 }, { dB: null, ratio: "0.8" }, { dB: "6e0", ratio: 0.25 }, { ratio: [2] }, { dB: true }, { dB: "inf" }, { dB: "Infinity" }]
    const cases = [null, {}, { common: { future: { retained: [1, true, null, "🙌"] } } }]
    for (const gain of values)
        for (const peak of [undefined, 0, "0.8", { ratio: 2 }, {}, -1]) {
            const common = { future: { retained: true }, replaygain_album_gain: gain, replaygain_track_gain: -9, ...(peak === undefined ? {} : { "ReplayGain Track_Peak": peak }) }
            cases.push({ common }, { common: { nested: [common], replaygain_track_gain: -3, replaygain_album_peak: 0.1 } })
        }
    cases.push({ common: { 8: { replaygain_track_gain: 8 }, 2: { replaygain_album_gain: -2 }, replaygain_track_peak: 4 } })
    const responses = rust(cases.map((metadata) => ({ op: "project", metadata })))
    for (const [index, metadata] of cases.entries()) compare(responses[index].value, clean(await projectOracle(metadata)), `case ${index}`)
})

test("empty, missing, malformed and non-file inputs match original null replies", async (t) => {
    const directory = temporary(t),
        corrupt = path.join(directory, "corrupt.mp3")
    fs.writeFileSync(corrupt, "not audio data")
    const paths = ["", path.join(directory, "missing.flac"), directory, corrupt]
    const responses = rust(paths.map(request))
    for (const [index, file] of paths.entries()) {
        assert.equal(responses[index].ok, true)
        compare(responses[index].value, clean(await getAudioMetadata(file)))
        assert.deepEqual(responses[index].diagnostics, [])
    }
})

test("real tagged MP3/FLAC/OGG/Opus/M4A/WAV replies match the original parser including Unicode and ReplayGain", { skip: available ? false : "REAL AUDIO PARITY NOT EXECUTED: ffmpeg unavailable" }, async (t) => {
    const directory = temporary(t)
    const metadata = { title: "敬拜 — café 🙌", artist: "Alpha / Beta", album_artist: "Album Artist", album: "Songs", date: "2025-07-02", track: "3/12", disc: "2/3", genre: "Rock", comment: "A note", composer: "Composer", replaygain_album_gain: "+9.0 dB", replaygain_track_gain: "-6.0 dB", replaygain_track_peak: "0.8", replaygain_album_peak: "2.0", future_unknown: "not in common" }
    const files = []
    for (const [extension, codec] of [
        ["mp3", "libmp3lame"],
        ["flac", "flac"],
        ["ogg", "libvorbis"],
        ["opus", "libopus"],
        ["m4a", "aac"],
        ["wav", "pcm_s16le"]
    ]) {
        files.push(encode(directory, extension, codec, metadata))
        const plain = encode(path.join(directory, "plain"), extension, codec)
        if (["flac", "ogg", "opus"].includes(extension)) files.push(extension === "flac" ? withFlacComments(files.at(-1), tagItems) : withOggComments(files.at(-1), tagItems))
        if (extension === "mp3") {
            files.push(
                withId3(
                    files.at(-1),
                    [
                        ["TIT2", "敬拜 café 🙌"],
                        ["TPE1", "Alpha\0Beta\0Alpha"],
                        ["TCOM", "One\0Two"],
                        ["TCON", "(17)(20)"],
                        ["TRCK", "0/12"],
                        ["TXXX", "REPLAYGAIN_ALBUM_GAIN\0+9 dB"],
                        ["TXXX", "REPLAYGAIN_TRACK_GAIN\0-6 dB"],
                        ["TXXX", "REPLAYGAIN_TRACK_PEAK\0.8"],
                        ["TXXX", "UNKNOWN_FUTURE\0ignored"]
                    ],
                    "multi"
                )
            )
            files.push(
                withId3(
                    files.at(-1),
                    [
                        ["TIT2", "ID3 v3"],
                        ["TPE1", "One/Two/Three"],
                        ["TCOM", "Composer/Other"],
                        ["TCON", "Rock/Jazz"]
                    ],
                    "v3",
                    3
                )
            )
            files.push(
                withId3(
                    files.at(-1),
                    [
                        ["TXXX", "REPLAYGAIN_TRACK_GAIN\0not a gain"],
                        ["TXXX", "REPLAYGAIN_TRACK_PEAK\u00000"]
                    ],
                    "invalid-gain"
                )
            )
        }
        if (extension === "m4a")
            files.push(
                withMp4Tags(files.at(-1), [
                    ["replaygain_album_gain", "+9 dB"],
                    ["replaygain_track_gain", "-6 dB"],
                    ["replaygain_track_peak", "0.8"],
                    ["FUTURE_UNKNOWN", "ignored"]
                ])
            )
        files.push(plain)
    }
    const responses = rust(files.map(request))
    for (const [index, file] of files.entries()) {
        assert.equal(responses[index].ok, true)
        assert.deepEqual(responses[index].diagnostics, [], path.basename(file))
        compare(responses[index].value, clean(await getAudioMetadata(file)), path.basename(file))
        assert.equal(responses[index].value.common.future_unknown, undefined)
    }
    assert.ok(
        responses.some((reply) => reply.value?.replayGainMultiplier === 1.25),
        "gain/peak selection and clamping must execute"
    )
    t.diagnostic(`Compared ${files.length} nonempty native replies against music-metadata 7.14.0 with real deterministic tagged audio.`)
})
