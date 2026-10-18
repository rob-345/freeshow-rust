import assert from "node:assert/strict"
import { test } from "node:test"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import yauzl from "yauzl"
import yazl from "yazl"
import { rust, request } from "./runner.mjs"
import { upstreamFunctions, storeDefinitions, fixtureShow, createSaveOracle, clean } from "./oracle.mjs"

const zipOracle = upstreamFunctions(
    "src/electron/data/zip.ts",
    ["compressToZip", "decompressZipStream", "processEntry", "bufferInMemory", "streamToDisk", "sanitizeZipPath"],
    {
        fs,
        path,
        yauzl,
        yazl,
        Buffer,
        ToMain: { ALERT: "ALERT" },
        sendToMain() {},
        getExtension: (name) => path.extname(name).slice(1).toLowerCase(),
        createFolder: (directory) => fs.mkdirSync(directory, { recursive: true })
    },
    "const STRING_CONVERT_LIMIT = 50 * 1024 * 1024;"
)

function profile(t, prefix = "freeshow-backup-") {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), prefix))
    t.after(() => fs.rmSync(temp, { recursive: true, force: true }))
    return { temp, app: path.join(temp, "app"), data: path.join(temp, "data"), args: ["--app-data", path.join(temp, "app"), "--data-dir", path.join(temp, "data")] }
}

const events = (response, channel) => {
    assert.equal(response.ok, true, JSON.stringify(response.error))
    return response.value.filter((event) => event.message.channel === channel).map((event) => event.message.data)
}

async function archiveContents(file) {
    const entries = await zipOracle.decompressZipStream(file)
    return Object.fromEntries(entries.map((entry) => [entry.name, JSON.parse(entry.content)]))
}

test("Rust manual and automatic backups are readable by the actual Electron ZIP reader and match its backup contents", async (t) => {
    const p = profile(t)
    const payload = { SETTINGS: { initialized: true, language: "en", special: {} }, SYNCED_SETTINGS: { categories: { song: { name: "Songs" } }, customFutureSetting: true }, PROJECTS: { projects: { sunday: { name: "Sunday", shows: [{ id: "song" }] } }, folders: {} }, OVERLAYS: { o1: { name: "Overlay" } }, showsCache: { song: { ...fixtureShow(7), name: "Grace 日本語 🙌" } }, customTriggers: {} }
    const responses = rust([request("SAVE", payload), request("SAVE", { ...payload, customTriggers: { backup: true } }), request("SAVE", { SETTINGS: payload.SETTINGS, showsCache: { unsaved: { name: "Do not save this on auto-backup" } }, customTriggers: { backup: true, isAutoBackup: true } }), request("BACKUPS")], p.args)
    const manual = events(responses[1], "BACKUP")[0]
    const automatic = events(responses[2], "BACKUP")[0]
    assert.equal(manual.finished, true)
    assert.equal(automatic.finished, true)
    assert.ok(automatic.path.endsWith("_auto.zip"))
    assert.equal(fs.existsSync(path.join(p.data, "Shows/Do not save this on auto-backup.show")), false)

    const oracleData = path.join(p.temp, "oracle-data")
    const oracle = createSaveOracle(path.join(p.temp, "oracle-app"), oracleData)
    await oracle.save(clean(payload))
    const { startBackup } = upstreamFunctions("src/electron/data/backup.ts", ["startBackup"], {
        fs,
        path,
        _store: oracle.stores,
        storeFilesData: storeDefinitions,
        getTimePointString: () => "reference",
        getDataFolderPath: (id) => {
            const folder = path.join(oracleData, { shows: "Shows", backups: "Backups", scriptures: "Bibles" }[id])
            fs.mkdirSync(folder, { recursive: true })
            return folder
        },
        readFolder: fs.readdirSync,
        compressToZip: zipOracle.compressToZip,
        ToMain: { BACKUP: "BACKUP", ALERT: "ALERT" },
        sendToMain() {},
        pruneAutoBackups() {},
        openInSystem() {}
    })
    await startBackup()
    const original = await archiveContents(path.join(oracleData, "Backups/reference.zip"))
    assert.deepEqual(await archiveContents(manual.path), original)
    assert.deepEqual(await archiveContents(automatic.path), original)
    assert.ok(Object.hasOwn(original, "SYNCED_SETTINGS.json"))
    assert.ok(!Object.hasOwn(original, "ACCESS.json"))
    assert.ok(!Object.hasOwn(original, "DRIVE_API_KEY.json"))
    assert.equal(events(responses[3], "BACKUPS")[0].length, 2)
})

