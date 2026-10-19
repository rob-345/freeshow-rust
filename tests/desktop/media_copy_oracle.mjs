import fs from "node:fs"
import { mediaCopyOracle } from "../parity/lib/media-copy-oracle.mjs"
const fixture = JSON.parse(fs.readFileSync(0, "utf8"))
const source = mediaCopyOracle(fixture)
const reply = await source[fixture.command](fixture.data)
await Promise.all(source.pending)
process.stdout.write(JSON.stringify({ reply: reply === undefined ? null : reply, alerts: source.alerts, opened: source.opened, copies: source.copies, folders: source.folders, errors: source.errors.length }))
