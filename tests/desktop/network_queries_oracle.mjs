import assert from "node:assert/strict"
import { createHash } from "node:crypto"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { root, upstreamFunctions, clean } from "../parity/oracle.mjs"

const source = fs.readFileSync(path.join(root, "src/electron/data/bonjour.ts"), "utf8")
assert.equal(createHash("sha256").update(source.replace(/\r\n/g, "\n")).digest("hex"), "6ff78829b6f8a383d7584fdbec69e134892bb3299fbd7189901e41bcd4c5e869")
const original = upstreamFunctions("src/electron/data/bonjour.ts", ["getLocalIPs"], { os })
const snapshot = { hostname: os.hostname(), interfaces: Object.entries(os.networkInterfaces()).map(([name, addresses]) => ({ name, addresses })) }
process.stdout.write(JSON.stringify({ runtime: process.version, ips: clean(original.getLocalIPs()), snapshot }) + "\n")
