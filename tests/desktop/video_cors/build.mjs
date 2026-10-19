// Complete retained Svelte application plus a delegation-only test observer.
import path from "node:path"
import fs from "node:fs"
import { fileURLToPath } from "node:url"
import { createHash } from "node:crypto"
import { execFileSync } from "node:child_process"
import { build } from "vite"
import configuration from "../../../vite.rust.config.mjs"
const root = fileURLToPath(new URL("../../../", import.meta.url)),
    file = "src/frontend/components/media/video/videoPlayer.ts",
    original = execFileSync("git", ["show", `2d94defcf98175e4f5c9504e7f4fd1d33c35c975:${file}`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1048576 }).replace(/\r\n/g, "\n"),
    digest = (bytes) => createHash("sha256").update(bytes).digest("hex"),
    control = process.env.FREESHOW_VIDEO_CORS_ORIGINAL === "1"
if (digest(original) !== "915987dd58d721b8a890a0ed7dee282daeac4ca9d18c3ceac27041da3afcc0b5") throw new Error("Immutable VideoPlayer source changed")
await build({
    ...configuration,
    configFile: false,
    plugins: [
        {
            name: "declared-video-cors-observer",
            enforce: "pre",
            transform(source, id) {
                if (id === path.join(root, "src/frontend/rust/main.ts")) return `import ${JSON.stringify(path.join(root, "tests/desktop/video_cors/hook.ts"))};\n${source}`
                if (control && id === path.join(root, file)) return original
            }
        },
        ...configuration.plugins
    ]
})
for (const entry of fs.readdirSync(path.join(root, "public"), { withFileTypes: true })) {
    if (!["index.html", "build"].includes(entry.name)) fs.cpSync(path.join(root, "public", entry.name), path.join(root, "dist-rust", entry.name), { recursive: true })
}
execFileSync(process.execPath, ["scripts/migration/build-frontend.mjs", "--clients-only"], { cwd: root, stdio: "inherit", timeout: 120000 })
fs.mkdirSync(path.join(root, "test-output-rust/video-cors"), { recursive: true })
fs.writeFileSync(
    path.join(root, "test-output-rust/video-cors/build.json"),
    JSON.stringify(
        {
            base: execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 }).trim(),
            developmentBase: "ed56f311c16d0771302dbc8e60d053998579e504",
            original: control,
            originalSha256: digest(original),
            effectiveVideoSha256: digest(control ? original : fs.readFileSync(path.join(root, file))),
            files: Object.fromEntries([file, "src/frontend/audio/audioAnalyser.ts", "src/frontend/audio/routing/audioRoutingManager.ts", "src/frontend/components/helpers/media.ts", "src/frontend/rust/bridge.ts", "tests/desktop/video_cors/hook.ts"].map((name) => [name, digest(fs.readFileSync(path.join(root, name)))])),
            boundary: "Full retained custom-protocol editor, real bridge/native grants/AudioAnalyser/routing; delegation-only constructor/source observations and declared fixture routing/seeks. No native source, codec, sink or API replacements."
        },
        null,
        2
    ) + "\n"
)
