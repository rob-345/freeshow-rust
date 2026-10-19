// Repeated Linux release comparison. No builds, global cache flushing or claims.
import assert from "node:assert/strict"
import { spawn, spawnSync } from "node:child_process"
import { createHash, randomUUID } from "node:crypto"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"
import { performance } from "node:perf_hooks"
import { aggregateSample, distribution, memorySummary, observedCpu, pairOrder, parseStat, readProcesses } from "./release-process-metrics.mjs"

const HERE = path.dirname(fileURLToPath(import.meta.url)),
    ROOT = path.resolve(HERE, "../.."),
    LAUNCHER = path.join(HERE, "release-process-launcher.py")
const PRODUCTION_HOOKS = ["__audioFullFork", "__audioContextProbe", "declared-fullfork-audio-observer"]
const BUILD_INPUTS = ["Cargo.toml", "Cargo.lock", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock", "package.json", "package-lock.json", "rust-toolchain.toml", "vite.rust.config.mjs", "scripts/migration/build-frontend.mjs"]
const OPTIONAL_BUILD_INPUTS = [".cargo/config", ".cargo/config.toml", ".env", ".env.local", ".env.production", ".env.production.local"]
const RUNTIME_PATHS = ["src", "src-tauri", "crates", "public", "config", "scripts", "vendor", ".cargo", "Cargo.toml", "Cargo.lock", "package.json", "package-lock.json", "rust-toolchain.toml"]
const FRONTEND_COMMAND = ["node", "scripts/migration/build-frontend.mjs"]
const NATIVE_COMMAND = ["cargo", "build", "--release", "--manifest-path", "src-tauri/Cargo.toml", "--features", "custom-protocol", "--locked"]
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
const json = (file, value) => fs.writeFileSync(file, JSON.stringify(value, null, 2) + "\n")
const shortError = (error) => ({ message: String(error), code: error.code })
const bounded = async (promise, ms, label) => {
    let timer
    try {
        return await Promise.race([
            promise,
            new Promise((_, reject) => {
                timer = setTimeout(() => reject(new Error(`Timeout: ${label}`)), ms)
            })
        ])
    } finally {
        clearTimeout(timer)
    }
}
function command(binary, args = [], options = {}) {
    const result = spawnSync(binary, args, { encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024, ...options })
    return { status: result.status, stdout: result.stdout?.trim(), stderr: result.stderr?.trim(), error: result.error?.message }
}
function executable(name) {
    const result = command("which", [name])
    if (result.status !== 0) throw new Error(`Executable unavailable: ${name}`)
    return result.stdout
}
export function options(args) {
    const value = { mode: "electron-rust", pairs: 5, warmups: 1, seconds: 10, settle: 20, interval: 250, order: "alternating", seed: Math.floor(Math.random() * 2 ** 32), width: 1280, height: 800, sourceRoot: ROOT, electronBin: path.join(os.homedir(), ".local/share/freeshow-electron-baseline/v1.6.7-beta.2/extracted/opt/FreeShow/freeshow"), rustBin: null, output: null, allowedSourceChanges: [] }
    const aliases = {
        "source-root": "sourceRoot",
        "electron-bin": "electronBin",
        "rust-bin": "rustBin",
        "rust-source-commit": "rustSourceCommit",
        "expect-rust-sha256": "expectRustSha256",
        "baseline-receipt": "baselineReceipt",
        "cgroup-parent": "cgroupParent",
        "font-dir": "fontDir",
        "electron-package": "electronPackage",
        "before-bin": "beforeBin",
        "after-bin": "afterBin",
        "before-label": "beforeLabel",
        "after-label": "afterLabel",
        "before-build-receipt": "beforeBuildReceipt",
        "after-build-receipt": "afterBuildReceipt",
        "expect-before-sha256": "expectBeforeSha256",
        "expect-after-sha256": "expectAfterSha256"
    }
    for (let i = 0; i < args.length; i++) {
        const argument = args[i]
        if (argument === "--help" || argument === "--dry-run") {
            value[argument === "--help" ? "help" : "dryRun"] = true
            continue
        }
        if (!argument.startsWith("--") || i + 1 >= args.length) throw new Error(`Invalid argument ${argument}`)
        if (argument === "--allowed-source-change") {
            const file = args[++i]
            if (value.allowedSourceChanges.length >= 16 || !/^[a-zA-Z0-9_./-]{1,256}$/.test(file) || file.startsWith("/") || file.split("/").some((part) => !part || part === "." || part === "..")) throw new Error("Invalid/bounded allowed source change")
            if (value.allowedSourceChanges.includes(file)) throw new Error("Duplicate allowed source change")
            value.allowedSourceChanges.push(file)
            continue
        }
        const key = aliases[argument.slice(2)] || argument.slice(2)
        if (key === "allowedSourceChanges") throw new Error("Use bounded --allowed-source-change")
        if (!(key in value) && !Object.values(aliases).includes(key)) throw new Error(`Unknown option ${argument}`)
        value[key] = ["pairs", "warmups", "seconds", "settle", "interval", "seed", "width", "height"].includes(key) ? Number(args[++i]) : args[++i]
    }
    for (const [key, min, max] of [
        ["pairs", 1, 20],
        ["warmups", 0, 3],
        ["seconds", 2, 120],
        ["settle", 0, 60],
        ["interval", 100, 2000],
        ["seed", 0, 2 ** 32 - 1],
        ["width", 800, 1920],
        ["height", 600, 1080]
    ])
        if (!Number.isInteger(value[key]) || value[key] < min || value[key] > max) throw new Error(`${key} must be in ${min}..${max}`)
    if (!["alternating", "random"].includes(value.order)) throw new Error("order must be alternating or random")
    if (!["electron-rust", "tauri-ablation"].includes(value.mode)) throw new Error("mode must be electron-rust or tauri-ablation")
    if (value.mode === "tauri-ablation" && !value.help) {
        for (const side of ["before", "after"]) {
            for (const key of ["Bin", "BuildReceipt", "Label"]) if (!value[side + key]) throw new Error(`Require --${side}-${key === "BuildReceipt" ? "build-receipt" : key.toLowerCase()}`)
            if (!/^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,47}$/.test(value[side + "Label"])) throw new Error("Labels must be distinct bounded identifiers")
            const key = "expect" + side[0].toUpperCase() + side.slice(1) + "Sha256"
            if (!/^[a-f0-9]{64}$/.test(value[key] || "")) throw new Error(`Require --expect-${side}-sha256`)
            value[side + "Bin"] = path.resolve(value[side + "Bin"])
            value[side + "BuildReceipt"] = path.resolve(value[side + "BuildReceipt"])
        }
        if (value.beforeLabel === value.afterLabel) throw new Error("Labels must differ")
        if (value.beforeBin === value.afterBin) throw new Error("Before/after artifacts must differ")
        if (!value.allowedSourceChanges.length) throw new Error("Require bounded --allowed-source-change")
    } else if (value.mode === "electron-rust" && (value.allowedSourceChanges.length || Object.keys(value).some((key) => /^(before|after|expectBefore|expectAfter)/.test(key)))) throw new Error("Ablation options require --mode tauri-ablation")
    value.sourceRoot = path.resolve(value.sourceRoot)
    value.rustBin = path.resolve(value.rustBin || path.join(value.sourceRoot, "target/release/freeshow-desktop"))
    value.electronBin = path.resolve(value.electronBin)
    value.output = path.resolve(value.output || path.join(value.sourceRoot, "test-output-rust/release-comparison", new Date().toISOString().replace(/[:.]/g, "-")))
    value.baselineReceipt = path.resolve(value.baselineReceipt || path.join(value.sourceRoot, "docs/migration/baselines/electron-release-linux.json"))
    return value
}
export function comparisonLanes(opts) {
    return opts.mode === "tauri-ablation"
        ? [
              { key: "before", label: opts.beforeLabel, adapter: "tauri" },
              { key: "after", label: opts.afterLabel, adapter: "tauri" }
          ]
        : [
              { key: "electron", label: "electron", adapter: "electron" },
              { key: "rust", label: "rust", adapter: "tauri" }
          ]
}
export function comparisonPlan(opts) {
    const lanes = comparisonLanes(opts)
    return pairOrder(opts.pairs, opts.warmups, opts.seed, opts.order).map((entry) => ({ ...entry, runtime: lanes[entry.runtime === "electron" ? 0 : 1].key, label: lanes[entry.runtime === "electron" ? 0 : 1].label }))
}
async function hash(file) {
    const digest = createHash("sha256")
    for await (const chunk of fs.createReadStream(file)) digest.update(chunk)
    return digest.digest("hex")
}
async function fileEvidence(file) {
    const stat = fs.statSync(file)
    assert.ok(stat.isFile(), file)
    return { path: fs.realpathSync(file), sha256: await hash(file), bytes: stat.size, mtime: stat.mtime.toISOString() }
}
function git(root, args) {
    const result = command("git", args, { cwd: root })
    assert.equal(result.status, 0, `Git evidence failed: ${result.stderr || result.error}`)
    return result.stdout
}
async function noProductionHooks(file) {
    let tail = Buffer.alloc(0)
    for await (const chunk of fs.createReadStream(file)) {
        const text = Buffer.concat([tail, chunk]).toString("utf8")
        assert.ok(!PRODUCTION_HOOKS.some((hook) => text.includes(hook)), `Production hook in ${file}`)
        tail = chunk.subarray(Math.max(0, chunk.length - 128))
    }
}
// Content manifests are bounded, deterministic and symlink-free. They attest
// observed files, not an independent reconstruction of embedded Tauri assets.
export async function artifactTree(directory) {
    const root = path.resolve(directory),
        files = []
    let bytes = 0,
        entries = 0,
        pathBytes = 0
    const walk = async (relative, depth) => {
        assert.ok(depth <= 32 && !fs.lstatSync(path.join(root, relative)).isSymbolicLink(), "Artifact directory depth/symlink")
        for (const entry of fs.readdirSync(path.join(root, relative), { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))) {
            assert.ok(++entries <= 8192, "Artifact entry limit")
            const name = path.join(relative, entry.name),
                file = path.join(root, name)
            pathBytes += Buffer.byteLength(name)
            assert.ok(pathBytes <= 1024 * 1024, "Artifact aggregate path limit")
            assert.ok(Buffer.byteLength(name) <= 4096 && !entry.isSymbolicLink(), "Artifact path/symlink limit")
            if (entry.isDirectory()) await walk(name, depth + 1)
            else {
                assert.ok(entry.isFile() && files.length < 4096, "Artifact ordinary-file/count limit")
                const stat = fs.statSync(file)
                bytes += stat.size
                assert.ok(stat.size <= 128 * 1024 * 1024 && bytes <= 512 * 1024 * 1024, "Artifact byte limit")
                if (/\.(?:[cm]?js|html)$/i.test(name)) await noProductionHooks(file)
                files.push({ file: name.split(path.sep).join("/"), bytes: stat.size, sha256: await hash(file) })
            }
        }
    }
    await walk("", 0)
    assert.ok(files.length, "Empty artifact tree")
    return { root: fs.realpathSync(root), files, bytes, sha256: createHash("sha256").update(JSON.stringify(files)).digest("hex") }
}
// Called by the trusted builder AFTER the normal frontend/native commands.
// The recorded command/log declaration is provenance, not a binary attestation.
export async function captureTauriBuild({ sourceRoot, binary, buildLog }) {
    sourceRoot = fs.realpathSync(sourceRoot)
    const sourceCommit = git(sourceRoot, ["rev-parse", "HEAD"])
    assert.match(sourceCommit, /^[a-f0-9]{40}$/)
    assert.equal(git(sourceRoot, ["status", "--porcelain=v1", "--untracked-files=no"]), "", "Tracked build source must be clean")
    assert.ok(fs.statSync(binary).size <= 1024 * 1024 * 1024, "Release executable exceeds1GiB")
    const executable = await fileEvidence(binary)
    assert.equal(path.basename(path.dirname(executable.path)), "release", "Require release artifact layout")
    assert.equal(path.basename(path.dirname(path.dirname(executable.path))), "target", "Keep physical target/release resource layout")
    const logBytes = fs.statSync(buildLog).size
    assert.ok(logBytes > 0 && logBytes <= 16 * 1024 * 1024, "Bounded actual build log required")
    const log = await fileEvidence(buildLog)
    const text = fs.readFileSync(log.path, "utf8")
    assert.match(text, /building[^\n]*production/i, "Normal production frontend build log required")
    assert.match(text, /Finished [`']release[`'] profile/, "Native release build log required")
    assert.ok(!PRODUCTION_HOOKS.some((hook) => text.includes(hook)), "Instrumented build log")
    const inputs = {}
    for (const file of BUILD_INPUTS) {
        assert.ok(fs.statSync(path.join(sourceRoot, file)).size <= 16 * 1024 * 1024, "Build input exceeds16MiB")
        const evidence = await fileEvidence(path.join(sourceRoot, file))
        inputs[file] = { sha256: evidence.sha256, bytes: evidence.bytes }
    }
    for (const file of OPTIONAL_BUILD_INPUTS) {
        const location = path.join(sourceRoot, file)
        if (!fs.existsSync(location)) inputs[file] = null
        else {
            assert.ok(fs.statSync(location).size <= 16 * 1024 * 1024, "Optional build input exceeds16MiB")
            const evidence = await fileEvidence(location)
            inputs[file] = { sha256: evidence.sha256, bytes: evidence.bytes }
        }
    }
    const frontend = await artifactTree(path.join(sourceRoot, "dist-rust")),
        resources = await artifactTree(path.join(path.dirname(executable.path), "remotes"))
    for (const kind of ["remote", "stage", "controller"])
        assert.ok(
            resources.files.some((file) => file.file === `${kind}/index.html`),
            `Missing actual ${kind} resource`
        )
    const tools = { node: process.versions, rustc: command("rustc", ["-Vv"], { cwd: sourceRoot }), cargo: command("cargo", ["-V"], { cwd: sourceRoot }) }
    assert.equal(tools.rustc.status, 0, "Record actual build rustc")
    assert.equal(tools.cargo.status, 0, "Record actual build cargo")
    const buildEnvironment = Object.fromEntries(
        Object.keys(process.env)
            .filter((key) => key !== "CARGO_TARGET_DIR" && /^(?:RUSTFLAGS|RUSTC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|CARGO_ENCODED_RUSTFLAGS|CARGO_BUILD_.*|CARGO_TARGET_.*|CARGO_PROFILE_RELEASE_.*|RUSTUP_TOOLCHAIN|NODE_ENV|CC|CXX|CFLAGS|CXXFLAGS|LDFLAGS|PKG_CONFIG_.*)$/.test(key))
            .sort()
            .map((key) => [key, process.env[key]])
    )
    // Vite variables may contain credentials. Compare their hashes, never dump
    // their raw values into receipts. Files above are likewise content hashes.
    const viteEnvironment = Object.fromEntries(
        Object.keys(process.env)
            .filter((key) => key.startsWith("VITE_"))
            .sort()
            .map((key) => [key, createHash("sha256").update(process.env[key]).digest("hex")])
    )
    assert.ok(Object.keys(buildEnvironment).length <= 64 && Buffer.byteLength(JSON.stringify(buildEnvironment)) <= 32768, "Build environment bound")
    assert.ok(Object.keys(viteEnvironment).length <= 128 && Buffer.byteLength(JSON.stringify(viteEnvironment)) <= 32768, "Vite environment bound")
    return { schemaVersion: 1, kind: "tauri-production-release-build", sourceRoot, sourceCommit, build: { profile: "release", features: ["custom-protocol"], instrumentation: "none", frontendCommand: FRONTEND_COMMAND, nativeCommand: NATIVE_COMMAND }, executable, buildLog: log, inputs, frontend, resources, tools, buildEnvironment, viteEnvironment, limit: "Host-recorded build provenance; logs/manifests do not independently prove embedded binary contents" }
}
export async function verifyTauriBuild(receiptFile, binary, expectedSha256) {
    assert.ok(fs.statSync(receiptFile).size <= 8 * 1024 * 1024, "Build receipt exceeds8MiB")
    const receipt = JSON.parse(fs.readFileSync(receiptFile, "utf8"))
    assert.equal(receipt.schemaVersion, 1)
    assert.equal(receipt.kind, "tauri-production-release-build")
    assert.deepEqual(receipt.build, { profile: "release", features: ["custom-protocol"], instrumentation: "none", frontendCommand: FRONTEND_COMMAND, nativeCommand: NATIVE_COMMAND })
    const observed = await captureTauriBuild({ sourceRoot: receipt.sourceRoot, binary, buildLog: receipt.buildLog.path })
    assert.equal(observed.executable.sha256, expectedSha256, "Selected binary changed")
    for (const key of ["sourceRoot", "sourceCommit", "inputs", "frontend", "resources"]) assert.deepEqual(observed[key], receipt[key], `Build ${key} changed`)
    for (const key of ["path", "sha256", "bytes"]) {
        assert.equal(observed.executable[key], receipt.executable[key], "Build executable mismatch")
        assert.equal(observed.buildLog[key], receipt.buildLog[key], "Build log changed")
    }
    return { ...receipt, receiptFile: await fileEvidence(receiptFile) }
}
export async function ablationProvenance(opts) {
    const before = await verifyTauriBuild(opts.beforeBuildReceipt, opts.beforeBin, opts.expectBeforeSha256),
        after = await verifyTauriBuild(opts.afterBuildReceipt, opts.afterBin, opts.expectAfterSha256)
    assert.notEqual(before.executable.sha256, after.executable.sha256, "Ablation binaries must differ")
    assert.notEqual(before.sourceCommit, after.sourceCommit, "Ablation source commits must differ")
    assert.deepEqual(before.inputs, after.inputs, "Build inputs/features/lockfiles differ")
    assert.deepEqual(before.tools, after.tools, "Recorded build toolchains differ")
    assert.deepEqual(before.buildEnvironment, after.buildEnvironment, "Recorded build environment differs")
    assert.deepEqual(before.viteEnvironment, after.viteEnvironment, "Recorded Vite environment differs")
    assert.equal(before.resources.sha256, after.resources.sha256, "Unrelated remote resources differ")
    const changed = git(opts.sourceRoot, ["diff", "--name-only", "-z", before.sourceCommit, after.sourceCommit, "--", ...RUNTIME_PATHS])
        .split("\0")
        .filter(Boolean)
    assert.ok(changed.length && changed.length <= 16, "Require bounded actual runtime change")
    assert.deepEqual([...changed].sort(), [...opts.allowedSourceChanges].sort(), "Runtime change differs from explicit allowlist")
    return { before, after, sourceChanges: changed, labels: { before: opts.beforeLabel, after: opts.afterLabel } }
}
export function assertProductionState(state) {
    assert.ok(Array.isArray(state.productionHooks) && state.productionHooks.length === 0, "Known test hook present in actual production WebView")
}
function fontFiles(directory) {
    const files = []
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
        const file = path.join(directory, entry.name)
        if (entry.isDirectory()) files.push(...fontFiles(file))
        else if (entry.isFile()) files.push(file)
    }
    return files.sort()
}
async function provenance(opts) {
    let selected
    if (opts.mode === "tauri-ablation") selected = { ablation: await ablationProvenance(opts) }
    else {
        const baseline = JSON.parse(fs.readFileSync(opts.baselineReceipt)),
            electron = await fileEvidence(opts.electronBin),
            rust = await fileEvidence(opts.rustBin)
        assert.equal(baseline.release, "v1.6.7-beta.2")
        assert.equal(baseline.upstreamCommit, "2d94defcf98175e4f5c9504e7f4fd1d33c35c975")
        assert.ok(baseline.sha256VerifiedAgainstPublishedDigest && baseline.readiness?.ready, "Require verified official release readiness receipt")
        if (opts.expectRustSha256) assert.equal(rust.sha256, opts.expectRustSha256, "Rust binary changed from expected release")
        const asset = path.resolve(opts.electronPackage || path.join(path.dirname(opts.electronBin), "../../..", baseline.asset.name)),
            packageFile = await fileEvidence(asset)
        assert.equal("sha256:" + packageFile.sha256, baseline.asset.digest, "Official package checksum mismatch")
        selected = { electron, rust, officialPackage: packageFile, officialReceipt: baseline, officialAppAsar: await fileEvidence(path.join(path.dirname(opts.electronBin), "resources/app.asar")) }
    }
    const fonts = [...(command("fc-list", ["--format", "%{file}\n"]).stdout?.split("\n").filter(Boolean) || []), ...(opts.fontDir ? fontFiles(path.resolve(opts.fontDir)) : [])]
    assert.ok(fonts?.length, "Font inventory is empty")
    const fontEvidence = []
    for (const file of [...new Set(fonts)].sort()) {
        if (fontEvidence.length >= 10000) throw new Error("Font inventory exceeds bound")
        fontEvidence.push(await fileEvidence(file))
    }
    const tools = {}
    for (const [name, binary] of Object.entries({ xvfb: executable("Xvfb"), sessionBus: executable("dbus-daemon"), python: executable("python3"), tauriDriver: process.env.TAURI_DRIVER || executable("tauri-driver"), webkitWebDriver: process.env.WEBKIT_WEBDRIVER || executable("WebKitWebDriver") })) tools[name] = await fileEvidence(binary)
    const source = { declaredRustBuildCommit: opts.rustSourceCommit ?? null, observedHead: command("git", ["rev-parse", "HEAD"], { cwd: opts.sourceRoot }), status: command("git", ["status", "--porcelain=v1"], { cwd: opts.sourceRoot }), note: "Observed checkout is not a build attestation; use declared immutable build commit + expected binary SHA." }
    const runner = []
    for (const file of [fileURLToPath(import.meta.url), LAUNCHER, path.join(HERE, "release-process-metrics.mjs")]) runner.push(await fileEvidence(file))
    return {
        ...selected,
        source,
        runner,
        tools,
        fonts: fontEvidence,
        fontManifestSha256: createHash("sha256")
            .update(JSON.stringify(fontEvidence.map(({ path, sha256 }) => ({ path, sha256 }))))
            .digest("hex"),
        environment: {
            platform: process.platform,
            arch: process.arch,
            kernel: os.release(),
            node: process.versions,
            cpus: os.cpus().map(({ model, speed }) => ({ model, speed })),
            parallelism: os.availableParallelism(),
            memoryTotal: os.totalmem(),
            loadAverage: os.loadavg(),
            osRelease: fs.readFileSync("/etc/os-release", "utf8"),
            clockTicks: Number(command("getconf", ["CLK_TCK"]).stdout),
            pageBytes: Number(command("getconf", ["PAGESIZE"]).stdout),
            webkit: command("pkg-config", ["--modversion", "webkit2gtk-4.1"]),
            webkitPackage: command("dpkg-query", ["-W", "-f=${Version}", "libwebkit2gtk-4.1-0"]),
            currentCgroup: fs.readFileSync("/proc/self/cgroup", "utf8"),
            schedulerConstraints: fs
                .readFileSync("/proc/self/status", "utf8")
                .split("\n")
                .filter((line) => /^(Cpus_allowed_list|Mems_allowed_list):/.test(line)),
            inheritedRuntimeOverrides: Object.fromEntries(["LD_LIBRARY_PATH", "GI_TYPELIB_PATH", "FONTCONFIG_FILE", "FONTCONFIG_PATH", "TAURI_DRIVER", "WEBKIT_WEBDRIVER", "FREESHOW_RUST_LTC_LIBRARY"].filter((key) => process.env[key]).map((key) => [key, process.env[key]])),
            audioBackend: { overrides: Object.fromEntries(["PULSE_SERVER", "PULSE_SINK", "PULSE_SOURCE", "PULSE_LATENCY_MSEC", "ALSA_CONFIG_PATH", "AUDIODEV", "GST_PLUGIN_PATH", "GST_PLUGIN_SYSTEM_PATH", "GST_PLUGIN_FEATURE_RANK"].filter((key) => process.env[key]).map((key) => [key, process.env[key]])), accounting: "Shared/externally activated audio services are outside the observed app tree; record/freeze the actual provider separately" },
            displayMode: "private Xvfb 1600x1100x24, 96 DPI; software-oriented render policy",
            cacheScope: "fresh application/XDG caches for every run; OS/filesystem caches NOT flushed and warmed by provenance hashing and earlier runs",
            observerCpuIncluded: false
        }
    }
}
async function fdService(binary, args, logFile, env) {
    const fd = fs.openSync(logFile, "a"),
        child = spawn(binary, args, { env, stdio: ["ignore", fd, fd, "pipe"], detached: true })
    fs.closeSync(fd)
    let output = ""
    child.stdio[3].setEncoding("utf8")
    const ready = new Promise((resolve, reject) => {
        child.once("error", reject)
        child.once("exit", (code) => reject(new Error(`${binary} exited ${code} before ready`)))
        child.stdio[3].on("data", (chunk) => {
            output += chunk
            if (output.includes("\n")) resolve(output.trim().split("\n")[0])
        })
    })
    try {
        return { child, value: await bounded(ready, 10000, binary) }
    } catch (error) {
        await stopProcess(child)
        throw error
    }
}
async function stopProcess(child) {
    if (!child || child.exitCode !== null || child.signalCode !== null) return
    try {
        process.kill(-child.pid, "SIGTERM")
    } catch (error) {
        if (error.code !== "ESRCH") throw error
    }
    const end = performance.now() + 2000
    while (child.exitCode === null && child.signalCode === null && performance.now() < end) await sleep(25)
    if (child.exitCode === null && child.signalCode === null) {
        try {
            process.kill(-child.pid, "SIGKILL")
        } catch (error) {
            if (error.code !== "ESRCH") throw error
        }
    }
    await bounded(
        new Promise((resolve) => {
            if (child.exitCode !== null || child.signalCode !== null) resolve()
            else child.once("exit", resolve)
        }),
        2000,
        "owned support process exit"
    )
}
function seedProfile(base, opts) {
    const app = path.join(base, "app"),
        data = path.join(base, "data"),
        config = path.join(base, "xdg-config")
    for (const folder of [app, path.join(data, "Config"), config, "xdg-cache", "xdg-data", "xdg-runtime", "home", "tmp"].map((folder) => (path.isAbsolute(folder) ? folder : path.join(base, folder)))) fs.mkdirSync(folder, { recursive: true })
    for (const name of ["DESKTOP", "DOWNLOAD", "DOCUMENTS", "MUSIC", "PICTURES", "VIDEOS", "TEMPLATES", "PUBLICSHARE"]) {
        const directory = path.join(base, name.toLowerCase())
        fs.mkdirSync(directory)
        fs.appendFileSync(path.join(config, "user-dirs.dirs"), `XDG_${name}_DIR="${directory}"\n`)
    }
    if (opts.fontDir) fs.cpSync(path.resolve(opts.fontDir), path.join(base, "xdg-data/fonts"), { recursive: true })
    const settings = { initialized: true, language: "en", outputs: {}, autoOutput: false, autoUpdates: false, alertUpdates: false, disabledServers: { remote: true, stage: true, controller: true, output_stream: true }, serverData: { companion: { enabled: false } }, special: { showClosePopup: false }, timeFormat: "24", autosave: "15min" }
    fs.chmodSync(path.join(base, "xdg-runtime"), 0o700)
    const configuration = { dataPath: data, maximized: false, bounds: { width: opts.width, height: opts.height, x: 0, y: 0 }, disableHardwareAcceleration: true }
    json(path.join(app, "settings.json"), settings)
    json(path.join(app, "config.json"), configuration)
    return {
        app,
        data,
        seed: { settings, config: { ...configuration, dataPath: "<PRIVATE_PROFILE>/data" } },
        env: {
            HOME: path.join(base, "home"),
            TMPDIR: path.join(base, "tmp"),
            NODE_ENV: "production",
            FS_MOCK_STORE_PATH: app,
            FREESHOW_RUST_APP_DATA: app,
            FREESHOW_RUST_DATA_DIR: data,
            XDG_CONFIG_HOME: config,
            XDG_CACHE_HOME: path.join(base, "xdg-cache"),
            XDG_DATA_HOME: path.join(base, "xdg-data"),
            XDG_RUNTIME_DIR: path.join(base, "xdg-runtime"),
            LANG: "C.UTF-8",
            LC_ALL: "C.UTF-8",
            GSETTINGS_BACKEND: "memory",
            GDK_BACKEND: "x11",
            LIBGL_ALWAYS_SOFTWARE: "1",
            WEBKIT_DISABLE_COMPOSITING_MODE: "1",
            WEBKIT_DISABLE_DMABUF_RENDERER: "1",
            TAURI_WEBVIEW_AUTOMATION: "true"
        }
    }
}
const readyScript = `return (()=>{const e=document.querySelector('.top'),r=e?.getBoundingClientRect();const css=e?getComputedStyle(e):null;return {ready:!!window.api?.send&&!!r&&r.width>0&&r.height>0&&css.display!=='none'&&css.visibility!=='hidden'&&!document.querySelector('#guideButtons')&&!document.querySelector('.popup'),viewport:{width:innerWidth,height:innerHeight,dpr:devicePixelRatio},title:document.title,url:location.href,fontStatus:document.fonts.status,nav:[...document.querySelectorAll('.top button')].map(e=>e.dataset.title||e.textContent.trim()),bodyText:document.body.innerText.slice(0,1000),documentReady:document.readyState,visibility:document.visibilityState,productionHooks:${JSON.stringify(PRODUCTION_HOOKS)}.filter(key=>key in window)}})()`
export function visibleEditorSignature(state) {
    return { viewport: state.viewport, nav: state.nav, emptyLibrary: ["Projects", "Nothing here", "New project", "New show", "Shows", "No shows", "Import"].map((label) => ({ label, present: state.bodyText.includes(label) })) }
}
async function wait(fn, label, ms = 45000) {
    const end = performance.now() + ms
    let error
    while (performance.now() < end) {
        try {
            const value = await fn()
            if (value) return value
        } catch (found) {
            error = found
        }
        await sleep(150)
    }
    throw new Error(`Readiness timeout ${label}: ${error || "not ready"}`)
}
async function request(base, method, route, payload) {
    const response = await fetch(base + route, { method, headers: payload ? { "content-type": "application/json" } : {}, body: payload ? JSON.stringify(payload) : undefined, signal: AbortSignal.timeout(45000) })
    const text = await response.text()
    if (!response.ok) throw new Error(`WebDriver ${method} ${route}: ${text.slice(0, 1500)}`)
    const result = JSON.parse(text).value
    if (result?.error) throw new Error(JSON.stringify(result))
    return result
}
async function freePort() {
    const net = await import("node:net")
    const server = net.createServer()
    await new Promise((resolve, reject) => {
        server.once("error", reject)
        server.listen(0, "127.0.0.1", resolve)
    })
    const port = server.address().port
    await new Promise((resolve) => server.close(resolve))
    return port
}
async function tauriAdapter(wrapper, env, artifacts) {
    const port = await freePort(),
        nativePort = await freePort(),
        log = fs.openSync(path.join(artifacts, "driver.log"), "a")
    const driver = spawn(process.env.TAURI_DRIVER || executable("tauri-driver"), ["--port", String(port), "--native-port", String(nativePort), "--native-driver", process.env.WEBKIT_WEBDRIVER || executable("WebKitWebDriver")], { env, stdio: ["ignore", log, log], detached: true })
    fs.closeSync(log)
    const base = `http://127.0.0.1:${port}`,
        native = `http://127.0.0.1:${nativePort}`
    let session
    const adapter = {
        driver,
        async execute(source) {
            return request(native, "POST", `/session/${session}/execute/sync`, { script: source, args: [] })
        },
        async resize(width, height) {
            for (let i = 0; i < 3; i++) {
                const view = await adapter.execute("return {width:innerWidth,height:innerHeight}")
                if (view.width === width && view.height === height) return
                const rect = await request(native, "GET", `/session/${session}/window/rect`)
                await request(native, "POST", `/session/${session}/window/rect`, { x: 0, y: 0, width: rect.width + width - view.width, height: rect.height + height - view.height })
                await sleep(150)
            }
        },
        async screenshot(file) {
            fs.writeFileSync(file, Buffer.from(await request(native, "GET", `/session/${session}/screenshot`), "base64"))
        },
        async close() {
            try {
                if (session) await bounded(request(native, "DELETE", `/session/${session}`), 5000, "WebDriver close")
            } finally {
                await stopProcess(driver)
            }
        }
    }
    try {
        await wait(async () => {
            if (driver.exitCode !== null) throw new Error("tauri-driver exited")
            return (await request(base, "GET", "/status")).ready
        }, "WebDriver")
        const created = await request(base, "POST", "/session", { capabilities: { alwaysMatch: { "tauri:options": { application: wrapper } } } })
        session = created.sessionId
        adapter.capabilities = created.capabilities
        return adapter
    } catch (error) {
        await adapter.close().catch(() => {})
        throw error
    }
}
async function electronAdapter(wrapper, env, artifacts) {
    const { _electron } = await import("playwright")
    const app = await _electron.launch({ executablePath: wrapper, args: ["--no-sandbox", "--disable-gpu"], env, timeout: 45000 })
    const logs = []
    app.on("console", (message) => {
        if (logs.length < 100) logs.push({ type: message.type(), text: message.text().slice(0, 2048) })
    })
    let main
    await wait(() => {
        main = app.windows().find((page) => page.url().includes("index.html"))
        return main
    }, "Electron main window")
    const adapter = {
        async execute(source) {
            return main.evaluate("(function(){" + source + "})()")
        },
        async resize(width, height) {
            const window = await app.browserWindow(main)
            await window.evaluate(
                (window, size) => {
                    window.setContentSize(size.width, size.height)
                    window.setPosition(0, 0)
                },
                { width, height }
            )
            await sleep(150)
        },
        async screenshot(file) {
            await main.screenshot({ path: file })
        },
        async close() {
            try {
                await bounded(
                    app.evaluate(({ app }) => app.exit(0)).catch(() => {}),
                    3000,
                    "Electron exit"
                )
                await bounded(
                    app.close().catch(() => {}),
                    5000,
                    "Electron close"
                )
            } finally {
                json(path.join(artifacts, "console.json"), logs)
            }
        }
    }
    adapter.runtime = await app.evaluate(({ app }) => ({ packaged: app.isPackaged, version: app.getVersion(), versions: process.versions, paths: Object.fromEntries(["userData", "appData", "cache", "documents", "downloads"].map((key) => [key, app.getPath(key)])) }))
    return adapter
}
function cgroupPids(folder) {
    const pids = new Set()
    if (!folder) return pids
    const visit = (dir, depth) => {
        if (depth > 16) throw new Error("cgroup nesting exceeds bound")
        for (const pid of fs.readFileSync(path.join(dir, "cgroup.procs"), "utf8").trim().split(/\s+/)) if (pid) pids.add(Number(pid))
        for (const entry of fs.readdirSync(dir, { withFileTypes: true })) if (entry.isDirectory()) visit(path.join(dir, entry.name), depth + 1)
    }
    visit(folder, 0)
    return pids
}
function cgroupCpu(folder) {
    if (!folder) return null
    const match = /^usage_usec (\d+)$/m.exec(fs.readFileSync(path.join(folder, "cpu.stat"), "utf8"))
    return match ? Number(match[1]) : null
}
class Monitor {
    constructor({ artifacts, receipt, runId, cgroup, hz, pageBytes }) {
        Object.assign(this, { receipt, runId, cgroup, hz, pageBytes })
        this.known = new Set()
        this.roots = []
        this.samples = []
        this.history = new Map()
        this.fd = fs.openSync(path.join(artifacts, "samples.jsonl"), "w")
        this.phase = "launch"
    }
    sample() {
        const begin = performance.now()
        if (!this.roots.length && fs.existsSync(this.receipt)) this.roots = [JSON.parse(fs.readFileSync(this.receipt))]
        const snapshot = readProcesses({ pageBytes: this.pageBytes, marker: this.runId, cgroupPids: cgroupPids(this.cgroup), known: this.known, roots: this.roots })
        for (const row of snapshot.processes) {
            this.known.add(row.identity)
            const previous = this.history.get(row.identity)
            this.history.set(row.identity, { ...row, firstObservedMs: previous?.firstObservedMs ?? begin, lastObservedMs: begin })
        }
        const sample = { phase: this.phase, monotonicMs: begin, uptimeSeconds: Number(fs.readFileSync("/proc/uptime", "utf8").split(" ")[0]), ...snapshot, ...aggregateSample(snapshot), cgroupCpuUsec: cgroupCpu(this.cgroup), observer: { pid: process.pid, cpu: process.cpuUsage(), rssBytes: process.memoryUsage().rss }, collectionMs: performance.now() - begin }
        sample.cgroupReadMonotonicMs = performance.now()
        sample.observationEndMs = sample.cgroupReadMonotonicMs
        if (this.phase === "steady") this.samples.push(sample)
        fs.writeSync(this.fd, JSON.stringify(sample) + "\n")
        return sample
    }
    start(interval) {
        this.interval = setInterval(() => {
            try {
                this.sample()
            } catch (error) {
                this.error = error
                clearInterval(this.interval)
            }
        }, interval)
    }
    close() {
        clearInterval(this.interval)
        if (this.fd !== null) {
            fs.closeSync(this.fd)
            this.fd = null
        }
    }
}
async function cleanupApps(monitor) {
    const killed = [],
        deadline = performance.now() + 5000
    for (let round = 0; round < 40; round++) {
        const sample = monitor.sample(),
            living = sample.processes.filter((row) => row.state !== "Z")
        if (!living.length) {
            await sleep(100)
            if (!monitor.sample().processes.some((row) => row.state !== "Z")) return { complete: true, killed, zombies: sample.processes.filter((row) => row.state === "Z").map((row) => row.identity) }
        }
        const signal = performance.now() > deadline ? "SIGKILL" : "SIGTERM"
        for (const row of living) {
            try {
                const current = parseStat(fs.readFileSync(`/proc/${row.pid}/stat`, "utf8"))
                if (current.identity === row.identity) {
                    process.kill(row.pid, signal)
                    killed.push({ identity: row.identity, signal })
                }
            } catch (error) {
                if (!["ENOENT", "ESRCH"].includes(error.code)) throw error
            }
        }
        await sleep(150)
    }
    return { complete: false, killed, remaining: monitor.sample().processes }
}
async function runOne(entry, index, opts, display, evidence) {
    const artifacts = path.join(opts.output, `${String(index).padStart(2, "0")}-${entry.runtime}${entry.warmup ? "-warmup" : ""}`)
    fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 })
    const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-release-comparison-")),
        profile = seedProfile(temporary, opts),
        runId = randomUUID(),
        receipt = path.join(artifacts, "launch.json")
    const lane = comparisonLanes(opts).find((lane) => lane.key === entry.runtime)
    assert.ok(lane, "Unknown comparison lane")
    const record = {
        ...entry,
        runtimeName: entry.runtime,
        label: lane.label,
        adapter: lane.adapter,
        index,
        runId,
        artifacts,
        temporary,
        profileSeed: profile.seed,
        profileSeedSha256: createHash("sha256").update(JSON.stringify(profile.seed)).digest("hex"),
        startup: { comparable: false, reason: opts.mode === "tauri-ablation" ? "Driver/readiness/polling/resizing overhead included; diagnostic only" : "Different Playwright/Electron debugger and WebKitWebDriver/Tauri launch paths; not a startup metric" },
        valid: false
    }
    record.hostLoadAtStart = os.loadavg()
    let adapter,
        bus,
        monitor,
        cgroup = null
    try {
        if (opts.cgroupParent) {
            try {
                cgroup = path.join(path.resolve(opts.cgroupParent), `freeshow-comparison-${runId}`)
                fs.mkdirSync(cgroup)
                assert.ok(fs.existsSync(path.join(cgroup, "cgroup.procs")))
                record.cgroup = { path: cgroup, mode: "private delegated cgroup v2" }
            } catch (error) {
                record.cgroup = { unavailable: shortError(error) }
                if (cgroup && fs.existsSync(cgroup)) fs.rmdirSync(cgroup)
                cgroup = null
            }
        }
        const supportEnv = { ...process.env, ...profile.env, DISPLAY: display }
        for (const key of ["NODE_OPTIONS", "ELECTRON_RUN_AS_NODE", "FREESHOW_COMPARISON_RUN_ID", "FREESHOW_COMPARISON_LAUNCH_CONFIG"]) delete supportEnv[key]
        const began = performance.now()
        bus = await fdService(executable("dbus-daemon"), ["--session", "--nofork", "--print-address=3"], path.join(artifacts, "dbus.log"), supportEnv)
        supportEnv.DBUS_SESSION_BUS_ADDRESS = bus.value
        const launchConfig = path.join(artifacts, "launch-config.json")
        json(launchConfig, { binary: opts[entry.runtime + "Bin"], receipt, runId, cgroup, env: profile.env })
        const wrapper = path.join(temporary, "application-launcher")
        fs.copyFileSync(LAUNCHER, wrapper)
        fs.chmodSync(wrapper, 0o700)
        supportEnv.FREESHOW_COMPARISON_LAUNCH_CONFIG = launchConfig
        monitor = new Monitor({ artifacts, receipt, runId, cgroup, hz: evidence.environment.clockTicks, pageBytes: evidence.environment.pageBytes })
        monitor.start(opts.interval)
        adapter = lane.adapter === "electron" ? await electronAdapter(wrapper, supportEnv, artifacts) : await tauriAdapter(wrapper, supportEnv, artifacts)
        record.startup.adapterReadyMs = performance.now() - began
        await wait(async () => {
            const state = await adapter.execute(readyScript)
            return state.ready && state.documentReady === "complete" && state.fontStatus === "loaded" && state
        }, "visible editor")
        await adapter.resize(opts.width, opts.height)
        const before = await wait(async () => {
            const state = await adapter.execute(readyScript)
            return state.ready && state.fontStatus === "loaded" && state.viewport.width === opts.width && state.viewport.height === opts.height && state
        }, "same content viewport")
        record.startup.observedVisibleEditorMs = performance.now() - began
        if (adapter.runtime) {
            record.runtimeObservation = adapter.runtime
            for (const location of Object.values(adapter.runtime.paths)) assert.ok(location.startsWith(temporary + path.sep), `Escaped Electron profile ${location}`)
            assert.equal(adapter.runtime.packaged, true)
            assert.equal(adapter.runtime.version, "1.6.7-beta.2")
            for (const [name, version] of Object.entries(evidence.officialReceipt.readiness.versions)) assert.equal(adapter.runtime.versions[name], version, `Official runtime ${name} differs from readiness receipt`)
        } else
            record.runtimeObservation = {
                bridgeVersion: await adapter.execute(`return new Promise((resolve,reject)=>{const id='release-version-${runId}',timer=setTimeout(()=>reject(new Error('version timeout')),3000);window.api.receive('MAIN',(m,l)=>{if(m.channel==='VERSION'&&l===id){clearTimeout(timer);window.api.removeListener('MAIN',id);resolve(m.data)}},id);window.api.send('MAIN',{channel:'VERSION'},id)})`),
                library: await adapter.execute("return window.__TAURI__.core.invoke('library_status')"),
                capabilities: adapter.capabilities
            }
        if (lane.adapter === "tauri") assert.equal(record.runtimeObservation.library.context.dataPath, profile.data, "Escaped Rust library profile")
        if (opts.mode === "tauri-ablation") assertProductionState(before)
        record.viewport = before.viewport
        record.before = before
        assert.ok(
            visibleEditorSignature(before).emptyLibrary.every((row) => row.present),
            "Expected empty-library workload"
        )
        record.launch = JSON.parse(fs.readFileSync(receipt))
        assert.equal(record.launch.sid, record.launch.pid, "App must own a private session")
        monitor.phase = "settle"
        await sleep(opts.settle * 1000)
        if (monitor.error) throw monitor.error
        monitor.phase = "steady"
        monitor.sample()
        await sleep(opts.seconds * 1000)
        monitor.sample()
        monitor.phase = "validation"
        if (monitor.error) throw monitor.error
        record.after = await adapter.execute(readyScript)
        if (opts.mode === "tauri-ablation") assertProductionState(record.after)
        assert.equal(record.after.ready, true)
        assert.deepEqual(record.after.viewport, before.viewport)
        await adapter.screenshot(path.join(artifacts, "editor.png"))
        assert.equal(JSON.parse(fs.readFileSync(path.join(profile.app, "config.json"))).dataPath, profile.data)
        const samples = monitor.samples
        assert.ok(samples.length >= 2)
        assert.ok(
            samples.every((sample) => sample.processCount > 0),
            "No application membership during sampling"
        )
        const observed = observedCpu(samples, evidence.environment.clockTicks),
            first = samples[0],
            last = samples.at(-1),
            exact = Number.isFinite(first.cgroupCpuUsec) && Number.isFinite(last.cgroupCpuUsec)
        record.cpu = exact ? { method: "cgroup-v2-cpu.stat", cpuSeconds: (last.cgroupCpuUsec - first.cgroupCpuUsec) / 1e6, percentOneCore: (100 * (last.cgroupCpuUsec - first.cgroupCpuUsec)) / 1000 / (last.cgroupReadMonotonicMs - first.cgroupReadMonotonicMs), observedLowerBound: observed } : observed
        record.memory = memorySummary(samples)
        record.hostLoadAtEnd = os.loadavg()
        record.sampleCount = samples.length
        record.collectionMs = distribution(samples.map((sample) => sample.collectionMs))
        record.membership = [...monitor.history.values()]
        record.unmatchedMarkerReadFailures = distribution(samples.map((sample) => sample.unmatchedMarkerReadFailures))
        record.valid = true
    } catch (error) {
        record.error = shortError(error)
    } finally {
        if (monitor) monitor.phase = "cleanup"
        if (adapter)
            try {
                await bounded(adapter.close(), 10000, "adapter teardown")
            } catch (error) {
                record.adapterCleanupError = shortError(error)
            }
        if (monitor) {
            try {
                record.cleanup = await cleanupApps(monitor)
            } catch (error) {
                record.cleanup = { complete: false, error: shortError(error) }
            } finally {
                monitor.close()
            }
        }
        if (bus)
            try {
                await stopProcess(bus.child)
            } catch (error) {
                record.busCleanupError = shortError(error)
            }
        if (cgroup)
            try {
                fs.rmdirSync(cgroup)
                record.cgroup.removed = true
            } catch (error) {
                record.cgroup.cleanupError = shortError(error)
            }
        fs.rmSync(temporary, { recursive: true, force: true })
        record.temporaryProfileRemoved = !fs.existsSync(temporary)
        if (!record.cleanup?.complete || record.adapterCleanupError || record.busCleanupError || record.cgroup?.cleanupError) record.valid = false
        json(path.join(artifacts, "run.json"), record)
    }
    return record
}
export function summarize(
    runs,
    lanes = [
        { key: "electron", label: "electron" },
        { key: "rust", label: "rust" }
    ]
) {
    const measured = runs.filter((run) => !run.warmup),
        valid = measured.length > 0 && measured.every((run) => run.valid),
        pssComplete = valid && measured.every((run) => run.memory?.pssComplete)
    const runtime = {}
    for (const { key: name, label } of lanes) {
        const rows = measured.filter((run) => run.runtime === name || run.runtimeName === name || run.kind === name)
        runtime[name] = {
            label,
            runs: rows.length,
            validRuns: rows.filter((row) => row.valid).length,
            completePssRuns: rows.filter((row) => row.valid && row.memory?.pssComplete).length,
            perRunMedianPssBytes: pssComplete ? distribution(rows.map((row) => row.memory.pssSumBytes.p50)) : null,
            completePssRunSubset: { meaning: "Selected complete runs only, not a universal comparison", distribution: distribution(rows.filter((row) => row.valid && row.memory?.pssComplete).map((row) => row.memory.pssSumBytes.p50)) },
            pssCoverage: rows.map((row) => ({ run: row.index, valid: row.valid, ...row.memory?.coverage })),
            perRunMedianRssSumBytes: distribution(rows.filter((row) => row.valid).map((row) => row.memory.rssSumBytes.p50)),
            perRunCpuPercentOneCore: distribution(rows.filter((row) => row.valid).map((row) => row.cpu.percentOneCore)),
            cpuMethods: [...new Set(rows.filter((row) => row.valid).map((row) => row.cpu.method))]
        }
    }
    return { valid, primaryMemoryMetric: pssComplete ? "sum_pss_bytes_all_observed_processes" : "no-complete-PSS-comparison; RSS sum reported separately", startupComparable: false, exactCpuComparable: valid && measured.every((run) => run.cpu?.method === "cgroup-v2-cpu.stat"), runtime, ratios: null, note: "Descriptive run-level distributions only. No startup speedup, physical-memory reduction or production/hardware performance claim." }
}
async function main(opts) {
    assert.equal(process.platform, "linux", "Linux /proc harness only")
    assert.ok(!fs.existsSync(opts.output) || !fs.readdirSync(opts.output).length, "Output directory must be new/empty")
    fs.mkdirSync(opts.output, { recursive: true, mode: 0o700 })
    const evidence = await provenance(opts),
        report = {
            schemaVersion: 2,
            kind: opts.mode === "tauri-ablation" ? "tauri-release-idle-editor-ablation" : "release-idle-editor-comparison",
            startedAt: new Date().toISOString(),
            settings: opts,
            lanes: comparisonLanes(opts),
            evidence,
            workload: "empty-library visible retained editor, no projection/media/network servers; no automation requests during fixed steady window",
            cacheScope: evidence.environment.cacheScope,
            measurements: null,
            runs: [],
            scope: [
                opts.mode === "tauri-ablation" ? "Before/after use the same native adapter; allowed runtime source changes and actual build/resource receipts are checked. This idle workload does not establish full application performance" : "Rust declared source is a partial feature migration; matching visible workload does not establish background feature parity",
                "software-oriented Xvfb Linux only",
                "App processes include Chromium/WebKit helpers through ancestry, private session/group, retained identities, marker and optional cgroup",
                "Automation driver, observer, Xvfb and session-bus services excluded; externally activated services are not app descendants",
                "Unseen escaped helpers/permission-denied metadata remain an attribution limitation; inspect raw membership evidence"
            ]
        }
    json(path.join(opts.output, "report.json"), report)
    if (opts.dryRun) {
        report.plan = comparisonPlan(opts)
        report.state = "preflight-only-no-GUI-launched"
        json(path.join(opts.output, "report.json"), report)
        console.log(`Preflight only: ${opts.output}`)
        return
    }
    let xvfb
    try {
        xvfb = await fdService(executable("Xvfb"), ["-displayfd", "3", "-screen", "0", "1600x1100x24", "-dpi", "96", "-nolisten", "tcp"], path.join(opts.output, "xvfb.log"), { ...process.env, LC_ALL: "C.UTF-8", LANG: "C.UTF-8" })
        report.display = { display: `:${xvfb.value}`, pid: xvfb.child.pid }
        for (const [index, entry] of comparisonPlan(opts).entries()) {
            console.log(`Run ${index + 1}: ${entry.label}${entry.warmup ? " warmup" : ""}`)
            const run = await runOne(entry, index, opts, report.display.display, evidence)
            report.runs.push(run)
            json(path.join(opts.output, "report.json"), report)
            if (!run.valid) throw new Error(`Invalid ${entry.runtime} run: ${JSON.stringify(run.error || run.cleanup || run.adapterCleanupError)}`)
            if (opts.mode === "tauri-ablation") await ablationProvenance(opts)
            else {
                assert.equal(await hash(opts.rustBin), evidence.rust.sha256, "Rust release changed during comparison")
                assert.equal(await hash(opts.electronBin), evidence.electron.sha256, "Electron release changed during comparison")
            }
        }
        const seeds = new Set(report.runs.map((run) => run.profileSeedSha256))
        assert.equal(seeds.size, 1, "Profiles differ semantically")
        const visibleSignature = visibleEditorSignature(report.runs[0].before)
        for (const run of report.runs) {
            assert.deepEqual(visibleEditorSignature(run.before), visibleSignature, "Visible editor workload differs across runs")
            assert.deepEqual(visibleEditorSignature(run.after), visibleSignature, "Visible editor workload changed during run")
        }
        report.measurements = summarize(report.runs, comparisonLanes(opts))
        report.state = "completed"
        report.finishedAt = new Date().toISOString()
    } catch (error) {
        report.state = "failed"
        report.error = shortError(error)
        throw error
    } finally {
        if (xvfb)
            try {
                await stopProcess(xvfb.child)
                report.display.closed = true
            } catch (error) {
                report.display.cleanupError = shortError(error)
                report.state = "failed"
            }
        json(path.join(opts.output, "report.json"), report)
    }
    console.log(`Raw per-process samples and report: ${opts.output}`)
    console.log(report.measurements.note)
}
if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
    try {
        const opts = options(process.argv.slice(2))
        if (opts.help)
            console.log(
                "Linux release idle-editor comparison; --dry-run performs preflight only. Default --mode electron-rust: --electron-bin PATH --rust-bin PATH --baseline-receipt PATH --electron-package PATH --rust-source-commit SHA --expect-rust-sha256 SHA. --mode tauri-ablation: --before-bin PATH --after-bin PATH --before-label ID --after-label ID --before-build-receipt PATH --after-build-receipt PATH --expect-before-sha256 SHA --expect-after-sha256 SHA --allowed-source-change EXACT_PATH (1..16). Common: --source-root PATH --pairs 5 --warmups 1 --settle 20 --seconds 10 --interval 250 --order alternating|random --seed N --font-dir PATH --cgroup-parent DELEGATED_PATH --output NEW_DIRECTORY. Never builds or launches apps for --dry-run. Startup remains diagnostic; no automatic speedup ratios."
            )
        else if (!opts.dryRun && !process.env.FREESHOW_COMPARISON_SUPERVISED) {
            const child = spawn(executable("python3"), [LAUNCHER, "--supervise", process.execPath, fileURLToPath(import.meta.url), ...process.argv.slice(2), "--output", opts.output], { stdio: "inherit", env: { ...process.env, FREESHOW_COMPARISON_SUPERVISOR_RECEIPT: path.join(opts.output, "supervisor.json") } })
            child.once("error", (error) => {
                console.error(error)
                process.exitCode = 1
            })
            child.once("exit", (code, signal) => {
                process.exitCode = code ?? (signal ? 128 : 1)
            })
        } else await main(opts)
    } catch (error) {
        console.error(error)
        process.exitCode = 1
    }
}
