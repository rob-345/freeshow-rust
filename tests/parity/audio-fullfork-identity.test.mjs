import test from "node:test"
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { execFileSync } from "node:child_process"
import { analyserIdentity, EAGER_SHA256, LAZY_SHA256 } from "../desktop/audio_context_fullfork/identity.mjs"
const root = fileURLToPath(new URL("../../", import.meta.url))
const source = execFileSync("git", ["show", "2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/frontend/audio/audioAnalyser.ts"], { cwd: root, encoding: "utf8", maxBuffer: 1048576, timeout: 10000 }).replace(/\r\n/g, "\n")

test("actual immutable eager source identifies itself without an override, including CRLF", () => {
    const expected = { analyserVariant: "immutable-eager-control", effectiveAnalyserSha256: EAGER_SHA256 }
    assert.deepEqual(analyserIdentity(source), expected)
    assert.deepEqual(analyserIdentity(source.replace(/\n/g, "\r\n")), expected)
    const current = analyserIdentity(fs.readFileSync(path.join(root, "src/frontend/audio/audioAnalyser.ts"), "utf8"))
    assert.ok([EAGER_SHA256, LAZY_SHA256].includes(current.effectiveAnalyserSha256))
})

test("unknown or oversized source cannot be reported as the lazy candidate", () => {
    assert.throws(() => analyserIdentity(source + "\n// changed\n"), /Unsupported effective analyser source/)
    assert.throws(() => analyserIdentity("x".repeat(1048577)), /Bounded analyser source/)
    assert.throws(() => analyserIdentity(null), /Bounded analyser source/)
})

test("actual controller assertion follows archived effective identities rather than EAGER environment", () => {
    const archive = JSON.parse(fs.readFileSync(path.join(root, "tests/desktop/audio_context_fullfork/receipt.json"), "utf8"))
    const code = `import ast,json,sys,os
source=ast.parse(open(sys.argv[1],encoding="utf-8").read())
node=next(n for n in source.body if isinstance(n,ast.FunctionDef) and n.name=="expected_cold_contexts")
space={}
exec(compile(ast.Module(body=[node],type_ignores=[]),sys.argv[1],"exec"),space)
f=space["expected_cold_contexts"]
a,b=json.load(sys.stdin)
os.environ["FREESHOW_AUDIO_FULLFORK_EAGER"]="1"
assert f(a)==0
os.environ.pop("FREESHOW_AUDIO_FULLFORK_EAGER")
assert f(b)==1
for invalid in [dict(a,analyserVariant="immutable-eager-control"),dict(b,effectiveAnalyserSha256="0"*64)]:
 try: f(invalid)
 except AssertionError: pass
 else: raise AssertionError("inconsistent identity accepted")
print("actual controller identities verified")
`
    const result = execFileSync(process.platform === "win32" ? "python" : "python3", ["-c", code, path.join(root, "tests/desktop/audio_context_fullfork/run.py")], {
        input: JSON.stringify([archive.ordinary.lazy.build, archive.ordinary.eager.build]),
        encoding: "utf8",
        maxBuffer: 1048576,
        timeout: 10000
    })
    assert.equal(result.trim(), "actual controller identities verified")
})
