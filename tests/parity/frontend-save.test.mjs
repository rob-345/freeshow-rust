import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { test } from "node:test"
import ts from "typescript"
import { get, writable } from "svelte/store"
import { root, upstreamFunctions } from "./oracle.mjs"

const source = fs.readFileSync(path.join(root, "src/frontend/utils/save.ts"), "utf8")
const ast = ts.createSourceFile("save.ts", source, ts.ScriptTarget.Latest, true)
const storeImport = ast.statements.find((node) => ts.isImportDeclaration(node) && node.moduleSpecifier.text === "../stores")
const policy = upstreamFunctions("src/frontend/utils/savePolicy.ts", ["isBackupOnly"])

// Execute the actual shared frontend save functions. Only external side effects
// and Svelte stores are injected, so queue clearing and saved-state updates are
// checked independently of the Rust implementation.
function frontend(context) {
    const stores = Object.fromEntries(storeImport.importClause.namedBindings.elements.map((node) => [node.name.text, writable({})]))
    stores.saved.set(false)
    stores.statusIndicator.set("")
    stores.deletedShows.set([{ id: "deleted", name: "Old show" }])
    stores.renamedShows.set([{ id: "renamed", oldName: "Before", name: "After" }])
    const sent = [],
        timers = []
    const functions = upstreamFunctions("src/frontend/utils/save.ts", ["save", "getSyncedSettings", "saveComplete"], {
        ...stores,
        ...policy,
        get,
        Main: { SAVE: "SAVE" },
        updateSyncedOutputs() {},
        startAutosave() {},
        customActionActivation() {},
        setStatus: (value) => stores.statusIndicator.set(value),
        clone: structuredClone,
        newToast() {},
        setTimeout: (callback) => timers.push(callback),
        sendMain: (...args) => sent.push(args),
        syncWithCloud: async () => {},
        isOutCleared: () => true,
        isSyncing: false,
        window: { api: { getRuntimeContext: () => (context ? { ...context } : undefined) } }
    })
    return { ...functions, stores, sent, flush: () => timers.splice(0).forEach((callback) => callback()) }
}

test("automatic backup preserves unsaved state and queued delete/rename operations", async () => {
    const f = frontend()
    assert.equal(f.save(false, { backup: true, isAutoBackup: true }), true)
    f.flush()
    assert.equal(get(f.stores.deletedShows).length, 1)
    assert.equal(get(f.stores.renamedShows).length, 1)
    assert.equal(f.sent.length, 1)
    await f.saveComplete({ closeWhenFinished: false, customTriggers: { backup: true, isAutoBackup: true } })
    assert.equal(get(f.stores.saved), false)
})

test("ordinary save with a backup persists pending operations and keeps correlation", async () => {
    const f = frontend()
    assert.equal(f.save(false, { backup: true, isAutoBackup: true, autosave: true }, "folder-save"), true)
    f.flush()
    assert.equal(get(f.stores.deletedShows).length, 0)
    assert.equal(get(f.stores.renamedShows).length, 0)
    assert.equal(f.sent[0][1].deletedShows.length, 1)
    assert.equal(f.sent[0][1].renamedShows.length, 1)
    assert.equal(f.sent[0][2], "folder-save")
    await f.saveComplete({ closeWhenFinished: false, customTriggers: { backup: true, isAutoBackup: true, autosave: true } })
    assert.equal(get(f.stores.saved), true)
})

test("busy frontend save leaves pending operations intact and sends no request", () => {
    const f = frontend()
    f.stores.statusIndicator.set("saving")
    assert.equal(f.save(), false)
    f.flush()
    assert.equal(f.sent.length, 0)
    assert.equal(get(f.stores.deletedShows).length, 1)
})

test("save snapshots its library context before the deferred IPC send", () => {
    const context = { runtimeId: "session", libraryGeneration: 0, dataPath: "/old" }
    const f = frontend(context)
    f.save()
    context.libraryGeneration = 1
    context.dataPath = "/new"
    f.flush()
    assert.equal(f.sent[0][1].runtimeContext.libraryGeneration, 0)
    assert.equal(f.sent[0][1].runtimeContext.dataPath, "/old")
})
