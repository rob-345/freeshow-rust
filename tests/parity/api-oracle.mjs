import assert from "node:assert/strict"
import { createHash } from "node:crypto"
import fs from "node:fs"
import http from "node:http"
import { createRequire } from "node:module"
import vm from "node:vm"
import ts from "typescript"
import { root, upstreamFunctions } from "./oracle.mjs"

// Fixed oracle source hashes also work in shallow CI checkouts. Normalize only
// checkout line endings; the original TypeScript remains the executable oracle.
for (const [file, sha256] of Object.entries({
    "src/electron/utils/api.ts": "1094d6930435fa42afdde46f2b8acf7b53f9d7b4cd9a06772b458243cff38dec",
    "src/electron/utils/oscPatch.ts": "1891832fa358930b34c472c3f3f38471474afc8220483f772543d8da3fe7649b",
    "src/electron/utils/helpers.ts": "97d9e4f4e856dde2a90f74fc1307283ea671926a6e2722ea5c00c8e56200fb1c"
})) {
    const source = fs.readFileSync(new URL("../../" + file, import.meta.url), "utf8").replace(/\r\n/g, "\n")
    assert.equal(createHash("sha256").update(source).digest("hex"), sha256, `Pinned upstream changed: ${file}`)
}

const require = createRequire(import.meta.url)
export const ActualOSC = require("osc-js")
upstreamFunctions("src/electron/utils/oscPatch.ts", ["pad", "packString", "packInt32", "packInt64", "packUInt64", "packFloat32", "packFloat64", "packBlob", "patchOSC"], {
    OSC: ActualOSC,
    TextEncoder,
    Uint8Array,
    ArrayBuffer,
    DataView,
    BigInt
}).patchOSC()

// Execute the complete original module, replacing only OS binding addresses,
// host IPC, logging and timer speed. Express, Socket.IO and OSC are real.
export function apiOracle({ replyMs = 120, dynamicMs = 150, wildcard = false } = {}) {
    const events = []
    const servers = []
    const dynamic = new Map()
    let exports
    const observers = new Set()
    const event = (value) => {
        events.push(value)
        for (const observer of observers) observer(value)
    }
    const errors = (error) => event({ kind: "diagnostic", message: error.message })
    const actualHttp = {
        createServer(app) {
            const server = http.createServer(app)
            const listen = server.listen
            server.listen = (port, callback) => (wildcard ? listen.call(server, port, callback) : listen.call(server, port, "127.0.0.1", callback))
            servers.push(server)
            return server
        }
    }
    function express() {
        const app = require("express")()
        app.set("env", "test")
        app.listen = (port, callback) => actualHttp.createServer(app).listen(port, callback)
        return app
    }
    express.json = require("express").json
    class SocketServer extends require("socket.io").Server {
        on(name, callback) {
            if (name !== "connection") return super.on(name, callback)
            return super.on(name, (socket) => {
                const on = socket.on.bind(socket)
                socket.on = (name, callback) =>
                    on(name, (...args) => {
                        const result = callback(...args)
                        result?.catch?.(errors)
                        return result
                    })
                callback(socket)
            })
        }
    }
    class OSC extends ActualOSC {
        static DatagramPlugin = class extends ActualOSC.DatagramPlugin {
            open(options) {
                return super.open({ ...options, host: "127.0.0.1" })
            }
        }
        on(name, callback) {
            return super.on(name, (...args) => {
                const result = callback(...args)
                result?.catch?.(errors)
                return result
            })
        }
    }
    const wait = upstreamFunctions("src/electron/utils/helpers.ts", ["waitUntilValueIsDefined"], {
        setTimeout: (callback, delay) => setTimeout(callback, delay === 10000 ? replyMs : delay),
        setInterval: (callback) => setInterval(callback, 2),
        clearTimeout,
        clearInterval
    }).waitUntilValueIsDefined
    const module = { exports: {} }
    const context = vm.createContext({
        module,
        exports: module.exports,
        setTimeout,
        clearTimeout,
        setInterval,
        clearInterval,
        console: { info() {}, error() {}, warn() {} },
        require(name) {
            if (name === "express") return express
            if (name === "http") return actualHttp
            if (name === "osc-js") return OSC
            if (name === "./oscPatch") return {}
            if (name === "socket.io") return { Server: SocketServer }
            if (name === "../../types/IPC/ToMain") return { ToMain: { API_TRIGGER2: "API_TRIGGER2", WEBSOCKET: "WEBSOCKET", GET_DYNAMIC_VALUES: "GET_DYNAMIC_VALUES" } }
            if (name === "./helpers") return { waitUntilValueIsDefined: wait }
            if (name === "../IPC/main")
                return {
                    sendToMain(channel, data) {
                        if (channel === "WEBSOCKET") event({ kind: "connected" })
                        else event({ kind: "trigger", data: structuredClone(data) })
                    },
                    requestToMain(channel, keys) {
                        assert.equal(channel, "GET_DYNAMIC_VALUES")
                        const listener_id = `GET_DYNAMIC_VALUES${require("uid").uid(5)}`
                        return new Promise((resolve) => {
                            const timer = setTimeout(() => {
                                dynamic.delete(listener_id)
                                resolve(null)
                            }, dynamicMs)
                            dynamic.set(listener_id, (value) => {
                                clearTimeout(timer)
                                dynamic.delete(listener_id)
                                resolve(value)
                            })
                            event({ kind: "dynamic_values", listener_id, keys: structuredClone(keys) })
                        })
                    }
                }
            return require(name)
        }
    })
    const source = fs.readFileSync(new URL("../../src/electron/utils/api.ts", import.meta.url), "utf8")
    const javascript = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, esModuleInterop: true } }).outputText
    new vm.Script(javascript, { filename: "actual-electron-api.ts" }).runInContext(context)
    exports = module.exports
    return {
        events,
        observe(callback) {
            observers.add(callback)
            return () => observers.delete(callback)
        },
        async start(data) {
            exports.startWebSocketAndRest(data)
            await new Promise((resolve) => setTimeout(resolve, 20))
            return { websocket: { port: data?.port || 5505 }, rest: { port: data?.port ? data.port + 1 : 5506 }, osc: { port: data?.port || 5505 }, tcpAddresses: servers.filter((server) => server.listening).map((server) => server.address()) }
        },
        complete(data) {
            exports.apiReturnData(structuredClone(data))
        },
        dynamic(listenerId, value) {
            dynamic.get(listenerId)?.(value)
        },
        async stop() {
            exports.stopApiListener()
            await Promise.all(
                servers.map((server) =>
                    server.listening
                        ? new Promise((resolve) => {
                              server.closeAllConnections?.()
                              server.close(resolve)
                          })
                        : null
                )
            )
            await new Promise((resolve) => setTimeout(resolve, 10))
        }
    }
}

export const sourceAuth = upstreamFunctions("src/electron/utils/api.ts", ["getAuthToken"], {}, 'let apiPassword = "secret"').getAuthToken
export const queryOracle = (raw) => require("qs").parse(raw, { allowPrototypes: true })
export { root }

export const sourceEncode = (text, charset) => createRequire(require.resolve("body-parser"))("iconv-lite").encode(text, charset)
