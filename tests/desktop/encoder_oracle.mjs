// Compare live detection and cache values; preserve raw evidence before masking
// only process-specific addresses in FFmpeg's bracketed log prefixes.
import assert from "node:assert/strict"
import fs from "node:fs"
import { original } from "../parity/lib/encoders-oracle.mjs"
let input = ""
for await (const chunk of process.stdin) input += chunk
const fixture = JSON.parse(input)
if (fixture.mtimeMs === undefined) {
    try {
        fixture.mtimeMs = fs.statSync(fixture.sourcePath).mtimeMs
    } catch {
        fixture.mtimeMs = 0
    }
}
const source = original(fixture)
const detection = await source.detection.detectEncoders(fixture.force || false)
const normalize = (value) => JSON.parse(JSON.stringify(value), (key, value) => (key === "reason" && typeof value === "string" ? value.replace(/(\[[^\]\r\n]* @ )0x[0-9a-f]+(?=\])/gi, "$10xPTR") : value))
assert.deepEqual(normalize(detection), normalize(fixture.actual))
if (source.writes.length) {
    const actualCache = JSON.parse(fixture.cacheAfter)
    assert.deepEqual(normalize(JSON.parse(source.writes.at(-1))), normalize(actualCache))
    assert.deepEqual(actualCache.detection, fixture.actual)
} else assert.equal(fixture.cacheAfter, fixture.cache)
process.stdout.write(JSON.stringify({ detection, calls: source.calls, writes: source.writes.length, cacheValuesMatchSource: source.writes.length > 0, addressNormalization: "Only 0x addresses in bracketed FFmpeg log prefixes; raw replies and cache bytes retained" }) + "\n")
