import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import test from "node:test"
import { oracle } from "./lib/ai-local-oracle.mjs"
const root = fileURLToPath(new URL("../../", import.meta.url))
const binary = process.env.FREESHOW_AI_LOCAL_BIN || path.join(root, "target/debug/examples/inventory-parity" + (process.platform === "win32" ? ".exe" : ""))
assert.ok(fs.existsSync(binary), "Build the isolated inventory-parity example or set FREESHOW_AI_LOCAL_BIN")
function native(userData) {
    const result = spawnSync(binary, [userData], { encoding: "utf8", timeout: 10000, maxBuffer: 128 * 1024 })
    assert.equal(result.status, 0, result.stderr || result.error?.message)
    return JSON.parse(result.stdout)
}
async function compare(userData) {
    const source = await oracle(userData).inventory()
    const observed = native(userData)
    assert.equal(observed.ok, true, JSON.stringify(observed))
    assert.deepEqual(observed.value, source)
    for (const row of source) {
        assert.deepEqual(Object.keys(row), ["path", "name", "size"])
        assert.equal(typeof row.path, "string")
        assert.equal(row.name, "nemotron")
        assert.ok(Number.isSafeInteger(row.size) && row.size > 0)
    }
    return source
}
async function fixture(run) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-ai-"))
    const userData = path.join(dir, "app 敬拜")
    fs.mkdirSync(userData)
    try {
        return await run({ dir, userData, engine: path.join(userData, "bin/nemotron") })
    } finally {
        fs.rmSync(dir, { recursive: true, force: true })
    }
}
test("actual immutable inventory handles missing/empty/zero-size/engine-file without setup effects", async () => {
    await fixture(async ({ userData, engine }) => {
        assert.deepEqual(await compare(userData), [])
        assert.equal(fs.existsSync(path.join(userData, "bin")), false)
        fs.mkdirSync(engine, { recursive: true })
        fs.writeFileSync(path.join(engine, "zero.bin"), "")
        fs.mkdirSync(path.join(engine, "empty"))
        assert.deepEqual(await compare(userData), [])
        fs.rmSync(engine, { recursive: true })
        fs.writeFileSync(engine, "ordinary file is not an inventory directory")
        assert.deepEqual(await compare(userData), [])
        assert.deepEqual(fs.readdirSync(userData), ["bin"])
    })
})
test("actual filesystem sum includes nested/Unicode/hidden files, other engines ignored, apparent sparse size and duplicate hardlinks", async () => {
    await fixture(async ({ userData, engine }) => {
        fs.mkdirSync(path.join(engine, "models/nested"), { recursive: true })
        fs.writeFileSync(path.join(engine, "models/nested/模型.bin"), Buffer.from([0, 1, 2, 3, 4]))
        fs.writeFileSync(path.join(engine, ".hidden"), "abc")
        fs.linkSync(path.join(engine, ".hidden"), path.join(engine, "hardlink"))
        const fd = fs.openSync(path.join(engine, "sparse"), "w")
        try {
            fs.ftruncateSync(fd, 8 * 1024 * 1024 + 17)
        } finally {
            fs.closeSync(fd)
        }
        if (process.platform !== "win32") assert.ok(fs.statSync(path.join(engine, "sparse")).blocks * 512 < 8 * 1024 * 1024, "Sparse fixture must allocate fewer blocks than its apparent size")
        fs.mkdirSync(path.join(userData, "bin/unknown"))
        fs.writeFileSync(path.join(userData, "bin/unknown/must-not-count"), "ignored")
        assert.equal((await compare(userData))[0].size, 5 + 3 + 3 + 8 * 1024 * 1024 + 17)
    })
})
test("actual original MAIN dispatch preserves outer channel/listener and ignores path/engine payload", async () => {
    await fixture(async ({ userData, engine }) => {
        fs.mkdirSync(engine, { recursive: true })
        fs.writeFileSync(path.join(engine, "model"), "model")
        const source = oracle(userData)
        const payload = {
            get path() {
                throw Error("Ignored payload getter was evaluated")
            },
            engine: "unknown"
        }
        const rows = await source.inventory()
        assert.deepEqual(await source.dispatch(payload, "listener-敬拜"), [["MAIN", { channel: "AI_GET_BIN", data: rows }, "listener-敬拜"]])
        assert.deepEqual(await source.dispatch(null, undefined), [["MAIN", { channel: "AI_GET_BIN", data: rows }, undefined]])
        assert.deepEqual(source.calls, ["userData"], "BIN_DIR is captured once, and no model/backend is imported")
        await compare(userData)
    })
})
test("POSIX real denied directory =>zero but unreadable file stat still contributes size", { skip: process.platform === "win32" }, async () => {
    assert.notEqual(process.getuid(), 0, "Actual permission fixtures require non-root")
    await fixture(async ({ userData, engine }) => {
        fs.mkdirSync(path.join(engine, "denied"), { recursive: true })
        fs.writeFileSync(path.join(engine, "denied/hidden"), "must not count")
        const file = path.join(engine, "no-read")
        fs.writeFileSync(file, "still stat-able")
        fs.chmodSync(path.join(engine, "denied"), 0)
        fs.chmodSync(file, 0)
        try {
            assert.equal((await compare(userData))[0].size, Buffer.byteLength("still stat-able"))
        } finally {
            fs.chmodSync(path.join(engine, "denied"), 0o700)
            fs.chmodSync(file, 0o600)
        }
        fs.chmodSync(engine, 0)
        try {
            assert.deepEqual(await compare(userData), [])
        } finally {
            fs.chmodSync(engine, 0o700)
        }
    })
})
test("POSIX real symlinks follow engine root; child directory links contribute stat size without recursion", { skip: process.platform === "win32" }, async () => {
    await fixture(async ({ dir, userData, engine }) => {
        const external = path.join(dir, "outside")
        fs.mkdirSync(external)
        fs.writeFileSync(path.join(external, "large"), Buffer.alloc(32768))
        fs.mkdirSync(path.dirname(engine), { recursive: true })
        fs.symlinkSync(external, engine, "dir")
        assert.equal((await compare(userData))[0].size, 32768)
        fs.unlinkSync(engine)
        fs.mkdirSync(engine)
        fs.symlinkSync(external, path.join(engine, "dir-link"), "dir")
        fs.symlinkSync(path.join(external, "large"), path.join(engine, "file-link"), "file")
        fs.symlinkSync(path.join(dir, "absent"), path.join(engine, "dangling"))
        fs.symlinkSync("loop", path.join(engine, "loop"))
        assert.equal((await compare(userData))[0].size, fs.statSync(external).size + 32768)
        const alias = path.join(dir, "app-link")
        fs.symlinkSync(userData, alias, "dir")
        assert.equal((await compare(alias))[0].path, path.join(alias, "bin/nemotron"))
    })
})
test("Windows actual junctions remain explicit unsupported rather than fabricated source-zero", { skip: process.platform !== "win32" }, async () => {
    await fixture(async ({ dir, userData, engine }) => {
        const external = path.join(dir, "outside")
        fs.mkdirSync(external)
        fs.writeFileSync(path.join(external, "model"), "real bytes")
        fs.mkdirSync(path.dirname(engine), { recursive: true })
        fs.symlinkSync(external, engine, "junction")
        assert.equal((await oracle(userData).inventory())[0].size, 10)
        const result = native(userData)
        assert.equal(result.ok, false)
        assert.equal(result.error.code, "ai_local_unsupported")
    })
})
test("real deeper-than-budget filesystem is a truthful native resource gap, not empty success", async () => {
    await fixture(async ({ userData, engine }) => {
        let folder = engine
        for (let index = 0; index < 34; index++) folder = path.join(folder, "d")
        fs.mkdirSync(folder, { recursive: true })
        fs.writeFileSync(path.join(folder, "model"), "x")
        assert.equal((await oracle(userData).inventory())[0].size, 1)
        const result = native(userData)
        assert.equal(result.ok, false)
        assert.equal(result.error.code, "ai_local_limit")
    })
})

test("POSIX undecodable entry spelling is an explicit capability error alongside actual source result", { skip: process.platform === "win32" }, async () => {
    await fixture(async ({ userData, engine }) => {
        fs.mkdirSync(engine, { recursive: true })
        fs.writeFileSync(Buffer.concat([Buffer.from(engine + "/"), Buffer.from([0xff])]), "bytes")
        fs.writeFileSync(path.join(engine, "valid"), "x")
        assert.equal((await oracle(userData).inventory())[0].size, 1, "Node replaces the entry spelling, then that stat fails")
        const result = native(userData)
        assert.equal(result.ok, false)
        assert.equal(result.error.code, "ai_local_unsupported")
    })
})
