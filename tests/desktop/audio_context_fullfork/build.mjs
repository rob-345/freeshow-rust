// Complete production Svelte bundle, plus a declared test-only early hook.
import path from "node:path"
import fs from "node:fs"
import { fileURLToPath } from "node:url"
import { createHash } from "node:crypto"
import { execFileSync } from "node:child_process"
import { build } from "vite"
import configuration from "../../../vite.rust.config.mjs"
import { analyserIdentity } from "./identity.mjs"
const root = fileURLToPath(new URL("../../../", import.meta.url)),
    hook = path.join(root, "tests/desktop/audio_context_fullfork/hook.ts"),
    hash = (file) =>
        createHash("sha256")
            .update(fs.readFileSync(path.join(root, file)))
            .digest("hex")
const eager = process.env.FREESHOW_AUDIO_FULLFORK_EAGER === "1",
    originalAnalyser = eager ? execFileSync("git", ["show", "2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/frontend/audio/audioAnalyser.ts"], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1048576 }).replace(/\r\n/g, "\n") : undefined
const identity = analyserIdentity(originalAnalyser ?? fs.readFileSync(path.join(root, "src/frontend/audio/audioAnalyser.ts"), "utf8"))
if (eager && identity.analyserVariant !== "immutable-eager-control") throw new Error("Immutable eager source mismatch")
const originalPlayer = process.env.FREESHOW_AUDIO_FULLFORK_ORIGINAL_PLAYER === "1" ? execFileSync("git", ["show", "2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/frontend/audio/audioPlayer.ts"], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1048576 }).replace(/\r\n/g, "\n") : undefined
if (originalPlayer && createHash("sha256").update(originalPlayer).digest("hex") !== "0731f9e0de7a001e9bf71ff650b4b4e19bcd10e59474c54d258719d6d3fda918") throw new Error("Immutable player source mismatch")
await build({
    ...configuration,
    configFile: false,
    plugins: [
        {
            name: "declared-fullfork-audio-observer",
            enforce: "pre",
            transform(source, id) {
                if (id === path.join(root, "src/frontend/rust/main.ts")) return `import ${JSON.stringify(hook)};\n${source}`
                if (eager && id === path.join(root, "src/frontend/audio/audioAnalyser.ts")) return originalAnalyser
                if (originalPlayer && id === path.join(root, "src/frontend/audio/audioPlayer.ts")) return originalPlayer
            }
        },
        ...configuration.plugins
    ]
})
for (const entry of fs.readdirSync(path.join(root, "public"), { withFileTypes: true })) {
    if (!["index.html", "build"].includes(entry.name)) fs.cpSync(path.join(root, "public", entry.name), path.join(root, "dist-rust", entry.name), { recursive: true })
}
// Vite empties dist-rust; real standalone resources must be built afterwards.
execFileSync(process.execPath, ["scripts/migration/build-frontend.mjs", "--clients-only"], { cwd: root, stdio: "inherit", timeout: 120000 })
fs.mkdirSync(path.join(root, "test-output-rust/audio-context-fullfork"), { recursive: true })
fs.writeFileSync(
    path.join(root, "test-output-rust/audio-context-fullfork/build.json"),
    JSON.stringify(
        {
            harnessHistoricalBase: "ef7fadfbb1d9986ae119f9f2ec94290036516400",
            ...identity,
            immutableEagerOverlayRequested: eager,
            playerVariant: originalPlayer ? "immutable-player-control" : "native-asset-cors",
            effectivePlayerSha256: originalPlayer ? createHash("sha256").update(originalPlayer).digest("hex") : hash("src/frontend/audio/audioPlayer.ts"),
            head: execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim(),
            files: Object.fromEntries(["src/frontend/audio/audioAnalyser.ts", "src/frontend/audio/audioPlayer.ts", "src/frontend/audio/audioSender.ts", "src/frontend/rust/bridge.ts", "public/assets/pcm-worklet.js", "public/assets/soundtouch-processor.js", "tests/desktop/audio_context_fullfork/hook.ts"].map((file) => [file, hash(file)])),
            boundary: "Complete retained Svelte custom-protocol application; delegation-preserving test-only early browser/IPC observation. No native/source routing replacement."
        },
        null,
        2
    ) + "\n"
)
