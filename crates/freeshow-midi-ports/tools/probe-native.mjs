import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import { createRequire } from "node:module"
const root = fileURLToPath(new URL("../../../", import.meta.url))
const require = createRequire(new URL("../../../package.json", import.meta.url))
const binary = process.env.FREESHOW_MIDI_PORTS_PARITY || path.join(root, "target/debug/freeshow-midi-ports-parity" + (process.platform === "win32" ? ".exe" : ""))
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const source = spawnSync("git", ["show", `${revision}:src/electron/utils/midi.ts`], { cwd: root, encoding: "utf8", timeout: 10000 })
if (source.status !== 0) throw new Error(source.stderr)
const native = spawnSync(binary, [], { cwd: root, input: '{"op":"native"}\n', encoding: "utf8", timeout: 10000 })
if (native.status !== 0) throw new Error(native.stderr)
// A separate process initializes the actual installed jazz-midi node backend.
// Enumeration only: no input is connected and no bytes are sent to an OS port.
const original = spawnSync(
    process.execPath,
    [
        "-e",
        `
const JZZ=require("jzz"),vm=require("node:vm"),ts=require("typescript");
const source=JSON.parse(process.argv[1]);
(async()=>{const j=await JZZ({engine:"node"});const ast=ts.createSourceFile("midi.ts",source,ts.ScriptTarget.Latest,true);
const code=["getMidiInputs","getMidiOutputs"].map(name=>ast.statements.find(n=>ts.isFunctionDeclaration(n)&&n.name?.text===name).getText(ast).replace(/^export\\s+/,"")).join("\\n");
const context=vm.createContext({JZZ});new vm.Script(ts.transpileModule(code+";globalThis.result={getMidiInputs,getMidiOutputs}",{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText).runInContext(context);
console.log(JSON.stringify({engine:j.info().engine,inputs:context.result.getMidiInputs(),outputs:context.result.getMidiOutputs()}));JZZ().close();})().catch(e=>{console.error(e);process.exitCode=1});
`,
        JSON.stringify(source.stdout)
    ],
    { cwd: root, encoding: "utf8", timeout: 10000 }
)
if (original.status !== 0) throw new Error(original.stderr)
const sha256 = (data) => createHash("sha256").update(data).digest("hex")
const receipt = {
    recordedAt: new Date().toISOString(),
    platform: process.platform,
    arch: process.arch,
    node: process.version,
    baseline: revision,
    sourceSha256: sha256(source.stdout),
    jzz: require("jzz/package.json").version,
    jzzSha256: sha256(fs.readFileSync(require.resolve("jzz"))),
    jazzMidi: require("jazz-midi/package.json").version,
    midir: "0.11.1",
    nativeBinarySha256: sha256(fs.readFileSync(binary)),
    native: JSON.parse(native.stdout),
    nativeStderr: native.stderr.trim(),
    original: JSON.parse(original.stdout),
    originalStderr: original.stderr.trim(),
    scope: "Real enumeration only; no successful hardware open, MIDI delivery, loopback, full-frame SysEx or Electron GUI parity established."
}
if (process.argv[2]) fs.writeFileSync(process.argv[2], JSON.stringify(receipt, null, 2) + "\n")
console.log(JSON.stringify(receipt, null, 2))
