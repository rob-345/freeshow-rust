import fs from "node:fs"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { root } from "./oracle-path.mjs"

const read = (file) => JSON.parse(fs.readFileSync(path.join(root, file), "utf8"))
const inventory = read("docs/migration/upstream-inventory.json")
const status = read("docs/migration/parity-status.json")
if (inventory.upstream.commit !== status.baseline) throw new Error("Parity status refers to a different upstream baseline")
const binary = process.env.FREESHOW_CORE_BIN || path.join(root, "target/debug", process.platform === "win32" ? "freeshow-core.exe" : "freeshow-core")
const result = spawnSync(binary, [], { input: '{"op":"capabilities"}\n', encoding: "utf8" })
if (result.error || result.status !== 0) throw new Error("Build freeshow-core before generating the parity report")
const implemented = JSON.parse(result.stdout).value.main
for (const command of implemented) if (!inventory.mainCommands.includes(command)) throw new Error(`Unknown command claimed: ${command}`)
const missing = inventory.mainCommands.filter((command) => !implemented.includes(command))
for (const feature of status.featureAreas) {
    if (!["pending", "partial", "verified"].includes(feature.status)) throw new Error(`Invalid status: ${feature.id}`)
    for (const test of feature.tests) if (!fs.existsSync(path.join(root, test))) throw new Error(`Missing test evidence: ${test}`)
    if (feature.status === "verified" && (!feature.tests.length || feature.remaining?.length)) throw new Error(`Incomplete evidence for verified feature ${feature.id}`)
}
const incomplete = status.featureAreas.filter((feature) => feature.status !== "verified")
console.log(`Baseline: ${inventory.upstream.version} (${inventory.upstream.commit})`)
console.log(`Core command handlers: ${implemented.length}/${inventory.mainCommands.length}. Handler existence is not a parity measurement.`)
for (const feature of status.featureAreas) console.log(`${feature.status.padEnd(8)} ${feature.id}${feature.remaining ? `: ${feature.remaining.join("; ")}` : ""}`)
console.log(`Unported core commands (${missing.length}): ${missing.join(", ")}`)
console.log(`Full parity gate: ${incomplete.length || missing.length ? "FAIL — migration incomplete" : "PASS"}`)
if (process.argv.includes("--require-complete") && (incomplete.length || missing.length)) process.exitCode = 1
