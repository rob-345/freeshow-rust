// Test-only untouched packaged Electron canvas/nativeImage oracle; private profile.
const assert = require("node:assert/strict")
const fs = require("node:fs")
const os = require("node:os")
const path = require("node:path")
const { _electron: electron } = require("playwright")
;(async () => {
    const fixture = JSON.parse(fs.readFileSync(0, "utf8"))
    const executablePath = process.env.FREESHOW_ELECTRON_BIN
    assert.ok(executablePath && fs.statSync(executablePath).isFile(), "Set FREESHOW_ELECTRON_BIN to untouched official executable")
    const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-thumbnail-electron-"))
    const appData = path.join(temporary, "app"),
        data = path.join(temporary, "data"),
        config = path.join(temporary, "config")
    const env = { ...process.env, NODE_ENV: "production", FS_MOCK_STORE_PATH: appData, XDG_CONFIG_HOME: config, XDG_CACHE_HOME: path.join(temporary, "cache"), XDG_DATA_HOME: path.join(temporary, "xdg-data"), LANG: "C.UTF-8", LC_ALL: "C.UTF-8" }
    for (const dir of [appData, path.join(data, "Config"), config, env.XDG_CACHE_HOME, env.XDG_DATA_HOME]) fs.mkdirSync(dir, { recursive: true })
    const desktopPaths = {}
    for (const name of ["DESKTOP", "DOWNLOAD", "DOCUMENTS", "MUSIC", "PICTURES", "VIDEOS", "TEMPLATES", "PUBLICSHARE"]) {
        const dir = path.join(temporary, name.toLowerCase())
        fs.mkdirSync(dir)
        desktopPaths[name] = dir
    }
    fs.writeFileSync(
        path.join(config, "user-dirs.dirs"),
        Object.entries(desktopPaths)
            .map(([name, dir]) => `XDG_${name}_DIR="${dir}"`)
            .join("\n") + "\n"
    )
    fs.writeFileSync(path.join(appData, "config.json"), JSON.stringify({ dataPath: data, maximized: false, bounds: { width: 1000, height: 700, x: 0, y: 0 } }))
    fs.writeFileSync(path.join(appData, "settings.json"), JSON.stringify({ initialized: true, language: "en", autoUpdates: false, special: { showClosePopup: false } }))
    let app
    const result = { privateProfile: true, temporaryRemoved: false, pngPairs: [], replies: [] }
    try {
        app = await electron.launch({ executablePath, args: ["--no-sandbox", "--disable-gpu"], env, timeout: 45000 })
        result.runtime = await app.evaluate(({ app }) => ({ version: app.getVersion(), packaged: app.isPackaged, versions: process.versions, paths: Object.fromEntries(["userData", "appData", "cache", "documents", "downloads"].map((key) => [key, app.getPath(key)])) }))
        assert.equal(result.runtime.version, "1.6.7-beta.2")
        assert.equal(result.runtime.versions.node, "22.21.1")
        for (const value of Object.values(result.runtime.paths)) assert.ok(value.startsWith(temporary + path.sep))
        let main
        for (let index = 0; index < 300; index++) {
            main = app.windows().find((page) => page.url().includes("index.html"))
            if (main) break
            await new Promise((resolve) => setTimeout(resolve, 100))
        }
        assert.ok(main)
        await main.locator(".top").first().waitFor({ state: "visible", timeout: 30000 })
        for (const mode of ["alpha", "opaque", "p3"]) {
            const canvas = await main.evaluate(async (mode) => {
                const canvas = document.createElement("canvas")
                canvas.width = 256
                canvas.height = 8
                const ctx = canvas.getContext("2d", { colorSpace: mode === "p3" ? "display-p3" : "srgb" })
                const pixels = ctx.createImageData(canvas.width, canvas.height)
                for (let i = 0; i < pixels.data.length; i += 4) {
                    pixels.data[i] = (i * 13 + 17) % 256
                    pixels.data[i + 1] = (i * 7 + 103) % 256
                    pixels.data[i + 2] = (i * 3 + 251) % 256
                    pixels.data[i + 3] = mode === "alpha" ? (i / 4) % 256 : 255
                }
                ctx.putImageData(pixels, 0, 0)
                const blob = await new Promise((resolve) => canvas.toBlob(resolve, "image/png"))
                return btoa(String.fromCharCode(...new Uint8Array(await blob.arrayBuffer())))
            }, mode)
            const native = await app.evaluate(({ nativeImage }, canvas) => nativeImage.createFromBuffer(Buffer.from(canvas, "base64")).toPNG().toString("base64"), canvas)
            result.pngPairs.push({ mode, canvas, native })
        }
        for (const [index, request] of (fixture.requests || []).entries()) {
            const reply = await main.evaluate(
                ({ index, request }) =>
                    new Promise((resolve, reject) => {
                        const id = `thumbnail-oracle-${index}`
                        const timer = setTimeout(() => {
                            window.api.removeListener("MAIN", id)
                            reject(new Error("Thumbnail IPC timeout"))
                        }, 25000)
                        window.api.receive(
                            "MAIN",
                            (message, listenerId) => {
                                if (listenerId !== id || message.channel !== request.command) return
                                clearTimeout(timer)
                                window.api.removeListener("MAIN", id)
                                resolve(message.data)
                            },
                            id
                        )
                        window.api.send("MAIN", { channel: request.command, data: request.data }, id)
                    }),
                { index, request }
            )
            let png = null
            if (reply?.output && fs.existsSync(reply.output)) png = fs.readFileSync(reply.output).toString("base64")
            result.replies.push({ reply, png })
        }
    } finally {
        if (app) {
            await app.evaluate(({ app }) => app.exit(0)).catch(() => {})
            await app.close().catch(() => {})
        }
        fs.rmSync(temporary, { recursive: true, force: true })
        result.temporaryRemoved = !fs.existsSync(temporary)
    }
    process.stdout.write(JSON.stringify(result) + "\n")
})().catch((error) => {
    console.error(error)
    process.exitCode = 1
})
