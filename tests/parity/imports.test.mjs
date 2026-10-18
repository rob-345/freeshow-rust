import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { createRequire } from "node:module"
import { createHash } from "node:crypto"
import { test } from "node:test"
import ts from "typescript"
import { root, upstreamFunctions, upstreamClasses, clean, fixtureShow } from "./oracle.mjs"
const require = createRequire(import.meta.url)
const yauzl = require("yauzl"),
    yazl = require("yazl"),
    upath = require("upath")
const binary = process.env.FREESHOW_IMPORTS_BIN || path.join(root, "target/debug", process.platform === "win32" ? "freeshow-imports.exe" : "freeshow-imports")
function variables(filename, names) {
    const ast = ts.createSourceFile(filename, fs.readFileSync(path.join(root, filename), "utf8"), ts.ScriptTarget.Latest, true)
    return names.map((name) => (ast.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText(ast) === name))?.getText(ast) || assert.fail(`Missing original variable ${name}`)).replace(/^export\s+/, "")).join("\n")
}
function declarations(filename, predicate = () => true) {
    const ast = ts.createSourceFile(filename, fs.readFileSync(path.join(root, filename), "utf8"), ts.ScriptTarget.Latest, true)
    return ast.statements
        .filter((node) => ts.isFunctionDeclaration(node) && predicate(node.name.text))
        .map((node) => node.getText(ast).replace(/^export\s+/, ""))
        .join("\n")
}
function temporary(t) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-import-parity-"))
    t.after(() => fs.rmSync(dir, { recursive: true, force: true }))
    return dir
}
function write(dir, name, content) {
    const file = path.join(dir, name)
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, content)
    return file
}
function rust(cases) {
    const result = spawnSync(binary, [], { input: cases.map((item) => JSON.stringify(item)).join("\n") + "\n", encoding: "utf8", timeout: 30000, maxBuffer: 32 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return result.stdout.trim().split("\n").map(JSON.parse)
}
const request = (id, paths, importsRoot) => ({ command: "IMPORT_FILES", data: { id, paths }, importsRoot })
function oracle(importsRoot) {
    const events = []
    const ToMain = { ALERT: "ALERT", IMPORT2: "IMPORT2" }
    const sendToMain = (channel, data) => events.push({ channel: "MAIN", message: { channel, data: clean(data) } })
    const files = upstreamFunctions("src/electron/utils/files.ts", ["readFileAsync", "safeBufferToString", "getExtension", "asyncPool"], { fs, path }, variables("src/electron/utils/files.ts", ["MAX_NODE_STRING_LENGTH"]))
    const zip = upstreamFunctions("src/electron/data/zip.ts", ["compressToZip", "getZipErrorAlert", "decompressZip", "decompressZipStream", "sanitizeZipPath", "processEntry", "streamToDisk", "bufferInMemory", "isZip"], { fs, path, yauzl, yazl, Buffer, ToMain, sendToMain, getExtension: files.getExtension, createFolder: (folder) => fs.mkdirSync(folder, { recursive: true }) }, variables("src/electron/data/zip.ts", ["STRING_CONVERT_LIMIT"]))
    const { filePathHashCode } = upstreamFunctions("src/electron/data/thumbnails.ts", ["filePathHashCode"])
    const imports = upstreamFunctions(
        "src/electron/data/import.ts",
        ["importShow", "readFile", "importProject", "importTemplate", "importOverlay", "extractZipDataAndMedia", "checkSpecial"],
        {
            ...files,
            ...zip,
            path,
            upath,
            Buffer,
            ToMain,
            sendToMain,
            doesPathExist: fs.existsSync,
            filePathHashCode,
            getDataFolderPath: (_kind, folder) => {
                const destination = path.join(importsRoot, folder)
                fs.mkdirSync(destination, { recursive: true })
                return destination
            }
        },
        variables("src/electron/data/import.ts", ["getFileName"]) + "\nconst specialImports = {};"
    )
    return { ...imports, ...zip, filePathHashCode, events }
}
function canonicalEvents(events) {
    return events.map((event) => (event.message.channel === "IMPORT2" ? { ...event, message: { ...event.message, data: { ...event.message.data, data: [...event.message.data.data].sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b))) } } } : event))
}
async function compareOracle(cases) {
    const actual = rust(cases)
    for (const [index, item] of cases.entries()) {
        const expected = oracle(item.importsRoot)
        await expected.importShow(item.data.id, item.data.paths)
        assert.equal(actual[index].error, undefined, JSON.stringify(actual[index].error))
        assert.deepEqual(canonicalEvents(actual[index].events), canonicalEvents(expected.events), JSON.stringify(item.data))
        for (const media of actual[index].media) {
            const originalBytes = fs.readFileSync(path.join(item.importsRoot, media.destination))
            assert.deepEqual(Buffer.from(media.content), originalBytes, `staged media equals original extraction: ${media.destination}`)
            assert.equal(media.sha256, createHash("sha256").update(originalBytes).digest("hex"))
        }
    }
    return actual
}

