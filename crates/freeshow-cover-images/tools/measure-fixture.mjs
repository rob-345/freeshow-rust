// One actual maximum-pixel fixture, not a throughput/performance claim.
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import assert from "node:assert/strict"
import { png } from "./fixtures.mjs"
const root = fileURLToPath(new URL("../../../", import.meta.url))
const binary = process.env.FREESHOW_COVER_IMAGES_PARITY || path.join(root, "target/debug/freeshow-cover-images-parity")
const bytes = png(2048, 2048),
    hash = (b) => createHash("sha256").update(b).digest("hex")
const run = spawnSync("/usr/bin/time", ["-f", "%M", binary], { input: JSON.stringify({ ops: [{ op: "create", base64: bytes.toString("base64"), alternate: true }, { op: "png" }, { op: "jpeg" }, { op: "release" }] }), encoding: "utf8", timeout: 30000, maxBuffer: 8 * 1024 * 1024 })
assert.equal(run.status, 0, run.stderr)
const actual = JSON.parse(run.stdout)
for (const result of actual.results) assert.equal(result.error, undefined)
assert.equal(actual.status.finished_workers, 2)
assert.equal(actual.status.reserved_bytes, 0)
const receipt = {
    platform: process.platform,
    arch: process.arch,
    binarySha256: hash(fs.readFileSync(binary)),
    profile: "debug with debug=0 incremental=false",
    width: 2048,
    height: 2048,
    pixels: 4194304,
    inputPngBytes: bytes.length,
    inputPngSha256: hash(bytes),
    png: actual.results[1].bytes,
    jpeg: actual.results[2].bytes,
    maximumResidentKiB: Number(run.stderr.trim()),
    status: actual.status,
    scope: "Whole private driver RSS for this one actual fixture, including JSON/base64; not a worst-case bound or performance comparison."
}
delete receipt.png.base64
delete receipt.jpeg.base64
assert.ok(Number.isSafeInteger(receipt.maximumResidentKiB) && receipt.maximumResidentKiB > 0)
if (process.env.FREESHOW_COVER_IMAGES_MEMORY_RECEIPT) fs.writeFileSync(process.env.FREESHOW_COVER_IMAGES_MEMORY_RECEIPT, JSON.stringify(receipt, null, 2) + "\n")
console.log(JSON.stringify(receipt))
