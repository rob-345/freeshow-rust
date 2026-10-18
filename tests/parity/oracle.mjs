import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { fileURLToPath } from "node:url"
import ts from "typescript"

export const root = fileURLToPath(new URL("../../", import.meta.url))
export const defaults = JSON.parse(fs.readFileSync(path.join(root, "tests/parity/fixtures/defaults.json"), "utf8"))

// Extract the actual pinned upstream declarations. We never copy a reference
// algorithm into the test. Only OS/Electron boundaries are injected.
export function upstreamFunctions(filename, names, injected = {}, prelude = "") {
    const source = fs.readFileSync(path.join(root, filename), "utf8")
    const ast = ts.createSourceFile(filename, source, ts.ScriptTarget.Latest, true)
    const declarations = names.map((name) => {
        const node = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === name)
        if (!node) throw new Error(`Upstream function ${name} missing in ${filename}`)
        return node.getText(ast).replace(/^export\s+/, "")
    })
    const context = vm.createContext({ ...injected, console: { error() {}, warn() {}, info() {} } })
    const javascript = ts.transpileModule(prelude + "\n" + declarations.join("\n") + `\nglobalThis.result = {${names.join(",")}}`, {
        compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS }
    }).outputText
    new vm.Script(javascript, { filename }).runInContext(context, { timeout: 1000 })
    return context.result
}

export const showOracle = upstreamFunctions("src/electron/utils/shows.ts", ["trimShow", "getShowTextContent", "showHasLayoutContent"])
export const fileOracle = upstreamFunctions("src/electron/utils/files.ts", ["parseJSON", "getTextCacheString"])

export const clean = (value) => JSON.parse(JSON.stringify(value))

export function fixtureShow(seed) {
    const words = ["Amazing Grace", "Hallelujah", "Ελληνικά", "日本語", "مَرْحَبًا", "🙌🏾", "", " line\n"]
    const slides = {}
    // Include insertion order different from lexical and numeric-key order.
    for (const [index, key] of ["z", "10", "2", "a"].entries()) {
        slides[key] = {
            items: [
                { type: "text", lines: Array.from({ length: (seed + index) % 4 }, (_, line) => ({ text: [{ value: words[(seed + line) % words.length], font: { size: 80 } }, { value: words[(seed + line + index + 1) % words.length] }] })) },
                { type: "media", src: "fixtures/background.jpg" }
            ]
        }
    }
    return {
        name: `Song ${seed} — ${words[seed % words.length]}`,
        category: seed % 3 ? "song" : null,
        timestamps: { created: seed * 100, modified: seed * 101 },
        quickAccess: seed % 2 ? {} : null,
        slides: seed % 13 ? slides : {},
        layouts: { default: { slides: seed % 7 ? [{ id: "z" }] : [] } },
        ...(seed % 2 ? { private: seed % 3 !== 0 } : {}),
        ...(seed % 5 ? { origin: { provider: "test", id: `${seed}` } } : {}),
        ...(seed % 11 ? { locked: false } : { locked: true }),
        ...(seed % 17 ? {} : { reference: { type: "scripture" } }),
        customFutureField: { preserved: [seed, true, null] }
    }
}

export function createSaveOracle(appData, dataDir) {
    const stores = {}
    const storeIds = ["SHOWS", "SETTINGS", "SYNCED_SETTINGS", "PROJECTS", "STAGE", "THEMES", "OVERLAYS", "TEMPLATES", "EVENTS", "MEDIA", "HISTORY", "CACHE", "CACHE_SYNC", "USAGE", "ERROR_LOG", "DRIVE_API_KEY", "ACCESS"]
    const portable = ["SYNCED_SETTINGS", "PROJECTS", "STAGE", "THEMES", "OVERLAYS", "TEMPLATES", "EVENTS"]
    const storePath = (id) => path.join(portable.includes(id) ? path.join(dataDir, "Config") : appData, `${id === "SYNCED_SETTINGS" ? "settings_synced" : ["ACCESS", "DRIVE_API_KEY"].includes(id) ? id : id.toLowerCase()}.json`)
    const write = (file, content) => {
        fs.mkdirSync(path.dirname(file), { recursive: true })
        fs.writeFileSync(file, content)
    }
    for (const id of storeIds) {
        const initial = id === "SETTINGS" ? defaults.defaultSettings : id === "SYNCED_SETTINGS" ? defaults.defaultSyncedSettings : id === "PROJECTS" ? { projects: {}, folders: {}, projectTemplates: {} } : id === "ACCESS" ? { contentProviders: {}, secrets: {} } : {}
        stores[id] = {
            get store() {
                return fs.existsSync(storePath(id)) ? JSON.parse(fs.readFileSync(storePath(id), "utf8")) : initial
            },
            set store(value) {
                write(storePath(id), JSON.stringify(value))
            }
        }
    }
    const events = []
    const getDataFolderPath = (id) => {
        const folder = path.join(dataDir, { shows: "Shows", scriptures: "Bibles" }[id])
        fs.mkdirSync(folder, { recursive: true })
        return folder
    }
    const { save } = upstreamFunctions(
        "src/electron/data/save.ts",
        ["save", "isValidJSON"],
        {
            path,
            _store: stores,
            defaultSettings: defaults.defaultSettings,
            defaultSyncedSettings: defaults.defaultSyncedSettings,
            currentlyDeletedShows: [],
            getDataFolderPath,
            doesPathExist: fs.existsSync,
            readFile: (file) => fs.readFileSync(file, "utf8"),
            writeFile: write,
            deleteFile: fs.unlinkSync,
            parseShow: fileOracle.parseJSON,
            checkIfMatching: (a, b) => JSON.stringify(a) === JSON.stringify(b),
            clone: structuredClone,
            wait: async () => {},
            safeStoreSet: async (store, value) => {
                store.store = value
            },
            setLocalFileModified: async () => {},
            startBackup: async () => {
                throw new Error("Backups require their own integration oracle")
            },
            renameShows: async (shows, directory) => {
                for (const show of shows) {
                    const oldPath = path.join(directory, show.oldName + ".show")
                    if (fs.existsSync(oldPath)) fs.renameSync(oldPath, path.join(directory, (show.name || show.id) + ".show"))
                }
            },
            ToMain: { SAVE2: "SAVE2" },
            sendToMain: (channel, data) => events.push({ channel: "MAIN", message: { channel, data: clean(data) } }),
            sendMain: (channel, data) => events.push({ channel: "MAIN", message: { channel, data: clean(data) } })
        },
        'let isSaving = false; const TRACK_FILE_MODIFIED = ["EVENTS", "THEMES", "MEDIA"];'
    )
    return { save, events }
}
