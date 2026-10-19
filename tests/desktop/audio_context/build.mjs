// Bundle actual application modules, with only declared application/native sinks
// redirected. The immutable analyser is a separate original control build.
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import { build } from "esbuild"
import ts from "typescript"
const here = path.dirname(fileURLToPath(import.meta.url)),
    root = path.resolve(here, "../../.."),
    destination = path.resolve(process.argv[2] || path.join(root, "test-output-rust/audio-context-webkit/site"))
fs.mkdirSync(path.join(destination, "assets"), { recursive: true })
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975",
    analyser = path.join(root, "src/frontend/audio/audioAnalyser.ts"),
    boundaries = path.join(here, "boundaries.ts")
function original(file) {
    const result = spawnSync("git", ["show", `${revision}:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 2 * 1024 * 1024 })
    if (result.status !== 0) throw new Error(result.stderr)
    return result.stdout
}
const originalAnalyser = original("src/frontend/audio/audioAnalyser.ts")
if (createHash("sha256").update(originalAnalyser.replace(/\r\n/g, "\n")).digest("hex") !== "ff000b9ddcd06eb88069a210abe09fb293e75a34e116bc702de0045768146eff") throw new Error("Pinned analyser source changed")
const files = new Set()
for (const control of [false, true])
    await build({
        entryPoints: [path.join(here, "probe.ts")],
        outfile: path.join(destination, control ? "original.js" : "candidate.js"),
        bundle: true,
        platform: "browser",
        format: "esm",
        target: "es2021",
        sourcemap: true,
        define: { __ORIGINAL__: String(control) },
        plugins: [
            {
                name: "explicit-controlled-application-boundaries",
                setup(builder) {
                    builder.onResolve({ filter: /./ }, (args) => {
                        if (!args.importer || !args.path.startsWith(".")) return
                        const resolved = path.resolve(path.dirname(args.importer), args.path).replace(/\.ts$/, "")
                        if (["src/frontend/stores", "src/frontend/utils/request", "src/frontend/utils/common", "src/frontend/utils/language", "src/frontend/IPC/main", "src/frontend/components/helpers/output", "src/frontend/components/drawer/live/recorder", "src/frontend/audio/audioPlayer", "src/frontend/audio/audioMicrophone"].some((file) => resolved === path.join(root, file))) return { path: boundaries }
                        if (resolved === path.join(root, "src/frontend/audio/routing/audioRoutingInit")) return { path: resolved, namespace: "actual-deduplication" }
                    })
                    builder.onLoad({ filter: /.*/, namespace: "actual-deduplication" }, () => {
                        const file = "src/frontend/audio/routing/audioRoutingInit.ts",
                            source = fs.readFileSync(path.join(root, file), "utf8"),
                            ast = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true),
                            declaration = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "deduplicateConnections")
                        files.add(file)
                        return { contents: declaration.getText(ast), loader: "ts" }
                    })
                    builder.onLoad({ filter: /\.(ts|js)$/ }, (args) => {
                        if (!args.path.startsWith(path.join(root, "src"))) return
                        files.add(path.relative(root, args.path))
                        // Current native-only enum additions do not belong in
                        // the source control; use the pinned original enums in both.
                        if (args.path.startsWith(path.join(root, "src/types/"))) return { contents: original(path.relative(root, args.path)), loader: "ts" }
                        if (control && args.path === analyser) return { contents: originalAnalyser, loader: "ts", resolveDir: path.dirname(analyser) }
                    })
                }
            }
        ]
    })
for (const file of ["pcm-worklet.js", "soundtouch-processor.js"]) fs.copyFileSync(path.join(root, "public/assets", file), path.join(destination, "assets", file))
fs.writeFileSync(path.join(destination, "index.html"), '<!doctype html><meta charset="utf-8"><title>FreeShow Audio Context Harness</title><button id="start" style="position:fixed;left:10px;top:10px;width:500px;height:100px">Start genuine audio first use</button><pre id="result"></pre><script>const original=new URLSearchParams(location.search).get("original")==="1";import(original?"./original.js":"./candidate.js")</script>')
const receipt = {
    revision,
    originalAnalyserSha256: createHash("sha256").update(originalAnalyser.replace(/\r\n/g, "\n")).digest("hex"),
    candidateAnalyserSha256: createHash("sha256").update(fs.readFileSync(analyser)).digest("hex"),
    files: [...files].sort().map((file) => {
        const sha256 = createHash("sha256")
                .update(fs.readFileSync(path.join(root, file), "utf8").replace(/\r\n/g, "\n"))
                .digest("hex"),
            originalSha256 = createHash("sha256").update(original(file).replace(/\r\n/g, "\n")).digest("hex")
        if (!file.startsWith("src/types/") && path.join(root, file) !== analyser && sha256 !== originalSha256) throw new Error("Control dependency differs from immutable source: " + file)
        return { path: file, sha256, originalSha256, pinnedEnum: file.startsWith("src/types/") }
    }),
    assets: ["pcm-worklet.js", "soundtouch-processor.js"].map((file) => ({
        path: "public/assets/" + file,
        sha256: createHash("sha256")
            .update(fs.readFileSync(path.join(destination, "assets", file)))
            .digest("hex")
    }))
}
fs.writeFileSync(path.join(destination, "build.json"), JSON.stringify(receipt, null, 2))
console.log(JSON.stringify({ destination, sourceModules: receipt.files.length }))
