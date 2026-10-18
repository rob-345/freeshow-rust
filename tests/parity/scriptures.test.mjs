import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import { scriptureOracle as oracle } from "./lib/scriptures-oracle.mjs"

const root = fileURLToPath(new URL("../../", import.meta.url))
const binary = process.env.FREESHOW_SCRIPTURES_PARITY || path.join(root, "target/debug/freeshow-scriptures-parity" + (process.platform === "win32" ? ".exe" : ""))
const windows = process.platform === "win32"
const supported = ["linux", "darwin", "win32"].includes(process.platform)
const coverage = { platform: process.platform, node: process.version, comparisons: 0, generatedVerses: 0, skippedCapabilities: [] }
test.after(() => console.log("Scripture parity coverage:", JSON.stringify(coverage)))
function fixture(t) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-scripture-"))
    t.after(() => fs.rmSync(dir, { recursive: true, force: true }))
    const dataRoot = path.join(dir, "selected 敬拜"),
        documentsRoot = path.join(dir, "Documents é")
    fs.mkdirSync(path.join(dataRoot, "Bibles"), { recursive: true })
    fs.mkdirSync(path.join(documentsRoot, "Bibles"), { recursive: true })
    return { dir, dataRoot, documentsRoot }
}
const request = (roots, name, id = "requested", extra = {}) => ({ dataRoot: roots.dataRoot, documentsRoot: roots.documentsRoot, payload: { name, id }, ...extra })
function write(roots, name, value, legacy = false) {
    const file = path.join(legacy ? roots.documentsRoot : roots.dataRoot, "Bibles", name + ".fsb")
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, Buffer.isBuffer(value) || typeof value === "string" ? value : JSON.stringify(value))
    return file
}
function native(cases) {
    const result = spawnSync(binary, [], { input: cases.map(JSON.stringify).join("\n") + "\n", encoding: "utf8", timeout: 30000, maxBuffer: 64 * 1024 * 1024 })
    assert.equal(result.status, 0, result.stderr || result.error?.message)
    return result.stdout.trim().split("\n").map(JSON.parse)
}
function propertyOrder(actual, expected) {
    if (!expected || typeof expected !== "object") return
    assert.deepEqual(Object.keys(actual), Object.keys(expected))
    for (const key of Object.keys(expected)) propertyOrder(actual[key], expected[key])
}
function compare(cases) {
    const results = native(cases)
    for (const [i, input] of cases.entries()) {
        const source = oracle(input.dataRoot, input.documentsRoot)
        const expected = structuredClone(source.load(input.payload))
        assert.equal(results[i].ok, true, JSON.stringify(results[i]))
        assert.equal(results[i].complete, true)
        assert.deepEqual(results[i].reply.data, expected, JSON.stringify(input))
        propertyOrder(results[i].reply.data, expected)
        assert.deepEqual(results[i].reply.stats.loadPaths, source.loads)
        const diagnostics = results[i].reply.diagnostics.map(({ code, operation, sourceErrorLogged }) => ({ code, operation, sourceErrorLogged }))
        const expectedDiagnostics = source.errors.map(([message]) => {
            assert.ok(message === "Error parsing show" || message === "Error when reading file:", message)
            return { code: message === "Error parsing show" ? "source_parse_error" : "source_read_error", operation: message === "Error parsing show" ? "parseJSON" : "readFile", sourceErrorLogged: true }
        })
        assert.deepEqual(diagnostics, expectedDiagnostics)
        assert.deepEqual(source.alerts, [])
        coverage.comparisons++
    }
    return results
}
function bible(verses) {
    return ["original", { books: [{ name: "Genesis", chapters: [{ verses }] }], future: { nested: [true, 0, null, "敬拜 e\u0301 🙌"], __protoField: "preserve" } }]
}

test("actual BIBLE handler preserves generated original shapes and exact first-value/falsy-text migration gate", { skip: !supported }, (t) => {
    const roots = fixture(t),
        cases = []
    const values = [undefined, null, false, true, 0, 1, "", "verse 日本語 🙌", [], {}, ["future"], { future: [1, false] }]
    for (const [firstIndex, first] of values.entries())
        for (const [textIndex, text] of values.entries()) {
            const verses = [first === undefined ? {} : { value: first }, { number: 2, text, value: "replacement", future: { keep: true } }, { number: 3, value: 0 }, { number: 4, value: false }, { number: 5 }, { text: "retained", value: "old remains" }]
            const name = `generated-${firstIndex}-${textIndex}`
            write(roots, name, bible(verses))
            cases.push(request(roots, name))
            coverage.generatedVerses += verses.length
        }
    compare(cases)
})

