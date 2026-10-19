import assert from "node:assert/strict"
import { test } from "node:test"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { numberBits, project, sourceResults } from "../../crates/freeshow-browse/tools/macos-source.mjs"

test("pinned actual Node Stats source projects binary64 fields with signed timestamp operation order", () => {
    const row = ["-1", "33184", "2", "501", "20", "-2", "4096", "9007199254740993", "9223372036854775807", "-1", "-1", "-250000000", "1700000000", "12348", "123", "123456789", "456", "654321000"],
        receipt = project([row]),
        stat = receipt.results[0].data
    assert.equal(receipt.actualFilesystemObserved, false)
    assert.equal(receipt.mode, "pure-node-projection")
    assert.equal(stat.ino, Number(9007199254740993n))
    assert.equal(stat.dev, Number(BigInt.asUintN(64, -1n)))
    assert.equal(stat.atimeMs, -1250)
    assert.notEqual(stat.mtimeMs, Number(1700000000n * 1000000000n + 12348n) / 1000000, "this exact source result detects premature integer-nanosecond aggregation")
    assert.notEqual(stat.ctimeMs, stat.birthtimeMs)
    const bits = receipt.results[0].numberBits,
        bytes = Buffer.alloc(8)
    bytes.writeDoubleBE(stat.mtimeMs)
    assert.equal(bits["/mtimeMs"], bytes.toString("hex"), "source bits are captured before decimal transport")
    assert.equal(Object.keys(bits).length, 14)
    assert.equal(bits["/ino"], "4340000000000000")
    assert.deepEqual(JSON.parse(JSON.stringify(receipt)).results[0].numberBits, { ...bits })
    assert.equal(numberBits({ "~/": [stat.mtimeMs] })["/~0~1/0"], bits["/mtimeMs"], "JSON pointer escaping preserves paths")
    for (const key of ["atime", "mtime", "birthtime", "isFile"]) assert.equal(Object.hasOwn(stat, key), false)
    assert.throws(() => project([["0"]]))
    assert.throws(() => project([row.map(() => "unbounded")]))
})

test("actual immutable POSIX browse functions execute on a bounded local tree (source helper control, not native macOS proof)", async () => {
    const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-browse-macos-source-")).split(path.sep).join("/")
    try {
        const first = path.posix.join(temporary, "song-1.mp3"),
            second = path.posix.join(temporary, "song-2.mp3")
        fs.writeFileSync(first, "real stat bytes")
        fs.writeFileSync(second, "real source candidate")
        const result = await sourceResults([
            { command: "FILE_INFO", data: first },
            { command: "READ_FOLDER", data: { path: temporary } },
            { command: "GET_SIMILAR", data: { paths: [first] } }
        ])
        assert.equal(result.platform, process.platform)
        assert.equal(result.results[0].data.stat.size, 15)
        const sourceStat = result.results[0].data.stat,
            sourceBits = result.results[0].numberBits,
            bytes = Buffer.alloc(8)
        assert.equal(Object.keys(sourceBits).length, 14)
        for (const [key, value] of Object.entries(sourceStat)) {
            bytes.writeDoubleBE(value)
            assert.equal(sourceBits[`/stat/${key}`], bytes.toString("hex"), "real source filesystem numeric bits, independent of decimal transport")
        }
        assert.equal(result.results[0].data.folder, false)
        assert.deepEqual(result.results[1].undefinedNoMedia, [temporary])
        assert.deepEqual(result.results[1].data[temporary].files, [first, second])
        assert.deepEqual(result.results[2].data, [{ path: second, name: "song-2" }])
        assert.equal(result.actualFilesystemObserved, undefined, "no native Darwin label on portable helper control")
    } finally {
        fs.rmSync(temporary, { recursive: true, force: true })
    }
})
