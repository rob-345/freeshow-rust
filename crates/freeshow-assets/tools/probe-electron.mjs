import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { createHash } from "node:crypto"
import { fileURLToPath } from "node:url"
import { Host, fixtures, request } from "./host.mjs"
import { withElectron } from "./electron.mjs"

const root = fileURLToPath(new URL("../../../", import.meta.url))
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex")
const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-assets-reference-"))
const host = await new Host().start({ origins: ["null", "file://"] })
try {
    const files = fixtures(directory)
    const grants = Object.fromEntries(await Promise.all(Object.entries(files).map(async ([name, file]) => [name, await host.grant(file)])))
    const receipt = await withElectron(async (app, runtime, executablePath) => {
        const samples = []
        const mimeSamples = []
        for (const extension of ["mp3", "wav", "ogg", "oga", "opus", "m4a", "aac", "flac", "aif", "aiff", "mid", "midi", "svg", "bmp", "avif", "pdf", "weba", "wma", "tif", "tiff", "jfif", "heic", "wmv"]) {
            const file = path.join(directory, "mime." + extension)
            fs.writeFileSync(file, "extension MIME probe")
            const reference = await app.evaluate(async ({ net }, file) => (await net.fetch(process.getBuiltinModule("url").pathToFileURL(file).href)).headers.get("content-type"), file)
            const grant = await host.grant(file)
            const native = (await request(grant.url, { method: "HEAD" })).headers["content-type"]
            assert.equal(native, reference, "Actual packaged file MIME: " + extension)
            mimeSamples.push({ extension, reference, native })
        }
        for (const [name, file] of Object.entries(files)) {
            for (const range of [undefined, "bytes=0-0", "bytes=-5", "bytes=999999-"]) {
                const reference = await app.evaluate(
                    async ({ net }, { file, range }) => {
                        try {
                            const response = await net.fetch(process.getBuiltinModule("url").pathToFileURL(file).href, { headers: range ? { Range: range } : {} })
                            return {
                                status: response.status,
                                type: response.headers.get("content-type"),
                                length: response.headers.get("content-length"),
                                range: response.headers.get("content-range"),
                                hash: process
                                    .getBuiltinModule("crypto")
                                    .createHash("sha256")
                                    .update(Buffer.from(await response.arrayBuffer()))
                                    .digest("hex")
                            }
                        } catch (error) {
                            return { error: error.message }
                        }
                    },
                    { file, range }
                )
                const native = await request(grants[name].url, { headers: range ? { Range: range } : {} })
                const normalized = { status: native.status, type: native.headers["content-type"] || null, length: native.headers["content-length"] || null, range: native.headers["content-range"] || null, hash: sha(native.bytes) }
                if (!range) {
                    assert.equal(reference.status, 200)
                    assert.equal(normalized.hash, reference.hash)
                    assert.equal(normalized.type, reference.type)
                }
                if (name !== "empty" && range !== "bytes=999999-" && range) {
                    assert.equal(normalized.hash, reference.hash)
                }
                samples.push({ name, requestRange: range || null, reference, native: normalized })
            }
        }
        // The application's startup splash is transient. A disposable isolated
        // renderer uses the actual embedded Chromium without changing app files.
        const probeUrl = "data:text/html,freeshow-assets-probe"
        await app.evaluate(async ({ BrowserWindow }, url) => {
            globalThis.__freeshowAssetsProbe = new BrowserWindow({ show: false, webPreferences: { nodeIntegration: false, contextIsolation: true, sandbox: true } })
            await globalThis.__freeshowAssetsProbe.loadURL(url)
        }, probeUrl)
        const page = app.windows().find((page) => page.url() === probeUrl)
        assert.ok(page, "The packaged Chromium probe renderer exists")
        const browser = await page.evaluate(
            async ({ image, audio, video }) => {
                const wait = (element, event, action) =>
                    new Promise((resolve, reject) => {
                        const timer = setTimeout(() => reject(new Error("Browser asset probe timeout: " + event)), 10000)
                        element.addEventListener(
                            event,
                            () => {
                                clearTimeout(timer)
                                resolve()
                            },
                            { once: true }
                        )
                        element.addEventListener(
                            "error",
                            () => {
                                clearTimeout(timer)
                                reject(new Error("Browser asset error: " + (element.error?.message || element.src)))
                            },
                            { once: true }
                        )
                        action()
                    })
                const img = new Image()
                img.crossOrigin = "anonymous"
                await wait(img, "load", () => (img.src = image))
                const canvas = document.createElement("canvas")
                canvas.width = 1
                canvas.height = 1
                canvas.getContext("2d").drawImage(img, 0, 0)
                const pixel = [...canvas.getContext("2d").getImageData(0, 0, 1, 1).data]
                if (pixel.length !== 4) throw new Error("No actual decoded image pixel")
                const withoutCors = new Image()
                await wait(withoutCors, "load", () => (withoutCors.src = image))
                canvas.getContext("2d").drawImage(withoutCors, 0, 0)
                let canvasWithoutCrossOrigin = "readable"
                try {
                    canvas.toDataURL()
                } catch (error) {
                    canvasWithoutCrossOrigin = error.name
                }
                const media = document.createElement("audio")
                media.preload = "auto"
                await wait(media, "loadedmetadata", () => (media.src = audio))
                const duration = media.duration
                await wait(media, "seeked", () => (media.currentTime = 0.75))
                const currentTime = media.currentTime
                media.muted = true
                await wait(media, "timeupdate", () => media.play().catch(() => {}))
                const audioPlayed = media.currentTime > currentTime
                media.pause()
                media.removeAttribute("src")
                media.load()
                const clip = document.createElement("video")
                clip.muted = true
                clip.preload = "auto"
                await wait(clip, "loadedmetadata", () => (clip.src = video))
                const videoDuration = clip.duration
                await wait(clip, "seeked", () => (clip.currentTime = 0.5))
                const videoSeek = clip.currentTime
                await wait(clip, "timeupdate", () => clip.play().catch(() => {}))
                const videoPlayed = clip.currentTime > videoSeek
                const videoSize = { width: clip.videoWidth, height: clip.videoHeight }
                clip.pause()
                clip.removeAttribute("src")
                clip.load()
                return { origin: location.origin, video: { duration: videoDuration, currentTime: videoSeek, played: videoPlayed, ...videoSize }, image: { width: img.naturalWidth, height: img.naturalHeight, pixel }, audio: { duration, currentTime, played: audioPlayed }, canvasWithoutCrossOrigin }
            },
            { image: grants.image.url, audio: grants.audio.url, video: grants.video.url }
        )
        assert.equal(browser.image.width, 1)
        assert.equal(browser.image.height, 1)
        assert.equal(browser.video.duration, 1)
        assert.equal(browser.video.currentTime, 0.5)
        assert.equal(browser.video.width, 16)
        assert.equal(browser.video.played, true)
        assert.equal(browser.audio.played, true)
        assert.equal(browser.audio.duration, 1)
        assert.equal(browser.audio.currentTime, 0.75)
        assert.equal(browser.canvasWithoutCrossOrigin, "SecurityError")
        return { schemaVersion: 1, kind: "native-local-assets-electron-reference", sourceCommit: "2d94defcf98175e4f5c9504e7f4fd1d33c35c975", executableSha256: sha(fs.readFileSync(executablePath)), runtime: { version: runtime.version, versions: runtime.versions }, samples, mimeSamples, browser, codecScope: "One PCM WAV, one VP8 WebM and one PNG only; patterned MP4 fixture verifies bytes, not decoding" }
    })
    const artifacts = process.env.FREESHOW_ASSETS_ARTIFACTS || path.join(root, "test-output-rust/local-assets")
    fs.mkdirSync(artifacts, { recursive: true })
    fs.writeFileSync(path.join(artifacts, "electron-assets.json"), JSON.stringify(receipt, null, 2) + "\n")
    process.stdout.write(JSON.stringify({ samples: receipt.samples.length, mimeSamples: receipt.mimeSamples, browser: receipt.browser }) + "\n")
} finally {
    await host.close()
    fs.rmSync(directory, { recursive: true, force: true })
}
