import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { fileURLToPath } from "node:url"
import ts from "typescript"
import Conf from "conf"

export const root = fileURLToPath(new URL("../../", import.meta.url))
export const defaults = JSON.parse(fs.readFileSync(path.join(root, "tests/parity/fixtures/defaults.json"), "utf8"))
export const storeDefinitions = JSON.parse(fs.readFileSync(path.join(root, "tests/parity/fixtures/stores.json"), "utf8"))

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
    const context = vm.createContext({
        ...injected,
        console: { error() {}, warn() {}, info() {} }
    })
    const javascript = ts.transpileModule(prelude + "\n" + declarations.join("\n") + `\nglobalThis.result = {${names.join(",")}}`, {
        compilerOptions: {
            target: ts.ScriptTarget.ES2022,
            module: ts.ModuleKind.CommonJS
        }
    }).outputText
    new vm.Script(javascript, { filename }).runInContext(context, {
        timeout: 1000
    })
    return context.result
}

export const showOracle = upstreamFunctions("src/electron/utils/shows.ts", ["trimShow", "getShowTextContent", "showHasLayoutContent"])
export const fileOracle = upstreamFunctions("src/electron/utils/files.ts", ["parseJSON", "getTextCacheString"])

export const clean = (value) => JSON.parse(JSON.stringify(value))

export function upstreamClasses(sources, injected = {}) {
    const declarations = sources.map(([filename, name]) => {
        const source = fs.readFileSync(path.join(root, filename), "utf8")
        const ast = ts.createSourceFile(filename, source, ts.ScriptTarget.Latest, true)
        const declaration = ast.statements.find((node) => ts.isClassDeclaration(node) && node.name?.text === name)
        if (!declaration) throw new Error(`Upstream class ${name} missing in ${filename}`)
        return declaration.getText(ast).replace(/^export\s+/, "")
    })
    const context = vm.createContext({
        ...injected,
        console: { error() {}, warn() {}, info() {} }
    })
    const javascript = ts.transpileModule(declarations.join("\n") + `\nglobalThis.result = {${sources.map(([, name]) => name).join(",")}}`, {
        compilerOptions: {
            target: ts.ScriptTarget.ES2022,
            module: ts.ModuleKind.CommonJS
        }
    }).outputText
    new vm.Script(javascript, {
        filename: "upstream-output-classes"
    }).runInContext(context, { timeout: 1000 })
    return context.result
}

export function fixtureShow(seed) {
    const words = ["Amazing Grace", "Hallelujah", "Ελληνικά", "日本語", "مَرْحَبًا", "🙌🏾", "", " line\n"]
    const slides = {}
    // Include insertion order different from lexical and numeric-key order.
    for (const [index, key] of ["z", "10", "2", "a"].entries()) {
        slides[key] = {
            items: [
                {
                    type: "text",
                    lines: Array.from({ length: (seed + index) % 4 }, (_, line) => ({
                        text: [
                            {
                                value: words[(seed + line) % words.length],
                                font: { size: 80 }
                            },
                            { value: words[(seed + line + index + 1) % words.length] }
                        ]
                    }))
                },
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
    const write = (file, content) => {
        fs.mkdirSync(path.dirname(file), { recursive: true })
        fs.writeFileSync(file, content)
    }
    new Conf({
        cwd: appData,
        projectName: "freeshow-parity",
        configName: "config",
        defaults: defaults.defaultConfig
    })
    for (const [id, data] of Object.entries(storeDefinitions)) {
        stores[id] = new Conf({
            cwd: data.portable ? path.join(dataDir, "Config") : appData,
            projectName: "freeshow-parity",
            configName: data.fileName,
            defaults: data.defaults,
            accessPropertiesByDotNotation: id !== "MEDIA"
        })
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
            sendToMain: (channel, data) =>
                events.push({
                    channel: "MAIN",
                    message: { channel, data: clean(data) }
                }),
            sendMain: (channel, data) =>
                events.push({
                    channel: "MAIN",
                    message: { channel, data: clean(data) }
                })
        },
        'let isSaving = false; const TRACK_FILE_MODIFIED = ["EVENTS", "THEMES", "MEDIA"];'
    )
    return { save, events, stores }
}
