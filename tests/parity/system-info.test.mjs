import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import test from "node:test"
import { clean, root, upstreamFunctions } from "./oracle.mjs"

const binary = process.env.FREESHOW_SYSTEM_PARITY || path.join(root, "target/debug/freeshow-system-parity" + (process.platform === "win32" ? ".exe" : ""))
assert.ok(fs.existsSync(binary), "Build freeshow-system-parity or set FREESHOW_SYSTEM_PARITY to its isolated target path")
export function probe(input, env = process.env) {
    const result = spawnSync(binary, [], { input: JSON.stringify(input) + "\n", encoding: "utf8", env, maxBuffer: 8 * 1024 * 1024, timeout: 10000 })
    assert.equal(result.status, 0, result.stderr || result.error?.message)
    return JSON.parse(result.stdout)
}
function value(input, env) {
    const result = probe(input, env)
    assert.equal(result.ok, true, JSON.stringify(result))
    return result.value
}
function batch(cases) {
    return value({ op: "batch", cases }).map((item) => {
        assert.equal(item.ok, true, JSON.stringify(item))
        return item.value
    })
}
const source = "src/electron/IPC/responsesMain.ts"
const number = (value) => {
    if (value === "NaN") return NaN
    if (value === "Infinity") return Infinity
    if (value === "-Infinity") return -Infinity
    if (value === "-0") return -0
    return value
}
function sourceDecision(total, free) {
    return upstreamFunctions(source, ["checkRamUsage"], { os: { totalmem: () => number(total), freemem: () => number(free) } }).checkRamUsage().performanceMode
}
function neighbor(value, up) {
    const buffer = new ArrayBuffer(8),
        view = new DataView(buffer)
    view.setFloat64(0, value)
    view.setBigUint64(0, view.getBigUint64(0) + (up ? 1n : -1n))
    return view.getFloat64(0)
}

test("actual source RAM decision preserves inclusive 8GiB and strict 98% f64 boundaries", () => {
    const cases = []
    const gib = 1024 ** 3
    for (const total of [8 * gib - 1, 8 * gib, 8 * gib + 1, neighbor(8 * gib, true), neighbor(8 * gib, false), 9 * gib, 16 * gib, 64 * gib, 2 ** 53, 1e20, Number.MAX_VALUE]) {
        const thresholdFree = total * 0.02
        for (const free of [0, 1, total, total + 1, thresholdFree, neighbor(thresholdFree, true), neighbor(thresholdFree, false), ...Array.from({ length: 21 }, (_, index) => thresholdFree + index - 10)]) cases.push({ op: "decision", total, free })
    }
    for (const total of [0, "-0", -1, "NaN", "Infinity", "-Infinity"]) for (const free of [0, 1, -1, "NaN", "Infinity", "-Infinity"]) cases.push({ op: "decision", total, free })
    let seed = 0x72616d
    for (let index = 0; index < 500; index++) {
        seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
        const total = seed * 1048576
        seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
        cases.push({ op: "decision", total, free: (seed / 0xffffffff) * total })
    }
    const actual = batch(cases)
    for (const [index, input] of cases.entries()) assert.equal(actual[index], sourceDecision(input.total, input.free), JSON.stringify(input))
    assert.equal(value({ op: "decision", total: 8 * gib, free: 8 * gib }), true)
    assert.equal(value({ op: "decision", total: 10_000_000_000, free: 200_000_000 }), false, "exact98% is not highUsage")
    assert.equal(value({ op: "decision", total: 10_000_000_000, free: 199_999_999 }), true)
})

test("device names execute the original first case-sensitive '.local' replacement", () => {
    const names = ["", "desktop", ".local", "my.local", "my.local.local", ".localname.local", "my.LOCAL", "my.localhost", "日本語.local.é.local", "Αθήνα.local", "🌍.local🌍.local", "a\u202eb.local", "e\u0301.local", "a".repeat(1024) + ".local"]
    const actual = batch(names.map((hostname) => ({ op: "device", hostname })))
    for (const [index, hostname] of names.entries()) assert.equal(actual[index], upstreamFunctions(source, ["getDeviceName"], { os: { hostname: () => hostname } }).getDeviceName())
})

