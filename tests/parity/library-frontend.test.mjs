import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { test } from "node:test"
import ts from "typescript"
import { get, writable } from "svelte/store"
import { root } from "./oracle.mjs"

const source = fs.readFileSync(path.join(root, "src/frontend/IPC/responsesMain.ts"), "utf8")
const ast = ts.createSourceFile("responsesMain.ts", source, ts.ScriptTarget.Latest, true)
const declaration = ast.statements
    .filter(ts.isVariableStatement)
    .flatMap((node) => [...node.declarationList.declarations])
    .find((node) => node.name.getText(ast) === "mainResponses")
const property = declaration.initializer.properties.find((node) => node.name.getText(ast) === "[Main.DATA_PATH]")
const javascript = ts.transpileModule(`globalThis.callback = ${property.initializer.getText(ast)}`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText

function renderer(previous, rust = true) {
    const names = ["showsCache", "cachedShowsData", "outputSlideCache", "allOutputs", "scripturesCache", "shows", "activeShow", "activeEdit", "editHistory", "activeProject"]
    const stores = Object.fromEntries(names.map((name) => [name, writable({ sameId: { content: "Old library" } })]))
    stores.dataPath = writable(previous)
    const context = vm.createContext({ ...stores, get, window: rust ? { __TAURI__: {} } : {} })
    new vm.Script(javascript).runInContext(context)
    return { stores, callback: context.callback }
}

test("real DATA_PATH callback invalidates old-library derived data and output references", () => {
    const { stores, callback } = renderer("/A")
    callback("/B")
    assert.equal(get(stores.dataPath), "/B")
    for (const name of ["showsCache", "cachedShowsData", "outputSlideCache", "allOutputs", "scripturesCache", "shows"]) assert.equal(Object.keys(get(stores[name])).length, 0, name)
    assert.equal(get(stores.activeShow), null)
    assert.equal(get(stores.activeProject), null)
    assert.equal(get(stores.activeEdit).items.length, 0)
    assert.equal(get(stores.editHistory).length, 0)
})

test("initial/no-op path replies and the Electron callback preserve current renderer data", () => {
    for (const [previous, next, rust] of [
        ["/A", "/A", true],
        ["", "/A", true],
        ["/A", "/B", false]
    ]) {
        const { stores, callback } = renderer(previous, rust)
        const before = get(stores.outputSlideCache)
        callback(next)
        assert.equal(get(stores.dataPath), next)
        assert.equal(get(stores.outputSlideCache), before)
        assert.equal(get(stores.showsCache).sameId.content, "Old library")
    }
})
