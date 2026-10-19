import assert from "node:assert/strict"
import { execFileSync, spawn, exec } from "node:child_process"
import { createHash } from "node:crypto"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import vm from "node:vm"
import { fileURLToPath } from "node:url"
import ts from "typescript"

export const root = fileURLToPath(new URL("../../", import.meta.url))
export const binary = process.env.FREESHOW_SPOTIFY_BIN || path.join(root, "target/debug/freeshow-spotify-parity" + (process.platform === "win32" ? ".exe" : ""))
const pin = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const source = execFileSync("git", ["show", `${pin}:src/electron/utils/spotify.ts`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 }).replace(/\r\n/g, "\n")
assert.equal(createHash("sha256").update(source).digest("hex"), "75d1ceab68e46c2a21dfc5a61f4e667f1d7d130e7b34b377938b7a97082918fd")
const compiled = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, esModuleInterop: true } }).outputText
export function sourceOracle(execute) {
    const module = { exports: {} }
    const calls = { errors: 0, commands: [] }
    const fn = new vm.Script(`(function(require,module,exports){${compiled}\n})`).runInNewContext({ process: { env: {} }, __filename: "spotify-oracle", console: { error: () => calls.errors++ }, setTimeout }, { timeout: 1000 })
    fn(
        (name) => {
            if (name === "os") return { platform: () => "linux" }
            if (name === "child_process") return { exec: (command, callback) => (calls.commands.push(command), execute(command, callback)) }
            throw Error("Unexpected source module")
        },
        module,
        module.exports
    )
    return { get: module.exports.getSpotifyState, calls }
}
export function restore(wire) {
    const value = wire.data
    for (const field of wire.undefinedFields) value[field] = undefined
    for (const { field, kind } of wire.specialNumbers) value[field] = { NaN: NaN, PositiveInfinity: Infinity, NegativeInfinity: -Infinity, NegativeZero: -0 }[kind]
    return value
}
export async function cleanup(...actions) {
    const results = await Promise.allSettled(actions)
    const failures = results.filter((result) => result.status === "rejected").map((result) => result.reason)
    if (failures.length) throw new AggregateError(failures, "Actual owned fixture cleanup failed")
}
function timeout(promise, message, ms = 8000) {
    let timer
    return Promise.race([promise, new Promise((_, reject) => (timer = setTimeout(() => reject(Error(message)), ms)))]).finally(() => clearTimeout(timer))
}
export class Lines {
    constructor(child, limit = 512) {
        this.child = child
        this.lines = []
        this.waiters = []
        this.buffer = ""
        this.stderr = ""
        child.stdout.setEncoding("utf8")
        child.stderr.setEncoding("utf8")
        child.stderr.on("data", (chunk) => {
            this.stderr += chunk
            if (this.stderr.length > 16384) child.kill("SIGKILL")
        })
        child.stdout.on("data", (chunk) => {
            this.buffer += chunk
            if (this.buffer.length > 2 * 1024 * 1024) return child.kill("SIGKILL")
            while (this.buffer.includes("\n")) {
                const end = this.buffer.indexOf("\n"),
                    line = this.buffer.slice(0, end)
                this.buffer = this.buffer.slice(end + 1)
                this.lines.push(line)
                if (this.lines.length > limit) return child.kill("SIGKILL")
                for (const waiter of [...this.waiters]) if (waiter.match(line, this.lines.length - 1)) waiter.resolve(line)
            }
        })
        this.exit = new Promise((resolve, reject) => {
            child.once("error", reject)
            child.once("close", (code, signal) => resolve({ code, signal }))
        })
        // Retain the actual exit promise even if an observer times out.
        this.exit.catch(() => {})
    }
    wait(match) {
        const existing = this.lines.find((line, index) => match(line, index))
        if (existing !== undefined) return Promise.resolve(existing)
        assert.ok(this.waiters.length < 16)
        let waiter
        const promise = new Promise((resolve) => {
            waiter = { match, resolve }
            this.waiters.push(waiter)
        })
        return timeout(
            Promise.race([
                promise,
                this.exit.then(() => {
                    throw Error("Controlled child exited before its event")
                })
            ]),
            "Controlled child event not observed"
        ).finally(() => (this.waiters = this.waiters.filter((item) => item !== waiter)))
    }
    async stop() {
        if (this.child.exitCode === null && this.child.signalCode === null) this.child.kill("SIGTERM")
        try {
            return await timeout(this.exit, "Controlled child did not exit", 5000)
        } catch (error) {
            this.child.kill("SIGKILL")
            await timeout(this.exit, "Controlled child could not be reaped", 5000)
            throw error
        }
    }
}
export class Driver extends Lines {
    constructor(env = process.env) {
        super(spawn(binary, [], { env, stdio: ["pipe", "pipe", "pipe"] }))
    }
    async request(command) {
        const after = this.lines.length
        this.child.stdin.write(JSON.stringify(command) + "\n")
        return JSON.parse(await this.wait((_, index) => index >= after))
    }
    async value(command) {
        const result = await this.request(command)
        assert.equal(result.ok, true, result.error?.code)
        return result.value
    }
    async finish() {
        this.child.stdin.end()
        try {
            const result = await timeout(this.exit, "Native driver did not drain")
            assert.equal(result.code, 0, "Native driver exit")
        } catch (error) {
            // Failure cleanup is not a successful native drain observation.
            await this.stop()
            throw error
        }
    }
}
export async function privateBus(profile) {
    assert.equal(process.platform, "linux")
    const folder = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-spotify-bus-"))
    fs.chmodSync(folder, 0o700)
    assert.ok(!/[<>&'"%;]/.test(folder), "Private fixture requires a plain local temporary path")
    // No service dirs/includes: even a missing-name query cannot auto-activate a
    // real desktop Spotify or any ambient installed session service.
    const config = path.join(folder, "session.conf")
    fs.writeFileSync(config, `<busconfig><type>session</type><listen>unix:path=${folder}/bus.sock</listen><auth>EXTERNAL</auth><policy context="default"><allow user="${process.getuid()}"/><allow own="*"/><allow send_destination="*"/><allow receive_sender="*"/></policy><limit name="max_completed_connections">32</limit><limit name="max_message_size">2097152</limit></busconfig>`, { mode: 0o600 })
    const daemon = new Lines(spawn("dbus-daemon", [`--config-file=${config}`, "--nofork", "--print-address=1", "--print-pid=1"], { stdio: ["ignore", "pipe", "pipe"] }))
    let provider
    const originals = new Set()
    async function close() {
        const failures = []
        for (const child of [provider, daemon]) {
            if (child) {
                try {
                    await child.stop()
                } catch (error) {
                    failures.push(error)
                }
            }
        }
        // Closing the owned bus first makes accepted source D-Bus calls settle;
        // keep and actually await each original shell rather than dropping it.
        for (const child of originals) {
            try {
                await timeout(new Promise((resolve) => child.once("close", resolve)), "Source shell did not exit")
            } catch (error) {
                child.kill("SIGTERM")
                await timeout(new Promise((resolve) => child.once("close", resolve)), "Source shell could not be reaped")
                failures.push(error)
            }
        }
        fs.rmSync(folder, { recursive: true, force: true })
        if (failures.length) throw new AggregateError(failures, "Private fixture cleanup failed")
        assert.equal(originals.size, 0)
    }
    try {
        const address = await daemon.wait((line) => line.startsWith("unix:"))
        const pid = await daemon.wait((line) => /^\d+$/.test(line))
        assert.equal(Number(pid), daemon.child.pid, "actual nonforking daemon ownership")
        const env = { ...process.env, DBUS_SESSION_BUS_ADDRESS: address, PATH: "/usr/bin:/bin" }
        if (profile) {
            provider = new Lines(spawn("/usr/bin/python3", [path.join(root, "tests/parity/fixtures/spotify/mpris.py"), profile], { env, stdio: ["pipe", "pipe", "pipe"] }))
            await provider.wait((line) => line === "READY")
        }
        const original = sourceOracle((command, callback) => {
            const child = exec(command, { env, maxBuffer: 1024 * 1024 }, callback)
            originals.add(child)
            child.once("close", () => originals.delete(child))
        })
        return {
            env,
            provider,
            original,
            close
        }
    } catch (error) {
        await close()
        throw error
    }
}
