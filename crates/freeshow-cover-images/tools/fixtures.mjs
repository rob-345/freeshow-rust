// Source-only deterministic containers; no downloaded audio/image or executable.
import { deflateSync } from "node:zlib"
export const u32 = (n, little = false) => {
    const b = Buffer.alloc(4)
    little ? b.writeUInt32LE(n) : b.writeUInt32BE(n)
    return b
}
export function crc(bytes) {
    let n = 0xffffffff
    for (const b of bytes) {
        n ^= b
        for (let i = 0; i < 8; i++) n = (n >>> 1) ^ (n & 1 ? 0xedb88320 : 0)
    }
    return (n ^ 0xffffffff) >>> 0
}
export const chunk = (type, bytes) => {
    const name = Buffer.from(type)
    return Buffer.concat([u32(bytes.length), name, bytes, u32(crc(Buffer.concat([name, bytes])))])
}
export function png(w, h, { rgb = false, alpha = false, partial = false, solid = false, extra = [] } = {}) {
    const header = Buffer.alloc(13)
    header.writeUInt32BE(w)
    header.writeUInt32BE(h, 4)
    header[8] = 8
    header[9] = rgb ? 2 : 6
    const channels = rgb ? 3 : 4,
        rows = Buffer.alloc((w * channels + 1) * h)
    for (let y = 0; y < h; y++)
        for (let x = 0; x < w; x++) {
            const p = y * (w * channels + 1) + 1 + x * channels
            rows[p] = solid ? 255 : (x * 17 + y * 3) % 256
            rows[p + 1] = solid ? 0 : (x * 3 + y * 11) % 256
            rows[p + 2] = solid ? 0 : (x * 7 + y * 13) % 256
            if (!rgb) rows[p + 3] = partial ? 128 : alpha && x % 2 ? 0 : 255
        }
    return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", header), ...extra, chunk("IDAT", deflateSync(rows)), chunk("IEND", Buffer.alloc(0))])
}
export const syncsafe = (n) => Buffer.from([(n >> 21) & 127, (n >> 14) & 127, (n >> 7) & 127, n & 127])
export function apic(data, { mime = "image/png", type = 3, description = "音楽 🎵", encoding = 3 } = {}) {
    let desc = Buffer.from(description, encoding === 0 ? "latin1" : encoding === 3 ? "utf8" : "utf16le")
    if (encoding === 1) desc = Buffer.concat([Buffer.from([255, 254]), desc])
    if (encoding === 2) desc.swap16()
    return Buffer.concat([Buffer.from([encoding]), Buffer.from(mime), Buffer.from([0, type]), desc, Buffer.alloc(encoding === 1 || encoding === 2 ? 2 : 1), data])
}
export function mp3(pictures, version = 4, extraFrames = []) {
    const frames = [...extraFrames, ...pictures.map((p) => ["APIC", p])].map(([name, bytes]) => Buffer.concat([Buffer.from(name), version === 4 ? syncsafe(bytes.length) : u32(bytes.length), Buffer.alloc(2), bytes]))
    const tag = Buffer.concat(frames)
    const frame = Buffer.alloc(417)
    frame.set([255, 251, 144, 100])
    return Buffer.concat([Buffer.from([73, 68, 51, version, 0, 0]), syncsafe(tag.length), tag, ...Array(8).fill(frame)])
}
export function picture(data, { mime = "IMAGE/PNG", type = 3, description = "Café 音楽 🎵", width = 123, height = 456, colourDepth = 24, indexedColor = 0 } = {}) {
    const m = Buffer.from(mime),
        d = Buffer.from(description)
    return Buffer.concat([u32(type), u32(m.length), m, u32(d.length), d, u32(width), u32(height), u32(colourDepth), u32(indexedColor), u32(data.length), data])
}
export function comments(entries = []) {
    return Buffer.concat([
        u32(3, true),
        Buffer.from("src"),
        u32(entries.length, true),
        ...entries.map(([k, v]) => {
            const b = Buffer.from(`${k}=${v}`)
            return Buffer.concat([u32(b.length, true), b])
        })
    ])
}
export function flac(pictures, extra = []) {
    const info = Buffer.alloc(34)
    info.writeUInt16BE(4096)
    info.writeUInt16BE(4096, 2)
    info.writeBigUInt64BE((44100n << 44n) | (1n << 41n) | (15n << 36n) | 4410n, 10)
    const blocks = [[0, info], [4, comments([["TITLE", "real container"]])], ...extra, ...pictures.map((p) => [6, p])]
    return Buffer.concat([
        Buffer.from("fLaC"),
        ...blocks.map(([type, bytes], i) => {
            const h = Buffer.alloc(4)
            h[0] = type | (i === blocks.length - 1 ? 128 : 0)
            h.writeUIntBE(bytes.length, 1, 3)
            return Buffer.concat([h, bytes])
        })
    ])
}
export function wav() {
    const fmt = Buffer.alloc(16)
    fmt.writeUInt16LE(1)
    fmt.writeUInt16LE(1, 2)
    fmt.writeUInt32LE(8000, 4)
    fmt.writeUInt32LE(16000, 8)
    fmt.writeUInt16LE(2, 12)
    fmt.writeUInt16LE(16, 14)
    const chunks = Buffer.concat([Buffer.from("fmt "), u32(fmt.length, true), fmt, Buffer.from("data"), u32(160, true), Buffer.alloc(160)])
    return Buffer.concat([Buffer.from("RIFF"), u32(chunks.length + 4, true), Buffer.from("WAVE"), chunks])
}
