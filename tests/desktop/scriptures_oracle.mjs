import fs from "node:fs"
import { scriptureOracle } from "../parity/lib/scriptures-oracle.mjs"
const fixture = JSON.parse(fs.readFileSync(0, "utf8"))
const source = scriptureOracle(fixture.dataRoot, fixture.documentsRoot)
const replies = fixture.payloads.map((payload) => structuredClone(source.load(payload)))
process.stdout.write(JSON.stringify({ replies, loads: source.loads, alerts: source.alerts, errors: source.errors.length }))