test("restoring an Electron ZIP keeps synced and local settings separate and preserves show contents", async (t) => {
    const p = profile(t)
    const show = ["restored-id", { ...fixtureShow(2), name: "Restored 日本語" }]
    const input = path.join(p.temp, "electron.zip")
    await zipOracle.compressToZip(
        [
            { name: "SETTINGS.json", content: JSON.stringify({ language: "de", dataPath: "/old-device", showsPath: "/old-shows" }) },
            { name: "SYNCED_SETTINGS.json", content: JSON.stringify({ categories: { restored: { name: "Restored" } }, customFutureField: [1, true] }) },
            { name: "PROJECTS.json", content: JSON.stringify({ projects: { restored: { name: "Sunday" } }, folders: {} }) },
            { name: "SHOWS/Restored 日本語.show", content: JSON.stringify(show) }
        ],
        input
    )
    const results = rust([request("RESTORE", { path: input }), request("SETTINGS"), request("SYNCED_SETTINGS"), request("SHOW", { id: show[0], name: show[1].name })], p.args)
    assert.deepEqual(events(results[0], "RESTORE2"), [{ starting: true }, { finished: true }])
    assert.deepEqual(events(results[1], "SETTINGS")[0], { language: "de" })
    assert.deepEqual(events(results[2], "SYNCED_SETTINGS")[0], { categories: { restored: { name: "Restored" } }, customFutureField: [1, true] })
    assert.deepEqual(events(results[3], "SHOW")[0].content, show)
    const restarted = rust([request("SETTINGS"), request("SYNCED_SETTINGS")], p.args)
    assert.equal(events(restarted[0], "SETTINGS")[0].language, "de")
    assert.deepEqual(events(restarted[1], "SYNCED_SETTINGS")[0].categories, { restored: { name: "Restored" } })
})

test("legacy JSON backups and SHOWS_CONTENT restore through the same runtime", (t) => {
    const p = profile(t)
    const legacy = path.join(p.temp, "legacy")
    fs.mkdirSync(legacy)
    fs.writeFileSync(path.join(legacy, "SETTINGS.json"), JSON.stringify({ language: "ja", dataPath: "old" }))
    fs.writeFileSync(path.join(legacy, "SHOWS_CONTENT.json"), JSON.stringify({ song: { name: "Legacy", slides: {} }, missingName: { slides: {} } }))
    const results = rust([request("RESTORE", { path: legacy }), request("SHOW", { id: "song", name: "Legacy" }), request("SHOW", { id: "missingName" })], p.args)
    assert.equal(events(results[0], "RESTORE2").at(-1).finished, true)
    assert.equal(events(results[1], "SHOW")[0].content[1].name, "Legacy")
    assert.equal(events(results[2], "SHOW")[0].content[0], "missingName")
})

test("invalid JSON and corrupt archives fail without partially replacing existing settings", async (t) => {
    const p = profile(t)
    const invalid = path.join(p.temp, "invalid.zip")
    const corrupt = path.join(p.temp, "corrupt.zip")
    await zipOracle.compressToZip(
        [
            { name: "SETTINGS.json", content: '{"language":"changed"}' },
            { name: "PROJECTS.json", content: "{invalid" }
        ],
        invalid
    )
    fs.writeFileSync(corrupt, "not a zip")
    const results = rust([request("SAVE", { SETTINGS: { language: "original" } }), request("RESTORE", { path: invalid }), request("RESTORE", { path: corrupt }), request("SETTINGS")], p.args)
    for (const index of [1, 2]) {
        assert.deepEqual(events(results[index], "RESTORE2"), [{ finished: false }])
        assert.equal(events(results[index], "ALERT").length, 1)
    }
    assert.deepEqual(events(results[3], "SETTINGS")[0], { language: "original" })
})

