// Private untouched N-API oracle build; never writes node_modules or installs tools.
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
const root = fileURLToPath(new URL("../../../", import.meta.url))
if (process.versions.node !== "22.21.1") throw Error("Use pinned Node22.21.1 for the oracle build")
if (process.platform !== "linux" || process.arch !== "x64") throw Error("Only the bundled Linux x64 library has been verified")
const original = path.join(root, "node_modules/libltc-wrapper"),
    out = path.resolve(process.env.FREESHOW_LTC_ORACLE_DIR || path.join(root, "target/ltc-original"))
const headers = path.resolve(process.env.FREESHOW_NODE_HEADERS || path.join(path.dirname(process.execPath), "../include/node"))
const inputs = { "libltc/bin/libltc.so.11": "f02c75f982fe01d16f345875d2b09404b8b326bb847f2b5456616f6d0227d09f", "libltc/include/ltc.h": "a2d5666fb300ffd05cbfada622029f2647dc26ba3ff03a834e3d5bd7ff77dff7", "src/ltcnative.c": "eda296ab102d1af0676cd593be7b704fb69d68bc4be33ac78b462b20e26e9036", "src/ltcnative.h": "2b38910857fb4375d5b396ffa4ebc5c8a0da4b8e8e08f9c97a5f23578dd46b30", "dist/binding.js": "11add39f95a974541316c242cb1558ae1863646e1abddeb258282e2e758e4717" }
const sha = (file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex")
for (const [file, expected] of Object.entries(inputs)) if (sha(path.join(original, file)) !== expected) throw Error("Unexpected pinned input " + file)
fs.mkdirSync(out, { recursive: true })
fs.copyFileSync(path.join(original, "libltc/bin/libltc.so.11"), path.join(out, "libltc.so.11"))
const args = ["-shared", "-fPIC", "-O2", "-I", headers, "-I", path.join(original, "libltc/include"), path.join(original, "src/ltcnative.c"), path.join(out, "libltc.so.11"), "-Wl,-rpath,$ORIGIN", "-o", path.join(out, "ltc-native.node")]
const compiled = spawnSync("cc", args, { encoding: "utf8", timeout: 30000 })
if (compiled.status !== 0) throw Error(compiled.stderr)
const receipt = { node: process.version, platform: process.platform, architecture: process.arch, inputs: { ...inputs, "src/ltcnative.c": sha(path.join(original, "src/ltcnative.c")), "src/ltcnative.h": sha(path.join(original, "src/ltcnative.h")), "dist/binding.js": sha(path.join(original, "dist/binding.js")) }, addon: sha(path.join(out, "ltc-native.node")), compiler: spawnSync("cc", ["--version"], { encoding: "utf8" }).stdout.split("\n")[0], args }
fs.writeFileSync(path.join(out, "receipt.json"), JSON.stringify(receipt, null, 2) + "\n")
console.log(JSON.stringify({ addon: path.join(out, "ltc-native.node"), receipt: path.join(out, "receipt.json") }))