test("raw native/text transport, names, UTF-8 replacement, missing files and empty selections match original handlers", async (t) => {
    const dir = temporary(t),
        importsRoot = path.join(dir, "Imports")
    const show = write(dir, "敬拜 🙌.show", JSON.stringify(["fixed-id", fixtureShow(1)]))
    const cases = [
        ["freeshow", show],
        ["freeshow_project", write(dir, "A.project", '{ "project": {"name":"P"}, "future":true }')],
        ["freeshow_template", write(dir, "T.fstemplate", '{"template":{"id":"t","items":[]}}')],
        ["freeshow_overlay", write(dir, "O.fsoverlay", '{"overlay":{"id":"o","items":[]}}')],
        ["freeshow_theme", write(dir, "色.fstheme", '{"name":"色","future":[1,true]}')],
        ["freeshow_action", write(dir, "A.fsaction", '{"action":{"triggers":[],"future":42}}')],
        ["freeshow_stage", write(dir, "S.fsstage", '{"settings":{},"items":{},"unknown":null}')],
        ["txt", write(dir, "歌词.txt", "Title\r\n\r\nVerse 1\n敬拜 🙌\n")],
        ["chordpro", write(dir, "Chords.cho", "{title: Café}\n[C]Hello [G]world")],
        ["csv", write(dir, "table.CSV", '"Hello, world",敬拜\r\n"""quoted""",last\rnext,line')],
        ["txt", write(dir, "invalid.txt", Buffer.from([0xff, 0xe2, 0x82, 0x41]))],
        ["txt", write(dir, "extensionless", "text")],
        ["txt", write(dir, ".dotfile", "text")],
        ["txt", path.join(dir, "missing.txt")],
        ["txt", dir]
    ]
    await compareOracle(cases.map(([id, file]) => request(id, [file], importsRoot)))
    await compareOracle([
        request(
            "txt",
            cases.slice(7).map(([, file]) => file),
            importsRoot
        ),
        ...["freeshow", "freeshow_project", "freeshow_template", "freeshow_overlay"].flatMap((id) => [request(id, [], importsRoot), request(id, null, importsRoot)])
    ])
})

