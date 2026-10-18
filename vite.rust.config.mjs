import path from "node:path"
import { fileURLToPath } from "node:url"
import { defineConfig } from "vite"
import { svelte, vitePreprocess } from "@sveltejs/vite-plugin-svelte"

const root = fileURLToPath(new URL(".", import.meta.url))

export default defineConfig({
    root: path.join(root, "public"),
    publicDir: false,
    plugins: [
        {
            name: "freeshow-rust-entry",
            transformIndexHtml: {
                enforce: "pre",
                transform(html) {
                    return html.replace("/src/frontend/main.ts", "/src/frontend/rust/main.ts")
                }
            }
        },
        svelte({
            preprocess: vitePreprocess(),
            onwarn(warning, handler) {
                if (!warning.code.startsWith("a11y-")) handler(warning)
            }
        })
    ],
    resolve: {
        dedupe: ["svelte"],
        alias: {
            "/src": path.join(root, "src"),
            "@sentry/electron/renderer": path.join(root, "src/frontend/rust/sentry.ts")
        }
    },
    server: { host: "127.0.0.1", port: 1420, strictPort: true, fs: { allow: [root] } },
    build: { outDir: path.join(root, "dist-rust"), emptyOutDir: true, target: "es2021", chunkSizeWarningLimit: 8000 }
})
