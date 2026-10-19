// Historical arithmetic replay. Never starts apps/providers or collects samples.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { createHash } from "node:crypto"
import { fileURLToPath } from "node:url"
const here = path.dirname(fileURLToPath(import.meta.url))
const read = (file, cap = 2 * 1024 * 1024) => {
    const info = fs.statSync(file),
        size = info.size
    assert.ok(info.isFile() && size >= 0 && size <= cap, "Bounded ordinary input required")
    return fs.readFileSync(file)
}
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex")
const hashFile = (file, cap) => {
    const info = fs.statSync(file)
    assert.ok(info.isFile() && info.size <= cap)
    const fd = fs.openSync(file, "r"),
        block = Buffer.alloc(1048576),
        digest = createHash("sha256")
    try {
        for (let bytes; (bytes = fs.readSync(fd, block, 0, block.length, null)); ) digest.update(block.subarray(0, bytes))
        return digest.digest("hex")
    } finally {
        fs.closeSync(fd)
    }
}
const nearest = (values, quantile = 0.5) => [...values].sort((a, b) => a - b)[Math.max(0, Math.ceil(values.length * quantile) - 1)]
const close = (actual, expected) => assert.ok(Number.isFinite(actual) && Math.abs(actual - expected) <= 1e-12, `${actual} != ${expected}`)
const unpack = (columns, values) => Object.fromEntries(columns.map((key, index) => [key, values[index]]))
function runNumbers(receipt, run) {
    assert.equal(run.steady.length, 42)
    const samples = run.steady.map((row) => {
        assert.equal(row.length, receipt.columns.sample.length)
        const sample = unpack(receipt.columns.sample, row)
        sample.processes = sample.processes.map((row) => unpack(receipt.columns.process, row))
        return sample
    })
    const first = samples[0],
        last = samples.at(-1),
        hz = receipt.environment.clockTicks
    const baseline = new Map(first.processes.map((p) => [p.identity, p.cpuTicks]))
    const maxima = new Map(baseline),
        late = []
    for (const [index, sample] of samples.entries()) {
        assert.ok(index === 0 || sample.monotonicMs > samples[index - 1].monotonicMs)
        assert.deepEqual(sample.scanErrors, [])
        assert.equal(sample.unmatchedMarkerReadFailures, 10, "Retain actual unmatched attribution failures")
        const living = sample.processes.filter((p) => p.state !== "Z")
        assert.equal(living.length, 4)
        assert.ok(living.every((p) => Number.isFinite(p.pssBytes) && !p.changedDuringRead && !p.exitedDuringRead))
        assert.equal(sample.processCount, living.length)
        assert.equal(sample.pssComplete, true)
        assert.equal(
            sample.pssSumBytes,
            living.reduce((sum, p) => sum + p.pssBytes, 0)
        )
        assert.equal(
            sample.rssSumBytes,
            living.reduce((sum, p) => sum + p.rssBytes, 0)
        )
        if (index)
            for (const process of sample.processes) {
                if (!baseline.has(process.identity)) {
                    const bornWithin = process.startTicks / hz >= first.uptimeSeconds + 1 / hz
                    baseline.set(process.identity, bornWithin ? 0 : process.cpuTicks)
                    if (!bornWithin) late.push(process.identity)
                }
                maxima.set(process.identity, Math.max(maxima.get(process.identity) || 0, process.cpuTicks))
            }
    }
    const ticks = [...maxima].reduce((sum, [id, value]) => sum + Math.max(0, value - baseline.get(id)), 0)
    const seconds = ticks / hz,
        wall = (last.observationEndMs - first.monotonicMs) / 1000
    const cpu = (100 * seconds) / wall,
        pss = nearest(samples.map((s) => s.pssSumBytes))
    assert.equal(run.cpu.method, "observed-process-counters-lower-bound")
    close(seconds, run.cpu.cpuSeconds)
    close(wall, run.cpu.wallSeconds)
    close(cpu, run.cpu.percentOneCore)
    assert.deepEqual(late, run.cpu.lateExisting)
    assert.equal(pss, run.memory.pssSumBytes.p50)
    assert.equal(run.memory.pssComplete, true)
    assert.deepEqual(run.memory.coverage, { totalSamples: 42, completePssSamples: 42, incompletePssSamples: 0, missingPssIdentities: [] })
    for (const phase of [run.before, run.after]) {
        assert.equal(phase.visibility, "visible")
        assert.equal(phase.ready, true)
        assert.equal(phase.fontStatus, "loaded")
        assert.deepEqual(phase.productionHooks, [])
        assert.deepEqual(phase.viewport, { dpr: 1, height: 800, width: 1280 })
    }
    assert.equal(run.valid, true)
    assert.equal(run.temporaryProfileRemoved, true)
    assert.equal(run.cleanup.complete, true)
    assert.deepEqual(run.optionalCleanupErrors, {})
    return { cpu, pss, ticks, wall }
}
export function validate(receipt) {
    assert.equal(receipt.kind, "archived-production-audio-analyser-idle-ablation")
    assert.equal(receipt.runs.length, 12)
    assert.equal(receipt.settings.pairs, 5)
    assert.equal(receipt.settings.warmups, 1)
    assert.equal(receipt.settings.settle, 20)
    assert.equal(receipt.settings.seconds, 10)
    assert.equal(receipt.settings.interval, 250)
    assert.deepEqual(receipt.collector.sourceChanges, ["src/frontend/audio/audioAnalyser.ts"])
    const [before, after] = [receipt.variants.before, receipt.variants.after]
    for (const key of ["inputs", "tools", "buildEnvironment", "viteEnvironment", "build"]) assert.deepEqual(before[key], after[key])
    assert.equal(before.resources.sha256, after.resources.sha256)
    assert.notEqual(before.executable.sha256, after.executable.sha256)
    assert.equal(receipt.supervisor.complete, true)
    assert.equal(receipt.supervisor.exitCode, 0)
    assert.deepEqual(receipt.supervisor.remainingAdoptedChildren, [])
    const provider = receipt.providerOperator
    assert.equal(provider.ok, true)
    assert.deepEqual(provider.before.identity, provider.after.identity)
    assert.deepEqual(provider.before.socket, provider.after.socket)
    assert.deepEqual(provider.before.modules, provider.after.modules)
    assert.equal(provider.providerCleanup.actualWaitReturned, true)
    assert.equal(provider.providerCleanup.forcedKill, false)
    assert.equal(provider.providerCleanup.returnCode, 0)
    assert.equal(provider.collectorCleanup.actualWaitReturned, true)
    assert.equal(provider.collectorCleanup.forcedKill, false)
    assert.equal(provider.collectorCleanup.returnCode, 0)
    assert.deepEqual(provider.providerLog, { keptBytes: 901, omittedBytes: 0, eof: true, actualThreadJoined: true })
    assert.equal(provider.privateSocketDirectoryRemovedAfterJoin, true)
    assert.deepEqual(provider.failures, [])
    assert.equal(receipt.supervisor.supervisorPid, provider.collectorWait.pid)
    const reaped = new Set(receipt.supervisor.reaped.map((r) => r.pid))
    const numbers = receipt.runs.map((run, index) => {
        for (const zombie of run.cleanup.zombies) assert.ok(reaped.has(Number(zombie.split(":")[0])), "Observed zombie missing final reap")
        assert.equal(run.index, index)
        assert.equal(run.warmup, index < 2)
        return runNumbers(receipt, run)
    })
    const pairs = []
    for (let pair = 0; pair < 5; pair++) {
        const a = receipt.runs.filter((r) => r.pair === pair && r.runtime === "after"),
            b = receipt.runs.filter((r) => r.pair === pair && r.runtime === "before")
        assert.equal(a.length, 1)
        assert.equal(b.length, 1)
        const after = numbers[a[0].index],
            before = numbers[b[0].index]
        pairs.push({ pair: pair + 1, beforeIndex: b[0].index, afterIndex: a[0].index, cpuDeltaPercentagePoints: after.cpu - before.cpu, cpuRelativePercent: (100 * (after.cpu - before.cpu)) / before.cpu, pssDeltaBytes: after.pss - before.pss, pssRelativePercent: (100 * (after.pss - before.pss)) / before.pss })
    }
    const medianPaired = Object.fromEntries(["cpuDeltaPercentagePoints", "cpuRelativePercent", "pssDeltaBytes", "pssRelativePercent"].map((key) => [key, nearest(pairs.map((p) => p[key]))]))
    assert.deepEqual(pairs, receipt.summary.pairDifferencesAfterMinusBefore)
    assert.deepEqual(medianPaired, receipt.summary.medianPaired)
    for (const side of ["before", "after"]) {
        const selected = receipt.runs.filter((r) => r.runtime === side && !r.warmup).map((r) => numbers[r.index])
        assert.equal(selected.length, 5)
        assert.deepEqual(receipt.summary.perSide[side], { cpuPercentOneCoreMedian: nearest(selected.map((n) => n.cpu)), pssRunMedianBytesMedian: nearest(selected.map((n) => n.pss)) })
    }
    return receipt.summary
}
function verifyRaw(receipt) {
    const root = receipt.rawRoot
    assert.ok(receipt.rawFiles.length <= 256)
    for (const file of receipt.rawFiles) {
        assert.ok(!path.isAbsolute(file.path) && file.path.split("/").every((p) => p && p !== "." && p !== ".."))
        const bytes = read(path.join(root, file.path), 16 * 1024 * 1024)
        assert.equal(bytes.length, file.bytes)
        assert.equal(hash(bytes), file.sha256, file.path)
    }
    for (const run of receipt.runs) {
        const rawRun = JSON.parse(read(path.join(root, run.artifacts, "run.json")))
        for (const key of ["index", "runtime", "pair", "warmup", "before", "after", "cpu", "memory", "membership", "cleanup", "temporaryProfileRemoved", "launch", "profileSeedSha256"]) assert.deepEqual(run[key], rawRun[key], `Raw run projection: ${key}`)
        assert.deepEqual(run.libraryContext, rawRun.runtimeObservation.library.context)
        const samples = read(path.join(root, run.artifacts, "samples.jsonl"), 8 * 1024 * 1024)
            .toString()
            .trim()
            .split("\n")
            .map(JSON.parse)
        assert.equal(samples.length, run.rawSampleLines)
        const packed = samples.filter((s) => s.phase === "steady").map((s) => [...receipt.columns.sample.slice(0, -1).map((key) => s[key]), s.processes.map((p) => receipt.columns.process.map((key) => p[key] ?? false))])
        assert.deepEqual(packed, run.steady)
    }
    for (const side of ["before", "after"]) {
        const executable = receipt.variants[side].executable
        assert.equal(hashFile(executable.path, 1024 * 1024 * 1024), executable.sha256)
    }
}
const receipt = JSON.parse(read(path.join(here, "receipt.json")))
const summary = validate(receipt)
if (process.argv.includes("--raw")) verifyRaw(receipt)
if (process.argv.includes("--self-test")) {
    for (const corrupt of [
        (r) => r.runs[2].cpu.cpuSeconds++,
        (r) => r.runs[2].steady[0].at(-1)[0][3]++,
        (r) => {
            r.supervisor.complete = false
        },
        (r) => {
            r.runs[0].warmup = false
        }
    ]) {
        const changed = structuredClone(receipt)
        corrupt(changed)
        assert.throws(() => validate(changed))
    }
}
console.log(JSON.stringify({ archivedRuns: 12, steadySamples: 504, rawCompared: process.argv.includes("--raw"), selfTests: process.argv.includes("--self-test") ? 4 : 0, summary }, null, 2))