test("real original project/template/overlay exports remap media and preserve frontend input bytes", async (t) => {
    const dir = temporary(t),
        importsRoot = path.join(dir, "Imports"),
        original = oracle(importsRoot)
    const source = write(dir, "source/背景 🙌.png", Buffer.from([0, 1, 255, 80, 78, 71])),
        second = write(dir, "source/other.bin", "binary-content")
    const exports = path.join(dir, "exports")
    fs.mkdirSync(exports)
    const jobs = []
    let completed
    const exportApi = upstreamFunctions("src/electron/data/export.ts", ["exportProject", "exportTemplate", "exportOverlay", "compressWithMedia", "getUniquePath"], {
        fs,
        join: path.join,
        dirname: path.dirname,
        basename: path.basename,
        extname: path.extname,
        Buffer,
        ToMain: { ALERT: "ALERT" },
        sendToMain() {},
        getDataFolderPath: () => exports,
        compressToZip: (...args) => {
            const promise = original.compressToZip(...args)
            jobs.push(promise)
            return promise
        },
        doesPathExist: fs.existsSync,
        filePathHashCode: original.filePathHashCode,
        doneWritingFile: (error) => {
            if (error) throw error
            completed++
        },
        customJSONExtensions: { TEMPLATE: ".fstemplate", OVERLAY: ".fsoverlay" }
    })
    completed = 0
    exportApi.exportProject({ type: "project", name: "P", file: { project: { id: "p", name: "Project" }, files: [source, second, path.join(dir, "absent.mp4")], unknown: { x: true }, text: source } })
    exportApi.exportTemplate({ name: "T", file: { template: { id: "t", name: "Template", items: [{ src: source }] }, files: [source] } })
    exportApi.exportOverlay({ name: "O", file: { overlay: { id: "o", name: "Overlay", items: [{ src: source }] }, files: [source] } })
    await Promise.all(jobs)
    assert.equal(completed, 3)
    fs.rmSync(path.dirname(source), { recursive: true })
    const cases = [
        ["freeshow_project", "P.project"],
        ["freeshow_template", "T.fstemplate"],
        ["freeshow_overlay", "O.fsoverlay"]
    ].map(([id, filename]) => request(id, [path.join(exports, filename)], importsRoot))
    const actual = await compareOracle(cases)
    for (const item of actual) {
        assert.ok(item.media.length)
        const references = JSON.parse(item.events.at(-1).message.data.data[0].content).files
        assert.ok(references.length)
        assert.ok(references.every((file) => typeof file === "string" && file.startsWith(importsRoot + path.sep)))
        for (const media of item.media) {
            assert.equal(media.length, media.content.length)
            assert.match(media.sha256, /^[a-f0-9]{64}$/)
        }
    }
    assert.equal(actual[0].plan.unresolved.length, 1)
    // Plain source path supplied; bundles have no path and keep source metadata shape.
    assert.equal(Object.hasOwn(actual[0].events.at(-1).message.data.data[0], "path"), false)
    assert.equal(actual[0].plan.directories[0], "Projects")
    const repeated = rust([cases[0]])[0]
    assert.equal(repeated.media.length, 0)
    assert.equal(repeated.plan.existing.length, 2)
})

test("ZIP branch ignores selected plain files; malformed/empty/data-less bundles retain original ALERT and empty distinctions", async (t) => {
    const dir = temporary(t),
        importsRoot = path.join(dir, "Imports"),
        upstream = oracle(importsRoot)
    const zip = path.join(dir, "text.zip")
    await upstream.compressToZip(
        [
            { name: "folder/歌词.TXT", content: "Café\n敬拜" },
            { name: "quoted.csv", content: '"a,b",c' }
        ],
        zip
    )
    const plain = write(dir, "ignore.txt", "ignored"),
        broken = write(dir, "bad.zip", "PK\x03\x04bad"),
        empty = path.join(dir, "empty.project")
    await upstream.compressToZip([], empty)
    const noData = path.join(dir, "nodata.fstemplate")
    await upstream.compressToZip([{ name: "other.json", content: "{}" }], noData)
    await compareOracle([request("txt", [plain, zip], importsRoot), request("txt", [broken, zip], importsRoot), request("freeshow_project", [empty], importsRoot), request("freeshow_template", [noData], importsRoot), request("freeshow_overlay", [empty], importsRoot), request("freeshow_template", [broken], importsRoot)])
})

