import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"
import test from "node:test"
import crypto from "node:crypto"
import ts from "typescript"
import { files, makeStamp, oracle, revision } from "./ai-status-oracle.mjs"
const root = fileURLToPath(new URL("../../", import.meta.url))
const binary = process.env.FREESHOW_AI_STATUS_BIN || path.join(root, "target/debug/freeshow-ai-status-parity" + (process.platform === "win32" ? ".exe" : ""))
assert.ok(fs.existsSync(binary), "Build freeshow-ai-status-parity or set FREESHOW_AI_STATUS_BIN")
const payload = { engineId: "nemotron" }
const reference = JSON.parse(fs.readFileSync(new URL("./ai-status-fixture.json", import.meta.url)))
function native(userData, data = payload, intelMac = false) {
    const child = spawnSync(binary, [userData, ...(intelMac ? ["intel-mac"] : [])], { input: JSON.stringify(data), encoding: "utf8", timeout: 15000, maxBuffer: 128 * 1024 })
    assert.equal(child.status, 0, child.stderr || child.error?.message)
    return JSON.parse(child.stdout)
}
async function fixture(run) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-ai-status-"))
    const userData = path.join(dir, "app 敬拜 😀")
    fs.mkdirSync(userData)
    const models = path.join(userData, "bin/nemotron/models")
    const stampPath = path.join(models, "model.json")
    const tiny = () => {
        fs.mkdirSync(models, { recursive: true })
        for (const [name] of files) fs.writeFileSync(path.join(models, name), name)
    }
    const stamp = () => fs.writeFileSync(stampPath, JSON.stringify(makeStamp(models), null, 4))
    try {
        return await run({ dir, userData, models, stampPath, tiny, stamp })
    } finally {
        fs.rmSync(dir, { recursive: true, force: true })
    }
}
async function compare(userData, data = payload, intelMac = false) {
    const source = oracle(userData, { intelMac })
    const value = await source.status(data)
    assert.deepEqual(Object.keys(value), ["nemotron"])
    assert.equal(typeof value.nemotron.ready, "boolean")
    assert.deepEqual(Object.keys(value.nemotron), value.nemotron.ready ? ["ready", "localPath"] : ["ready", "error"])
    const result = native(userData, data, intelMac)
    assert.equal(result.ok, true, JSON.stringify(result))
    assert.deepEqual(result.value, JSON.parse(JSON.stringify(value)))
    assert.deepEqual(result.evidence.stats, source.evidence.stats, "actual original pre-JSON IEEE stat bits and call order")
    assert.deepEqual(result.evidence.hashed, source.evidence.hashed, "actual complete streaming hash order")
    assert.equal(result.writeDiagnostic, null)
    assert.deepEqual(source.calls, ["userData"])
    return result
}

