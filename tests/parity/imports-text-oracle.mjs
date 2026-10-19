// Immutable source execution, with native/special-format work trapped explicitly.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import { createRequire } from "node:module"
import { fileURLToPath } from "node:url"
import ts from "typescript"

const require = createRequire(import.meta.url)
const yauzl = require("yauzl"),
    upath = require("upath")
export const root = fileURLToPath(new URL("../../", import.meta.url))
export const provenance = JSON.parse(fs.readFileSync(new URL("./imports-text-source.json", import.meta.url), "utf8"))
export const clean = (value) => JSON.parse(JSON.stringify(value))
const cache = new Map()
export function original(filename) {
    if (!cache.has(filename)) {
        assert.ok(Object.hasOwn(provenance.sources, filename), "Unreviewed import oracle source")
        const text = execFileSync("git", ["show", `${provenance.oracle}:${filename}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 }).replace(/\r\n/g, "\n")
        assert.equal(createHash("sha256").update(text).digest("hex"), provenance.sources[filename], filename)
        cache.set(filename, text)
    }
    return cache.get(filename)
}
function ast(filename) {
    return ts.createSourceFile(filename, original(filename), ts.ScriptTarget.Latest, true)
}
function load(filename, names, injected = {}) {
    const source = ast(filename)
    const declarations = source.statements.filter((node) => {
        if (ts.isFunctionDeclaration(node) || ts.isClassDeclaration(node)) return names.includes(node.name?.text)
        return ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => names.includes(item.name.getText(source)))
    })
    const context = vm.createContext({ console: { error() {}, warn() {}, info() {} }, ...injected })
    const code = declarations.map((node) => node.getText(source).replace(/^export\s+/, "")).join("\n") + `\nglobalThis.result={${names.join(",")}}`
    new vm.Script(ts.transpileModule(code, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, { filename }).runInContext(context, { timeout: 1000 })
    return context.result
}
function names(filename) {
    const source = ast(filename)
    return source.statements.flatMap((node) => (ts.isFunctionDeclaration(node) || ts.isClassDeclaration(node) ? [node.name.text] : ts.isVariableStatement(node) ? node.declarationList.declarations.map((item) => item.name.getText(source)) : []))
}
function enums(filename) {
    const exports = {}
    new Function("exports", ts.transpileModule(original(filename), { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText)(exports)
    return exports
}

export function transportOracle() {
    const events = [],
        replies = [],
        work = []
    const calls = { reads: [], special: [], unexpected: 0 }
    const trap = (kind) =>
        function (...args) {
            calls.special.push({ kind, path: typeof args[0] === "string" ? args[0] : undefined })
            throw new Error("Controlled unsupported source operation: " + kind)
        }
    const forbidden = () => {
        calls.unexpected++
        throw new Error("Unexpected import oracle side effect")
    }
    const sourceFs = new Proxy(fs, {
        get(target, key) {
            if (key === "readFile")
                return (file, ...args) => {
                    calls.reads.push(String(file))
                    return fs.readFile(file, ...args)
                }
            return target[key]
        }
    })
    const Main = enums("src/types/IPC/Main.ts"),
        ToMain = enums("src/types/IPC/ToMain.ts").ToMain
    const { sendToMain } = load("src/electron/IPC/main.ts", ["sendToMain"], {
        ToMain,
        MAIN: Main.MAIN,
        mainWindow: {
            isDestroyed: () => false,
            webContents: {
                isDestroyed: () => false,
                send: (channel, message, listenerId) => {
                    assert.equal(listenerId, undefined)
                    events.push(clean({ channel, message }))
                }
            }
        }
    })
    const files = load("src/electron/utils/files.ts", ["readFileAsync", "safeBufferToString", "readFileBufferAsync", "getExtension", "asyncPool", "MAX_NODE_STRING_LENGTH"], { fs: sourceFs, path, Buffer })
    const zip = load("src/electron/data/zip.ts", ["decompressZip", "decompressZipStream", "getZipErrorAlert", "sanitizeZipPath", "processEntry", "streamToDisk", "bufferInMemory", "isZip", "STRING_CONVERT_LIMIT"], {
        fs: sourceFs,
        path,
        yauzl,
        Buffer,
        ToMain,
        sendToMain,
        getExtension: files.getExtension,
        createFolder: forbidden
    })
    const imports = load("src/electron/data/import.ts", names("src/electron/data/import.ts"), {
        ...files,
        ...zip,
        path,
        upath,
        join: path.join,
        Buffer,
        ToMain,
        sendToMain,
        Database: trap("sqlite"),
        MDBReader: trap("mdb"),
        WordExtractor: trap("word"),
        pptToShow: trap("powerpoint"),
        protobufjs: { load: trap("protobuf") },
        __dirname: "/controlled-oracle",
        getDataFolderPath: forbidden,
        doesPathExist: forbidden,
        filePathHashCode: forbidden,
        detectFileType: forbidden
    })
    const { importFiles } = load("src/electron/IPC/responsesMain.ts", ["importFiles"], {
        importShow: (...args) => {
            const pending = imports.importShow(...args)
            work.push(pending)
            return pending
        }
    })
    const response = ast("src/electron/IPC/responsesMain.ts").statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText() === "mainResponses"))
    const handler = response.declarationList.declarations[0].initializer.properties.find((node) => node.name?.getText() === "[Main.IMPORT_FILES]")
    assert.ok(handler)
    const context = vm.createContext({ Main: Main.Main, MAIN: Main.MAIN, ToMain, importFiles, console: { error: forbidden } })
    new vm.Script(
        ts.transpileModule(
            `globalThis.mainResponses = {[Main.IMPORT_FILES]:${handler.initializer.getText()}};\n${ast("src/electron/IPC/main.ts")
                .statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "receiveMain")
                .getText()
                .replace(/^export\s+/, "")}\nglobalThis.receive=receiveMain`,
            { compilerOptions: { target: ts.ScriptTarget.ES2022 } }
        ).outputText
    ).runInContext(context, { timeout: 1000 })
    return {
        events,
        replies,
        calls,
        async dispatch(id, paths) {
            await context.receive({ reply: (...args) => replies.push(args) }, { channel: Main.Main.IMPORT_FILES, data: { id, paths } }, "import-text-listener")
            await Promise.all(work)
            assert.equal(replies.length, 0, "Original importFiles returns void; no source success ACK")
            assert.equal(calls.unexpected, 0)
            return events
        }
    }
}

// Actual retained converters and ShowObj; deterministic UI boundaries. XML DOM
// uses installed xmldom with only children/attribute iteration adaptations, not
// a browser parser oracle. HTML/OpenSong Bible variants are outside these tests.
export function frontendOracle(id) {
    const converted = [],
        histories = [],
        toasts = []
    const store = (value) => ({
        value,
        set(next) {
            this.value = next
        },
        update(fn) {
            this.value = fn(this.value)
        }
    })
    const stores = Object.fromEntries(["activePopup", "alertMessage", "activeShow", "activeProject", "activeRename", "activeDrawerTab", "drawerTabsData", "categories", "shows", "templates", "groups", "dictionary", "globalTags", "groupNumbers"].map((name) => [name, store({})]))
    stores.groupNumbers.value = true
    stores.activeShow.value = { id: "default" }
    stores.activeDrawerTab.value = "shows"
    stores.drawerTabsData.value = { shows: { activeSubTab: "all" } }
    stores.groups.value = { verse: { name: "Verse" }, chorus: { name: "Chorus" } }
    stores.dictionary.value = { groups: { verse: "Verse", chorus: "Chorus" } }
    let sequence = 0
    const injected = {
        ...stores,
        get: (s) => s.value,
        uid: () => "generated-" + ++sequence,
        history: (entry) => histories.push(clean(entry)),
        newToast: (text) => toasts.push(text),
        checkName: (name) => name,
        translateText: (text) => text,
        clone: structuredClone,
        atob,
        TextDecoder,
        Uint8Array,
        setTimeout: (callback) => callback(),
        requestAnimationFrame: (callback) => callback(),
        Date: class extends Date {
            constructor(...args) {
                super(...(args.length ? args : [1700000000000]))
            }
            static now() {
                return 1700000000000
            }
        },
        _show: () => ({ get: () => null }),
        setQuickAccessMetadata: (show, key, value) => {
            show.quickAccess[key] = value
            return show
        }
    }
    const helpers = load("src/frontend/converters/importHelpers.ts", ["createCategory", "setTempShows"], injected)
    const showHelpers = load("src/frontend/components/helpers/show.ts", ["getLabelId", "getGlobalGroup", "initializeMetadata", "newSlide"], injected)
    const chords = load("src/frontend/components/edit/scripts/chords.ts", ["isChordLine", "parseChordLine"], injected)
    const { ShowObj } = load("src/frontend/classes/Show.ts", ["ShowObj"], injected)
    const { DOMParser: NativeDOMParser } = require("@xmldom/xmldom")
    class DOMParser {
        parseFromString(text, type) {
            assert.equal(type, "text/xml", "HTML variants need actual browser DOM evidence")
            const doc = new NativeDOMParser().parseFromString(text, type)
            function adapt(node) {
                Object.defineProperty(node, "children", { get: () => Array.from(node.childNodes || []).filter((item) => item.nodeType === 1) })
                if (node.attributes)
                    node.attributes[Symbol.iterator] = function* () {
                        for (let i = 0; i < this.length; i++) yield this[i]
                    }
                for (const child of Array.from(node.childNodes || [])) adapt(child)
            }
            adapt(doc)
            return doc
        }
    }
    const xml = load("src/frontend/converters/xml.ts", ["xml2json", "xmlParser"], { DOMParser })
    const file = `src/frontend/converters/${id}.ts`
    const functions = load(file, names(file), {
        ...injected,
        ...helpers,
        ...showHelpers,
        ...xml,
        ...chords,
        ShowObj,
        DOMParser,
        DEFAULT_ITEM_STYLE: "controlled-source-style",
        setTempShows: (shows) => {
            converted.push(...clean(shows))
            helpers.setTempShows(shows)
        }
    })
    const method = { openlp: "convertOpenLP", opensong: "convertOpenSong", mediashout: "convertMediaShout", quelea: "convertQuelea", songbeamer: "convertSongbeamerFiles", easyslides: "convertEasyslides", verseview: "convertVerseVIEW", videopsalm: "convertVideopsalm" }[id]
    const frontendSource = ast("src/frontend/IPC/responsesMain.ts")
    let import2
    function visit(node) {
        if (ts.isPropertyAssignment(node) && node.name.getText(frontendSource) === "[ToMain.IMPORT2]") import2 = node
        ts.forEachChild(node, visit)
    }
    visit(frontendSource)
    assert.ok(import2)
    const context = vm.createContext({ ...functions, console: { error() {} } })
    new vm.Script(ts.transpileModule(`globalThis.receive=${import2.initializer.getText(frontendSource)}`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText).runInContext(context, { timeout: 1000 })
    return {
        converted,
        histories,
        toasts,
        consume(files) {
            assert.ok(method)
            context.receive({ channel: id, data: files })
            return converted
        }
    }
}
