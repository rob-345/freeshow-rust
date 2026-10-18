import fs from "node:fs"
import path from "node:path"
import { build } from "vite"
import { root } from "./oracle-path.mjs"

await build({ configFile: path.join(root, "vite.rust.config.mjs") })
// Upstream uses relative paths to fonts, translations, import logos and proto
// definitions at runtime. Vite's publicDir is disabled to avoid copying the
// development index and unrelated Electron-only pages over the compiled index.
for (const entry of fs.readdirSync(path.join(root, "public"), { withFileTypes: true })) {
    if (["index.html", "build"].includes(entry.name)) continue
    fs.cpSync(path.join(root, "public", entry.name), path.join(root, "dist-rust", entry.name), { recursive: true })
}
