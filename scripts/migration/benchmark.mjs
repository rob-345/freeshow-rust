import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { performance } from "node:perf_hooks"
import { root, showOracle, fixtureShow } from "../../tests/parity/oracle.mjs"

const binary = process.env.FREESHOW_CORE_BIN || path.join(root, "target/release", process.platform === "win32" ? "freeshow-core.exe" : "freeshow-core")
const corpus = Array.from({ length: 1000 }, (_, seed) => fixtureShow(seed))
const rounds = 9,
    iterations = 20
const javascript = []
let checksum = 0
for (let round = 0; round < rounds + 3; round++) {
    let sum = 0
    const start = performance.now()
    for (let repeat = 0; repeat < iterations; repeat++) for (const value of corpus) sum += showOracle.getShowTextContent(value).length
    const elapsed = performance.now() - start
    if (round >= 3) javascript.push(elapsed)
    checksum = sum
}
const result = spawnSync(binary, [], { input: JSON.stringify({ op: "bench.show.text", corpus, rounds, iterations }) + "\n", encoding: "utf8", maxBuffer: 64 * 1024 * 1024, timeout: 60000 })
assert.equal(result.error, undefined)
assert.equal(result.status, 0, result.stderr)
const response = JSON.parse(result.stdout)
assert.equal(response.ok, true, JSON.stringify(response.error))
assert.equal(response.value.checksum, checksum, "Benchmark outputs must match")
const median = (values) => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)]
const report = {
    measuredAt: new Date().toISOString(),
    scenario: "warm lyric extraction from 1000 already-parsed generated shows; 20000 extractions per sample",
    scope: "microbenchmark only; excludes startup, JSON/IPC, disk IO, rendering, audio/video and network; cannot establish application performance parity",
    environment: { platform: process.platform, arch: process.arch, cpu: os.cpus()[0]?.model, node: process.version, rust: spawnSync("rustc", ["--version"], { encoding: "utf8" }).stdout.trim() },
    warmupRounds: 3,
    rounds,
    iterations,
    corpusSize: corpus.length,
    checksum,
    javascript: { medianMs: median(javascript), samplesMs: javascript },
    rust: { medianMs: median(response.value.samplesMs), samplesMs: response.value.samplesMs },
    speedup: median(javascript) / median(response.value.samplesMs)
}
const output = path.join(root, "test-output-rust/benchmarks/lyric-extraction.json")
fs.mkdirSync(path.dirname(output), { recursive: true })
fs.writeFileSync(output, JSON.stringify(report, null, 2) + "\n")
console.log(`Lyric extraction median: JS ${report.javascript.medianMs.toFixed(2)} ms; Rust ${report.rust.medianMs.toFixed(2)} ms; speedup ${report.speedup.toFixed(2)}x`)
console.log(`Full sample data: ${output}`)
console.log(report.scope)
