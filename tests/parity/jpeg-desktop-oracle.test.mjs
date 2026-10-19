import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import { test } from "node:test"

const root = fileURLToPath(new URL("../../", import.meta.url))
test("desktop JPEG oracle validates actual source recordings and tamper controls without an installed Electron executable", () => {
    const result = spawnSync(process.execPath, ["tests/desktop/jpeg_now_playing_oracle.mjs", "--self-test"], {
        cwd: root,
        env: { ...process.env, FREESHOW_ELECTRON_BIN: "/not-installed" },
        encoding: "utf8",
        timeout: 15000,
        maxBuffer: 131072
    })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    assert.equal(result.stderr, "")
    const receipt = JSON.parse(result.stdout)
    assert.equal(receipt.revision, "2d94defcf98175e4f5c9504e7f4fd1d33c35c975")
    assert.equal(receipt.recordingPresent, true)
    assert.ok(receipt.checks.length >= 5)
    assert.equal(receipt.originalRecordingExecuted, false)
    assert.equal(receipt.guiExecuted, false)
})
