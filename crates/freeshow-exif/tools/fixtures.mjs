// Original tiny grayscale JPEG and TIFF/EXIF fixtures; no third-party photographs.
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

const segment = (marker, bytes) => {
    const size = Buffer.alloc(2)
    size.writeUInt16BE(bytes.length + 2)
    return Buffer.concat([Buffer.from([255, marker]), size, bytes])
}
export const plainJpeg = Buffer.concat([Buffer.from([255, 216]), segment(0xdb, Buffer.from([0, ...Array(64).fill(1)])), segment(0xc0, Buffer.from([8, 0, 8, 0, 8, 1, 1, 0x11, 0])), segment(0xc4, Buffer.from([0, 1, ...Array(15).fill(0), 0, 0x10, 1, ...Array(15).fill(0), 0])), segment(0xda, Buffer.from([1, 1, 0, 0, 63, 0])), Buffer.from([0x3f, 255, 217])])
const sizes = { 1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 6: 1, 7: 1, 8: 2, 9: 4, 10: 8, 11: 4, 12: 8 }
export function tiff(directories, big = false) {
    const names = Object.keys(directories)
    const offsets = {}
    let length = 8
    for (const name of names) {
        offsets[name] = length
        length += 2 + 12 * directories[name].length + 4
    }
    const numeric = (buffer, method, value, offset) => buffer[method + (big ? "BE" : "LE")](value, offset)
    const chunks = []
    const header = Buffer.alloc(length)
    header.write(big ? "MM" : "II")
    numeric(header, "writeUInt16", 42, 2)
    numeric(header, "writeUInt32", offsets.image, 4)
    for (const name of names) {
        const entries = directories[name]
        numeric(header, "writeUInt16", entries.length, offsets[name])
        entries.forEach((entry, index) => {
            const offset = offsets[name] + 2 + index * 12
            const values = entry.values || []
            const format = entry.format || 4
            const data = entry.data || Buffer.alloc(values.length * (sizes[format] || 1))
            if (!entry.data) {
                values.forEach((value, i) => {
                    if (format === 1 || format === 6) data[format === 1 ? "writeUInt8" : "writeInt8"](value, i)
                    else if (format === 3 || format === 8) numeric(data, format === 3 ? "writeUInt16" : "writeInt16", value, i * 2)
                    else if (format === 4 || format === 9) numeric(data, format === 4 ? "writeUInt32" : "writeInt32", value, i * 4)
                    else if (format === 5 || format === 10) {
                        numeric(data, format === 5 ? "writeUInt32" : "writeInt32", value[0], i * 8)
                        numeric(data, format === 5 ? "writeUInt32" : "writeInt32", value[1], i * 8 + 4)
                    }
                })
            }
            numeric(header, "writeUInt16", entry.tag, offset)
            numeric(header, "writeUInt16", format, offset + 2)
            numeric(header, "writeUInt32", entry.count ?? (entry.pointer ? 1 : entry.data ? data.length / (sizes[format] || 1) : values.length), offset + 4)
            if (entry.pointer) numeric(header, "writeUInt32", offsets[entry.pointer], offset + 8)
            else if (entry.offset !== undefined) numeric(header, "writeUInt32", entry.offset, offset + 8)
            else if (data.length <= 4 && ![5, 10].includes(format)) data.copy(header, offset + 8)
            else {
                numeric(header, "writeUInt32", length, offset + 8)
                chunks.push(data)
                length += data.length
            }
        })
        if (name === "image" && offsets.thumbnail) numeric(header, "writeUInt32", offsets.thumbnail, offsets[name] + 2 + entries.length * 12)
    }
    return Buffer.concat([header, ...chunks])
}
export function jpeg(data, { firstApp1, announcedLength } = {}) {
    const exif = segment(0xe1, Buffer.concat([Buffer.from("Exif\0\0"), data]))
    if (announcedLength !== undefined) exif.writeUInt16BE(announcedLength, 2)
    return Buffer.concat([plainJpeg.subarray(0, 2), ...(firstApp1 ? [segment(0xe1, firstApp1)] : []), exif, plainJpeg.subarray(2)])
}
export function fullDirectories() {
    return {
        image: [
            { tag: 0x0100, format: 1, values: [255, 0, 2, 3, 4] },
            { tag: 0x0101, format: 3, values: [720] },
            { tag: 0x010f, format: 2, data: Buffer.from([0xc9, ...Buffer.from("camera"), 0, 0]) },
            { tag: 0x0112, format: 8, values: [-2, 32767, -32768] },
            { tag: 0x011a, format: 5, values: [[300, 1]] },
            {
                tag: 0x011b,
                format: 10,
                values: [
                    [-1, 2],
                    [1, 0],
                    [0, 0],
                    [0, -1],
                    [-1, 0]
                ]
            },
            { tag: 0x010e, format: 6, values: [-128, 127, -1] },
            { tag: 0x0111, format: 4, values: [0, 0xffffffff, 12345] },
            { tag: 0x0116, format: 9, values: [-2147483648, 2147483647, -1] },
            { tag: 0x0131, format: 7, data: Buffer.from([1, 2, 3, 4, 5, 6]) },
            { tag: 0x0132, format: 2, data: Buffer.from("2026:10:18 12:34:56\0") },
            { tag: 0xdead, format: 1, count: 0xffffffff },
            { tag: 0x013b, format: 12, count: 1, data: Buffer.alloc(8) },
            { tag: 0x8769, pointer: "exif" },
            { tag: 0x8825, pointer: "gps" }
        ],
        exif: [
            { tag: 0x9003, format: 2, data: Buffer.from("2001:02:03 04:05:06\0") },
            { tag: 0x9000, format: 7, data: Buffer.from("0231") },
            { tag: 0x927c, format: 7, data: Buffer.from("unknown-maker\0") },
            { tag: 0x9286, format: 7, data: Buffer.from([65, 83, 67, 73, 73, 0, 0, 0, 0xff]) },
            { tag: 0x829a, format: 5, values: [[1, 125]] },
            { tag: 0xa002, format: 3, values: [1920] },
            { tag: 0xa005, pointer: "interop" }
        ],
        gps: [
            { tag: 0, format: 1, values: [2, 3, 0, 0] },
            { tag: 1, format: 2, data: Buffer.from("N\0") },
            {
                tag: 2,
                format: 5,
                values: [
                    [59, 1],
                    [30, 1],
                    [1234, 1000]
                ]
            },
            { tag: 0x1d, format: 2, data: Buffer.from("2026:10:18\0") }
        ],
        interop: [
            { tag: 1, format: 2, data: Buffer.from("R98\0") },
            { tag: 0xbeef, format: 1, values: [1] },
            { tag: 0x0100, format: 3, values: [999] }
        ],
        thumbnail: [
            { tag: 0x0100, format: 3, values: [8] },
            { tag: 0x0201, values: [2048] },
            { tag: 0x0202, values: [plainJpeg.length] }
        ]
    }
}
export function corpus() {
    const fixtures = new Map()
    for (const big of [false, true]) {
        const raw = tiff(fullDirectories(), big)
        fixtures.set(`full-${big ? "MM" : "II"}.jpg`, jpeg(raw))
        fixtures.set(`raw-${big ? "MM" : "II"}.tiff`, raw)
        fixtures.set(`short-segment-${big}.jpg`, jpeg(raw, { announcedLength: 2 }))
    }
    fixtures.set("plain.jpg", plainJpeg)
    fixtures.set("xmp-first.jpg", jpeg(tiff(fullDirectories()), { firstApp1: Buffer.from("http://ns.adobe.com/xap/1.0/\0") }))
    fixtures.set("bad-order.jpg", jpeg(Buffer.from("ZZ12345678")))
    fixtures.set("empty.jpg", Buffer.alloc(0))
    fixtures.set("empty-ifd.jpg", jpeg(tiff({ image: [] })))
    for (const prefix of ["OLYMP\0\x01", "OLYMP\0\x02", "AGFA \0\x01", "EPSON\0\x01\0", "FUJIFILM", "Panasonic", "SANYO"]) {
        fixtures.set(`maker-${fixtures.size}.jpg`, jpeg(tiff({ image: [{ tag: 0x8769, pointer: "exif" }], exif: [{ tag: 0x927c, format: 7, data: Buffer.concat([Buffer.from(prefix), Buffer.alloc(64, 1)]) }] })))
    }
    fixtures.set("short-epson.jpg", jpeg(tiff({ image: [{ tag: 0x8769, pointer: "exif" }], exif: [{ tag: 0x927c, format: 7, data: Buffer.from("EPSON\0\x01") }] })))
    fixtures.set("wrong-maker-type.jpg", jpeg(tiff({ image: [{ tag: 0x8769, pointer: "exif" }], exif: [{ tag: 0x927c, values: [1] }] })))
    const capped = Array.from({ length: 129 }, (_, i) => ({ tag: i === 128 ? 0 : 0x0100, format: 3, values: [i] }))
    fixtures.set("ifd0-capped.jpg", jpeg(tiff({ image: capped })))
    fixtures.set("exif-not-capped.jpg", jpeg(tiff({ image: [{ tag: 0x8769, pointer: "exif" }], exif: capped })))
    fixtures.set(
        "cycle.jpg",
        jpeg(
            tiff({
                image: [
                    { tag: 0x8769, pointer: "image" },
                    { tag: 0xa005, pointer: "image" }
                ]
            })
        )
    )
    fixtures.set("zero-components.jpg", jpeg(tiff({ image: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10].map((format) => ({ tag: 0x0100 + format, format, count: 0 })) })))
    for (const format of [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]) fixtures.set(`out-of-bounds-${format}.jpg`, jpeg(tiff({ image: [{ tag: 0x0100, format, count: 7, offset: 0xfffffff0 }] })))
    const full = jpeg(tiff(fullDirectories()))
    for (const length of [1, 2, 3, 4, 5, 6, 10, 14, 17, 19, 20, 21, 30, 50, 75, 128, full.length - 16]) fixtures.set(`truncated-${length}.jpg`, full.subarray(0, length))
    return fixtures
}
export function generatedCorpus() {
    const fixtures = new Map()
    let seed = 0x65786966
    const random = () => (seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0)
    for (let index = 0; index < 160; index++) {
        const entries = []
        for (let i = 0; i < 8; i++) {
            const format = 1 + (random() % 12)
            const count = random() % 33
            const data = Buffer.from(Array.from({ length: count * ([5, 10, 12].includes(format) ? 8 : [4, 9, 11].includes(format) ? 4 : [3, 8].includes(format) ? 2 : 1) }, () => random() & 255))
            entries.push({ tag: [0x0100, 0x0101, 0x010e, 0x010f, 0x011a, 0x0132, 0xea1c, 0xdead][i], format, count, data, ...(index % 4 === 0 ? { offset: [0, 1, 0xfffffff0, 0x7fffffff][random() % 4] } : {}) })
        }
        fixtures.set("generated-" + index + ".jpg", jpeg(tiff({ image: entries }, !!(index % 2))))
    }
    return fixtures
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
    const destination = path.join(path.dirname(fileURLToPath(import.meta.url)), "../fixtures")
    fs.mkdirSync(destination, { recursive: true })
    const all = corpus()
    for (const name of ["full-II.jpg", "full-MM.jpg", "raw-II.tiff"]) fs.writeFileSync(path.join(destination, name), all.get(name))
}