function store(value) {
    return {
        value,
        set(next) {
            this.value = next
        },
        update(update) {
            this.value = update(this.value)
        }
    }
}
function frontend() {
    const histories = [],
        toasts = [],
        converted = []
    let sequence = 0
    const stores = Object.fromEntries(["activePopup", "alertMessage", "activeShow", "activeProject", "activeRename", "activeDrawerTab", "actionTags", "shows", "categories", "drawerTabsData", "templates", "groups", "dictionary", "special", "splitLines", "formatNewShow", "customMetadata", "groupNumbers", "folders", "categoriesStore", "overlayStores", "effectsStores", "actionsStores", "mediaStores"].map((name) => [name, store(null)]))
    stores.categories.value = {}
    stores.actionTags.value = {}
    stores.drawerTabsData.value = { shows: { activeSubTab: "all" } }
    stores.templates.value = {}
    stores.activeShow.value = { id: "default" }
    for (const name of ["special", "customMetadata", "folders", "categoriesStore", "overlayStores", "effectsStores", "actionsStores", "mediaStores"]) stores[name].value = {}
    stores.groups.value = { verse: { name: "Verse" }, chorus: { name: "Chorus" } }
    stores.dictionary.value = { groups: { verse: "Verse", chorus: "Chorus" } }
    stores.splitLines.value = 0
    stores.groupNumbers.value = true
    const injected = { ...stores, get: (store) => store.value, uid: () => `generated-${++sequence}`, history: (entry) => histories.push(clean(entry)), newToast: (message) => toasts.push(message), checkName: (name) => name, convertOldShowValues: async (show) => show, setTimeout: (callback) => callback(), clone: structuredClone, translateText: (text) => text, _show: () => ({ get: () => null }) }
    const helpers = upstreamFunctions("src/frontend/converters/importHelpers.ts", ["importShow", "fixShowIssues", "setTempShows", "importTemplate", "importOverlay", "importAction", "importStage", "importSpecific"], injected)
    const { ShowObj } = upstreamClasses([["src/frontend/classes/Show.ts", "ShowObj"]], injected)
    const csv = upstreamFunctions(
        "src/frontend/converters/csv.ts",
        ["convertCSV", "createShow", "getLayout", "parseCSVLine"],
        {
            ...injected,
            ShowObj,
            DEFAULT_ITEM_STYLE: "fixture-style",
            setTempShows: (shows) => {
                converted.push(...clean(shows))
                helpers.setTempShows(shows)
            }
        },
        variables("src/frontend/converters/csv.ts", ["DEFAULT_SLIDE"])
    )
    const capture = (shows) => {
        converted.push(...clean(shows))
        helpers.setTempShows(shows)
    }
    const showHelpers = upstreamFunctions("src/frontend/components/helpers/show.ts", ["getLabelId", "getGlobalGroup", "getCustomMetadata", "initializeMetadata"], injected)
    const textStyle = upstreamFunctions("src/frontend/components/edit/scripts/textStyle.ts", ["getSlideText", "getItemText"])
    const chords = upstreamFunctions("src/frontend/components/edit/scripts/chords.ts", ["createChord"], injected)
    const boxes = upstreamFunctions("src/frontend/components/show/formatTextEditor.ts", ["linesToTextboxes", "getChords", "getLine"], { ...injected, ...chords, VIRTUAL_BREAK_CHAR: "[_VB]" }, variables("src/frontend/components/show/formatTextEditor.ts", ["DEFAULT_ITEM_STYLE", "defaultItem", "textboxRegex"]))
    const text = upstreamFunctions(
        "src/frontend/converters/txt.ts",
        ["convertTexts", "convertText", "trimNameFromString", "isCCLIBlock", "extractCCLIMetadata", "isHeaderLine", "isChordLine", "preprocessLines", "insertChordsIntoLyrics", "createSlides", "removeSlideDuplicates", "linesToItems", "checkRepeats", "fixText", "similarity", "editDistance", "findGroupMatch"],
        { ...injected, ...showHelpers, ...textStyle, ...boxes, ShowObj, setTempShows: capture, VIRTUAL_BREAK_CHAR: "[_VB]" },
        variables("src/frontend/converters/txt.ts", ["CHORD_CORE", "CHORD_EXACT_REGEX", "CHORD_WORD_REGEX"]) + "\n" + variables("src/frontend/converters/txtAutoSections.ts", ["SIMILARITY_THRESHOLD"]) + "\n" + declarations("src/frontend/converters/txtAutoSections.ts")
    )
    const chordpro = upstreamFunctions("src/frontend/converters/chordpro.ts", ["convertChordPro", "createShow", "getLayout"], { ...injected, ...showHelpers, ShowObj, DEFAULT_ITEM_STYLE: "fixture-style", setTempShows: capture }, variables("src/frontend/converters/chordpro.ts", ["metaKeys", "chorus", "defaultSlide"]))
    const project = upstreamFunctions("src/frontend/converters/project.ts", ["importProject"], injected)
    return { ...helpers, ...csv, ...text, ...chordpro, importProject: project.importProject, ...stores, histories, converted, toasts }
}
test("native transport feeds actual existing frontend show/template/overlay/action/stage/theme and CSV converters", async (t) => {
    const dir = temporary(t),
        importsRoot = path.join(dir, "Imports")
    const payloads = [
        ["freeshow", ["fixed", fixtureShow(1)], "show.show"],
        ["freeshow_template", { template: { id: "t", items: [], future: true } }, "t.fstemplate"],
        ["freeshow_overlay", { overlay: { id: "o", items: [], future: true } }, "o.fsoverlay"],
        ["freeshow_action", { action: { id: "a", triggers: [], tags: ["new"], future: 42 } }, "a.fsaction"],
        ["freeshow_stage", { id: "s", settings: {}, items: {}, future: true }, "s.fsstage"],
        ["freeshow_theme", { name: "Theme", unknown: true }, "t.fstheme"]
    ]
    const results = rust(payloads.map(([id, payload, file]) => request(id, [write(dir, file, JSON.stringify(payload))], importsRoot)))
    for (const [index, result] of results.entries()) {
        const ui = frontend()
        const files = result.events.at(-1).message.data.data
        const id = payloads[index][0]
        if (id === "freeshow") {
            await ui.importShow(files)
            assert.equal(ui.histories[0].oldData.id, "fixed")
            assert.equal(ui.histories[0].newData.data.customFutureField.preserved[0], 1)
        } else if (id === "freeshow_theme") {
            const themes = store({})
            ui.importSpecific(files, themes)
            assert.equal(Object.values(themes.value)[0].unknown, true)
        } else {
            const method = { freeshow_template: "importTemplate", freeshow_overlay: "importOverlay", freeshow_action: "importAction", freeshow_stage: "importStage" }[id]
            ui[method](files)
            assert.equal(ui.histories.length, 1)
            assert.equal(ui.histories[0].newData.data.future, id === "freeshow_action" ? 42 : true)
            assert.equal(Object.hasOwn(ui.histories[0].newData.data, "id"), false)
        }
    }
    const csv = rust([request("csv", [write(dir, "表.csv", '"Hello, world",敬拜\r\n"""quoted""",last\rnext,line')], importsRoot)])[0]
    const ui = frontend()
    ui.convertCSV(csv.events[0].message.data.data)
    assert.equal(ui.converted[0].show.name, "表")
    assert.deepEqual(
        Object.values(ui.converted[0].show.slides).map((slide) => slide.items.map((item) => item.lines[0].text[0].value)),
        [
            ["Hello, world", "敬拜"],
            ['"quoted"', "last"],
            ["next", "line"]
        ]
    )
    assert.equal(ui.histories[0].location.page, "show")
})

