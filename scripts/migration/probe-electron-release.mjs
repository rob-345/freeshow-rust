// Check the untouched packaged oracle before measuring comparable workloads.
// This probe records readiness and provenance, never a performance comparison.
import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { _electron as electron } from "playwright"
import { root } from "./oracle-path.mjs"

assert.equal(process.platform, "linux", "This profile-isolation probe is Linux-specific")
const executablePath = process.env.FREESHOW_ELECTRON_BIN
assert.ok(executablePath && fs.statSync(executablePath).isFile(), "Set FREESHOW_ELECTRON_BIN to the extracted official executable")
const artifacts = process.env.FREESHOW_BASELINE_ARTIFACTS || path.join(root, "test-output-rust/electron-release")
fs.mkdirSync(artifacts, { recursive: true })
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-electron-oracle-"))
const appData = path.join(temporary, "app")
const data = path.join(temporary, "data")
const configRoot = path.join(temporary, "xdg-config")
const env = {
    ...process.env,
    NODE_ENV: "production",
    FS_MOCK_STORE_PATH: appData,
    XDG_CONFIG_HOME: configRoot,
    XDG_CACHE_HOME: path.join(temporary, "xdg-cache"),
    XDG_DATA_HOME: path.join(temporary, "xdg-data"),
    LANG: "C.UTF-8",
    LC_ALL: "C.UTF-8"
}
for (const folder of [appData, path.join(data, "Config"), configRoot, env.XDG_CACHE_HOME, env.XDG_DATA_HOME]) fs.mkdirSync(folder, { recursive: true })
const desktopPaths = {}
for (const name of ["DESKTOP", "DOWNLOAD", "DOCUMENTS", "MUSIC", "PICTURES", "VIDEOS", "TEMPLATES", "PUBLICSHARE"]) {
    const directory = path.join(temporary, name.toLowerCase())
    fs.mkdirSync(directory)
    desktopPaths[name] = directory
}
fs.writeFileSync(
    path.join(configRoot, "user-dirs.dirs"),
    Object.entries(desktopPaths)
        .map(([name, directory]) => `XDG_${name}_DIR="${directory}"`)
        .join("\n") + "\n"
)
fs.writeFileSync(path.join(appData, "config.json"), JSON.stringify({ dataPath: data, maximized: false, bounds: { width: 1280, height: 800, x: 0, y: 0 } }))
fs.writeFileSync(path.join(appData, "settings.json"), JSON.stringify({ initialized: true, language: "en", autoUpdates: false, special: { showClosePopup: false } }))

let app
const logs = []
const receipt = { schemaVersion: 1, kind: "official-electron-release-readiness", measurements: null, temporaryProfile: temporary, executablePath, probeRuntime: { node: process.versions.node, icu: process.versions.icu, unicode: process.versions.unicode } }
try {
    app = await electron.launch({ executablePath, args: ["--no-sandbox", "--disable-gpu"], env, timeout: 45000 })
    app.on("console", (message) => {
        if (logs.length < 100) logs.push({ type: message.type(), text: message.text().slice(0, 4096) })
    })
    receipt.runtime = await app.evaluate(({ app }) => ({
        version: app.getVersion(),
        packaged: app.isPackaged,
        versions: process.versions,
        paths: Object.fromEntries(["userData", "appData", "cache", "documents", "downloads"].map((name) => [name, app.getPath(name)]))
    }))
    assert.equal(receipt.runtime.version, "1.6.7-beta.2")
    assert.equal(receipt.runtime.packaged, true)
    assert.equal(receipt.runtime.versions.electron, "37.10.3")
    assert.equal(receipt.runtime.versions.node, "22.21.1")
    for (const location of Object.values(receipt.runtime.paths)) assert.ok(location.startsWith(temporary + path.sep), `Profile path is not isolated: ${location}`)
    const deadline = Date.now() + 30000
    let main
    while (Date.now() < deadline) {
        main = app.windows().find((window) => window.url().includes("index.html"))
        if (main) break
        await new Promise((resolve) => setTimeout(resolve, 100))
    }
    assert.ok(main, "The packaged main window did not load")
    await main.locator(".top").first().waitFor({ state: "visible", timeout: 30000 })
    receipt.renderer = await main.evaluate(() => ({ title: document.title, url: location.href, preloadAvailable: typeof window.api?.send === "function", topVisible: !!document.querySelector(".top"), bodyText: document.body.innerText.slice(0, 512) }))
    assert.equal(receipt.renderer.preloadAvailable, true)
    assert.equal(JSON.parse(fs.readFileSync(path.join(appData, "config.json"))).dataPath, data)
    await main.screenshot({ path: path.join(artifacts, "ready.png") })
    receipt.ready = true
} catch (error) {
    receipt.ready = false
    receipt.error = String(error)
    throw error
} finally {
    if (app) {
        await app.evaluate(({ app }) => app.exit(0)).catch(() => {})
        await app.close().catch(() => {})
    }
    fs.writeFileSync(path.join(artifacts, "receipt.json"), JSON.stringify(receipt, null, 2) + "\n")
    fs.writeFileSync(path.join(artifacts, "console.json"), JSON.stringify(logs, null, 2) + "\n")
    fs.rmSync(temporary, { recursive: true, force: true })
    receipt.temporaryProfileRemoved = !fs.existsSync(temporary)
    fs.writeFileSync(path.join(artifacts, "receipt.json"), JSON.stringify(receipt, null, 2) + "\n")
}
process.stdout.write(JSON.stringify({ ready: receipt.ready, version: receipt.runtime?.version, measurements: receipt.measurements }) + "\n")