test("actual MAIN/status/model helper returns missing without setup/IO side effects", async () => {
    await fixture(async ({ userData, models }) => {
        assert.equal((await compare(userData)).value.nemotron.error, "The local model is missing")
        assert.equal(fs.existsSync(path.dirname(models)), false)
        const source = oracle(userData)
        assert.deepEqual(JSON.parse(JSON.stringify(await source.dispatch(payload, "敬拜-listener"))), [["MAIN", { channel: "AI_GET_STATUS", data: { nemotron: { ready: false, error: "The local model is missing" } } }, "敬拜-listener"]])
        const noListener = await source.dispatch(payload, undefined)
        assert.equal(noListener[0][2], undefined)
        assert.deepEqual(Object.keys(noListener[0][1]), ["channel", "data"])
    })
})
test("real tiny hashes all execute; later missing overrides earlier outdated digest", async () => {
    await fixture(async ({ userData, models, tiny, stampPath }) => {
        tiny()
        const bad = await compare(userData)
        assert.equal(bad.value.nemotron.error, "The local model is outdated")
        assert.deepEqual(
            bad.evidence.hashed,
            files.map(([name]) => name)
        )
        assert.equal(fs.existsSync(stampPath), false)
        for (let index = 4; index >= 0; index--) {
            fs.unlinkSync(path.join(models, files[index][0]))
            const missing = await compare(userData)
            assert.equal(missing.value.nemotron.error, "The local model is missing")
            assert.equal(missing.evidence.hashed.length, index)
        }
    })
})
test("genuine stamp shortcut accepts tiny/empty files: source integrity status is not usable-model proof", async () => {
    await fixture(async ({ userData, models, tiny, stamp, stampPath }) => {
        tiny()
        fs.writeFileSync(path.join(models, files[1][0]), "")
        stamp()
        const before = fs.readFileSync(stampPath)
        const result = await compare(userData)
        assert.equal(result.evidence.fast_path, true)
        assert.equal(result.value.nemotron.localPath, models)
        assert.equal(result.evidence.hashed.length, 0)
        assert.deepEqual(fs.readFileSync(stampPath), before)
        assert.deepEqual(Object.keys(result.value.nemotron), ["ready", "localPath"])
        fs.unlinkSync(path.join(models, files[2][0]))
        fs.mkdirSync(path.join(models, files[2][0]))
        stamp()
        assert.equal((await compare(userData)).evidence.fast_path, true, "original fast path stats directories too; still not usable model proof")
    })
})
test("invalid/malformed/stale stamp falls back in original short-circuit stat/hash order", async () => {
    await fixture(async ({ userData, models, stampPath, tiny, stamp }) => {
        tiny()
        const variants = [
            (s) => ({ ...s, revision: "old" }),
            (s) => ({ ...s, files: null }),
            (s) => {
                s.files[files[2][0]].sha256 = "bad"
                return s
            },
            (s) => {
                s.files[files[2][0]].size++
                return s
            },
            (s) => {
                s.files[files[2][0]].mtimeMs += 0.125
                return s
            },
            (s) => {
                s.files[files[2][0]].mtimeMs = String(s.files[files[2][0]].mtimeMs)
                return s
            },
            (s) => {
                delete s.files[files[3][0]]
                return s
            }
        ]
        for (const mutate of variants) {
            fs.writeFileSync(stampPath, JSON.stringify(mutate(makeStamp(models))))
            assert.equal((await compare(userData)).value.nemotron.error, "The local model is outdated")
        }
        for (const text of ["{bad", "null", "[]", "true", "0", "{}", '"string"']) {
            fs.writeFileSync(stampPath, text)
            assert.equal((await compare(userData)).value.nemotron.error, "The local model is outdated")
        }
        stamp()
        fs.appendFileSync(path.join(models, files[4][0]), "changed bytes")
        assert.equal((await compare(userData)).evidence.fast_path, false)
        fs.unlinkSync(stampPath)
        fs.mkdirSync(stampPath)
        assert.equal((await compare(userData)).value.nemotron.error, "The local model is outdated")
    })
})
test("JSON duplicate keys, ignored huge exponents and correctly rounded raw stat numbers", async () => {
    await fixture(async ({ userData, models, tiny, stampPath }) => {
        tiny()
        const target = path.join(models, files[0][0])
        fs.utimesSync(target, 1700000000.000499841, 1700000000.000499841)
        const stamp = JSON.stringify(makeStamp(models))
        fs.writeFileSync(stampPath, stamp.replace('"revision":', '"unknown":1e400,"revision":"old","revision":'))
        assert.equal((await compare(userData)).evidence.fast_path, true)
        const source = oracle(userData)
        assert.equal((await source.status()).nemotron.ready, true)
        assert.match(source.evidence.stats[0].mtime_bits, /^[a-f0-9]{16}$/)
    })
})
test("explicit source falsy model variants ignore customPath; truthy/all-provider paths remain unsupported", async () => {
    await fixture(async ({ userData, tiny, stamp }) => {
        tiny()
        stamp()
        for (const modelId of [undefined, null, false, 0, -0, ""]) {
            const result = await compare(userData, { ...payload, modelId, customPath: "/must/not/select/models" })
            assert.equal(result.evidence.fast_path, true)
        }
        for (const modelId of [NaN, undefined]) assert.equal((await oracle(userData).status({ ...payload, modelId })).nemotron.ready, true)
        for (const data of [{}, null, { engineId: "ollama" }, { ...payload, modelId: "tiny" }, { ...payload, modelId: [] }, { ...payload, modelId: {} }, { ...payload, modelId: true }]) assert.equal(native(userData, data).error.code, "ai_status_unsupported")
        assert.equal((await oracle(userData).status({ ...payload, modelId: "tiny" })).nemotron.ready, undefined, "truthy model branch is materially different and outside native API")
    })
})
test("actual Intel Mac source branch returns exact error before any filesystem observation", async () => {
    await fixture(async ({ userData }) => {
        const result = await compare(userData, payload, true)
        assert.deepEqual(result.evidence.stats, [])
        assert.equal(result.value.nemotron.error, "Nemotron transcription is not supported on Intel Mac")
    })
})
test("actual denied reads/exists and ordinary POSIX symlink following are source-observed", { skip: process.platform === "win32" }, async () => {
    assert.notEqual(process.getuid(), 0)
    await fixture(async ({ dir, userData, models, tiny, stamp, stampPath }) => {
        tiny()
        stamp()
        fs.chmodSync(stampPath, 0)
        try {
            assert.equal((await compare(userData)).evidence.fast_path, false)
        } finally {
            fs.chmodSync(stampPath, 0o600)
        }
        fs.unlinkSync(stampPath)
        const first = path.join(models, files[0][0])
        fs.chmodSync(first, 0)
        try {
            await assert.rejects(oracle(userData).status(), { code: "EACCES" })
            assert.equal(native(userData).error.code, "ai_status_io")
        } finally {
            fs.chmodSync(first, 0o600)
        }
        fs.chmodSync(models, 0)
        try {
            assert.equal((await compare(userData)).value.nemotron.error, "The local model is missing")
        } finally {
            fs.chmodSync(models, 0o700)
        }
        const actual = path.join(dir, "external bytes")
        fs.renameSync(first, actual)
        fs.symlinkSync(actual, first)
        assert.equal((await compare(userData)).value.nemotron.error, "The local model is outdated")
    })
})
test("oversized/deep/scalar-surrogate stamp and nonregular hash inputs are truthful bounded gaps", async () => {
    await fixture(async ({ userData, models, tiny, stampPath }) => {
        tiny()
        const gaps = [
            [" ".repeat(65537), "ai_status_limit"],
            ['{"x":' + "[".repeat(17) + "0" + "]".repeat(17) + "}", "ai_status_limit"],
            ['{"x":"\\ud800"}', "ai_status_unsupported"]
        ]
        for (const [raw, code] of gaps) {
            fs.writeFileSync(stampPath, raw)
            assert.equal((await oracle(userData).status()).nemotron.error, "The local model is outdated")
            assert.equal(native(userData).error.code, code)
        }
        fs.unlinkSync(stampPath)
        const first = path.join(models, files[0][0])
        fs.unlinkSync(first)
        fs.mkdirSync(first)
        await assert.rejects(oracle(userData).status())
        assert.equal(native(userData).error.code, "ai_status_unsupported")
    })
})
test("labelled injected digest boundary proves original slow-success exact stamp bytes and ignored real write failure", async () => {
    await fixture(async ({ userData, models, tiny, stampPath }) => {
        tiny()
        for (const [name] of files) {
            const file = path.join(models, name)
            fs.utimesSync(file, 1700000000.5, 1700000000.5)
            assert.equal(fs.statSync(file).mtimeMs, 1700000000500, "actual filesystem must retain fixture timestamp")
        }
        const source = oracle(userData, { injectedDigests: true })
        assert.equal((await source.status()).nemotron.ready, true)
        assert.equal(source.errors.length, 0)
        assert.equal(fs.readFileSync(stampPath, "utf8"), JSON.stringify(makeStamp(models), null, 4))
        assert.equal(fs.readFileSync(stampPath, "utf8"), reference.stampBody, "same source-generated bytes are checked by native injected-boundary test")
        assert.equal(JSON.parse(fs.readFileSync(stampPath)).revision, revision)
        assert.deepEqual(
            source.evidence.hashed,
            files.map(([name]) => name)
        )
        // This real stamp now independently exercises native/source fast path;
        // real successful pinned hashes were NOT established by this test.
        assert.equal((await compare(userData)).evidence.fast_path, true)
        fs.unlinkSync(stampPath)
        fs.mkdirSync(stampPath)
        const failure = oracle(userData, { injectedDigests: true })
        assert.equal((await failure.status()).nemotron.ready, true)
        assert.deepEqual(failure.errors, ["[nemotron] Could not write the model stamp:"])
        assert.equal(fs.statSync(stampPath).isDirectory(), true)
    })
})
test("actual pinned Node Stats msFromTimeSpec boundary receipt keeps pre-JSON bits", () => {
    assert.equal(reference.node, process.versions.node)
    const source = process.binding("natives")["internal/fs/utils"]
    const ast = ts.createSourceFile("fs-utils.js", source, ts.ScriptTarget.Latest, true)
    const declaration = ast.statements.find((n) => ts.isFunctionDeclaration(n) && n.name?.text === "msFromTimeSpec").getText(ast)
    assert.equal(crypto.createHash("sha256").update(declaration).digest("hex"), reference.msFromTimeSpecSha256)
    const ms = new Function("kMsPerSec", "kNsPerMs", `return (${declaration})`)(1000, 1000000)
    for (const { seconds, nanos, bits } of reference.posixCases) {
        const bytes = Buffer.alloc(8)
        bytes.writeDoubleBE(ms(seconds, nanos))
        assert.equal(bytes.toString("hex"), bits)
    }
    assert.notEqual(ms(1700000000, 499841), Number(1700000000n * 1000000000n + 499841n) / 1000000, "integer aggregate shortcut is a genuine one-ULP negative control")
})
