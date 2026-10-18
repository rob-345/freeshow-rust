// Run under xvfb-run -a. This launches the untouched packaged app and sends the
// actual source handler result through Electron IPC + a contextBridge preload.
import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { createHash } from "node:crypto"
import { createRequire } from "node:module"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import ts from "typescript"
import { withElectron } from "./electron.mjs"
import { corpus, generatedCorpus } from "./fixtures.mjs"
import { wire } from "./wire.mjs"
const root = fileURLToPath(new URL("../../../", import.meta.url))
const require = createRequire(import.meta.url)
const binary = process.env.FREESHOW_EXIF_PARITY || path.join(root, "target/debug/freeshow-exif-parity" + (process.platform === "win32" ? ".exe" : ""))
const sourceFile = path.join(root, "src/electron/utils/files.ts")
const source = fs.readFileSync(sourceFile, "utf8")
const ast = ts.createSourceFile(sourceFile, source, ts.ScriptTarget.Latest, true)
const declaration = ast.statements
    .find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "readExifData")
    .getText(ast)
    .replace(/^export\s+/, "")
const javascript = ts.transpileModule(declaration, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
const folder = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-exif-wire-日本語-"))
const hash = (data) => createHash("sha256").update(data).digest("hex")
try {
    const ids = [...corpus(), ...generatedCorpus()].map(([name, data]) => {
        const file = path.join(folder, name)
        fs.writeFileSync(file, data)
        return file
    })
    ids.push(path.join(folder, "missing.jpg"))
    const preload = path.join(folder, "preload.cjs")
    fs.writeFileSync(preload, 'const {ipcRenderer,contextBridge}=require("electron"); const promise=new Promise(resolve=>ipcRenderer.once("exif-oracle-clone",(_event,data)=>resolve(data))); contextBridge.exposeInMainWorld("exifProbe",{read:()=>promise});')
    const receipt = await withElectron(async (app, runtime, executable) => {
        const results = await app.evaluate(
            async (_electron, { javascript, module, wireSource, ids }) => {
                const require = process.getBuiltinModule("module").createRequire(module)
                const { ExifImage } = require(module)
                const readExifData = new Function("ExifImage", "actionComplete", javascript + ";return readExifData")(ExifImage, () => {})
                globalThis.exifCloneSuccess = await readExifData({ id: ids[0] })
                globalThis.exifCloneMissing = await readExifData({ id: ids[ids.length - 1] })
                const observer = new Function("return (" + wireSource + ")")()
                const results = []
                for (const id of ids) results.push(observer(await readExifData({ id })))
                return results
            },
            { javascript, module: require.resolve("exif"), wireSource: wire.toString(), ids }
        )
        const native = spawnSync(binary, [], { input: ids.map((id) => JSON.stringify({ id })).join("\n") + "\n", encoding: "utf8", timeout: 20000, maxBuffer: 16 * 1024 * 1024 })
        assert.equal(native.status, 0, native.stderr || native.error?.message)
        const values = native.stdout.trim().split("\n").map(JSON.parse)
        results.forEach((original, i) => assert.deepEqual({ reply: values[i].reply, hydration: values[i].hydration }, original, ids[i]))
        const windowPromise = app.waitForEvent("window")
        const windowId = await app.evaluate(
            async ({ BrowserWindow }, { preload }) => {
                const window = new BrowserWindow({ show: false, webPreferences: { preload, contextIsolation: true, sandbox: true, nodeIntegration: false } })
                await window.loadURL("data:text/html,<title>EXIF clone probe</title>")
                window.webContents.send("exif-oracle-clone", { success: globalThis.exifCloneSuccess, missing: globalThis.exifCloneMissing })
                return window.id
            },
            { preload }
        )
        const page = await windowPromise
        const clone = await page.evaluate(async (observerSource) => {
            const payload = await window.exifProbe.read()
            const observer = new Function("return (" + observerSource + ")")()
            const bytes = payload.success.exif.exif.ExifVersion
            return { wire: observer(payload.success), missingOwn: Object.hasOwn(payload.missing, "exif"), missingUndefined: payload.missing.exif === undefined, byteConstructor: bytes.constructor.name, byteView: ArrayBuffer.isView(bytes), special: [Number.isNaN(payload.success.exif.image.YResolution[2]), Object.is(payload.success.exif.image.YResolution[3], -0)] }
        }, wire.toString())
        assert.deepEqual(clone.wire, results[0])
        assert.equal(clone.missingOwn, true)
        assert.equal(clone.missingUndefined, true)
        assert.equal(clone.byteConstructor, "Uint8Array")
        assert.equal(clone.byteView, true)
        assert.deepEqual(clone.special, [true, true])
        const image = await page.evaluate(
            async (url) => {
                const image = new Image()
                image.src = url
                await image.decode()
                return { width: image.naturalWidth, height: image.naturalHeight }
            },
            "data:image/jpeg;base64," + fs.readFileSync(ids[0]).toString("base64")
        )
        assert.deepEqual(image, { width: 8, height: 8 })
        await app.evaluate(({ BrowserWindow }, id) => {
            BrowserWindow.fromId(id)?.destroy()
            delete globalThis.exifCloneSuccess
            delete globalThis.exifCloneMissing
        }, windowId)
        return { runtime, executableSha256: hash(fs.readFileSync(executable)), dependencyVersion: require("exif/package.json").version, parserSha256: hash(fs.readFileSync(require.resolve("exif/lib/exif/ExifImage.js"))), handlerSha256: hash(declaration), fixturesCompared: ids.length, image, clone }
    })
    const output = path.join(root, "test-output-rust/exif/electron-exif.json")
    fs.mkdirSync(path.dirname(output), { recursive: true })
    fs.writeFileSync(output, JSON.stringify(receipt, null, 2) + "\n")
    console.log(JSON.stringify({ output, fixturesCompared: receipt.fixturesCompared, image: receipt.image, byteConstructor: receipt.clone.byteConstructor, missingOwn: receipt.clone.missingOwn, runtime: receipt.runtime.versions.electron }))
} finally {
    fs.rmSync(folder, { recursive: true, force: true })
}
