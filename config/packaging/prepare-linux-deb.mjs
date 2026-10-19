// Native developer packaging input only. Nothing here is renderer/IPC authority.
import fs from "node:fs"
import path from "node:path"
import { createHash } from "node:crypto"
import { execFileSync } from "node:child_process"
import { fileURLToPath } from "node:url"
const root = fileURLToPath(new URL("../../", import.meta.url)),
    base = process.env.FREESHOW_PACKAGE_ROOT || "/mnt/data/cache/freeshow-rust-build/linux-deb-package",
    stage = path.join(base, "stage"),
    docs = path.join(stage, "docs"),
    sha = (bytes) => createHash("sha256").update(bytes).digest("hex")
if (process.platform !== "linux" || process.arch !== "x64") throw new Error("This first package is Linux x86_64 only")
if (!path.isAbsolute(base) || !path.resolve(base).startsWith("/mnt/data/")) throw new Error("Package/cache staging must be under /mnt/data")
fs.mkdirSync(docs, { recursive: true })
const library = fs.readFileSync(path.join(root, "node_modules/libltc-wrapper/libltc/bin/libltc.so.11")),
    ltcHash = "f02c75f982fe01d16f345875d2b09404b8b326bb847f2b5456616f6d0227d09f",
    pinned = fs.readFileSync(path.join(root, "crates/freeshow-ltc/src/lib.rs"), "utf8")
if (library.length > 1024 * 1024 || sha(library) !== ltcHash || !pinned.includes(ltcHash) || library.subarray(0, 4).toString("hex") !== "7f454c46" || library[4] !== 2 || library.readUInt16LE(18) !== 62) throw new Error("Pinned verified Linux x64 LTC resource mismatch")
const codec = path.join(stage, "codecs/libltc.so.11")
fs.mkdirSync(path.dirname(codec), { recursive: true })
fs.writeFileSync(codec, library, { mode: 0o644 })
const inventory = [],
    MAX_FILES = 8192,
    MAX_BYTES = 64 * 1024 * 1024
let total = 0,
    files = 0
function copyNotice(source, relative) {
    const stat = fs.statSync(source)
    if (!stat.isFile() || stat.size > 2 * 1024 * 1024 || files >= MAX_FILES || total + stat.size > MAX_BYTES) throw new Error("Package notice bounds exceeded: " + relative)
    const bytes = fs.readFileSync(source)
    if (bytes.length !== stat.size || bytes.includes(0)) throw new Error("Unexpected/mutating notice input: " + relative)
    const destination = path.join(docs, relative)
    fs.mkdirSync(path.dirname(destination), { recursive: true })
    fs.writeFileSync(destination, bytes)
    total += bytes.length
    files++
    return { file: relative, bytes: bytes.length, sha256: sha(bytes) }
}
for (const [source, destination] of [
    ["LICENSE", "GPL-3.0.txt"],
    ["config/packaging/LOCAL-REVIEW.md", "LOCAL-REVIEW.md"],
    ["crates/freeshow-ltc/vendor/COPYING.LESSER", "libltc/COPYING.LESSER"],
    ["crates/freeshow-ltc/vendor/COPYING", "libltc/COPYING"],
    ["crates/freeshow-ltc/vendor/NOTICE.md", "libltc/NOTICE.md"],
    ["crates/freeshow-ltc/vendor/ltc.h", "libltc/ltc.h"],
    ["node_modules/libltc-wrapper/package.json", "libltc/wrapper-package.json"],
    ["node_modules/libltc-wrapper/README.md", "libltc/wrapper-README.md"],
    ["node_modules/libltc-wrapper/libltc/build.md", "libltc/wrapper-build.md"],
    ["vendor/socketioxide/FREESHOW-NOTICE.md", "application/socketioxide-NOTICE.md"],
    ["crates/freeshow-audio/MUSIC-METADATA-LICENSE.txt", "application/MUSIC-METADATA-LICENSE.txt"],
    ["crates/freeshow-exif/NOTICE", "application/exif-NOTICE"],
    ["crates/freeshow-midi/NOTICE", "application/midi-NOTICE"],
    ["crates/freeshow-cover-http/NOTICES.md", "application/cover-http-NOTICES.md"]
])
    copyNotice(path.join(root, source), destination)