test("Rust target aliases preserve Node's compiled target names", () => {
    const mappings = [
        ["linux", "x86_64", true, "linux", "x64"],
        ["macos", "aarch64", true, "darwin", "arm64"],
        ["windows", "x86", true, "win32", "ia32"],
        ["linux", "arm", true, "linux", "arm"],
        ["linux", "powerpc64", false, "linux", "ppc64"],
        ["linux", "s390x", false, "linux", "s390x"],
        ["linux", "riscv64", true, "linux", "riscv64"],
        ["linux", "loongarch64", true, "linux", "loong64"],
        ["linux", "mips", true, "linux", "mipsel"],
        ["linux", "mips", false, "linux", "mips"],
        ["linux", "mips64", true, "linux", "mips64el"]
    ]
    const actual = batch(mappings.map(([platform, architecture, littleEndian]) => ({ op: "aliases", platform, architecture, littleEndian })))
    for (const [index, [, , , platform, arch]] of mappings.entries()) {
        const expected = clean(upstreamFunctions(source, ["getOS"], { os: { platform: () => platform, arch: () => arch, hostname: () => "fixture.local" } }).getOS())
        assert.deepEqual(actual[index], { platform: expected.platform, arch: expected.arch })
    }
    assert.deepEqual(value({ op: "aliases", platform: "unknown", architecture: "unknown" }), { platform: null, arch: null })
})

test("native OS/hostname/target and stable total match the original Node OS boundary", () => {
    const native = value({ op: "snapshot" })
    const original = upstreamFunctions(source, ["getOS", "getDeviceName", "checkRamUsage"], { os })
    assert.deepEqual(native.os, clean(original.getOS()))
    assert.equal(native.deviceName, original.getDeviceName())
    assert.equal(native.ram.total, original.checkRamUsage().total)
    assert.ok(native.ram.total > 0 && Number.isFinite(native.ram.total))
    assert.ok(native.ram.free >= 0 && Number.isFinite(native.ram.free))
    assert.equal(native.ram.performanceMode, sourceDecision(native.ram.total, native.ram.free))
    if (process.platform === "linux") {
        assert.equal(native.memory.totalSource, "/proc/meminfo:MemTotal")
        assert.equal(native.memory.freeSource, "/proc/meminfo:MemAvailable")
        const fields = fs.readFileSync("/proc/meminfo", "utf8")
        assert.equal(value({ op: "meminfo", text: fields, key: "MemTotal:" }), native.memory.total)
        assert.equal(value({ op: "meminfo", text: "MemFree: 123 kB\nMemAvailable: 987654 kB\n", key: "MemAvailable:" }), 987654 * 1024)
    }
    // Free changes between calls. Do not assert equality or conceal differences
    // behind a tolerance; decisions are compared on the exact captured inputs.
})

test("spoofed hostname environment never replaces the native kernel hostname", () => {
    for (const hostname of ["spoofed.local", "日本語.fake.local", "$(touch /tmp/never-run-system-probe)", "", "x".repeat(4096)]) {
        const native = value({ op: "snapshot" }, { ...process.env, HOSTNAME: hostname, COMPUTERNAME: hostname, HOST: hostname })
        assert.equal(native.os.name, os.hostname())
        assert.equal(native.deviceName, upstreamFunctions(source, ["getDeviceName"], { os }).getDeviceName())
    }
})

test("cancelled and expired native requests return explicit errors", () => {
    for (const [extra, code] of [
        [{ cancelled: true }, "system_cancelled"],
        [{ deadlineMs: 0 }, "system_deadline"]
    ]) {
        const actual = probe({ op: "os", ...extra })
        assert.equal(actual.ok, false)
        assert.equal(actual.error.code, code)
    }
})