test("native text and project replies feed actual TXT/ChordPro/project conversion and history", async (t) => {
    const dir = temporary(t),
        importsRoot = path.join(dir, "Imports")
    const cases = [
        request("txt", [write(dir, "敬拜.txt", "[Verse]\n[C]Hello 敬拜\nSecond line\n\n[Chorus]\n[G]World 🙌\n")], importsRoot),
        request("chordpro", [write(dir, "chords.cho", "{title: Café}\n{artist: Alpha}\n[C]Hello [G]world\n")], importsRoot),
        request("freeshow_project", [write(dir, "p.project", JSON.stringify({ project: { id: "p", name: "Project", future: 42 }, parentFolder: "Existing", shows: { s: fixtureShow(2) }, categories: { custom: { name: "Custom" } }, overlays: { o: { items: [], future: true } }, media: { "file.png": { volume: 0.5 } } }))], importsRoot)
    ]
    const results = rust(cases)
    const txt = frontend()
    txt.convertTexts(results[0].events.at(-1).message.data.data)
    assert.equal(txt.converted[0].show.name, "敬拜")
    const slides = Object.values(txt.converted[0].show.slides)
    assert.equal(slides.length, 2)
    assert.equal(slides[0].items[0].lines[0].text[0].value, "Hello 敬拜")
    assert.equal(slides[0].items[0].lines[0].chords[0].key, "C")
    assert.equal(txt.histories[0].location.page, "show")
    const chords = frontend()
    chords.convertChordPro(results[1].events[0].message.data.data)
    assert.equal(chords.converted[0].show.name, "Café")
    assert.equal(chords.converted[0].show.meta.artist, "Alpha")
    assert.deepEqual(
        Object.values(chords.converted[0].show.slides)[0].items[0].lines[0].chords.map((chord) => [chord.pos, chord.key]),
        [
            [0, "C"],
            [6, "G"]
        ]
    )
    const project = frontend()
    project.folders.value = { folder: { name: "Existing" } }
    project.importProject(results[2].events.at(-1).message.data.data)
    assert.equal(project.histories.length, 2)
    assert.equal(project.histories[0].id, "SHOWS")
    assert.equal(project.histories[1].oldData.id, "p")
    assert.equal(project.histories[1].newData.data.parent, "folder")
    assert.equal(project.histories[1].newData.data.sourcePath, cases[2].data.paths[0])
    assert.equal(project.histories[1].newData.data.future, 42)
    assert.equal(project.overlayStores.value.o.future, true)
    assert.equal(project.mediaStores.value["file.png"].volume, 0.5)
})

