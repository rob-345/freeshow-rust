// Launch the untouched official application with a disposable Linux profile.
import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { _electron as electron } from "playwright"

export async function withElectron(callback) {
    assert.equal(process.platform, "linux", "Packaged oracle isolation currently requires Linux")
    const executablePath = process.env.FREESHOW_ELECTRON_BIN || path.join(os.homedir(), ".local/share/freeshow-electron-baseline/v1.6.7-beta.2/extracted/opt/FreeShow/freeshow")
    assert.ok(fs.statSync(executablePath).isFile())
    const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-files-oracle-"))
    const appData = path.join(temporary, "app")
    const data = path.join(temporary, "data")
    const configRoot = path.join(temporary, "xdg-config")
    const env = { ...process.env, NODE_ENV: "production", FS_MOCK_STORE_PATH: appData, XDG_CONFIG_HOME: configRoot, XDG_CACHE_HOME: path.join(temporary, "xdg-cache"), XDG_DATA_HOME: path.join(temporary, "xdg-data"), LANG: "C.UTF-8", LC_ALL: "C.UTF-8" }
    for (const folder of [appData, path.join(data, "Config"), configRoot, env.XDG_CACHE_HOME, env.XDG_DATA_HOME]) fs.mkdirSync(folder, { recursive: true })
    const names = ["DESKTOP", "DOWNLOAD", "DOCUMENTS", "MUSIC", "PICTURES", "VIDEOS", "TEMPLATES", "PUBLICSHARE"]
    fs.writeFileSync(
        path.join(configRoot, "user-dirs.dirs"),
        names
            .map((name) => {
                const folder = path.join(temporary, name.toLowerCase())
                fs.mkdirSync(folder)
                return `XDG_${name}_DIR="${folder}"`
            })
            .join("\n") + "\n"
    )
    fs.writeFileSync(path.join(appData, "config.json"), JSON.stringify({ dataPath: data, maximized: false, bounds: { width: 1280, height: 800, x: 0, y: 0 } }))
    fs.writeFileSync(path.join(appData, "settings.json"), JSON.stringify({ initialized: true, language: "en", autoUpdates: false, special: { showClosePopup: false } }))
    let app
    try {
        app = await electron.launch({ executablePath, args: ["--no-sandbox", "--disable-gpu"], env, timeout: 45000 })
        const runtime = await app.evaluate(({ app }) => ({ version: app.getVersion(), packaged: app.isPackaged, versions: process.versions, paths: Object.fromEntries(["userData", "appData", "cache", "documents", "downloads"].map((name) => [name, app.getPath(name)])) }))
        assert.equal(runtime.version, "1.6.7-beta.2")
        assert.equal(runtime.packaged, true)
        assert.equal(runtime.versions.electron, "37.10.3")
        assert.equal(runtime.versions.node, "22.21.1")
        assert.equal(runtime.versions.icu, "74.2")
        assert.equal(runtime.versions.unicode, "15.1")
        for (const location of Object.values(runtime.paths)) assert.ok(location.startsWith(temporary + path.sep), `Profile path is not isolated: ${location}`)
        return await callback(app, runtime, executablePath)
    } finally {
        if (app) {
            await app.evaluate(({ app }) => app.exit(0)).catch(() => {})
            await app.close().catch(() => {})
        }
        fs.rmSync(temporary, { recursive: true, force: true })
    }
}
