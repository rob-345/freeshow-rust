// Test-only packaged Electron IPC oracle. Uses an isolated app/XDG/library profile.
import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { createRequire } from "node:module"
const require = createRequire(import.meta.url)
const { _electron: electron } = require("playwright")
const executablePath = process.env.FREESHOW_ELECTRON_BIN
assert.ok(executablePath && fs.statSync(executablePath).isFile(), "Set FREESHOW_ELECTRON_BIN to the untouched packaged oracle")
const cases = JSON.parse(fs.readFileSync(0, "utf8"))
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-browse-electron-"))
const appData = path.join(temporary, "app"),
    data = path.join(temporary, "data"),
    configRoot = path.join(temporary, "config")
const env = { ...process.env, NODE_ENV: "production", FS_MOCK_STORE_PATH: appData, XDG_CONFIG_HOME: configRoot, XDG_CACHE_HOME: path.join(temporary, "cache"), XDG_DATA_HOME: path.join(temporary, "xdg-data"), LANG: "C.UTF-8", LC_ALL: "C.UTF-8" }
for (const folder of [appData, path.join(data, "Config"), configRoot, env.XDG_CACHE_HOME, env.XDG_DATA_HOME]) fs.mkdirSync(folder, { recursive: true })
const desktopPaths = {}
for (const name of ["DESKTOP", "DOWNLOAD", "DOCUMENTS", "MUSIC", "PICTURES", "VIDEOS", "TEMPLATES", "PUBLICSHARE"]) {
    const folder = path.join(temporary, name.toLowerCase())
    fs.mkdirSync(folder)
    desktopPaths[name] = folder
}
fs.writeFileSync(
    path.join(configRoot, "user-dirs.dirs"),
    Object.entries(desktopPaths)
        .map(([name, folder]) => `XDG_${name}_DIR="${folder}"`)
        .join("\n") + "\n"
)
fs.writeFileSync(path.join(appData, "config.json"), JSON.stringify({ dataPath: data, maximized: false, bounds: { width: 1000, height: 700, x: 0, y: 0 } }))
fs.writeFileSync(path.join(appData, "settings.json"), JSON.stringify({ initialized: true, language: "en", autoUpdates: false, special: { showClosePopup: false } }))
const receipt = { executablePath, isolated: true, temporaryRemoved: false, results: [] }
let app
try {
    app = await electron.launch({ executablePath, args: ["--no-sandbox", "--disable-gpu"], env, timeout: 45000 })
    receipt.runtime = await app.evaluate(({ app }) => ({ version: app.getVersion(), packaged: app.isPackaged, versions: process.versions, paths: Object.fromEntries(["userData", "appData", "cache", "documents", "downloads"].map((name) => [name, app.getPath(name)])) }))
    assert.equal(receipt.runtime.version, "1.6.7-beta.2")
    assert.equal(receipt.runtime.packaged, true)
    assert.equal(receipt.runtime.versions.electron, "37.10.3")
    assert.equal(receipt.runtime.versions.node, "22.21.1")
    for (const location of Object.values(receipt.runtime.paths)) assert.ok(location.startsWith(temporary + path.sep), `Non-private profile path: ${location}`)
    let main
    for (let index = 0; index < 300; index++) {
        main = app.windows().find((window) => window.url().includes("index.html"))
        if (main) break
        await new Promise((resolve) => setTimeout(resolve, 100))
    }
    assert.ok(main)
    await main.locator(".top").first().waitFor({ state: "visible", timeout: 30000 })
    for (const [index, request] of cases.entries()) {
        const result = await main.evaluate(
            ({ index, request }) =>
                new Promise((resolve, reject) => {
                    const id = `browse-oracle-${index}`
                    const timeout = setTimeout(() => {
                        window.api.removeListener("MAIN", id)
                        reject(new Error("Packaged browse IPC timeout"))
                    }, 15000)
                    window.api.receive(
                        "MAIN",
                        (message, listenerId) => {
                            if (listenerId !== id || message.channel !== request.command) return
                            clearTimeout(timeout)
                            window.api.removeListener("MAIN", id)
                            const stats =
                                request.command === "FILE_INFO"
                                    ? [message.data?.stat].filter(Boolean)
                                    : Object.values(message.data || {})
                                          .filter((entry) => entry?.stats)
                                          .map((entry) => entry.stats)
                            const statShapes = stats.map((stat) => ({ keys: Object.keys(stat), prototype: Object.getPrototypeOf(stat) === Object.prototype ? "Object" : "other", dates: ["atime", "mtime", "ctime", "birthtime"].map((key) => ({ key, own: Object.hasOwn(stat, key), type: typeof stat[key], date: stat[key] instanceof Date })), methods: typeof stat.isFile }))
                            const folderShapes =
                                request.command === "READ_FOLDER"
                                    ? Object.fromEntries(
                                          Object.entries(message.data)
                                              .filter(([, entry]) => entry.isFolder)
                                              .map(([key, entry]) => [key, { ownNoMedia: Object.hasOwn(entry, "noMedia"), noMediaType: typeof entry.noMedia }])
                                      )
                                    : {}
                            resolve({ data: message.data, statShapes, folderShapes })
                        },
                        id
                    )
                    window.api.send("MAIN", { channel: request.command, data: request.data }, id)
                }),
            { index, request }
        )
        receipt.results.push(result)
    }
} finally {
    if (app) {
        await app.evaluate(({ app }) => app.exit(0)).catch(() => {})
        await app.close().catch(() => {})
    }
    fs.rmSync(temporary, { recursive: true, force: true })
    receipt.temporaryRemoved = !fs.existsSync(temporary)
}
process.stdout.write(JSON.stringify(receipt) + "\n")
