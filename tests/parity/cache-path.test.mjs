import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { test } from "node:test"
import { upstreamFunctions } from "./oracle.mjs"
import { rust, request } from "./runner.mjs"

test("GET_CACHE_PATH creates and returns the source media-cache root with original correlation", (t) => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-cache-path-"))
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }))
    const oracleApp = path.join(directory, "oracle"), nativeApp = path.join(directory, "native")
    fs.mkdirSync(oracleApp)
    const source = upstreamFunctions("src/electron/data/thumbnails.ts", ["getThumbnailFolderPath"], {
        path, appDataPath: oracleApp, doesPathExist: fs.existsSync,
        makeDir: (folder) => fs.mkdirSync(folder, { recursive: true }),
        app: { getPath: () => { throw new Error("Writable private app data must not fall back") } }
    }, 'let thumbnailFolderPath = "";')
    const expected = source.getThumbnailFolderPath()
    assert.equal(source.getThumbnailFolderPath(), expected)
    const actual = rust([request("GET_CACHE_PATH", undefined, "cache-one"), request("GET_CACHE_PATH", undefined, "cache-two")], ["--app-data", nativeApp, "--data-dir", path.join(directory, "library")])
    for (const [index, response] of actual.entries()) {
        assert.equal(response.ok, true)
        assert.equal(response.value[0].channel, "MAIN")
        assert.equal(response.value[0].message.channel, "GET_CACHE_PATH")
        assert.equal(response.value[0].listenerId, ["cache-one", "cache-two"][index])
        assert.equal(path.relative(nativeApp, response.value[0].message.data), path.relative(oracleApp, expected))
        assert.equal(fs.statSync(response.value[0].message.data).isDirectory(), true)
    }
    assert.equal(fs.existsSync(path.join(nativeApp, "Thumbnails")), false)
})
