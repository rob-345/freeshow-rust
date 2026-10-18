import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { test } from "node:test"
import ts from "typescript"
import { get, writable } from "svelte/store"
import { fixtureShow, root, upstreamFunctions } from "./oracle.mjs"

const source = fs.readFileSync(path.join(root, "src/frontend/utils/save.ts"), "utf8")
const ast = ts.createSourceFile("save.ts", source, ts.ScriptTarget.Latest, true)
const storeImport = ast.statements.find((node) => ts.isImportDeclaration(node) && node.moduleSpecifier.text === "../stores")
const policy = upstreamFunctions("src/frontend/utils/savePolicy.ts", ["isBackupOnly"])

// Execute the actual shared frontend save functions. Only external side effects
// and Svelte stores are injected, so queue clearing and saved-state updates are
// checked independently of the Rust implementation.
function frontend(context, rust = false) {
    const stores = Object.fromEntries(storeImport.importClause.namedBindings.elements.map((node) => [node.name.text, writable({})]))
    stores.saved.set(false)
    stores.statusIndicator.set("")
    stores.errorHasOccurred.set(false)
    stores.deletedShows.set([{ id: "deleted", name: "Old show" }])
    stores.renamedShows.set([{ id: "renamed", oldName: "Before", name: "After" }])
    const sent = [],
        closed = [],
        timers = []
    const revision = {}
    const revisionSource = fs.readFileSync(path.join(root, "src/frontend/rust/saveRevision.ts"), "utf8")
    new vm.Script(ts.transpileModule(revisionSource, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText).runInNewContext({ exports: revision, crypto: globalThis.crypto })
    const declarations = ["customSavedListener", "saveList"].map((name) => ast.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((declaration) => declaration.name.getText(ast) === name)).getText(ast)).join("\n")
    const functions = upstreamFunctions(
        "src/frontend/utils/save.ts",
        ["save", "getSyncedSettings", "saveComplete", "unsavedUpdater", "initializeClosing"],
        {
            ...stores,
            ...policy,
            ...revision,
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
            socketDisconnect: async () => {},
            closeApp: () => closed.push(true),
            keysToID: () => [],
            removeDeleted: (items) => items,
            isOutCleared: () => true,
            isSyncing: false,
            window: { ...(rust ? { __TAURI__: {} } : {}), api: { getRuntimeContext: () => (context ? { ...context } : undefined) } }
        },
        declarations
    )
    if (rust) functions.unsavedUpdater()
    return { ...functions, stores, sent, closed, flush: () => timers.splice(0).forEach((callback) => callback()) }
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

const acknowledgment = (sent) => ({ closeWhenFinished: sent[1].closeWhenFinished, customTriggers: sent[1].customTriggers, runtimeSaveToken: sent[1].runtimeSaveToken })

test("Rust acknowledgment saves the captured queues without treating their internal removal as an edit", async () => {
    const f = frontend(undefined, true)
    f.save()
    f.flush()
    assert.equal(f.sent[0][1].deletedShows.length, 1)
    assert.equal(get(f.stores.deletedShows).length, 0)
    assert.equal(typeof f.sent[0][1].runtimeSaveToken, "string")
    await f.saveComplete(acknowledgment(f.sent[0]))
    assert.equal(get(f.stores.saved), true)
})

test("Rust acknowledgment leaves newer lyrics unsaved and a subsequent save captures them", async () => {
    const f = frontend(undefined, true)
    f.stores.showsCache.set({ song: fixtureShow(7) })
    f.save()
    f.flush()
    const newer = fixtureShow(7)
    newer.slides.z.items[0].lines[0].text[0].value = "Edited while saving"
    f.stores.showsCache.set({ song: newer })
    await f.saveComplete(acknowledgment(f.sent[0]))
    assert.equal(get(f.stores.saved), false)
    assert.equal(get(f.stores.statusIndicator), "")
    f.save()
    f.flush()
    assert.equal(f.sent[1][1].showsCache.song.slides.z.items[0].lines[0].text[0].value, "Edited while saving")
    assert.notEqual(f.sent[0][1].showsCache.song.slides.z.items[0].lines[0].text[0].value, "Edited while saving")
    await f.saveComplete(acknowledgment(f.sent[1]))
    assert.equal(get(f.stores.saved), true)
})

test("unchanged store notifications and the existing timestamp/layout exclusions do not invent newer edits", async () => {
    const f = frontend(undefined, true)
    const show = fixtureShow(7)
    f.stores.showsCache.set({ song: show })
    f.save()
    f.flush()
    f.stores.showsCache.set({ song: structuredClone(show) })
    f.stores.themes.set({})
    const updated = structuredClone(show)
    updated.timestamps.modified = 123456
    updated.settings = { activeLayout: "minor-layout-change" }
    f.stores.showsCache.set({ song: updated })
    await f.saveComplete(acknowledgment(f.sent[0]))
    assert.equal(get(f.stores.saved), true)
})

test("an older manual-backup save cannot clear the status of a later ordinary save", async () => {
    for (const editBetween of [false, true]) {
        const f = frontend(undefined, true)
        f.save(false, { backup: true })
        f.flush()
        if (editBetween) f.stores.themes.set({ newer: { name: "New theme" } })
        f.save()
        f.flush()
        assert.notEqual(f.sent[0][1].runtimeSaveToken, f.sent[1][1].runtimeSaveToken)
        await f.saveComplete(acknowledgment(f.sent[0]))
        assert.equal(get(f.stores.statusIndicator), "saving")
        assert.equal(get(f.stores.saved), false)
        await f.saveComplete(acknowledgment(f.sent[1]))
        assert.equal(get(f.stores.saved), true)
    }
})

test("a different renderer/module epoch cannot acknowledge the current snapshot", async () => {
    const first = frontend(undefined, true)
    const second = frontend(undefined, true)
    first.save()
    second.save()
    first.flush()
    second.flush()
    assert.notEqual(first.sent[0][1].runtimeSaveToken, second.sent[0][1].runtimeSaveToken)
    await second.saveComplete(acknowledgment(first.sent[0]))
    assert.equal(get(second.stores.statusIndicator), "saving")
    assert.equal(get(second.stores.saved), false)
    await second.saveComplete(acknowledgment(second.sent[0]))
    assert.equal(get(second.stores.saved), true)
})

test("closing after an outdated save first sends a new snapshot and waits for its acknowledgment", async () => {
    const f = frontend(undefined, true)
    f.save(true)
    f.flush()
    f.stores.themes.set({ newer: { name: "Edited while closing" } })
    await f.saveComplete(acknowledgment(f.sent[0]))
    assert.equal(f.closed.length, 0)
    assert.equal(get(f.stores.saved), false)
    f.flush()
    assert.equal(f.sent.length, 2)
    assert.equal(f.sent[1][1].THEMES.newer.name, "Edited while closing")
    assert.equal(f.sent[1][1].closeWhenFinished, true)
    await f.saveComplete(acknowledgment(f.sent[1]))
    assert.equal(f.closed.length, 1)
})

test("new rename/delete operations survive an older successful acknowledgment", async () => {
    const f = frontend(undefined, true)
    f.save()
    f.flush()
    f.stores.deletedShows.set([{ id: "later-delete", name: "Later" }])
    f.stores.renamedShows.set([{ id: "later-rename", oldName: "A", name: "B" }])
    f.flush()
    await f.saveComplete(acknowledgment(f.sent[0]))
    assert.equal(get(f.stores.saved), false)
    f.save()
    f.flush()
    assert.equal(f.sent[1][1].deletedShows[0].id, "later-delete")
    assert.equal(f.sent[1][1].renamedShows[0].id, "later-rename")
    await f.saveComplete(acknowledgment(f.sent[1]))
    assert.equal(get(f.stores.saved), true)
})

test("normal Rust Quit snapshots settings even when the legacy saved flag is true", async () => {
    const f = frontend(undefined, true)
    f.stores.maxConnections.set(19)
    f.stores.saved.set(true)
    f.initializeClosing()
    f.flush()
    assert.equal(f.sent.length, 1)
    assert.equal(f.sent[0][1].SETTINGS.maxConnections, 19)
    assert.equal(f.sent[0][1].closeWhenFinished, true)
    assert.equal(f.closed.length, 0)
    await f.saveComplete(acknowledgment(f.sent[0]))
    assert.equal(f.closed.length, 1)
})

test("Quit during an ordinary save waits for it, then captures newer edits in a closing save", async () => {
    const f = frontend(undefined, true)
    f.save()
    f.flush()
    f.stores.themes.set({ newer: { name: "While saving" } })
    f.initializeClosing()
    f.flush()
    assert.equal(f.sent.length, 1)
    assert.equal(f.closed.length, 0)
    await f.saveComplete(acknowledgment(f.sent[0]))
    f.flush()
    assert.equal(f.sent.length, 2)
    assert.equal(f.sent[1][1].THEMES.newer.name, "While saving")
    assert.equal(f.sent[1][1].closeWhenFinished, true)
    assert.equal(f.closed.length, 0)
    await f.saveComplete(acknowledgment(f.sent[1]))
    assert.equal(f.closed.length, 1)
})

test("a superseded acknowledgment cannot consume a deferred Quit", async () => {
    const f = frontend(undefined, true)
    f.save(false, { backup: true })
    f.flush()
    f.save()
    f.flush()
    f.initializeClosing()
    await f.saveComplete(acknowledgment(f.sent[0]))
    f.flush()
    assert.equal(f.sent.length, 2)
    assert.equal(get(f.stores.statusIndicator), "saving")
    await f.saveComplete(acknowledgment(f.sent[1]))
    f.flush()
    assert.equal(f.sent.length, 3)
    assert.equal(f.sent[2][1].closeWhenFinished, true)
    assert.equal(f.closed.length, 0)
})

test("the saved-state Quit popup captures a final Rust snapshot and retains Electron behavior", async () => {
    for (const rust of [false, true]) {
        const f = frontend(undefined, rust)
        f.stores.saved.set(true)
        await f.saveComplete({ closeWhenFinished: true })
        f.flush()
        assert.equal(f.sent.length, rust ? 1 : 0)
        assert.equal(f.closed.length, rust ? 0 : 1)
    }
})

test("only explicit Rust discard sends discard metadata after stopping interactions", async () => {
    for (const rust of [false, true]) {
        for (const discard of [false, true, { type: "click" }]) {
            const stopped = [],
                sent = []
            const { closeApp } = upstreamFunctions("src/frontend/utils/save.ts", ["closeApp"], {
                stopAllInteractions: async () => stopped.push("interactions"),
                stopRemoteController: async () => stopped.push("remote"),
                autoDisableRemoteController: () => stopped.push("disable"),
                stopMediaRecorder: async () => stopped.push("media"),
                stopAllChannelRecordings: async () => stopped.push("audio"),
                setTimeout() {},
                Main: { CLOSE: "CLOSE" },
                sendMain: (...args) => sent.push(args),
                window: rust ? { __TAURI__: {} } : {}
            })
            await closeApp(discard)
            assert.deepEqual(stopped, ["interactions", "remote", "disable", "media", "audio"])
            assert.deepEqual(structuredClone(sent), [["CLOSE", rust && discard === true ? { discardUnsaved: true } : undefined]])
        }
    }
})

test("edits during asynchronous close cleanup require another snapshot before native exit", async () => {
    for (const completion of ["current", "edited", "superseded"]) {
        const sent = [],
            retries = []
        const saved = writable(true),
            statusIndicator = writable("")
        let stopped = false
        const { closeApp } = upstreamFunctions("src/frontend/utils/save.ts", ["closeApp"], {
            stopAllInteractions: async () => {
                stopped = true
            },
            stopRemoteController: async () => {},
            autoDisableRemoteController() {},
            stopMediaRecorder: async () => {},
            stopAllChannelRecordings: async () => {},
            setTimeout() {},
            Main: { CLOSE: "CLOSE" },
            sendMain: (...args) => sent.push(args),
            window: { __TAURI__: {} },
            acknowledgeSave: (token) => {
                assert.equal(stopped, true)
                assert.equal(token, "snapshot")
                return completion
            },
            saved,
            statusIndicator,
            get,
            setStatus: (value) => statusIndicator.set(value),
            save: (...args) => retries.push(args)
        })
        await closeApp(false, "snapshot")
        assert.equal(sent.length, completion === "current" ? 1 : 0)
        assert.deepEqual(retries, completion === "current" ? [] : [[true]])
        assert.equal(get(saved), completion === "current")
    }
})