function crc32(bytes) {
    let value = 0xffffffff
    for (const byte of bytes) {
        value ^= byte
        for (let index = 0; index < 8; index++) value = (value >>> 1) ^ (value & 1 ? 0xedb88320 : 0)
    }
    return (value ^ 0xffffffff) >>> 0
}
function rawZip(name, content, { flags = 0, size = content.length, method = 0, mode = 0, localName = name } = {}) {
    const encoded = Buffer.from(name),
        localEncoded = Buffer.from(localName),
        local = Buffer.alloc(30),
        central = Buffer.alloc(46),
        end = Buffer.alloc(22)
    local.writeUInt32LE(0x04034b50)
    local.writeUInt16LE(20, 4)
    local.writeUInt16LE(flags, 6)
    local.writeUInt16LE(method, 8)
    local.writeUInt32LE(crc32(content), 14)
    local.writeUInt32LE(content.length, 18)
    local.writeUInt32LE(size, 22)
    local.writeUInt16LE(localEncoded.length, 26)
    central.writeUInt32LE(0x02014b50)
    central.writeUInt16LE(0x0314, 4)
    central.writeUInt16LE(20, 6)
    central.writeUInt16LE(flags, 8)
    central.writeUInt16LE(method, 10)
    central.writeUInt32LE(crc32(content), 16)
    central.writeUInt32LE(content.length, 20)
    central.writeUInt32LE(size, 24)
    central.writeUInt16LE(encoded.length, 28)
    central.writeUInt32LE((mode * 65536) >>> 0, 38)
    const start = local.length + localEncoded.length + content.length
    end.writeUInt32LE(0x06054b50)
    end.writeUInt16LE(1, 8)
    end.writeUInt16LE(1, 10)
    end.writeUInt32LE(central.length + encoded.length, 12)
    end.writeUInt32LE(start, 16)
    return Buffer.concat([local, localEncoded, content, central, encoded, end])
}
test("archive resource/path/format failures retain diagnostics and never claim successful empty import", (t) => {
    const dir = temporary(t),
        importsRoot = path.join(dir, "Imports"),
        data = Buffer.from("{}")
    const cases = [
        ["../data.json", {}, "import_invalid_path"],
        ["/data.json", {}, "import_invalid_path"],
        ["C:/data.json", {}, "import_invalid_path"],
        ["x/./data.json", {}, "import_invalid_path"],
        ["x/../data.json", {}, "import_invalid_path"],
        ["data.json", { size: 0xffffffff }, "unsupported_archive"],
        ["data.json", { size: 100000000 }, "import_resource_limit"],
        ["data.json", { flags: 1 }, "import_encrypted_archive"],
        ["data.json", { method: 12 }, "unsupported_archive"],
        ["data.json", { mode: 0o120777 }, "import_invalid_path"],
        ["x/".repeat(17) + "data.json", {}, "import_invalid_path"]
    ]
    const inputs = cases.map(([name, options], index) => request("freeshow_project", [write(dir, `hostile-${index}.project`, rawZip(name, data, options))], importsRoot))
    for (const [index, actual] of rust(inputs).entries()) {
        assert.equal(actual.error.code, cases[index][2])
        assert.equal(actual.events, undefined)
        assert.equal(actual.error.priorEvents[0].message.data, "popup.importing")
    }
    const invalid = rust([request("pdf", ["a.pdf"], importsRoot), request("txt", [write(dir, "binary.zip", rawZip("binary.pro", Buffer.from([0, 1, 255])))], importsRoot), request("txt", Array(65).fill("missing.txt"), importsRoot), request("freeshow_project", [write(dir, "invalid.project", rawZip("data.json", Buffer.from('{"files":{}}')))], importsRoot)])
    assert.deepEqual(
        invalid.map((item) => item.error.code),
        ["unsupported_format", "unsupported_format", "import_resource_limit", "import_invalid_bundle"]
    )
    assert.equal(fs.existsSync(importsRoot), false)
})

