import assert from "node:assert/strict"
import fs from "node:fs"
import vm from "node:vm"
import { test } from "node:test"
import ts from "typescript"
import { clean, root, upstreamFunctions } from "./oracle.mjs"

function module(filename, boundaries = {}, globals = {}) {
    const exports = {}
    const code = ts.transpileModule(fs.readFileSync(root + filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText
    vm.runInNewContext(code, { exports, require: (name) => boundaries[name] || {}, console, setTimeout, URL, ...globals })
    return exports
}
const { createNativeDrop, dropCoordinates } = module("src/frontend/rust/nativeDrop.ts")
const extensions = module("src/frontend/values/extensions.ts")
const media = upstreamFunctions("src/frontend/components/helpers/media.ts", ["getExtension", "getFileName", "removeExtension", "getMediaType"], extensions)
const mover = upstreamFunctions("src/frontend/components/helpers/mover.ts", ["addToPos"])
const context = { runtimeId: "runtime-1", libraryGeneration: 3, dataPath: "/library/日本語" }
const tick = () => new Promise(setImmediate)

function fixture(publishAssets) {
    const listeners = new Map()
    const events = []
    const errors = []
    class MouseEvent {
        constructor(type, options) {
            Object.assign(this, options, { type, defaultPrevented: false })
        }
        preventDefault() {
            this.defaultPrevented = true
        }
        stopImmediatePropagation() {
            this.stopped = true
        }
    }
    const node = {
        attributes: new Map(),
        closest(selector) {
            return selector.includes(".droparea") ? this : null
        },
        setAttribute(key, value) {
            this.attributes.set(key, value)
        },
        removeAttribute(key) {
            this.attributes.delete(key)
        },
        dispatchEvent(event) {
            event.target = this
            for (const listener of listeners.get(event.type) || []) listener(event)
            if (!event.stopped) {
                events.push(event)
                this.handler?.(event)
            }
        }
    }
    const target = {
        MouseEvent,
        devicePixelRatio: 1,
        innerWidth: 1000,
        innerHeight: 800,
        document: { elementFromPoint: (x, y) => (x >= 100 && y >= 50 ? node : null) },
        addEventListener(type, listener) {
            listeners.set(type, [...(listeners.get(type) || []), listener])
        },
        removeEventListener(type, listener) {
            listeners.set(
                type,
                (listeners.get(type) || []).filter((entry) => entry !== listener)
            )
        },
        dispatchEvent(event) {
            for (const listener of listeners.get(event.type) || []) listener(event)
        }
    }
    let current = { ...context }
    const adapter = createNativeDrop(target, { getRuntimeContext: () => current, reportError: (error) => errors.push(error), publishAssets })
    const files = ["/媒体/Grace 日本語.png", "/music/A&B.mp3", "/slides/Deck.pptx", "/projects/教会.project"].map((path, index) => ({ path, name: path.split("/").at(-1), directory: false, size: index + 8, lastModified: 1000 }))
    const payload = (phase, overrides = {}) => ({ phase, sequence: 1, context, files, position: { x: 200, y: 180, units: "logical", scaleFactor: 1 }, ...overrides })
    return { adapter, target, node, events, errors, files, payload, setContext: (value) => (current = value) }
}

test("only an accepted current drop publishes native grants before retained handlers", () => {
    const order = [], grants = [{ sourcePath: "/media.png" }]
    const f = fixture((captured, value) => { assert.deepEqual(captured, context); assert.equal(value, grants); order.push("grants"); return true })
    f.node.handler = (event) => { if (event.type === "drop") order.push("retained") }
    f.adapter.handle(f.payload("enter", { nativeAssets: grants }))
    f.adapter.handle(f.payload("over", { nativeAssets: grants }))
    assert.deepEqual(order, [])
    f.adapter.handle(f.payload("drop", { nativeAssets: grants }))
    assert.deepEqual(order, ["grants", "retained"])
    f.adapter.handle(f.payload("enter", { sequence: 2 }))
    f.setContext({ ...context, libraryGeneration: 4 })
    f.adapter.handle(f.payload("drop", { sequence: 2, nativeAssets: grants }))
    assert.deepEqual(order, ["grants", "retained"])
    f.adapter.dispose()
    const rejected = fixture(() => false)
    rejected.adapter.handle(rejected.payload("enter"))
    rejected.adapter.handle(rejected.payload("drop", { nativeAssets: grants }))
    assert.equal(rejected.events.filter((event) => event.type === "drop").length, 0)
    assert.equal(rejected.errors[0].code, "assets_publication")
    rejected.adapter.dispose()
})

function sourceDropHandlers(showFilePath) {
    const histories = []
    const sends = []
    const store = (value) => ({
        value,
        set(value) {
            this.value = value
        }
    })
    const stores = {
        selected: store({ id: null, data: [] }),
        activePage: store("show"),
        activeProject: store("p"),
        projects: store({ p: { shows: [{ id: "existing", type: "show" }] } }),
        editingProjectTemplate: store(null),
        activeShow: store(null),
        activeEdit: store({})
    }
    const history = (value) => histories.push(clean(value))
    const get = (store) => store.value
    const dropActions = module(
        "src/frontend/components/helpers/dropActions.ts",
        {
            "svelte/store": { get },
            uid: { uid: () => "new-id" },
            "../../../types/IPC/Main": { Main: { IMPORT_FILES: "IMPORT_FILES" } },
            "../../IPC/main": { sendMain: (command, data) => sends.push(clean({ command, data })) },
            "../../stores": stores,
            "../../values/extensions": extensions,
            "./media": media,
            "./mover": mover,
            "./history": { history },
            "./drop": {
                projectDropFolders: () => {
                    throw new Error("Unexpected folder dispatch")
                }
            }
        },
        { window: { api: { showFilePath } } }
    ).dropActions
    const source = fs.readFileSync(root + "src/frontend/components/system/DropArea.svelte", "utf8").match(/<script lang="ts">([\s\S]*?)<\/script>/)[1]
    const ast = ts.createSourceFile("DropArea.ts", source, ts.ScriptTarget.Latest, true)
    const declarations = ast.statements
        .filter(ts.isFunctionDeclaration)
        .map((node) => node.getText(ast))
        .join("\n")
    const selection = stores.selected
    const deselect = () => selection.set({ id: null, data: [] })
    const { ondrop } = upstreamFunctions("src/frontend/components/helpers/drop.ts", ["ondrop"], { ...stores, get, dropActions, history, deselect, areaChildren: {} })
    const javascript = ts.transpileModule(declarations + "\nglobalThis.dropEvent=dropEvent", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
    const sandbox = { ...stores, selected: selection, ondrop, id: "project", window: { api: { showFilePath } }, console: { info() {}, log() {} }, validateDrop: () => false, mediaExtensions: extensions.mediaExtensions }
    vm.runInNewContext(javascript, sandbox)
    // The real dropActions module resolves paths through the original window API.
    // Its VM has a separate global, so pass that boundary in the module above.
    return { drop: sandbox.dropEvent, histories, sends, selection }
}

test("OS drops preserve order/Unicode and execute original DropArea project media/import dispatch", async () => {
    const native = fixture()
    // Execute the original module with its preload boundary visible to all VMs.
    const baselinePaths = new WeakMap()
    const baselineFiles = native.files.map((info) => {
        const file = { name: info.name, type: info.name.endsWith(".png") ? "image/png" : info.name.endsWith(".mp3") ? "audio/mpeg" : "application/octet-stream" }
        baselinePaths.set(file, info.path)
        return file
    })
    let preload
    module(
        "src/electron/preload.ts",
        {
            electron: {
                contextBridge: {
                    exposeInMainWorld: (name, api) => {
                        assert.equal(name, "api")
                        preload = api
                    }
                },
                ipcRenderer: { on() {} },
                webUtils: { getPathForFile: (file) => baselinePaths.get(file) || "" }
            }
        },
        { process: { env: { NODE_ENV: "production" } } }
    )
    const baseline = sourceDropHandlers(preload.showFilePath)
    const actual = sourceDropHandlers(native.adapter.showFilePath)
    native.node.handler = (event) => {
        if (event.type === "drop") actual.drop(event)
    }
    await baseline.drop({ dataTransfer: { items: baselineFiles.map((file) => ({ kind: "file", getAsFile: () => file })) }, target: native.node })
    native.adapter.handle(native.payload("enter"))
    native.adapter.handle(native.payload("drop"))
    await tick()
    assert.deepEqual(actual.histories, baseline.histories)
    assert.deepEqual(actual.sends, baseline.sends)
    assert.equal(actual.histories[0].newData.data[1].id, native.files[0].path)
    assert.deepEqual(
        actual.sends.map((send) => send.data),
        [
            { id: "powerpoint", paths: [native.files[2].path] },
            { id: "freeshow_project", paths: [native.files[3].path] }
        ]
    )
    const drop = native.events.find((event) => event.type === "drop")
    assert.deepEqual(
        Array.from(drop.dataTransfer.files, native.adapter.showFilePath),
        native.files.map((file) => file.path)
    )
    assert.equal(drop.clientX, 200)
    assert.equal(drop.clientY, 180)
})

test("coordinate conversion distinguishes Wry GTK/AppKit logical units, WebView2 physical units and webview zoom", () => {
    for (const scaleFactor of [1, 1.25, 1.5, 2, 3])
        for (const zoom of [0.75, 1, 1.25, 2]) {
            const dpr = scaleFactor * zoom
            assert.deepEqual(clean(dropCoordinates({ x: 200 * zoom, y: 180 * zoom, units: "logical", scaleFactor }, dpr)), { x: 200, y: 180 })
            assert.deepEqual(clean(dropCoordinates({ x: 200 * dpr, y: 180 * dpr, units: "physical", scaleFactor }, dpr)), { x: 200, y: 180 })
        }
    assert.equal(dropCoordinates({ x: 1, y: 2, units: "logical", scaleFactor: 0 }, 1), null)
})

test("leave, Escape, out-of-viewport drops and internal editor drags never dispatch a native file drop", () => {
    for (const cancel of ["leave", "escape", "outside", "internal"]) {
        const native = fixture()
        native.adapter.handle(native.payload("enter"))
        assert.equal(native.node.attributes.get("data-native-drop-active"), "true")
        if (cancel === "leave") native.adapter.handle(native.payload("leave"))
        if (cancel === "escape") native.target.dispatchEvent({ type: "keydown", key: "Escape" })
        if (cancel === "internal") native.target.dispatchEvent({ type: "dragstart" })
        native.adapter.handle(native.payload("drop", cancel === "outside" ? { position: { x: -1, y: 180, units: "logical", scaleFactor: 1 } } : {}))
        assert.equal(native.events.filter((event) => event.type === "drop").length, 0, cancel)
        assert.equal(native.node.attributes.has("data-native-drop-active"), false, cancel)
    }
})

test("an accepted native drop can arrive after deferred metadata without losing the adapter gesture", async () => {
    const native = fixture()
    native.adapter.handle(native.payload("enter"))
    await tick()
    native.adapter.handle(native.payload("over"))
    await tick()
    // Native DragSession.accept_drop removes active, so subsequent platform
    // Leave emits no adapter payload. Its accepted metadata completion follows.
    native.adapter.handle(native.payload("drop"))
    assert.equal(native.events.filter((event) => event.type === "drop").length, 1)
    assert.equal(native.node.attributes.has("data-native-drop-active"), false)
    native.adapter.handle(native.payload("leave"))
    assert.equal(native.events.filter((event) => event.type === "drop").length, 1)
})

test("stale-library delivery and retained File/path descriptors cannot resolve against a new library", async () => {
    const native = fixture()
    native.adapter.handle(native.payload("enter"))
    native.setContext({ ...context, libraryGeneration: 4 })
    native.adapter.handle(native.payload("drop"))
    assert.equal(native.events.filter((event) => event.type === "drop").length, 0)
    native.setContext(context)
    native.adapter.handle(native.payload("enter", { sequence: 2 }))
    native.adapter.handle(native.payload("drop", { sequence: 2 }))
    const file = native.events.find((event) => event.type === "drop").dataTransfer.files[0]
    assert.equal(file.path, native.files[0].path)
    assert.throws(() => file.arrayBuffer(), /not browser File byte reads/)
    native.setContext({ ...context, dataPath: "/other" })
    assert.throws(() => native.adapter.showFilePath(file), /no longer active/)
    assert.throws(() => file.path, /no longer active/)
    const browserFile = new File(["real bytes"], "web-image.png", { type: "image/png" })
    assert.equal(native.adapter.showFilePath(browserFile), "")
    assert.equal(await browserFile.text(), "real bytes")
    assert.equal(browserFile.type, "image/png")
})
