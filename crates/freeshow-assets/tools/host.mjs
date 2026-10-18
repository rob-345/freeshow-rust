// Trusted test host helpers. Importing this module does not register tests.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import http from "node:http"
import { spawn } from "node:child_process"
import { once } from "node:events"
import readline from "node:readline"
import { fileURLToPath } from "node:url"
const root = fileURLToPath(new URL("../../../", import.meta.url))
const binary = process.env.FREESHOW_ASSETS_PARITY || path.join(root, "target/debug/freeshow-assets-parity" + (process.platform === "win32" ? ".exe" : ""))
assert.ok(fs.existsSync(binary), "Build freeshow-assets-parity or set FREESHOW_ASSETS_PARITY")
export const origin = "http://freeshow.test"
export const context = { runtimeId: "assets-parity", libraryGeneration: 1, dataPath: "/fixture/library" }
export class Host {
    constructor() {
        this.child = spawn(binary, [], { stdio: ["pipe", "pipe", "pipe"] })
        this.waiting = []
        this.stderr = ""
        this.child.stderr.on("data", (data) => {
            this.stderr = (this.stderr + data).slice(-8192)
        })
        this.lines = readline.createInterface({ input: this.child.stdout })
        this.lines.on("line", (line) => {
            const pending = this.waiting.shift()
            if (pending) pending.resolve(JSON.parse(line))
        })
        this.child.on("exit", (code) => {
            for (const pending of this.waiting.splice(0)) pending.reject(new Error(`Asset host exited ${code}: ${this.stderr}`))
        })
    }
    command(input) {
        return new Promise((resolve, reject) => {
            this.waiting.push({ resolve, reject })
            this.child.stdin.write(JSON.stringify(input) + "\n")
        })
    }
    async call(input) {
        const result = await this.command(input)
        assert.equal(result.ok, true, JSON.stringify(result))
        return result.value
    }
    async start(config = {}) {
        await this.call({ op: "start", context, config: { origins: [origin, "null"], ...config } })
        return this
    }
    async grant(file, extra = {}) {
        return this.call({ op: "grant", path: file, ...extra })
    }
    async close() {
        if (this.child.exitCode !== null) return
        try {
            await this.call({ op: "quit" })
        } finally {
            this.child.stdin.end()
            await once(this.child, "exit").catch(() => {})
        }
    }
}
export function request(url, { method = "GET", headers = {} } = {}) {
    return new Promise((resolve, reject) => {
        const req = http.request(url, { method, headers, agent: false }, (response) => {
            const chunks = []
            let bytes = 0
            response.on("data", (chunk) => {
                bytes += chunk.length
                if (bytes > 4 * 1024 * 1024) response.destroy(new Error("Test response exceeds its collection bound"))
                else chunks.push(chunk)
            })
            response.on("end", () => resolve({ status: response.statusCode, headers: response.headers, bytes: Buffer.concat(chunks) }))
            response.on("error", reject)
        })
        req.setTimeout(5000, () => req.destroy(new Error("Asset HTTP test deadline elapsed")))
        req.on("error", reject)
        req.end()
    })
}
export function fixtures(directory) {
    const image = path.join(directory, process.platform === "win32" ? "日本語 é #%+.PNG" : "日本語 é #%+?.PNG")
    fs.writeFileSync(image, Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYPj/HwADAgH/5ncLrgAAAABJRU5ErkJggg==", "base64"))
    const audio = path.join(directory, "音楽 space.wav")
    const wav = Buffer.alloc(44 + 16000)
    wav.write("RIFF")
    wav.writeUInt32LE(wav.length - 8, 4)
    wav.write("WAVEfmt ", 8)
    wav.writeUInt32LE(16, 16)
    wav.writeUInt16LE(1, 20)
    wav.writeUInt16LE(1, 22)
    wav.writeUInt32LE(8000, 24)
    wav.writeUInt32LE(16000, 28)
    wav.writeUInt16LE(2, 32)
    wav.writeUInt16LE(16, 34)
    wav.write("data", 36)
    wav.writeUInt32LE(16000, 40)
    fs.writeFileSync(audio, wav)
    const data = path.join(directory, "range.mp4")
    fs.writeFileSync(data, Buffer.from(Array.from({ length: 8193 }, (_, index) => index % 251)))
    const empty = path.join(directory, "empty.png")
    fs.writeFileSync(empty, "")
    const video = path.join(directory, "blue video.webm")
    fs.copyFileSync(new URL("./fixtures/blue.webm", import.meta.url), video)
    return { image, audio, data, empty, video }
}