test("Unicode/path spelling, scalar/default IDs and meaningful unknown fields retain source semantics", { skip: !supported }, (t) => {
    const roots = fixture(t),
        cases = []
    for (const name of ["", "敬拜 🙌", "e\u0301", "é", "slash\\name", "nested/name", "nested/../same", "../outside", "/leading", "line\nbreak", 'quote"name']) {
        // Control characters/quotes cannot name native Windows files. Still run
        // their missing-file requests against the real source and Rust backend.
        if (!windows || !/[\x00-\x1f"<>|?*]/.test(name)) write(roots, name, bible([{ text: "modern", future: { unknown: [false, null, { x: true }] } }]))
        cases.push(request(roots, name))
    }
    write(roots, "ids", ["file id", { unknown: { __proto__: "data", 10: "ten", 2: "two" } }])
    for (const id of ["", null, false, 0, true, 2, [], {}, ["array id"], { native: "retained JSON id" }]) cases.push(request(roots, "ids", id))
    cases.push({ ...request(roots, "ids"), payload: { name: "ids", unused: "untouched" } })
    cases.push(request(roots, "NUL\0name"))
    compare(cases)
})

test("real UTF-8 bytes, JSON repair, finite binary64 and duplicate keys match immutable source", { skip: !supported }, (t) => {
    const roots = fixture(t),
        cases = []
    const texts = ["", "not json", "null", "false", "0", '""', "[]", "{}", "[]junk", "{}junk", "1\ngarbage", '["id",{"nested":{}}] trailing', '["id",{"text":"}}] inside string"}] trailing', '\uFEFF["id",{}]', '["id",{"number":9007199254740993,"small":8.3163996793004788882e-275,"subnormal":5e-324,"z":-0,"duplicate":1,"other":2,"duplicate":3}]', '["id",{"escaped":"\\ud83d\\ude4c \\u00e9 \\u0000","line":"\\r\\n"}]']
    texts.push(Buffer.from('["id",{"text":"before ' + "after" + '"}]'))
    for (const bytes of [[0xff], [0xed, 0xa0, 0x80], [0xe2, 0x82], [0xe2, 0x28, 0xa1]]) texts.push(Buffer.concat([Buffer.from('["id",{"text":"'), Buffer.from(bytes), Buffer.from('"}]')]))
    for (const [index, text] of texts.entries()) {
        const name = "bytes-" + index
        write(roots, name, text)
        cases.push(request(roots, name, index === 10 ? "" : "id"))
    }
    write(roots, "unknown-proto", '["stored",{"__proto__":{"preserved":true},"future":[{"constructor":"data"}],"metadata":{"unknown":1}}]')
    cases.push(request(roots, "unknown-proto"))
    compare(cases)
})

test("primary/fallback source precedence includes duplicate loads, direct directories and absent/corrupt cases", { skip: !supported }, (t) => {
    const roots = fixture(t),
        cases = []
    write(roots, "primary", ["id", { from: "primary" }])
    write(roots, "primary", ["id", { from: "legacy" }], true)
    cases.push(request(roots, "primary"))
    for (const [name, content] of [
        ["missing", undefined],
        ["empty", ""],
        ["corrupt", "not json"],
        ["falsy", "null"]
    ]) {
        if (content !== undefined) write(roots, name, content)
        write(roots, name, ["id", { from: "legacy " + name }], true)
        cases.push(request(roots, name))
    }
    cases.push(request(roots, "both-absent"))
    write(roots, "both-corrupt", "invalid")
    write(roots, "both-corrupt", "invalid", true)
    cases.push(request(roots, "both-corrupt"))
    fs.mkdirSync(path.join(roots.dataRoot, "Bibles/directory.fsb"))
    cases.push(request(roots, "directory"))
    const results = compare(cases)
    assert.equal(results[0].reply.stats.loadPaths[0], results[0].reply.stats.loadPaths[1])
    assert.notEqual(results[1].reply.stats.loadPaths[0], results[1].reply.stats.loadPaths[1])
})

function capabilitySkip(t, capability, reason) {
    coverage.skippedCapabilities.push({ capability, reason })
    t.skip(reason)
}

test("native symlink regular/directory/dangling/loop decisions match actual source", { skip: !supported }, async (t) => {
    for (const kind of ["file", "directory", "dangling", "loop", ...(windows ? ["junction"] : [])])
        await t.test(kind, (t) => {
            const roots = fixture(t),
                file = write(roots, "real", ["id", { actual: true }]),
                link = path.join(roots.dataRoot, "Bibles", "link.fsb")
            const target = kind === "file" ? file : ["directory", "junction"].includes(kind) ? roots.dataRoot : kind === "loop" ? link : path.join(roots.dir, "absent")
            try {
                fs.symlinkSync(target, link, kind === "junction" ? "junction" : kind === "directory" ? "dir" : "file")
            } catch (error) {
                if (windows && ["EPERM", "EACCES", "ENOSYS"].includes(error.code)) return capabilitySkip(t, "symlink:" + kind, error.code + " creating a native symlink")
                throw error
            }
            compare([request(roots, "link")])
        })
})

test("native POSIX read permission denial matches source log/no-log decision", { skip: !supported || (windows && "POSIX permission bits do not configure Windows ACL denial") }, (t) => {
    const roots = fixture(t),
        denied = write(roots, "denied", ["id", { private: true }])
    fs.chmodSync(denied, 0)
    t.after(() => {
        if (fs.existsSync(denied)) fs.chmodSync(denied, 0o600)
    })
    try {
        fs.readFileSync(denied)
        return capabilitySkip(t, "permission-denial", "current identity can still read chmod(000) file")
    } catch (error) {
        assert.ok(["EACCES", "EPERM"].includes(error.code), error.message)
    }
    const source = oracle(roots.dataRoot, roots.documentsRoot)
    source.load({ name: "denied", id: "requested" })
    assert.equal(source.errors.length, 1)
    compare([request(roots, "denied")])
})

test("Windows actual private-file ACL read denial preserves source error/fallback decisions", { skip: !windows }, (t) => {
    const roots = fixture(t),
        denied = write(roots, "denied", ["id", { private: true }])
    const encodedPath = Buffer.from(denied).toString("base64")
    const acl = (remove) => {
        const script = `$ErrorActionPreference = 'Stop'; $p = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('${encodedPath}')); $acl = Get-Acl -LiteralPath $p; $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User; $rule = [Security.AccessControl.FileSystemAccessRule]::new($sid, [Security.AccessControl.FileSystemRights]::ReadData, [Security.AccessControl.AccessControlType]::Deny); $acl.${remove ? "RemoveAccessRuleSpecific" : "AddAccessRule"}($rule); Set-Acl -LiteralPath $p -AclObject $acl`
        return spawnSync("powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(script, "utf16le").toString("base64")], { encoding: "utf8", timeout: 15000, maxBuffer: 1024 * 1024, windowsHide: true })
    }
    const applied = acl(false)
    if (applied.error?.code === "ENOENT") return capabilitySkip(t, "Windows ACL denial", "powershell.exe is unavailable")
    assert.equal(applied.status, 0, applied.stderr || applied.error?.message)
    try {
        assert.throws(
            () => fs.readFileSync(denied),
            (error) => ["EPERM", "EACCES"].includes(error.code)
        )
        const source = oracle(roots.dataRoot, roots.documentsRoot)
        source.load({ name: "denied", id: "requested" })
        assert.equal(source.errors.length, 1)
        compare([request(roots, "denied")])
    } finally {
        const restored = acl(true)
        assert.equal(restored.status, 0, restored.stderr || restored.error?.message)
    }
})

test("native root spelling, separator normalization and case/Unicode lookup use actual OS files", { skip: !supported }, (t) => {
    const roots = fixture(t),
        cases = []
    for (const name of ["case", "CASE", "é", "e\u0301", "mixed/path", "mixed\\path", "dots/../same", "trailing/", "/leading", "//leading", "deep///nested//book"]) {
        write(roots, name, ["id", { name }])
        cases.push(request(roots, name))
    }
    const parents = [roots.dataRoot + path.sep, roots.dataRoot + path.sep + path.sep, roots.dataRoot + path.sep + ".", path.join(roots.dataRoot, "Bibles") + path.sep + ".."]
    if (windows) parents.push(roots.dataRoot.replaceAll("\\", "/"), path.toNamespacedPath(roots.dataRoot))
    for (const dataRoot of parents) cases.push(request({ ...roots, dataRoot }, "case"))
    // Absolute-looking child fragments are joined, not resolved over the root.
    if (windows) for (const name of ["D:\\elsewhere", "C:/elsewhere", "\\\\local-only\\share\\missing"]) cases.push(request(roots, name))
    compare(cases)
})

test("Windows extended root and alternate stream read real source/native bytes", { skip: !windows }, (t) => {
    const roots = fixture(t)
    write(roots, "extended", ["id", { extended: true }])
    const extended = { ...roots, dataRoot: path.toNamespacedPath(roots.dataRoot), documentsRoot: path.toNamespacedPath(roots.documentsRoot) }
    compare([request(extended, "extended")])
    const base = path.join(roots.dataRoot, "Bibles", "stream-base")
    fs.writeFileSync(base, "base bytes")
    const stream = base + ":scripture.fsb"
    try {
        fs.writeFileSync(stream, '["id",{"stream":true}]')
    } catch (error) {
        if (["EINVAL", "ENOTSUP", "EPERM", "EACCES"].includes(error.code)) return capabilitySkip(t, "alternate-data-stream", error.code + " creating stream on native temp filesystem")
        throw error
    }
    compare([request(roots, "stream-base:scripture")])
})

test("Windows device/pipe requests reject explicitly before filesystem IO", { skip: !windows }, (t) => {
    const roots = fixture(t)
    const cases = ["NUL", "nul", "CON", "COM1", "LPT¹", "NUL:stream"].map((name) => request(roots, name))
    cases.push({ ...request(roots, "unused"), dataRoot: "\\\\.\\pipe\\freeshow-scripture-test-never-open" })
    for (const result of native(cases)) {
        assert.equal(result.ok, false)
        assert.equal(result.error.code, "scripture_unsupported")
    }
    // The source could block on devices/pipes; deliberately do not open those
    // OS boundaries in the oracle. These are explicit safety gaps, not parity.
})

test("actual source second-read and fallback decisions stay observable under controlled real filesystem changes", { skip: !supported }, (t) => {
    const roots = fixture(t),
        file = write(roots, "changing", '["first",{"from":"first"}]')
    write(roots, "changing", '["legacy",{"from":"legacy"}]', true)
    let source = oracle(roots.dataRoot, roots.documentsRoot, { afterFirstRead: () => fs.writeFileSync(file, '["second",{"from":"second"}]') })
    assert.equal(source.load({ name: "changing", id: "requested" }).content[1].from, "second")
    assert.deepEqual(source.loads, [file, file])
    fs.writeFileSync(file, "invalid")
    source = oracle(roots.dataRoot, roots.documentsRoot, { afterFirstRead: () => fs.writeFileSync(file, '["new",{"from":"new"}]') })
    assert.equal(source.load({ name: "changing", id: "requested" }).content[1].from, "legacy")
    source = oracle(roots.dataRoot, roots.documentsRoot, { afterFirstRead: () => fs.unlinkSync(file) })
    assert.deepEqual(structuredClone(source.load({ name: "changing", id: "requested" })), { error: "not_found", id: "requested" })
    // The matching native replacement/removal barrier runs in the crate test;
    // no mutation/debug observer is exposed in the production service API.
})

test("directory preparation is explicit, read-only and allows fallback after a failed host mkdir", { skip: !supported }, (t) => {
    const roots = fixture(t)
    fs.rmdirSync(path.join(roots.dataRoot, "Bibles"))
    fs.rmdirSync(roots.dataRoot)
    write(roots, "legacy", ["id", { from: "legacy" }], true)
    const input = request(roots, "legacy"),
        [pending] = native([input])
    assert.equal(pending.complete, false)
    assert.equal(pending.reply.data, null)
    assert.equal(pending.reply.folderIntents[0].path, path.join(roots.dataRoot, "Bibles"))
    assert.equal(fs.existsSync(roots.dataRoot), false)
    const [actual] = native([{ ...input, afterDirectoryAttempt: true }])
    const source = oracle(roots.dataRoot, roots.documentsRoot, { failMkdir: true })
    assert.deepEqual(actual.reply.data, structuredClone(source.load(input.payload)))
    assert.equal(source.alerts.length, 1)
    assert.equal(fs.existsSync(roots.dataRoot), false)
    // A successful host attempt must be rerun through the service as well.
    fs.mkdirSync(path.join(roots.dataRoot, "Bibles"), { recursive: true })
    compare([input])
})

test("obstructing Bibles file requests the original failed mkdir before legacy fallback", { skip: !supported }, (t) => {
    const roots = fixture(t),
        folder = path.join(roots.dataRoot, "Bibles")
    fs.rmdirSync(folder)
    fs.writeFileSync(folder, "preexisting obstruction")
    write(roots, "legacy", ["id", { from: "legacy after failed mkdir" }], true)
    const input = request(roots, "legacy"),
        [pending] = native([input])
    assert.equal(pending.complete, false)
    assert.equal(pending.reply.folderIntents[0].path, folder)
    assert.deepEqual(pending.reply.stats.loadPaths, [])
    const source = oracle(roots.dataRoot, roots.documentsRoot),
        expected = structuredClone(source.load(input.payload))
    assert.deepEqual(source.mkdir, [folder])
    assert.equal(source.alerts.length, 1)
    const [actual] = native([{ ...input, afterDirectoryAttempt: true }])
    assert.deepEqual(actual.reply.data, expected)
    assert.deepEqual(actual.reply.stats.loadPaths, source.loads)
    assert.equal(fs.readFileSync(folder, "utf8"), "preexisting obstruction")
    coverage.comparisons++
})

test("symlink Bibles root requests the source mkdir attempt and then follows the real directory", { skip: !supported }, (t) => {
    const roots = fixture(t),
        folder = path.join(roots.dataRoot, "Bibles"),
        target = path.join(roots.dir, "real-bibles")
    fs.rmdirSync(folder)
    fs.mkdirSync(target)
    fs.writeFileSync(path.join(target, "book.fsb"), '["stored",{"from":"symlink root"}]')
    try {
        fs.symlinkSync(target, folder, "dir")
    } catch (error) {
        if (windows && ["EPERM", "EACCES", "ENOSYS"].includes(error.code)) return capabilitySkip(t, "symlink:root", error.code + " creating a native directory symlink")
        throw error
    }
    const input = request(roots, "book"),
        [pending] = native([input])
    assert.equal(pending.complete, false)
    assert.equal(pending.reply.folderIntents[0].path, folder)
    const source = oracle(roots.dataRoot, roots.documentsRoot),
        expected = structuredClone(source.load(input.payload))
    assert.deepEqual(source.mkdir, [folder])
    assert.equal(source.alerts.length, 0)
    const [actual] = native([{ ...input, afterDirectoryAttempt: true }])
    assert.deepEqual(actual.reply.data, expected)
    assert.deepEqual(actual.reply.stats.loadPaths, source.loads)
    assert.equal(fs.lstatSync(folder).isSymbolicLink(), true)
    coverage.comparisons++
})

test("source throws and unrepresentable values are explicit unsupported outcomes, never not_found success", { skip: !supported }, (t) => {
    const roots = fixture(t),
        cases = []
    for (const [index, text] of ["true", '"primitive"', '["id",{"books":[{"chapters":[{"verses":[{"value":"first"},null]}]}]}]', '["id",{"number":1e309}]', '["id",{"text":"\\ud800"}]'].entries()) {
        const name = "unsupported-" + index
        write(roots, name, text)
        cases.push(request(roots, name))
    }
    const results = native(cases)
    for (const result of results) {
        assert.equal(result.ok, false)
        assert.equal(result.error.code, "scripture_unsupported")
    }
    for (const item of cases.slice(0, 3)) assert.throws(() => oracle(item.dataRoot, item.documentsRoot).load(item.payload))
    assert.equal(oracle(roots.dataRoot, roots.documentsRoot).load(cases[3].payload).content[1].number, Infinity)
    assert.equal(oracle(roots.dataRoot, roots.documentsRoot).load(cases[4].payload).content[1].text.charCodeAt(0), 0xd800)
})

test("full-sized generated 31680-verse local Bible remains bounded and preserves original migration and metadata", { skip: !supported }, (t) => {
    const roots = fixture(t)
    const books = Array.from({ length: 66 }, (_, b) => ({ name: "Book " + b, chapters: Array.from({ length: 20 }, (_, c) => ({ chapter: c + 1, verses: Array.from({ length: 24 }, (_, v) => ({ verse: v + 1, value: `Verse ${b}:${c}:${v} — original UTF-8 敬拜 text and preserved unknown metadata.`, future: { keep: true } })) })) }))
    write(roots, "large", ["stored", { books, metadata: { customName: "Original 日本語", copyright: "generated fixture" }, future: { keep: "unchanged" } }])
    const [result] = compare([request(roots, "large")])
    assert.equal(result.reply.stats.visitedVerses, 31680)
    assert.equal(result.reply.stats.migratedVerses, 31680)
    coverage.generatedVerses += 31680
    assert.ok(result.reply.stats.allocatedBytes <= 128 * 1024 * 1024)
    assert.equal(result.reply.stats.loadPaths.length, 2)
    coverage.fullBible = { readBytes: result.reply.stats.readBytes, allocatedBytes: result.reply.stats.allocatedBytes, parsedNodes: result.reply.stats.parsedNodes, operations: result.reply.stats.operations }
})

test("other OS backends report explicit unsupported instead of claiming filesystem parity", { skip: supported }, () => {
    const [result] = native([{ dataRoot: os.tmpdir(), documentsRoot: os.tmpdir(), payload: { name: "test", id: "id" } }])
    assert.equal(result.ok, false)
    assert.equal(result.error.code, "scripture_unsupported")
})
