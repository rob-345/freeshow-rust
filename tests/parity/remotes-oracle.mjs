// Actual pinned servers.ts with only Electron/OS/service boundaries injected.
// HTTP/Express/Socket.IO transport and clients are the real installed libraries.
import fs from "node:fs"
import { createHash } from "node:crypto"
import http from "node:http"
import { EventEmitter } from "node:events"
import path from "node:path"
import vm from "node:vm"
import { fileURLToPath } from "node:url"
import ts from "typescript"
import express from "express"
import { Server } from "socket.io"

export const root = fileURLToPath(new URL("../../", import.meta.url))
export function originalServers(directory, { mockedListen = false } = {}) {
    const events = [],
        listens = [],
        ipcMain = new EventEmitter()
    let now = 1000
    const source = fs.readFileSync(path.join(root, "src/electron/servers.ts"), "utf8")
    // Works in shallow CI checkouts while pinning the unchanged parity source.
    if (createHash("sha256").update(source.replace(/\r\n/g, "\n")).digest("hex") !== "2b8c81b56e77b754dc617e31777e4b3e1d9d15b368c369efc9d58030c1af4b41") throw new Error("servers.ts differs from pinned Electron parity oracle 2d94defc")
    const output = ts.transpileModule(source + "\nexport const __test = { servers, serverPorts };", { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText
    class MockServer {
        constructor() {
            this.handlers = {}
        }
        on(name, callback) {
            this.handlers[name] = callback
        }
        emit() {}
        to() {
            return this
        }
        close(callback) {
            callback?.()
        }
    }
    const httpBoundary = {
        createServer(app) {
            if (mockedListen)
                return {
                    listen(port, callback) {
                        listens.push(port)
                        callback()
                    },
                    once() {},
                    close() {}
                }
            const server = http.createServer(app)
            const listen = server.listen
            server.listen = function (port, callback) {
                return listen.call(this, port, "127.0.0.1", callback)
            }
            return server
        }
    }
    const context = vm.createContext({
        exports: {},
        __dirname: directory,
        console: { info() {}, error() {} },
        Date: class extends Date {
            static now() {
                return now
            }
        },
        require(name) {
            if (name === "electron") return { ipcMain }
            if (name === "express") return express
            if (name === "http") return httpBoundary
            if (name === "path") return { join: path.join }
            if (name === "socket.io") return { Server: mockedListen ? MockServer : Server }
            if (name.includes("nowPlaying")) return { getNowPlayingMediaPath: () => "" }
            if (name.includes("CaptureHelper")) return { CaptureHelper: {} }
            if (name.includes("bonjour")) return { publishPort() {}, unpublishPorts() {} }
            if (name === "./index") return { toApp: (server, message) => events.push({ server, message }) }
            if (name.includes("OutputHelper")) return { OutputHelper: { getOutput: () => null } }
            if (name.includes("utils/files")) return { doesPathExist: () => false }
            throw new Error(`Unexpected oracle dependency ${name}`)
        }
    })
    new vm.Script(output, { filename: "actual-servers.ts" }).runInContext(context, { timeout: 1000 })
    const functions = context.exports
    return {
        events,
        listens,
        start(payload) {
            functions.startServers(payload)
        },
        update: (data) => functions.updateServerData(data),
        data: (kind) => functions.getServerData(kind),
        os: (ua) => functions.getOS(ua),
        renderer: (kind, message) => ipcMain.emit(kind, {}, message),
        backend: (kind, message) => functions.toServer(kind, message),
        stage: (message) => functions.toStageStreamSubscribers(message),
        subscribers: () => Array.from(functions.getStageStreamSubscriberIds()),
        clock: (value) => {
            now = value
        },
        async endpoints() {
            const answer = {}
            for (const [kind, server] of Object.entries(functions.__test.servers)) {
                if (kind === "OUTPUT_STREAM") continue
                for (let i = 0; i < 100 && !server.server.address?.(); i++) await new Promise((resolve) => setTimeout(resolve, 2))
                const address = server.server.address?.()
                if (address) answer[kind] = address.port
            }
            return answer
        },
        async close() {
            await Promise.all(Object.values(functions.__test.servers).map((server) => new Promise((resolve) => server.io.close(() => resolve()))))
        }
    }
}
