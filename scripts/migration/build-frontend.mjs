import fs from "node:fs"
import path from "node:path"
import { build } from "vite"
import { root } from "./oracle-path.mjs"
import { getServerViteConfig } from "../../config/building/vite.config.servers.mjs"

if (!process.argv.includes("--clients-only")) {
    await build({ configFile: path.join(root, "vite.rust.config.mjs") })
    // Upstream uses relative paths to fonts, translations, import logos and proto
    // definitions at runtime. Vite's publicDir is disabled to avoid copying the
    // development index and unrelated Electron-only pages over the compiled index.
    for (const entry of fs.readdirSync(path.join(root, "public"), { withFileTypes: true })) {
        if (["index.html", "build"].includes(entry.name)) continue
        fs.cpSync(path.join(root, "public", entry.name), path.join(root, "dist-rust", entry.name), { recursive: true })
    }
}

// Build the unchanged standalone clients with their original Svelte/Vite
// configuration. Their HTTP server needs real files independently of the
// desktop frontend's embedded asset protocol.
for (const id of ["remote", "stage", "controller"]) {
    const config = getServerViteConfig(id, true)
    const destination = path.join(root, "dist-rust", "remotes", id)
    await build({
        ...config,
        configFile: false,
        plugins: config.plugins.filter((plugin) => plugin.name !== "copy-server-files"),
        build: { ...config.build, outDir: destination }
    })
    for (const name of ["index.html", "manifest.json"]) fs.copyFileSync(path.join(root, "src/server", id, name), path.join(destination, name))
    for (const name of ["icon.png", "sw.js"]) fs.copyFileSync(path.join(root, "src/server", name), path.join(destination, name))
    if (id === "stage") {
        fs.mkdirSync(path.join(destination, "html"), { recursive: true })
        for (const name of ["navigation.js", "show.css"]) fs.copyFileSync(path.join(root, "src/server/stage/html", name), path.join(destination, "html", name))
    }
}