test("traversal names and oversized ZIP headers are rejected before any restore write", async (t) => {
    const p = profile(t)
    const base = path.join(p.temp, "base.zip")
    const malicious = "SHOWS/../../<img>.show"
    const safe = "x".repeat(malicious.length - 5) + ".show"
    await zipOracle.compressToZip(
        [
            { name: safe, content: '["id",{"name":"Escape"}]' },
            { name: "SETTINGS.json", content: '{"language":"changed"}' }
        ],
        base
    )
    const traversal = path.join(p.temp, "traversal.zip")
    const bytes = fs.readFileSync(base)
    let offset = bytes.indexOf(Buffer.from(safe)),
        replacements = 0
    while (offset !== -1) {
        Buffer.from(malicious).copy(bytes, offset)
        replacements++
        offset = bytes.indexOf(Buffer.from(safe), offset + safe.length)
    }
    assert.equal(replacements, 2)
    fs.writeFileSync(traversal, bytes)
    const oversized = path.join(p.temp, "oversized.zip")
    const header = fs.readFileSync(base)
    const central = header.indexOf(Buffer.from([0x50, 0x4b, 0x01, 0x02]))
    assert.notEqual(central, -1)
    header.writeUInt32LE(100 * 1024 * 1024, central + 24)
    fs.writeFileSync(oversized, header)
    const results = rust([request("SAVE", { SETTINGS: { language: "original" } }), request("RESTORE", { path: traversal }), request("RESTORE", { path: oversized }), request("SETTINGS")], p.args)
    for (const index of [1, 2]) assert.deepEqual(events(results[index], "RESTORE2"), [{ finished: false }])
    assert.ok(events(results[1], "ALERT")[0].includes("&lt;img&gt;"))
    assert.ok(!events(results[1], "ALERT")[0].includes("<img>"))
    assert.deepEqual(events(results[3], "SETTINGS")[0], { language: "original" })
    assert.equal(fs.existsSync(path.join(p.data, "evil.show")), false)
    assert.equal(fs.existsSync(path.join(p.temp, "evil.show")), false)
})

test("backup deletion is confined to the selected backup directory and supports legacy folders", (t) => {
    const p = profile(t)
    fs.mkdirSync(path.join(p.data, "Backups/legacy"), { recursive: true })
    fs.writeFileSync(path.join(p.data, "Backups/legacy/SETTINGS.json"), "{}")
    const outside = path.join(p.temp, "outside.json")
    fs.writeFileSync(outside, "preserve")
    const results = rust([request("BACKUPS"), request("DELETE_BACKUP", { path: outside }), request("DELETE_BACKUP", { path: "legacy" }), request("BACKUPS")], p.args)
    assert.equal(events(results[0], "BACKUPS")[0][0].name, "legacy")
    assert.equal(results[1].error.code, "invalid_request")
    assert.equal(fs.readFileSync(outside, "utf8"), "preserve")
    assert.deepEqual(events(results[3], "BACKUPS")[0], [])
})

test("automatic backup retention matches the original pruning algorithm across count/age boundaries", () => {
    const now = Date.UTC(2027, 7, 31, 12)
    class FixedDate extends Date {
        constructor(...args) {
            super(...(args.length ? args : [now]))
        }
    }
    const cutoff = new Date(now)
    cutoff.setMonth(cutoff.getMonth() - 5)
    const cases = [],
        expected = []
    for (let count = 0; count <= 35; count++) {
        const backups = Array.from({ length: count }, (_, index) => ({ path: `backup-${index}.zip`, name: index % 9 === 0 ? `manual-${index}` : `backup-${index}_auto`, date: Date.UTC(2025, index % 24, index + 1), size: 123 }))
        for (const ignoreFloor of [false, true]) {
            const removed = []
            const { pruneAutoBackups } = upstreamFunctions("src/electron/data/backup.ts", ["pruneAutoBackups"], { getBackups: () => clean(backups), removeBackup: (path) => removed.push(path), Date: FixedDate })
            pruneAutoBackups(ignoreFloor)
            expected.push(removed)
            cases.push({ op: "backup.retention", backups, cutoffMs: cutoff.getTime(), ignoreFloor })
        }
    }
    const [result] = rust([{ op: "batch", cases }])
    assert.equal(result.ok, true)
    assert.deepEqual(result.value, expected)
})