function collect(folder, kind, name, version, license, repository) {
    const names = fs.readdirSync(folder, { withFileTypes: true }).filter((entry) => entry.isFile() && /^(licen[cs]e|copying|copyright|notice|unlicense)([._-].*)?$/i.test(entry.name))
    if (names.length > 16) throw new Error("Unexpected license-file count")
    const notices = names.map((entry) => copyNotice(path.join(folder, entry.name), `${kind}/${encodeURIComponent(name + "@" + version)}/${entry.name}`))
    inventory.push({ kind, name, version, license: license ?? null, repository: repository ?? null, notices, missingRootNotice: notices.length === 0 })
}
const metadata = JSON.parse(execFileSync("cargo", ["metadata", "--manifest-path", "src-tauri/Cargo.toml", "--format-version", "1", "--filter-platform", "x86_64-unknown-linux-gnu", "--locked", "--offline"], { cwd: root, encoding: "utf8", timeout: 120000, maxBuffer: 32 * 1024 * 1024 }))
if (metadata.packages.length > 2048) throw new Error("Rust dependency inventory bound")
for (const dependency of metadata.packages) collect(path.dirname(dependency.manifest_path), "rust", dependency.name, dependency.version, dependency.license, dependency.repository)
const lock = JSON.parse(fs.readFileSync(path.join(root, "package-lock.json"), "utf8")),
    modules = Object.entries(lock.packages).filter(([key]) => key.startsWith("node_modules/"))
if (modules.length > 4096) throw new Error("Node dependency inventory bound")
for (const [key, dependency] of modules) {
    if (key.split("/").includes("..")) throw new Error("Invalid locked package path")
    const directory = path.join(root, key)
    if (!fs.existsSync(directory)) {
        inventory.push({ kind: "node", name: key, version: dependency.version, license: dependency.license ?? null, notInstalled: true })
        continue
    }
    const manifestPath = path.join(directory, "package.json"),
        manifest = fs.existsSync(manifestPath) ? JSON.parse(fs.readFileSync(manifestPath, "utf8")) : {}
    collect(directory, "node", manifest.name || key, manifest.version || dependency.version || "unknown", manifest.license || dependency.license, manifest.repository)
}
const source = {
    sourceHead: execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8", timeout: 10000 }).trim(),
    applicationLicense: "GPL-3.0-only",
    packageVersion: "0.1.0",
    runtimeVersion: "1.6.7-beta.2-rust.1",
    node: process.version,
    ltc: { bytes: library.length, sha256: ltcHash, wrapperRevision: "9637e39773317e82e4112ff42b2c63afd144549f", upstreamGitBlobSha1: "b3ed83fb52084710a49869a6448168da04e9d9a9", correspondingBinarySourceUnresolved: true, wrapperLicenseDeclaration: "MIT", wrapperLicenseTextMissingAtPinnedRevision: true },
    scope: "Conservative installed dependency notice superset for private unsigned local review; no external release/license-completeness claim",
    noticeFiles: files,
    noticeBytes: total,
    inventory
}
fs.writeFileSync(path.join(docs, "dependency-notices.json"), JSON.stringify(source, null, 2) + "\n")
const config = {
    bundle: {
        resources: { "../dist-rust/remotes/": "remotes/", [codec]: "codecs/libltc.so.11" },
        linux: { deb: { files: { "/usr/share/doc/freeshow-desktop": docs } } }
    }
}
fs.writeFileSync(path.join(stage, "tauri-stage.json"), JSON.stringify(config, null, 2) + "\n")
console.log(JSON.stringify({ stage, codecSha256: sha(fs.readFileSync(codec)), dependencies: inventory.length, noticeFiles: files, noticeBytes: total, config: path.join(stage, "tauri-stage.json") }))
