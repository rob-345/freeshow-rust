import assert from "node:assert/strict"
import { spawn } from "node:child_process"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { test } from "node:test"
import { aggregateSample, distribution, membership, memorySummary, observedCpu, pairOrder, parseRollup, parseStat, readProcesses } from "../../scripts/migration/release-process-metrics.mjs"
import { options, summarize, visibleEditorSignature } from "../../scripts/migration/compare-releases.mjs"

const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
const fixture = (pid, start, extra = {}) => ({ pid, startTicks: start, identity: `${pid}:${start}`, ppid: 1, pgid: pid, sid: pid, cpuTicks: 0, rssBytes: 4096, state: "S", ...extra })
test("proc stat parsing handles spaces/parentheses and uses self CPU, not child counters", () => {
    const fields = Array(30).fill("0")
    fields[0] = "S"
    for (const [index, value] of [
        [1, 2],
        [2, 3],
        [3, 4],
        [11, 17],
        [12, 19],
        [13, 9999],
        [14, 8888],
        [19, 12345],
        [21, 11]
    ])
        fields[index] = String(value)
    const parsed = parseStat(`123 (odd ) ( name) ${fields.join(" ")}`, 4096)
    assert.deepEqual({ pid: parsed.pid, comm: parsed.comm, cpu: parsed.cpuTicks, identity: parsed.identity, rss: parsed.rssBytes }, { pid: 123, comm: "odd ) ( name", cpu: 36, identity: "123:12345", rss: 45056 })
    assert.throws(() => parseStat("bad"))
    assert.throws(() => parseStat("1 (x) S"))
})
test("membership retains reparented Chromium/WebKit helpers and rejects reused root PID", () => {
    const root = fixture(10, 100),
        rows = [root, fixture(11, 101, { ppid: 10 }), fixture(12, 102, { sid: 10 }), fixture(13, 103, { markerMatch: true }), fixture(14, 104, { cgroupMatch: true }), fixture(15, 105), fixture(16, 106)]
    const selected = membership(rows, [root], new Set(["15:105"]))
    assert.deepEqual(
        selected.map((row) => row.pid),
        [10, 11, 12, 13, 14, 15]
    )
    assert.ok(selected.find((row) => row.pid === 12).reasons.includes("private-session"))
    assert.deepEqual(membership([fixture(10, 200), fixture(20, 201, { sid: 10 })], [root]), [])
})
test("PSS is complete only for every observed living process; RSS remains a repeated shared-page sum", () => {
    assert.deepEqual(parseRollup("Rss: 100 kB\nPss: 60 kB\nPrivate_Clean: 10 kB\nPrivate_Dirty: 20 kB\nSwapPss: 2 kB\n"), { rssBytes: 102400, pssBytes: 61440, privateBytes: 30720, swapPssBytes: 2048 })
    const a = fixture(10, 100, { rssBytes: 100, pssBytes: 60 }),
        b = fixture(11, 101, { rssBytes: 100, pssBytes: 60 })
    assert.deepEqual(aggregateSample({ processes: [a, b] }), { processCount: 2, rssSumBytes: 200, pssSumBytes: 120, pssComplete: true })
    assert.equal(aggregateSample({ processes: [a, { ...b, pssBytes: null }] }).pssSumBytes, null)
    assert.equal(aggregateSample({ processes: [a, { ...b, state: "Z", pssBytes: null }] }).pssSumBytes, 60)
})
test("CPU lower bound follows identities, new births and lost existing processes without child double counting", () => {
    const samples = [
        { monotonicMs: 0, uptimeSeconds: 10, processes: [fixture(1, 100, { cpuTicks: 20 })] },
        { monotonicMs: 1000, uptimeSeconds: 11, processes: [fixture(1, 100, { cpuTicks: 30 }), fixture(2, 1050, { cpuTicks: 5 }), fixture(3, 500, { cpuTicks: 200 })] },
        { monotonicMs: 2000, uptimeSeconds: 12, processes: [fixture(2, 1050, { cpuTicks: 15 }), fixture(3, 500, { cpuTicks: 210 }), fixture(1, 1150, { cpuTicks: 4 })] }
    ]
    const result = observedCpu(samples, 100)
    assert.equal(result.cpuSeconds, 0.39)
    assert.equal(result.percentOneCore, 19.5)
    assert.deepEqual(result.lateExisting, ["3:500"])
})
test("partial PSS samples and runs are explicit subsets and never the primary comparison", () => {
    const complete = { processes: [fixture(1, 100, { pssBytes: 60 })], pssComplete: true, pssSumBytes: 60, rssSumBytes: 100 },
        incomplete = { processes: [fixture(2, 100, { pssBytes: null, exitedDuringRead: true, memoryError: "ENOENT" })], pssComplete: false, pssSumBytes: null, rssSumBytes: 200 },
        partial = memorySummary([complete, incomplete]),
        full = memorySummary([complete])
    assert.equal(partial.pssSumBytes, null)
    assert.equal(partial.completeSampleSubsetPssSumBytes.distribution.count, 1)
    assert.equal(partial.coverage.totalSamples, 2)
    assert.equal(partial.coverage.incompletePssSamples, 1)
    assert.deepEqual(partial.coverage.missingPssIdentities, [{ identity: "2:100", samples: 1, reasons: ["ENOENT", "exited-during-read"] }])
    const summary = summarize([
        { runtime: "electron", valid: true, memory: partial, cpu: {} },
        { runtime: "rust", valid: true, memory: full, cpu: {} }
    ])
    assert.equal(summary.runtime.electron.perRunMedianPssBytes, null)
    assert.equal(summary.runtime.rust.perRunMedianPssBytes, null)
    assert.equal(summary.runtime.rust.completePssRunSubset.distribution.count, 1)
    assert.equal(summary.runtime.electron.completePssRunSubset.distribution.count, 0)
    assert.match(summary.primaryMemoryMetric, /no-complete/)
})
test("visible workload matching ignores time/version text but requires navigation, empty library and geometry", () => {
    const first = { viewport: { width: 1280, height: 800, dpr: 1 }, nav: ["Presenting [1]"], bodyText: "FreeShow 1.6.7 Projects Nothing here New project New show Shows No shows Import 12:10" },
        second = { ...first, bodyText: first.bodyText.replace("1.6.7", "1.6.7-rust.1").replace("12:10", "13:10") }
    assert.deepEqual(visibleEditorSignature(first), visibleEditorSignature(second))
    assert.notDeepEqual(visibleEditorSignature(first), visibleEditorSignature({ ...second, nav: [] }))
    assert.notDeepEqual(visibleEditorSignature(first), visibleEditorSignature({ ...second, bodyText: "Projects" }))
})
test("paired orders are reproducible, bounded, balanced and keep warmups out of summary", () => {
    const ordered = pairOrder(5, 1, 123, "alternating")
    assert.deepEqual(ordered, pairOrder(5, 1, 123, "alternating"))
    assert.equal(ordered.length, 12)
    assert.equal(ordered.filter((row) => row.warmup).length, 2)
    for (let i = 2; i < ordered.length; i += 2) assert.deepEqual(new Set(ordered.slice(i, i + 2).map((row) => row.runtime)), new Set(["electron", "rust"]))
    assert.notEqual(ordered[0].runtime, ordered[2].runtime)
    const randomFirst = pairOrder(20, 0, 123, "random")
        .filter((_, index) => index % 2 === 0)
        .map((row) => row.runtime)
    assert.ok(
        randomFirst.some((name, index) => index > 0 && name === randomFirst[index - 1]),
        "Random mode must not use the alternating low bit of an LCG"
    )
    assert.equal(options([]).settle, 20)
    assert.equal(options([]).seconds, 10)
    assert.equal(options([]).pairs, 5)
    assert.equal(options(["--pairs", "1", "--dry-run"]).dryRun, true)
    assert.throws(() => options(["--seconds", "0"]))
    assert.throws(() => options(["--pairs", "1000"]))
    const row = { valid: true, memory: { pssComplete: true, pssSumBytes: { p50: 12 }, rssSumBytes: { p50: 20 } }, cpu: { method: "observed-process-counters-lower-bound", percentOneCore: 3 } }
    const summary = summarize([
        { ...row, runtime: "electron" },
        { ...row, runtime: "rust" },
        { ...row, runtime: "rust", warmup: true }
    ])
    assert.equal(summary.runtime.rust.runs, 1)
    assert.equal(summary.exactCpuComparable, false)
    assert.equal(summary.ratios, null)
    assert.equal(summary.startupComparable, false)
    assert.deepEqual(distribution([9, 1, 4]), { count: 3, min: 1, p50: 4, p95: 9, max: 9 })
})
test("exec launcher and real proc reader attribute a child after it changes session and is reparented", { skip: process.platform !== "linux", timeout: 10000 }, async () => {
    const folder = fs.mkdtempSync(path.join(os.tmpdir(), "release-metric-test-")),
        receipt = path.join(folder, "launch.json"),
        runId = `synthetic-${process.pid}-${Date.now()}`
    const configuration = path.join(folder, "config.json"),
        code = "import os,time; child=os.fork();\nif child==0:\n os.setsid(); time.sleep(20)\nelse:\n time.sleep(.5)"
    fs.writeFileSync(configuration, JSON.stringify({ binary: "/usr/bin/python3", receipt, runId, env: {} }))
    const supervisor = path.join(folder, "supervisor.json"),
        child = spawn("/usr/bin/python3", [path.resolve("scripts/migration/release-process-launcher.py"), "--supervise", "/usr/bin/python3", path.resolve("scripts/migration/release-process-launcher.py"), "-c", code], { env: { ...process.env, FREESHOW_COMPARISON_LAUNCH_CONFIG: configuration, FREESHOW_COMPARISON_SUPERVISOR_RECEIPT: supervisor }, stdio: "ignore" })
    let found = []
    try {
        for (let i = 0; i < 40 && !fs.existsSync(receipt); i++) await pause(25)
        const root = JSON.parse(fs.readFileSync(receipt))
        assert.equal(root.pid, root.sid)
        for (let i = 0; i < 30; i++) {
            found = readProcesses({ marker: runId, roots: [root] }).processes
            if (found.length >= 2) break
            await pause(25)
        }
        assert.ok(found.length >= 2)
        const known = new Set(found.map((row) => row.identity))
        await pause(600)
        const orphan = readProcesses({ marker: runId, roots: [root], known }).processes.find((row) => row.pid !== root.pid && row.state !== "Z")
        assert.ok(orphan)
        assert.notEqual(orphan.sid, root.sid)
        assert.ok(orphan.reasons.includes("previously-observed"))
        assert.ok(orphan.reasons.includes("run-marker"))
    } finally {
        for (const row of found)
            try {
                const current = parseStat(fs.readFileSync(`/proc/${row.pid}/stat`, "utf8"))
                if (current.identity === row.identity) process.kill(row.pid, "SIGKILL")
            } catch {}
        if (child.exitCode === null) await Promise.race([new Promise((resolve) => child.once("exit", resolve)), pause(2000)])
        if (child.exitCode === null) child.kill("SIGKILL")
        const supervision = JSON.parse(fs.readFileSync(supervisor))
        assert.equal(supervision.complete, true)
        assert.ok(supervision.reaped.length >= 2)
        for (const row of found) assert.equal(fs.existsSync(`/proc/${row.pid}`), false, `Unreaped process ${row.identity}`)
        fs.rmSync(folder, { recursive: true, force: true })
    }
})
