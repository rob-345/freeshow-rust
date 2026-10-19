import assert from "node:assert/strict"
import { test } from "node:test"
import fs from "node:fs"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { root } from "./oracle.mjs"

test("the pinned inventory and generated fixtures validate after a CRLF checkout", (t) => {
    const parent = path.join(root, "test-output-rust")
    fs.mkdirSync(parent, { recursive: true })
    // Keep the isolated checkout below the repo so Node resolves the installed
    // TypeScript package without modifying or copying node_modules.
    const checkout = fs.mkdtempSync(path.join(parent, "snapshot-crlf-"))
    t.after(() => fs.rmSync(checkout, { recursive: true, force: true }))
    const inventory = JSON.parse(fs.readFileSync(path.join(root, "docs/migration/upstream-inventory.json"), "utf8"))
    const files = new Set(["package.json", "src/types/Channels.ts", "src/types/IPC/Main.ts", "src/types/IPC/ToMain.ts", "scripts/migration/snapshot.mjs", "docs/migration/upstream-inventory.json", "tests/parity/fixtures/defaults.json", "tests/parity/fixtures/stores.json", ...inventory.sourceFiles.map((file) => file.path)])
    for (const file of files) {
        const target = path.join(checkout, file)
        fs.mkdirSync(path.dirname(target), { recursive: true })
        fs.writeFileSync(target, fs.readFileSync(path.join(root, file), "utf8").replace(/\r?\n/g, "\r\n"))
    }
    const result = spawnSync(process.execPath, [path.join(checkout, "scripts/migration/snapshot.mjs"), "--check"], { cwd: checkout, encoding: "utf8", timeout: 10000 })
    assert.equal(result.status, 0, result.stdout + result.stderr)
})
