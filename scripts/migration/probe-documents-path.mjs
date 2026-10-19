// Actual packaged Electron and native GLib path authority, in private XDG profiles.
import assert from "node:assert/strict"
import { createHash } from "node:crypto"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { _electron as electron } from "playwright"
import { root } from "./oracle-path.mjs"
assert.equal(process.platform, "linux")
const executablePath = process.env.FREESHOW_ELECTRON_BIN
assert.ok(executablePath && fs.statSync(executablePath).isFile())
const nativePath = process.env.FREESHOW_DOCUMENTS_PATH_BIN
assert.ok(nativePath && fs.statSync(nativePath).isFile())
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-documents-path-"))
const receipt = { kind: "actual-electron-native-documents-paths", electron: "37.10.3", homeUnchanged: true, cases: [], temporaryRemoved: false }
receipt.upstreamRevision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
receipt.packagedBinarySha256 = createHash("sha256").update(fs.readFileSync(executablePath)).digest("hex")
assert.equal(receipt.packagedBinarySha256, "eb3263aad96ce7c35df3db4516bb04c67bdc6c2f30ae8ebc0cb5649783c82a15")
receipt.nativeResolverSha256 = createHash("sha256").update(fs.readFileSync(nativePath)).digest("hex")
try {
    for (const name of ["missing", "configured", "home-disabled"]) {
        const profile = path.join(temporary, name),
            appData = path.join(profile, "app"),
            data = path.join(profile, "data"),
            config = path.join(profile, "config")
        for (const folder of [appData, path.join(data, "Config"), path.join(data, "Bibles"), config]) fs.mkdirSync(folder, { recursive: true })
        let configuration = null
        if (name === "configured") configuration = `XDG_DOCUMENTS_DIR="${path.join(profile, "Documents 敬拜")}////"\n`
        if (name === "home-disabled") configuration = 'XDG_DOCUMENTS_DIR="$HOME/"\n'
        if (configuration !== null) fs.writeFileSync(path.join(config, "user-dirs.dirs"), configuration)
        fs.writeFileSync(path.join(appData, "config.json"), JSON.stringify({ dataPath: data, maximized: false }))
        fs.writeFileSync(path.join(appData, "settings.json"), JSON.stringify({ initialized: true, language: "en", autoUpdates: false, disabledServers: { remote: true, stage: true, controller: true, output_stream: true }, serverData: { companion: { enabled: false } } }))
        fs.writeFileSync(path.join(appData, "settingsSynced.json"), JSON.stringify({ scriptures: {} }))
        const env = { ...process.env, NODE_ENV: "production", FS_MOCK_STORE_PATH: appData, XDG_CONFIG_HOME: config, XDG_CACHE_HOME: path.join(profile, "cache"), XDG_DATA_HOME: path.join(profile, "local-data"), LANG: "C.UTF-8", LC_ALL: "C.UTF-8" }
        const native = spawnSync("/usr/bin/python3", ["-c", "import json;from gi.repository import GLib;print(json.dumps({'special':GLib.get_user_special_dir(GLib.UserDirectory.DIRECTORY_DOCUMENTS),'home':GLib.get_home_dir()}))"], { env, encoding: "utf8", timeout: 10000 })
        assert.equal(native.status, 0, native.stderr)
        const glib = JSON.parse(native.stdout)
        const rust = spawnSync(nativePath, [], { env, encoding: "utf8", timeout: 10000 })
        assert.equal(rust.status, 0, rust.stderr)
        const nativeDocuments = JSON.parse(rust.stdout).documents
        let app
        try {
            app = await electron.launch({ executablePath, args: ["--no-sandbox", "--disable-gpu"], env, timeout: 45000 })
            const original = await app.evaluate(({ app }) => ({ documents: app.getPath("documents"), home: app.getPath("home"), electron: process.versions.electron, packaged: app.isPackaged }))
            assert.equal(original.electron, "37.10.3")
            assert.equal(original.packaged, true)
            const proposed = path.join(nativeDocuments, ".")
            receipt.cases.push({ name, configuration, original, glib, nativeDocuments, proposed, match: proposed === original.documents })
        } finally {
            if (app) {
                await app.evaluate(({ app }) => app.exit(0)).catch(() => {})
                await app.close().catch(() => {})
            }
        }
    }
} finally {
    fs.rmSync(temporary, { recursive: true, force: true })
    receipt.temporaryRemoved = !fs.existsSync(temporary)
}
const artifacts = path.join(root, "test-output-rust/documents-path")
fs.mkdirSync(artifacts, { recursive: true })
fs.writeFileSync(path.join(artifacts, "receipt.json"), JSON.stringify(receipt, null, 2) + "\n")
process.stdout.write(JSON.stringify(receipt) + "\n")
assert.ok(
    receipt.cases.every((c) => c.match),
    "Native Documents path authority differs from packaged Electron"
)
