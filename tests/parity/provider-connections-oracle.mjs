import assert from "node:assert/strict"
import { execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { fileURLToPath } from "node:url"
import ts from "typescript"

export const root = fileURLToPath(new URL("../../", import.meta.url))
export const provenance = JSON.parse(fs.readFileSync(new URL("./fixtures/provider-connections-source.json", import.meta.url), "utf8"))
const hash = (text) => createHash("sha256").update(text).digest("hex")
const sources = new Map()
export function original(filename) {
    if (!sources.has(filename)) {
        assert.ok(Object.hasOwn(provenance.sources, filename), "Unreviewed oracle source")
        const text = execFileSync("git", ["show", `${provenance.oracle}:${filename}`], { cwd: root, encoding: "utf8", maxBuffer: 1024 * 1024, timeout: 10000 }).replace(/\r\n/g, "\n")
        assert.equal(hash(text), provenance.sources[filename], filename)
        sources.set(filename, text)
    }
    return sources.get(filename)
}
const compile = (text) => ts.transpileModule(text, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, esModuleInterop: true } }).outputText

export function providerOracle(initial = {}, read) {
    let snapshot = initial
    let storeAvailable = true
    let readError = null
    const calls = { apps: 0, routes: 0, keys: 0, reads: 0, writes: 0, listens: 0, backend: 0, opens: 0, events: 0, crypto: 0, timers: 0, logs: 0 }
    const forbidden = (kind) => () => {
        calls[kind]++
        throw new Error(`Blocked oracle ${kind}`)
    }
    const express = () => {
        calls.apps++
        return { get: () => calls.routes++, use: forbidden("backend"), listen: forbidden("listens") }
    }
    express.json = forbidden("backend")
    const boundary = new Proxy(forbidden("backend"), {
        get: (_, key) => (key === "__esModule" ? false : boundary)
    })
    const store = {
        get(key) {
            calls.reads++
            if (readError) throw readError
            return read ? read(key) : snapshot[key]
        },
        set(key, value) {
            calls.writes++
            snapshot[key] = value
        }
    }
    const stores = {
        get ACCESS() {
            return storeAvailable ? store : undefined
        }
    }
    const dataStore = {
        _store: stores,
        config: { get: forbidden("backend") },
        getStoreValue: ({ key }) => store.get(key),
        setStoreValue: ({ key, value }) => store.set(key, value)
    }
    const context = vm.createContext({
        console: Object.fromEntries(["log", "info", "warn", "error"].map((key) => [key, () => calls.logs++])),
        process: { env: {} },
        Buffer,
        URL,
        URLSearchParams,
        setTimeout: forbidden("timers"),
        clearTimeout: forbidden("timers")
    })
    const cache = new Map()
    const modules = new Set(Object.keys(provenance.sources).filter((file) => !["src/electron/IPC/main.ts", "src/electron/IPC/responsesMain.ts", "src/electron/data/store.ts"].includes(file)))
    function load(filename) {
        if (cache.has(filename)) return cache.get(filename).exports
        const module = { exports: {} }
        cache.set(filename, module)
        function require(id) {
            if (id === "express") return express
            if (id === "crypto") return { createHash: forbidden("crypto"), randomFillSync: forbidden("crypto") }
            if (id === "http") return { request: forbidden("backend") }
            const resolved = path.posix.normalize(path.posix.join(path.posix.dirname(filename), id)) + ".ts"
            if (resolved === "src/electron/data/store.ts") return dataStore
            if (resolved === "src/electron/utils/keys.ts") return { getKey: () => (calls.keys++, "fixture-public-config") }
            if (resolved === "src/electron/IPC/main.ts") return { sendToMain: forbidden("events"), sendMain: forbidden("events") }
            if (resolved === "src/electron/IPC/responsesMain.ts") return { openURL: forbidden("opens") }
            if (resolved === "src/electron/utils/requests.ts") return { httpsRequest: forbidden("backend") }
            if (modules.has(resolved)) return load(resolved)
            // Uncalled service/import/content-library helpers are strict traps.
            return boundary
        }
        new vm.Script(`(function(require,module,exports){${compile(original(filename))}\n})`, { filename }).runInContext(context, { timeout: 1000 })(require, module, module.exports)
        return module.exports
    }
    const registry = load("src/electron/contentProviders/ContentProviderRegistry.ts").ContentProviderRegistry
    const helper = load("src/electron/data/contentProviders.ts")
    const main = load("src/types/IPC/Main.ts")
    const responsesSource = ts.createSourceFile("responsesMain.ts", original("src/electron/IPC/responsesMain.ts"), ts.ScriptTarget.Latest, true)
    let property
    function visit(node) {
        if (ts.isPropertyAssignment(node) && node.name.getText(responsesSource) === "[Main.PROVIDER_CONNECTIONS]") property = node
        ts.forEachChild(node, visit)
    }
    visit(responsesSource)
    assert.ok(property)
    assert.equal(hash(property.getText(responsesSource)), provenance.handlerSha256)
    context.ContentProviderRegistry = registry
    context.Main = main.Main
    context.MAIN = main.MAIN
    context.ToMain = load("src/types/IPC/ToMain.ts").ToMain
    new vm.Script(compile(`globalThis.handler = (${property.initializer.getText(responsesSource)})`)).runInContext(context, { timeout: 1000 })
    context.mainResponses = { [main.Main.PROVIDER_CONNECTIONS]: context.handler }
    const ipcSource = ts.createSourceFile("main.ts", original("src/electron/IPC/main.ts"), ts.ScriptTarget.Latest, true)
    const receiveMain = ipcSource.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "receiveMain")
    assert.ok(receiveMain)
    new vm.Script(compile(receiveMain.getText(ipcSource).replace(/^export\s+/, "") + "\nglobalThis.receiveMain = receiveMain")).runInContext(context, { timeout: 1000 })
    return {
        calls,
        registry,
        helper,
        set(value) {
            snapshot = value
        },
        available(value) {
            storeAvailable = value
        },
        failRead(error) {
            readError = error
        },
        invoke() {
            return vm.runInContext("handler()", context, { timeout: 1000 })
        },
        async dispatch(data, listenerId = "provider-fixture", observeReply) {
            const replies = []
            await context.receiveMain(
                {
                    reply: (...args) => {
                        replies.push(args)
                        observeReply?.(...args)
                    }
                },
                { channel: main.Main.PROVIDER_CONNECTIONS, data },
                listenerId
            )
            return replies
        }
    }
}

export function assertNoProviderActivity(calls) {
    for (const kind of ["writes", "listens", "backend", "opens", "events", "crypto", "timers"]) assert.equal(calls[kind], 0, kind)
}
