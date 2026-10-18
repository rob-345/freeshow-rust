// Linux process accounting shared by the release runner and synthetic tests.
import fs from "node:fs"
import path from "node:path"

export function parseStat(text, pageBytes = 4096) {
    const open = text.indexOf("("),
        close = text.lastIndexOf(")")
    if (open < 1 || close < open) throw new Error("Malformed /proc/PID/stat command")
    const values = text
        .slice(close + 1)
        .trim()
        .split(/\s+/)
    if (values.length < 22) throw new Error("Truncated /proc/PID/stat")
    const number = (index) => {
        const value = Number(values[index])
        if (!Number.isSafeInteger(value)) throw new Error("Unsafe stat counter")
        return value
    }
    const row = { pid: Number(text.slice(0, open).trim()), comm: text.slice(open + 1, close), state: values[0], ppid: number(1), pgid: number(2), sid: number(3), cpuTicks: number(11) + number(12), startTicks: number(19), rssBytes: Math.max(0, number(21)) * pageBytes }
    row.identity = `${row.pid}:${row.startTicks}`
    return row
}
export function parseRollup(text) {
    const output = {}
    for (const line of text.split("\n")) {
        const match = /^(Rss|Pss|Private_Clean|Private_Dirty|SwapPss):\s+(\d+) kB$/.exec(line)
        if (match) output[match[1]] = Number(match[2]) * 1024
    }
    if (!Number.isFinite(output.Pss) || !Number.isFinite(output.Rss)) throw new Error("Rollup lacks Pss/Rss")
    return { rssBytes: output.Rss, pssBytes: output.Pss, privateBytes: (output.Private_Clean || 0) + (output.Private_Dirty || 0), swapPssBytes: output.SwapPss ?? null }
}
export function membership(rows, roots, known = new Set()) {
    const byPid = new Map(rows.map((row) => [row.pid, row])),
        selected = new Map()
    const validRoots = roots.filter((root) => !byPid.has(root.pid) || byPid.get(root.pid).startTicks === root.startTicks)
    for (const row of rows) {
        const why = []
        if (roots.some((root) => root.identity === row.identity)) why.push("launch-root")
        if (known.has(row.identity)) why.push("previously-observed")
        if (row.markerMatch) why.push("run-marker")
        if (row.cgroupMatch) why.push("cgroup-member")
        if (validRoots.some((root) => root.sid === row.sid && row.startTicks >= root.startTicks)) why.push("private-session")
        if (validRoots.some((root) => root.pgid === row.pgid && row.startTicks >= root.startTicks)) why.push("private-process-group")
        if (why.length) selected.set(row.identity, { ...row, reasons: why })
    }
    let changed = true
    while (changed) {
        changed = false
        for (const row of rows) {
            if (selected.has(row.identity)) continue
            const parent = byPid.get(row.ppid)
            if (parent && selected.has(parent.identity)) {
                selected.set(row.identity, { ...row, reasons: ["current-ancestry"] })
                changed = true
            }
        }
    }
    return [...selected.values()].sort((a, b) => a.pid - b.pid)
}
export function readProcesses({ proc = "/proc", pageBytes = 4096, marker, cgroupPids = new Set(), known = new Set(), roots = [] }) {
    const rows = [],
        scanErrors = [],
        uid = process.getuid()
    const entries = fs.readdirSync(proc).filter((name) => /^\d+$/.test(name))
    if (entries.length > 20000) throw new Error("Process inventory exceeds 20000 entries")
    for (const name of entries) {
        if (!/^\d+$/.test(name)) continue
        const base = path.join(proc, name)
        try {
            if (fs.statSync(base).uid !== uid) continue
            const row = parseStat(fs.readFileSync(path.join(base, "stat"), "utf8"), pageBytes)
            row.cgroupMatch = cgroupPids.has(row.pid)
            try {
                row.markerMatch = !!marker && fs.readFileSync(path.join(base, "environ")).toString().split("\0").includes(`FREESHOW_COMPARISON_RUN_ID=${marker}`)
            } catch (error) {
                row.markerReadable = false
                row.markerError = error.code
            }
            rows.push(row)
        } catch (error) {
            if (!["ENOENT", "ESRCH"].includes(error.code)) scanErrors.push({ pid: Number(name), code: error.code || error.message })
        }
    }
    const selected = membership(rows, roots, known)
    for (const row of selected) {
        const base = path.join(proc, String(row.pid))
        row.pssBytes = null
        try {
            row.exe = fs.readlinkSync(path.join(base, "exe"))
        } catch (error) {
            row.exeError = error.code
        }
        try {
            row.argv = fs.readFileSync(path.join(base, "cmdline")).toString().split("\0").filter(Boolean)
        } catch (error) {
            row.argvError = error.code
        }
        if (row.state === "Z") {
            row.rssBytes = 0
            row.pssBytes = 0
            row.memorySource = "zombie"
            continue
        }
        try {
            Object.assign(row, parseRollup(fs.readFileSync(path.join(base, "smaps_rollup"), "utf8")))
            row.memorySource = "smaps_rollup"
        } catch (error) {
            row.memorySource = "stat-rss-estimate"
            row.memoryError = error.code || error.message
        }
        // A PID can exit/reuse between the reads; never combine those identities.
        try {
            if (parseStat(fs.readFileSync(path.join(base, "stat"), "utf8"), pageBytes).identity !== row.identity) {
                row.changedDuringRead = true
                row.pssBytes = null
            }
        } catch (error) {
            row.exitedDuringRead = true
            row.memoryError = error.code || error.message
            row.pssBytes = null
        }
    }
    return { processes: selected, scanErrors, unmatchedMarkerReadFailures: rows.filter((row) => row.markerReadable === false && !selected.some((found) => found.identity === row.identity)).length }
}
export function aggregateSample(snapshot) {
    const living = snapshot.processes.filter((row) => row.state !== "Z")
    const complete = living.length > 0 && living.every((row) => Number.isFinite(row.pssBytes) && !row.changedDuringRead && !row.exitedDuringRead)
    return { processCount: living.length, rssSumBytes: living.reduce((sum, row) => sum + row.rssBytes, 0), pssSumBytes: complete ? living.reduce((sum, row) => sum + row.pssBytes, 0) : null, pssComplete: complete }
}
export function memorySummary(samples) {
    const complete = samples.filter((sample) => sample.pssComplete && Number.isFinite(sample.pssSumBytes)),
        pssComplete = samples.length > 0 && complete.length === samples.length,
        missing = new Map()
    for (const sample of samples)
        for (const row of sample.processes)
            if (row.state !== "Z" && (!Number.isFinite(row.pssBytes) || row.changedDuringRead || row.exitedDuringRead)) {
                const previous = missing.get(row.identity) || { identity: row.identity, samples: 0, reasons: [] }
                previous.samples++
                previous.reasons = [...new Set([...previous.reasons, row.memoryError || "unreadable-PSS", ...(row.changedDuringRead ? ["PID-changed-during-read"] : []), ...(row.exitedDuringRead ? ["exited-during-read"] : [])])]
                missing.set(row.identity, previous)
            }
    return {
        pssComplete,
        coverage: { totalSamples: samples.length, completePssSamples: complete.length, incompletePssSamples: samples.length - complete.length, missingPssIdentities: [...missing.values()] },
        pssSumBytes: pssComplete ? distribution(complete.map((sample) => sample.pssSumBytes)) : null,
        completeSampleSubsetPssSumBytes: { meaning: "Only samples with PSS for every observed living process; selected subset, not the whole run", distribution: distribution(complete.map((sample) => sample.pssSumBytes)) },
        rssSumBytes: distribution(samples.map((sample) => sample.rssSumBytes)),
        rssMeaning: "Sum of resident pages per observed process; shared mappings counted repeatedly, NOT unique physical memory",
        pssMeaning: "Proportional share of mapped resident pages among all host sharers, NOT a unique-physical-memory total; coverage describes observed membership only"
    }
}
export function percentile(values, p) {
    if (!values.length) return null
    const sorted = [...values].sort((a, b) => a - b)
    return sorted[Math.max(0, Math.ceil(p * sorted.length) - 1)]
}
export function distribution(values) {
    const finite = values.filter(Number.isFinite)
    return { count: finite.length, min: finite.length ? Math.min(...finite) : null, p50: percentile(finite, 0.5), p95: percentile(finite, 0.95), max: finite.length ? Math.max(...finite) : null }
}
export function observedCpu(samples, hz) {
    if (samples.length < 2) return { cpuSeconds: null, percentOneCore: null }
    const start = samples[0],
        baselines = new Map(),
        last = new Map(),
        lateExisting = []
    for (const row of start.processes) {
        baselines.set(row.identity, row.cpuTicks)
        last.set(row.identity, row.cpuTicks)
    }
    for (const sample of samples.slice(1))
        for (const row of sample.processes) {
            if (!baselines.has(row.identity)) {
                // Uptime/starttime have clock-tick resolution. Ambiguous births
                // at the first boundary use a conservative first-seen baseline.
                const bornWithinWindow = row.startTicks / hz >= start.uptimeSeconds + 1 / hz
                baselines.set(row.identity, bornWithinWindow ? 0 : row.cpuTicks)
                if (!bornWithinWindow) lateExisting.push(row.identity)
            }
            last.set(row.identity, Math.max(last.get(row.identity) || 0, row.cpuTicks))
        }
    let ticks = 0
    for (const [id, value] of last) ticks += Math.max(0, value - baselines.get(id))
    const seconds = ticks / hz,
        wallSeconds = ((samples.at(-1).observationEndMs ?? samples.at(-1).monotonicMs) - start.monotonicMs) / 1000
    return { method: "observed-process-counters-lower-bound", cpuSeconds: seconds, percentOneCore: wallSeconds > 0 ? (100 * seconds) / wallSeconds : null, wallSeconds, lateExisting, limitations: ["No cutime/cstime: avoids counting reaped child CPU twice", "Final CPU of helpers exiting between polls and entirely unobserved short-lived helpers can be missed", "Processes first observed late with earlier birth lose CPU before their first observation"] }
}
export function pairOrder(pairs, warmups, seed, mode = "alternating") {
    let state = seed >>> 0
    const random = () => {
        state = (Math.imul(state, 1664525) + 1013904223) >>> 0
        return state
    }
    const first = random() >>> 31,
        order = []
    for (let pair = 0; pair < pairs + warmups; pair++) {
        const reverse = mode === "random" ? random() >>> 31 : (first + pair) % 2
        for (const runtime of reverse ? ["rust", "electron"] : ["electron", "rust"]) order.push({ runtime, pair: pair - warmups, warmup: pair < warmups })
    }
    return order
}
