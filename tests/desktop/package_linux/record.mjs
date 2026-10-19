// Durable evidence for an already verified artifact; this does not build/run it.
import fs from "node:fs"
import path from "node:path"
import { createHash } from "node:crypto"
import { execFileSync } from "node:child_process"
import { fileURLToPath } from "node:url"
const root = fileURLToPath(new URL("../../../", import.meta.url)),
    artifacts = process.argv[2],
    verification = JSON.parse(fs.readFileSync(path.join(artifacts, "verification.json"), "utf8")),
    gui = JSON.parse(fs.readFileSync(path.join(artifacts, "gui/gui-receipt.json"), "utf8")),
    digest = (bytes) => createHash("sha256").update(bytes).digest("hex"),
    command = (name, args) => execFileSync(name, args, { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 }).trim(),
    inputs = [
        "Cargo.lock",
        "package-lock.json",
        "src-tauri/tauri.conf.json",
        "src-tauri/Cargo.toml",
        "src-tauri/src/main.rs",
        "crates/freeshow-ltc/src/backend.rs",
        "vite.rust.config.mjs",
        "scripts/migration/build-frontend.mjs",
        "config/packaging/linux-deb.json",
        "config/packaging/prepare-linux-deb.mjs",
        "config/packaging/build-linux-deb.sh",
        "config/packaging/LOCAL-REVIEW.md",
        "tests/desktop/package_linux/verify.py",
        "tests/desktop/package_linux/run.py",
        "tests/desktop/package_linux/probe.js",
        "tests/desktop/package_linux/record.mjs",
        "tests/desktop/timecode_probe.js"
    ]
if (!verification.ok || !gui.ok || verification.packageSha256 !== gui.packageSha256 || gui.actualFixtureCleanup.ownedChildren || !gui.normalCloseExitedBeforeFixtureTermination || !gui.actualCodecOrigin?.sealedMappedSnapshots?.length) throw new Error("Unsuccessful or mismatched actual package evidence")
const log = fs.readFileSync(path.join(artifacts, "build.log"), "utf8")
if (Buffer.byteLength(log) > 1024 * 1024 || !log.includes("Finished") || !log.includes(".deb")) throw new Error("Missing actual normal build log")
const receipt = {
    sourceHead: command("git", ["rev-parse", "HEAD"]),
    trackedDiffAtRecord: command("git", ["diff", "--name-only"]),
    inputs: inputs.map((file) => ({ file, sha256: digest(fs.readFileSync(path.join(root, file))) })),
    toolchain: { node: process.versions, cargo: command("cargo", ["--version"]), rustc: command("rustc", ["-Vv"]), tauriCli: JSON.parse(fs.readFileSync(path.join(root, "node_modules/@tauri-apps/cli/package.json"), "utf8")).version, dpkg: command("dpkg-deb", ["--version"]) },
    host: fs.readFileSync("/etc/os-release", "utf8"),
    buildLogSha256: digest(Buffer.from(log)),
    buildLog: log,
    guiLaunchLog: fs.readFileSync(path.join(artifacts, "gui-run.log"), "utf8"),
    previousFailedFixtures: ["gui-failed-session-timeout", "gui-failed-http-name"].map((name) => {
        const bytes = fs.readFileSync(path.join(artifacts, name, "gui-receipt.json"))
        if (bytes.length > 128 * 1024) throw new Error("Failed fixture receipt budget")
        return { name, sha256: digest(bytes), receipt: JSON.parse(bytes.toString("utf8")) }
    }),
    verification,
    gui
}
fs.writeFileSync(path.join(root, "tests/desktop/package_linux/receipt.json"), JSON.stringify(receipt, null, 2) + "\n")
console.log(JSON.stringify({ packageSha256: verification.packageSha256, binarySha256: verification.binarySha256, sourceHead: receipt.sourceHead, receiptSha256: digest(Buffer.from(JSON.stringify(receipt, null, 2) + "\n")) }))
