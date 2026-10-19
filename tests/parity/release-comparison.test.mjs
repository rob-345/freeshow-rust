import assert from "node:assert/strict"
import { spawn, execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { test } from "node:test"
import { aggregateSample, distribution, membership, memorySummary, observedCpu, pairOrder, parseRollup, parseStat, readProcesses } from "../../scripts/migration/release-process-metrics.mjs"
import { ablationProvenance, artifactTree, assertProductionState, captureTauriBuild, comparisonLanes, comparisonPlan, options, summarize, verifyTauriBuild, visibleEditorSignature } from "../../scripts/migration/compare-releases.mjs"

const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
const fixture = (pid, start, extra = {}) => ({ pid, startTicks: start, identity: `${pid}:${start}`, ppid: 1, pgid: pid, sid: pid, cpuTicks: 0, rssBytes: 4096, state: "S", ...extra })
const sha = (value) => createHash("sha256").update(value).digest("hex")
function ablationArgs(before = "/tmp/before/target/release/app", after = "/tmp/after/target/release/app") {
    return ["--mode", "tauri-ablation", "--before-bin", before, "--after-bin", after, "--before-label", "eager", "--after-label", "lazy", "--before-build-receipt", "/tmp/before.json", "--after-build-receipt", "/tmp/after.json", "--expect-before-sha256", "a".repeat(64), "--expect-after-sha256", "b".repeat(64), "--allowed-source-change", "src/frontend/audio/audioAnalyser.ts", "--seed", "42"]
}
async function buildFixture() {
    const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "release-provenance-test-")),
        repo = path.join(temporary, "repo")
    fs.mkdirSync(repo)
    const git = (...args) => execFileSync("git", args, { cwd: repo, encoding: "utf8", timeout: 5000, stdio: ["ignore", "pipe", "pipe"] }).trim()
    const write = (root, file, bytes) => {
        fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true })
        fs.writeFileSync(path.join(root, file), bytes)
    }
    git("init", "-q")
    for (const file of ["Cargo.toml", "Cargo.lock", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock", "package.json", "package-lock.json", "vite.rust.config.mjs", "scripts/migration/build-frontend.mjs"]) write(repo, file, `controlled provenance fixture ${file}\n`)
    write(repo, "rust-toolchain.toml", fs.readFileSync("rust-toolchain.toml"))
    write(repo, "src/frontend/audio/audioAnalyser.ts", "controlled eager source\n")
    git("add", ".")
    git("-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "before")
    const beforeCommit = git("rev-parse", "HEAD"),
        before = path.join(temporary, "before")
    git("worktree", "add", "--quiet", "--detach", before, beforeCommit)
    write(repo, "src/frontend/audio/audioAnalyser.ts", "controlled lazy source\n")
    git("add", ".")
    git("-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "after")
    const after = path.join(temporary, "after")
    git("worktree", "add", "--quiet", "--detach", after, git("rev-parse", "HEAD"))
    const receipts = {}
    for (const [side, root] of [
        ["before", before],
        ["after", after]
    ]) {
        write(root, "target/release/app", `controlled non-executed ${side} artifact`)
        write(root, "dist-rust/index.html", "<main>production fixture</main>")
        write(root, "dist-rust/assets/main.js", `globalThis.controlledVariant=${JSON.stringify(side)}`)
        for (const kind of ["remote", "stage", "controller"]) write(root, `target/release/remotes/${kind}/index.html`, "controlled common client")
        write(root, "build.log", "vite v6 building for production...\nFinished `release` profile [optimized]\n")
        // These are controlled provenance fixtures, never executed apps or
        // evidence that fake logs establish a genuine production binary.
        receipts[side] = await captureTauriBuild({ sourceRoot: root, binary: path.join(root, "target/release/app"), buildLog: path.join(root, "build.log") })
        write(temporary, `${side}.json`, JSON.stringify(receipts[side]))
    }
    const args = ablationArgs(path.join(before, "target/release/app"), path.join(after, "target/release/app"))
    for (const side of ["before", "after"]) {
        args[args.indexOf(`--${side}-build-receipt`) + 1] = path.join(temporary, `${side}.json`)
        args[args.indexOf(`--expect-${side}-sha256`) + 1] = receipts[side].executable.sha256
    }
    args.push("--source-root", repo)
    return { temporary, before, after, repo, receipts, write, git, opts: options(args), dispose: () => fs.rmSync(temporary, { recursive: true, force: true }) }
}
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
test("two-Tauri lanes preserve explicit labels, paired order and warmup exclusion", () => {
    const opts = options(ablationArgs())
    assert.deepEqual(comparisonLanes(opts), [
        { key: "before", label: "eager", adapter: "tauri" },
        { key: "after", label: "lazy", adapter: "tauri" }
    ])
    const plan = comparisonPlan(opts)
    assert.equal(plan.length, 12)
    assert.equal(plan.filter((row) => row.warmup).length, 2)
    for (let i = 0; i < plan.length; i += 2) assert.deepEqual(new Set(plan.slice(i, i + 2).map((row) => row.label)), new Set(["eager", "lazy"]))
    assert.notEqual(plan[0].runtime, plan[2].runtime)
    const row = { valid: true, memory: { pssComplete: true, pssSumBytes: { p50: 12 }, rssSumBytes: { p50: 20 } }, cpu: { method: "observed-process-counters-lower-bound", percentOneCore: 3 } }
    const summary = summarize(
        plan.map((entry) => ({ ...row, ...entry })),
        comparisonLanes(opts)
    )
    assert.equal(summary.runtime.before.label, "eager")
    assert.equal(summary.runtime.after.label, "lazy")
    assert.equal(summary.runtime.before.runs, 5)
    assert.equal(summary.runtime.after.runs, 5)
    assert.equal(summary.exactCpuComparable, false)
    assert.equal(summary.ratios, null)
    assert.deepEqual(
        comparisonLanes(options([])).map((row) => row.adapter),
        ["electron", "tauri"]
    )
})
test("ablation rejects ambiguous labels/artifacts, missing hashes and unbounded source allowlists", () => {
    const invalid = (flag, value) => {
        const args = ablationArgs()
        args[args.indexOf(flag) + 1] = value
        assert.throws(() => options(args))
    }
    invalid("--after-label", "eager")
    invalid("--after-label", "a".repeat(49))
    invalid("--after-label", "../lazy")
    invalid("--after-bin", "/tmp/before/target/release/app")
    invalid("--expect-before-sha256", "not-a-hash")
    invalid("--allowed-source-change", "src/../native.rs")
    assert.throws(() => options(["--mode", "tauri-ablation"]))
    assert.throws(() => options(["--before-bin", "file"]))
    assert.throws(() => options(["--allowedSourceChanges", "file"]))
    assert.throws(() => options([...ablationArgs(), "--allowed-source-change", "src/frontend/audio/audioAnalyser.ts"]))
    const args = ablationArgs()
    for (let i = 0; i < 16; i++) args.push("--allowed-source-change", `src/${i}.ts`)
    assert.throws(() => options(args))
})
test("frozen actual files/Git receipts verify two variants and reject changed resources, logs and binaries", { skip: process.platform !== "linux", timeout: 20000 }, async () => {
    const f = await buildFixture()
    try {
        const observed = await ablationProvenance(f.opts)
        assert.deepEqual(observed.labels, { before: "eager", after: "lazy" })
        assert.deepEqual(observed.sourceChanges, ["src/frontend/audio/audioAnalyser.ts"])
        assert.notEqual(observed.before.frontend.sha256, observed.after.frontend.sha256)
        const file = path.join(f.before, "target/release/remotes/remote/index.html"),
            original = fs.readFileSync(file)
        fs.writeFileSync(file, "changed external resource")
        await assert.rejects(ablationProvenance(f.opts), /resources changed/)
        fs.writeFileSync(file, original)
        const binary = f.opts.beforeBin,
            contents = fs.readFileSync(binary)
        fs.writeFileSync(binary, "different executable")
        await assert.rejects(ablationProvenance(f.opts), /Selected binary changed/)
        fs.writeFileSync(binary, contents)
        const env = path.join(f.before, ".env.production")
        fs.writeFileSync(env, "VITE_CONTROLLED_FIXTURE=changed")
        await assert.rejects(ablationProvenance(f.opts), /Build inputs changed/)
        fs.unlinkSync(env)
        fs.appendFileSync(path.join(f.before, "build.log"), "altered build receipt log")
        await assert.rejects(ablationProvenance(f.opts), /Build log changed/)
    } finally {
        f.dispose()
    }
})
test("build provenance rejects debug/instrumented logs, missing clients, hooks and dirty tracked inputs", { skip: process.platform !== "linux", timeout: 20000 }, async () => {
    const f = await buildFixture()
    const capture = () => captureTauriBuild({ sourceRoot: f.before, binary: f.opts.beforeBin, buildLog: path.join(f.before, "build.log") })
    try {
        const log = path.join(f.before, "build.log"),
            original = fs.readFileSync(log)
        fs.writeFileSync(log, "building for production\nFinished `dev` profile")
        await assert.rejects(capture(), /release build log/)
        fs.writeFileSync(log, original)
        fs.appendFileSync(log, "__audioFullFork")
        await assert.rejects(capture(), /Instrumented build log/)
        fs.writeFileSync(log, original)
        const client = path.join(f.before, "target/release/remotes/stage/index.html")
        fs.renameSync(client, client + ".moved")
        await assert.rejects(capture(), /Missing actual stage resource/)
        fs.renameSync(client + ".moved", client)
        const script = path.join(f.before, "dist-rust/assets/main.js"),
            code = fs.readFileSync(script)
        // The marker straddles a stream read boundary; whole-file scan only
        // via independent chunks must not overlook the injected early hook.
        fs.writeFileSync(script, "x".repeat(65530) + "__audioFullFork")
        await assert.rejects(capture(), /Production hook/)
        fs.writeFileSync(script, code)
        fs.appendFileSync(path.join(f.before, "Cargo.lock"), "changed input")
        await assert.rejects(capture(), /Tracked build source must be clean/)
    } finally {
        f.dispose()
    }
})
test("source delta and recorded tool/profile evidence cannot silently broaden an ablation", { skip: process.platform !== "linux", timeout: 20000 }, async () => {
    const f = await buildFixture()
    try {
        await assert.rejects(ablationProvenance({ ...f.opts, allowedSourceChanges: ["src-tauri/src/main.rs"] }), /Runtime change differs/)
        const receipt = structuredClone(f.receipts.after)
        receipt.tools.rustc.stdout = "a different toolchain"
        f.write(f.temporary, "after.json", JSON.stringify(receipt))
        await assert.rejects(ablationProvenance(f.opts), /toolchains differ/)
        receipt.tools = f.receipts.after.tools
        receipt.sourceCommit = "0".repeat(40)
        f.write(f.temporary, "after.json", JSON.stringify(receipt))
        await assert.rejects(ablationProvenance(f.opts), /Build sourceCommit changed/)
        receipt.sourceCommit = f.receipts.after.sourceCommit
        receipt.build.profile = "dev"
        f.write(f.temporary, "after.json", JSON.stringify(receipt))
        await assert.rejects(verifyTauriBuild(f.opts.afterBuildReceipt, f.opts.afterBin, f.opts.expectAfterSha256))
    } finally {
        f.dispose()
    }
})
test("artifact trees reject symlinks/empty inputs and actual-WebView hook checks fail closed", { skip: process.platform !== "linux" }, async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "release-tree-test-"))
    try {
        await assert.rejects(artifactTree(root), /Empty artifact/)
        fs.writeFileSync(path.join(root, "plain.js"), "ordinary production code")
        const actual = await artifactTree(root)
        assert.equal(actual.files[0].sha256, sha("ordinary production code"))
        fs.symlinkSync(path.join(root, "plain.js"), path.join(root, "alias.js"))
        await assert.rejects(artifactTree(root), /symlink/)
        fs.unlinkSync(path.join(root, "alias.js"))
        const oversized = path.join(root, "oversized.bin")
        fs.closeSync(fs.openSync(oversized, "w"))
        fs.truncateSync(oversized, 128 * 1024 * 1024 + 1)
        await assert.rejects(artifactTree(root), /Artifact byte limit/)
        fs.unlinkSync(oversized)
        assert.doesNotThrow(() => assertProductionState({ productionHooks: [] }))
        assert.throws(() => assertProductionState({ productionHooks: ["__audioFullFork"] }))
        assert.throws(() => assertProductionState({}))
    } finally {
        fs.rmSync(root, { recursive: true, force: true })
    }
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
