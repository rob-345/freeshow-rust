// The actual pinned module and real bonjour-service/dns-packet; only OS socket,
// process, interface, entropy and timer cleanup boundaries are controlled.
import assert from "node:assert/strict"
import { createHash } from "node:crypto"
import dgram from "node:dgram"
import { EventEmitter } from "node:events"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import vm from "node:vm"
import { fileURLToPath } from "node:url"
import ts from "typescript"
import { Bonjour, Service } from "bonjour-service"
import dns from "dns-packet"
export const root = fileURLToPath(new URL("../../", import.meta.url))
export { dns }
const source = fs.readFileSync(path.join(root, "src/electron/data/bonjour.ts"), "utf8")
export const sourceHash = createHash("sha256").update(source.replace(/\r\n/g, "\n")).digest("hex")
// Hash pins the unchanged original file even in shallow/Windows CI checkouts.
assert.equal(sourceHash, "6ff78829b6f8a383d7584fdbec69e134892bb3299fbd7189901e41bcd4c5e869")
export function originalDiscovery(snapshot, { bindPort = 0, peerPort = 0, mac = false, noIO = false, jitter = null } = {}) {
    const interfaces = Object.fromEntries(snapshot.interfaces.map((entry) => [entry.name, entry.addresses]))
    const timers = new Set(),
        logs = [],
        spawns = []
    let instance,
        closed = false
    const actualRandom = Math.random
    if (jitter !== null) Math.random = () => jitter / 250
    const actualOs = { networkInterfaces: os.networkInterfaces, hostname: os.hostname }
    if (!noIO && !mac) {
        os.networkInterfaces = () => interfaces
        os.hostname = () => snapshot.hostname
    }
    class BoundaryBonjour extends Bonjour {
        constructor(options, error) {
            const socket = dgram.createSocket("udp4")
            const realBind = socket.bind.bind(socket),
                realSend = socket.send.bind(socket)
            socket.bind = function (...args) {
                return realBind(
                    bindPort,
                    "127.0.0.1",
                    args.findLast((a) => typeof a === "function")
                )
            }
            socket.send = function (bytes, offset, length, _port, _address, callback) {
                return realSend(bytes, offset, length, peerPort, "127.0.0.1", callback)
            }
            super({ ...options, socket, multicast: false, ip: "127.0.0.1", port: peerPort }, error)
            instance = this
        }
    }
    class NoIOBonjour {
        constructor() {}
    }
    const output = ts.transpileModule(source + "\nexport const __test = {ips,instanceID};", { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText
    const context = vm.createContext({
        exports: {},
        Buffer,
        console: { warn: (...v) => logs.push(v), info: (...v) => logs.push(v), log: (...v) => logs.push(v) },
        setTimeout: (fn, ms) => {
            const timer = setTimeout(() => {
                timers.delete(timer)
                fn()
            }, ms)
            timers.add(timer)
            return timer
        },
        require(name) {
            if (name === "bonjour-service") return { __esModule: true, default: noIO ? NoIOBonjour : BoundaryBonjour }
            if (name === "child_process")
                return {
                    spawn(file, args, options) {
                        const child = new EventEmitter()
                        child.kill = (signal) => {
                            child.signal = signal
                        }
                        spawns.push({ file, args, options, child })
                        return child
                    }
                }
            if (name === "crypto") return { randomBytes: () => Buffer.from([0xab, 0xcd, 0xef]) }
            if (name === "os") return { networkInterfaces: () => interfaces, hostname: () => snapshot.hostname }
            if (name === "..") return { isMac: mac }
            throw new Error(`Unexpected Bonjour dependency ${name}`)
        }
    })
    new vm.Script(output, { filename: "actual-pinned-bonjour.ts" }).runInContext(context, { timeout: 1000 })
    const exports = context.exports
    return {
        ips: () => Array.from(exports.getLocalIPs()),
        args: (name, port) => {
            exports.publishPort(name, port)
            return spawns.at(-1)?.args
        },
        publish: (name = "REMOTE", port = 5510) => exports.publishPort(name, port),
        stop: () => exports.unpublishPorts(),
        logs,
        spawns,
        records(name = "REMOTE", port = 5510) {
            const before = { networkInterfaces: os.networkInterfaces, hostname: os.hostname }
            os.networkInterfaces = () => interfaces
            os.hostname = () => snapshot.hostname
            try {
                return new Service({ name: `freeshow-${name}-abcdef`, type: "freeshow", protocol: "udp", port, txt: { ip: exports.__test.ips[0], ips: exports.__test.ips } }).records()
            } finally {
                Object.assign(os, before)
            }
        },
        endpoint: () => instance?.server.mdns,
        async close() {
            if (closed) return
            closed = true
            for (const timer of timers) clearTimeout(timer)
            timers.clear()
            exports.unpublishPorts()
            if (instance) await new Promise((resolve) => instance.destroy(resolve))
            if (!noIO && !mac) Object.assign(os, actualOs)
            Math.random = actualRandom
        }
    }
}
