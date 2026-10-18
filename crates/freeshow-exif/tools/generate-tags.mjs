// Source table generation only; never invoked by production requests.
import assert from "node:assert/strict"
import fs from "node:fs"
import { createHash } from "node:crypto"
import { createRequire } from "node:module"
const require = createRequire(import.meta.url)
const { ExifImage } = require("exif")
assert.equal(require("exif/package.json").version, "0.6.0")
const source = fs.readFileSync(require.resolve("exif/lib/exif/ExifImage.js"))
const sha256 = createHash("sha256").update(source).digest("hex")
let output = `// Generated from untouched exif 0.6.0 TAGS; source SHA256 ${sha256}.\n// Names are source facts. See NOTICE for the upstream MIT license.\n`
for (const [group, table] of Object.entries(ExifImage.TAGS)) {
    output += `pub(crate) const ${group.toUpperCase()}: &[(u16, &str)] = &[\n`
    for (const [id, name] of Object.entries(table).sort(([a], [b]) => Number(a) - Number(b))) {
        assert.equal(typeof name, "string")
        output += `    (${id}, ${JSON.stringify(name)}),\n`
    }
    output += "];\n"
}
fs.mkdirSync(new URL("../src", import.meta.url), { recursive: true })
fs.writeFileSync(new URL("../src/tags.rs", import.meta.url), output)
fs.writeFileSync(new URL("../NOTICE", import.meta.url), "The source-compatible parser and generated tag names were developed from node-exif 0.6.0 by Daniel Leinich and Olivier Oeuillot.\nUpstream source SHA256: " + sha256 + "\n\n" + fs.readFileSync(require.resolve("exif/LICENSE.md"), "utf8"))
