import assert from "node:assert/strict"
import { test } from "node:test"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { upstreamFunctions, clean } from "./oracle.mjs"
import { rust } from "./runner.mjs"

test("file selection callbacks and optional reads match the original Electron handler", async (t) => {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-dialog-"))
    t.after(() => fs.rmSync(temp, { recursive: true, force: true }))
    const selected = [path.join(temp, "Lyrics 日本語.show"), path.join(temp, "second.txt")]
    fs.writeFileSync(selected[0], "Unicode 日本語 🙌")
    fs.writeFileSync(selected[1], "second\nline")
    for (const read of [false, true]) {
        const data = { channel: "IMPORT", id: "request-1", filter: { name: "Shows", extensions: ["show"] }, multiple: true, read }
        const emitted = []
        const { selectFiles } = upstreamFunctions("src/electron/utils/files.ts", ["selectFiles"], {
            selectFilesDialog: async () => selected,
            readFile: (file) => fs.readFileSync(file, "utf8"),
            ToMain: { OPEN_FILE2: "OPEN_FILE2" },
            sendToMain: (channel, data) => emitted.push({ channel: "MAIN", message: { channel, data: clean(data) } })
        })
        await selectFiles(data)
        const [result] = rust([{ op: "dialog.files", data, selected }])
        assert.equal(result.ok, true)
        assert.deepEqual(result.value, emitted)
    }
})

test("file selection cancellation emits no callback as in Electron", async () => {
    const emitted = []
    const { selectFiles } = upstreamFunctions("src/electron/utils/files.ts", ["selectFiles"], {
        selectFilesDialog: async () => [],
        ToMain: { OPEN_FILE2: "OPEN_FILE2" },
        sendToMain: (...args) => emitted.push(args)
    })
    const data = { channel: "IMPORT", id: "cancel", filter: { name: "Files", extensions: ["*"] } }
    await selectFiles(data)
    assert.deepEqual(rust([{ op: "dialog.files", data, selected: [] }])[0].value, emitted)
})

test("folder selection and cancellation preserve the original callback channel", async () => {
    for (const selected of ["", path.join(os.tmpdir(), "FreeShow 日本語")]) {
        const emitted = []
        const { selectFolder } = upstreamFunctions("src/electron/utils/files.ts", ["selectFolder"], {
            selectFolderDialog: async () => selected,
            ToMain: { OPEN_FOLDER2: "OPEN_FOLDER2" },
            sendToMain: (channel, data) => emitted.push({ channel: "MAIN", message: { channel, data: clean(data) } })
        })
        const data = { channel: "DATA_SHOWS", title: "Choose data folder" }
        await selectFolder(data)
        const [result] = rust([{ op: "dialog.folder", data, selected: selected || null }])
        assert.equal(result.ok, true)
        assert.deepEqual(result.value, emitted)
    }
})
