import assert from "node:assert/strict"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { root } from "./oracle.mjs"

export const binary = process.env.FREESHOW_CORE_BIN || path.join(root, "target", "debug", process.platform === "win32" ? "freeshow-core.exe" : "freeshow-core")
export function rust(cases, args = []) {
    const child = spawnSync(binary, args, { input: cases.map((input) => JSON.stringify(input)).join("\n") + "\n", encoding: "utf8", maxBuffer: 64 * 1024 * 1024, timeout: 30000 })
    assert.equal(child.error, undefined)
    assert.equal(child.status, 0, child.stderr)
    return child.stdout
        .trim()
        .split("\n")
        .map((line) => JSON.parse(line))
}
export const request = (command, data, listenerId) => ({ op: "runtime", request: { channel: "MAIN", data: { channel: command, data }, listenerId } })