test("Windows-style bundle references, UTF-16 hashes, collisions, dollar replacement tokens and raw formatting match original", async (t) => {
    const dir = temporary(t),
        importsRoot = path.join(dir, "Imports-$&-$$"),
        upstream = oracle(importsRoot)
    const windows = "C:\\Old\\敬拜 🙌.png",
        unix = "//old///a.png",
        other = "/other/a.png",
        missing = "/missing/absent.mp4"
    const content = JSON.stringify({ files: [windows, unix, other, missing, "https://example.test/live", "data:uri", null, 42], paths: { windows, unix, other }, unknown: { future: [1, true, null] } }, null, 2)
    const zip = path.join(dir, "references.project")
    await upstream.compressToZip(
        [
            { name: `敬拜 🙌__${upstream.filePathHashCode(windows)}.png`, content: Buffer.from([0, 1, 255]) },
            { name: "a.png", content: "collision maps to last referenced basename" },
            { name: "data.json", content }
        ],
        zip
    )
    const actual = await compareOracle([request("freeshow_project", [zip], importsRoot)])
    assert.equal(actual[0].media.length, 2)
    assert.equal(actual[0].plan.unresolved.length, 2)
})

test("real compressed bombs, duplicate names, missing archive files and existing raw references are distinguished", async (t) => {
    const dir = temporary(t),
        importsRoot = path.join(dir, "Imports"),
        upstream = oracle(importsRoot)
    const bomb = path.join(dir, "bomb.project"),
        duplicate = path.join(dir, "duplicate.project")
    await upstream.compressToZip([{ name: "data.json", content: "x".repeat(1024 * 1024) }], bomb)
    await upstream.compressToZip(
        [
            { name: "data.json", content: "{}" },
            { name: "data.json", content: "{}" }
        ],
        duplicate
    )
    const failures = rust([request("freeshow_project", [bomb], importsRoot), request("freeshow_project", [duplicate], importsRoot)])
    assert.deepEqual(
        failures.map((item) => item.error.code),
        ["import_resource_limit", "import_invalid_path"]
    )
    await compareOracle([request("txt", [path.join(dir, "missing.zip")], importsRoot)])
    const present = write(dir, "existing.png", "existing"),
        archive = path.join(dir, "present.project")
    await upstream.compressToZip([{ name: "data.json", content: JSON.stringify({ files: [present, dir, "http://example.test/x", "freeshow-protected://x"], project: { id: "p" }, unknown: present }) }], archive)
    const actual = await compareOracle([request("freeshow_project", [archive], importsRoot)])
    assert.equal(actual[0].media.length, 0)
    assert.deepEqual(actual[0].plan.unresolved, [])
})

test("malformed/missing show inputs and multiple text files retain original frontend outcomes", async (t) => {
    const dir = temporary(t),
        importsRoot = path.join(dir, "Imports")
    const recoverable = write(dir, "recover.show", JSON.stringify(["recovered-id", { name: "Recovered", slides: {}, layouts: {} }]) + "trailing export corruption")
    const invalid = write(dir, "invalid.show", "{not valid JSON"),
        absent = path.join(dir, "missing.show")
    const results = rust([request("freeshow", [recoverable], importsRoot), request("freeshow", [invalid, absent], importsRoot), request("txt", [write(dir, "one.txt", "[Verse]\nFirst line"), write(dir, "two.txt", "[Verse]\n敬拜 🙌")], importsRoot)])
    const recovered = frontend()
    await recovered.importShow(results[0].events[0].message.data.data)
    assert.equal(recovered.histories[0].oldData.id, "recovered-id")
    assert.equal(recovered.histories[0].newData.data.name, "Recovered")
    const bad = frontend()
    await bad.importShow(results[1].events[0].message.data.data)
    assert.equal(bad.histories[0].id, "SHOWS")
    assert.deepEqual(bad.histories[0].newData.data, [])
    const multiple = frontend()
    multiple.convertTexts(results[2].events[0].message.data.data)
    assert.equal(multiple.histories[0].id, "SHOWS")
    assert.deepEqual(
        multiple.histories[0].newData.data.map((item) => item.show.name),
        ["one", "two"]
    )
    assert.equal(multiple.converted.length, 2)
})
