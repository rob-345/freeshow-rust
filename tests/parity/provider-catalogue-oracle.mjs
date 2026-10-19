import assert from "node:assert/strict"
import { createHash } from "node:crypto"
import vm from "node:vm"
import ts from "typescript"
import { assertNoProviderActivity, original, providerOracle } from "./provider-connections-oracle.mjs"

const compile = (text) => ts.transpileModule(text, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
function enums(filename) {
    const exports = {}
    new Function("exports", "require", compile(original(filename)))(exports, () => {
        throw new Error("Unexpected immutable enum dependency")
    })
    return exports
}

// Reuse the existing strict immutable module graph, real registration/factory
// and actual provider constructors. Only the separate MAIN initializer changes.
export function catalogueOracle(initial = {}) {
    const provider = providerOracle(initial),
        main = enums("src/types/IPC/Main.ts"),
        toMain = enums("src/types/IPC/ToMain.ts"),
        source = ts.createSourceFile("responsesMain.ts", original("src/electron/IPC/responsesMain.ts"), ts.ScriptTarget.Latest, true),
        matches = []
    function visit(node) {
        if (ts.isPropertyAssignment(node) && node.name.getText(source) === "[Main.GET_CONTENT_PROVIDERS]") matches.push(node)
        ts.forEachChild(node, visit)
    }
    visit(source)
    assert.equal(matches.length, 1)
    const handler = matches[0]
    assert.equal(createHash("sha256").update(handler.getText(source)).digest("hex"), "3e5bceffed2ea7418b66395a1c4d134b309dc1dbf0058ccf8fd15fff9d862eb9")
    const context = vm.createContext({
        ContentProviderRegistry: provider.registry,
        Main: main.Main,
        MAIN: main.MAIN,
        ToMain: toMain.ToMain,
        console: { error: () => provider.calls.logs++ }
    })
    new vm.Script(compile(`globalThis.handler = (${handler.initializer.getText(source)})`)).runInContext(context, { timeout: 1000 })
    context.mainResponses = { [main.Main.GET_CONTENT_PROVIDERS]: context.handler }
    const ipc = ts.createSourceFile("main.ts", original("src/electron/IPC/main.ts"), ts.ScriptTarget.Latest, true),
        receive = ipc.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "receiveMain")
    assert.ok(receive)
    new vm.Script(compile(receive.getText(ipc).replace(/^export\s+/, "") + "\nglobalThis.receiveMain = receiveMain")).runInContext(context, { timeout: 1000 })
    return {
        calls: provider.calls,
        registry: provider.registry,
        failRead: provider.failRead,
        available: provider.available,
        invoke(data) {
            context.payload = data
            try {
                return vm.runInContext("handler(payload)", context, { timeout: 1000 })
            } finally {
                delete context.payload
            }
        },
        async dispatch(data, listenerId) {
            const replies = []
            await context.receiveMain({ reply: (...args) => replies.push(args) }, { channel: main.Main.GET_CONTENT_PROVIDERS, data }, listenerId)
            return replies
        }
    }
}

export function assertNoCatalogueActivity(calls) {
    assertNoProviderActivity(calls)
    assert.equal(calls.reads, 0, "catalogue must not read ACCESS or connection status")
    assert.equal(calls.logs, 0)
}
