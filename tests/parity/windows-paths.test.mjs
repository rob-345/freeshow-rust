import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { test, after } from "node:test"
import { root, sourceFunctions, sourceSha256, certifySource, win32 } from "./lib/windows-paths-oracle.mjs"

const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-win-paths-pure-"))
after(() => fs.rmSync(temporary, { recursive: true, force: true }))
const suffix = process.platform === "win32" ? ".exe" : ""
const binary = process.env.FREESHOW_WINDOWS_PATHS_BIN || path.join(temporary, `driver${suffix}`)
const fixture = path.join(root, "tests/parity/fixtures/browse-windows-paths-driver.rs")
function rustc(args) {
    const result = spawnSync(process.env.RUSTC || "rustc", ["--edition=2021", "-C", "debuginfo=0", ...args, fixture], { encoding: "utf8", timeout: 30000, maxBuffer: 4 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
}
if (!process.env.FREESHOW_WINDOWS_PATHS_BIN) rustc(["-o", binary])
const hex = (value) => Buffer.from(value).toString("hex")
const unhex = (value) => Buffer.from(value, "hex").toString("utf8")
function native(cases) {
    const result = spawnSync(binary, [], { input: cases.map((item) => [item[0], ...item.slice(1).map(hex)].join("\t")).join("\n") + "\n", encoding: "utf8", timeout: 10000, maxBuffer: 32 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    assert.ok(result.stdout.endsWith("\n"), "complete native protocol receipt")
    const replies = result.stdout
        .slice(0, -1)
        .split("\n")
        .map((line) => {
            const [status, ...fields] = line.split("\t")
            return status === "ok" ? { data: fields.map(unhex) } : { error: fields[0] }
        })
    assert.equal(replies.length, cases.length)
    return replies
}
function compare(cases) {
    // Batch independently; every expected output is the real Node function.
    for (let start = 0; start < cases.length; start += 128) {
        const chunk = cases.slice(start, start + 128)
        const replies = native(chunk)
        for (const [index, item] of chunk.entries()) {
            assert.equal(replies[index].error, undefined, JSON.stringify(item))
            if (item[0] === "P") {
                const actual = replies[index].data
                const parsed = win32.parse(item[1])
                const expected = [parsed.root, sourceFunctions.getParentFolderName(item[1]), win32.basename(item[1]), win32.extname(item[1]), parsed.name]
                assert.deepEqual(actual, expected, JSON.stringify(item))
                assert.equal(actual[1], parsed.dir)
                assert.equal(actual[3].substring(1).toLowerCase(), sourceFunctions.getExtension(item[1]))
            } else assert.deepEqual(replies[index].data, [win32.join(item[1], item[2])], JSON.stringify(item))
        }
    }
}

test("actual immutable source certification accepts LF/CRLF transport and rejects other mutations", () => {
    const checkedOut = fs.readFileSync(path.join(root, "src/electron/utils/files.ts"), "utf8"),
        normalized = checkedOut.replace(/\r\n/g, "\n"),
        crlf = normalized.replace(/\n/g, "\r\n")
    for (const working of [normalized, crlf]) for (const immutable of [normalized, crlf]) assert.equal(certifySource(working, immutable), normalized)
    const mutated = normalized + "\n// non-line-ending mutation control\n"
    assert.throws(() => certifySource(mutated, normalized), /must match immutable/)
    assert.throws(() => certifySource(normalized, mutated), /must match immutable/)
    assert.throws(() => certifySource(mutated, mutated), /must retain its pinned SHA/)
    assert.throws(() => certifySource(normalized + "\r", normalized), /must match immutable/)
})

test("actual Node and immutable retained source preserve ordinary drive/relative roots, trailing and repeated separators, dots and Unicode", () => {
    const bases = ["", ".", "..", "...", "....", ".hidden", "..hidden", ".hidden.", "name.", "name..", "multi.part.PNG", "音楽 🙌.FLAC", "e\u0301.İ", "Σ.ẞ", "__proto__", " leading space .x", "tail "]
    const roots = ["", "C:\\", "c:/", "Z:/", "D:\\\\", "relative\\", "../", "./a//"]
    const cases = roots.flatMap((prefix) => bases.flatMap((base) => ["", "/", "\\", "//", "\\\\"].map((tail) => ["P", prefix + base + tail]))).filter(([, value]) => !value.startsWith("/") && !value.startsWith("\\"))
    compare(cases)
    assert.equal(sourceSha256, "98e563c55026d2dbbdd217ee9b94962ed7fc8b82d893795355bd7dc0f54fc5de")
})

test("parse.dir retained contract is distinct from dirname fallback for relative leaves", () => {
    const leaves = ["foo", "", ".", "..", "音楽.flac"]
    compare(leaves.map((item) => ["P", item]))
    for (const item of leaves) {
        assert.equal(sourceFunctions.getParentFolderName(item), "")
        assert.equal(win32.dirname(item), ".")
    }
    // Existing POSIX parent('foo')=='' agrees with this retained parse.dir caller.
    // dirname would be a distinct helper contract, not a proven source gap.
})

test("independent generated Node corpus covers raw components and normalized joins without filesystem context", () => {
    let seed = 0x51ce2026
    const next = (limit) => {
        seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
        return seed % limit
    }
    const atoms = ["a", "b.txt", ".", "..", "...", ".x", "x..", "😀", "音楽", "é", "e\u0301", "Σ.İ", "zero0", "__proto__", " white space ", 'quote"name']
    const slashes = ["/", "\\", "//", "\\\\", "/\\", "\\/"]
    const make = (absolute) => {
        let value = absolute ? ["C:/", "D:\\", "z:/"][next(3)] : ""
        const count = 1 + next(9)
        for (let i = 0; i < count; i++) {
            if (i) value += slashes[next(slashes.length)]
            value += atoms[next(atoms.length)]
        }
        if (next(3) === 0) value += slashes[next(slashes.length)]
        return value
    }
    const cases = []
    for (let i = 0; i < 4096; i++) {
        cases.push(["P", make(next(2) === 0)])
        cases.push(["J", make(next(2) === 0), make(false)])
    }
    compare(cases)
})

test("real Node join dot normalization, root clamping and terminal separator match bounded new functions", () => {
    const parents = ["", ".", "..", "a", "a/../", "a//b", "C:/", "C:/a//", "c:\\a\\..\\b\\", "Z:\\..\\.."]
    const children = ["", ".", "..", "../", "../../b", "bar/", "..\\", "./", "one//../two\\", "a/../../../b/", "音楽.FLAC"]
    compare(parents.flatMap((parent) => children.map((child) => ["J", parent, child])))
})

test("capability exclusions remain explicit even where actual Node can lexically produce a string", () => {
    const paths = ["C:", "C:foo", "/rooted", "\\rooted", "\\\\server\\share", "//server/share", "\\\\?\\C:\\file", "\\\\.\\pipe\\name", "foo:stream"]
    const replies = native(paths.map((item) => ["P", item]))
    for (const [index, item] of paths.entries()) {
        assert.equal(replies[index].error, "unsupported_windows_path", item)
        assert.equal(typeof win32.parse(item).dir, "string")
    }
    for (const item of [
        ["J", "C:/base", "D:/child"],
        ["J", "C:/base", "/rooted"],
        ["J", "", "\\rooted"]
    ])
        assert.equal(native([item])[0].error, "unsupported_windows_path")
    assert.equal(native([["P", "nul\0name"]])[0].error, "invalid_request")
    assert.equal(win32.basename("nul\0name"), "nul\0name")
})

test("input/output/component limits report errors without successful truncation", () => {
    const maximum = "C:\\" + "x".repeat(32768 - 3)
    compare([
        ["P", maximum],
        ["P", Array(4096).fill("x").join("/")]
    ])
    for (const item of [
        ["P", "x".repeat(32769)],
        ["P", "x/".repeat(4096)],
        ["J", maximum, "x"],
        ["J", "x".repeat(32767), "x"]
    ])
        assert.equal(native([item])[0].error, "browse_resource_limit")
})

test("native Rust unit checks execute actual borrowed-slice and resource/error paths", () => {
    const executable = path.join(temporary, `unit${suffix}`)
    rustc(["--test", "-o", executable])
    const result = spawnSync(executable, [], { encoding: "utf8", timeout: 10000, maxBuffer: 2 * 1024 * 1024 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stdout + result.stderr)
    assert.match(result.stdout, /3 passed/)
})
