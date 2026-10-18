import assert from "node:assert/strict"
import fs from "node:fs"
import { createHash } from "node:crypto"
import { fileURLToPath } from "node:url"
import { spawnSync } from "node:child_process"
import ts from "typescript"
import { withElectron } from "./electron.mjs"

const sourceFile = new URL("../../../src/electron/utils/files.ts", import.meta.url)
const source = fs.readFileSync(sourceFile, "utf8")
const ast = ts.createSourceFile("files.ts", source, ts.ScriptTarget.Latest, true)
const declaration = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "normalizeFileNameForMatch").getText(ast)
const javascript = ts.transpileModule(declaration, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
const sha = (data) => createHash("sha256").update(data).digest("hex")
const data = await withElectron(async (app, runtime, executablePath) => {
    const result = await app.evaluate((_, javascript) => {
        const normalize = new Function(javascript + ";return normalizeFileNameForMatch")()
        const mappings = [],
            folded = [],
            properties = { CASED: [], CASE_IGNORABLE: [] }
        const expressions = { CASED: /\p{Cased}/u, CASE_IGNORABLE: /\p{Case_Ignorable}/u }
        for (let cp = 0; cp <= 0x10ffff; cp++) {
            if (cp >= 0xd800 && cp <= 0xdfff) continue
            const character = String.fromCodePoint(cp),
                lower = character.toLowerCase(),
                fold = normalize(character)
            if (character !== lower) mappings.push([cp, lower])
            if (character !== fold) folded.push([cp, fold])
            for (const [name, expression] of Object.entries(expressions)) {
                if (!expression.test(character)) continue
                const ranges = properties[name]
                if (ranges.at(-1)?.[1] === cp - 1) ranges.at(-1)[1] = cp
                else ranges.push([cp, cp])
            }
        }
        const contexts = ["AΣ", "AΣA", "Σ", "A\u0345Σ", "\u0345Σ", "AΣ\u0345", "AΣ\u0345A", "A'Σ", "AΣ'B", "ΟΣ.JPG", "Οσ.JPG", "Ος.JPG", "İıI", "\u212b\u0301", "a\u0315\u0300", "a\u0300\u0315", "\u1100\u1161\u11a8", "\u{113c2}\u{113b8}"]
        let seed = 151
        const alphabet = ["A", "Σ", "İ", "a", "\u0300", "\u0315", "\u0345", "\u0301", "\u1100", "\u1161", "\u11a8", "\u212b", "\u{10d50}", "\u{113c2}", "\u{113b8}"]
        for (let i = 0; i < 512; i++) {
            let text = ""
            for (let j = 0; j < 8; j++) {
                seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
                text += alphabet[seed % alphabet.length]
            }
            contexts.push(text)
        }
        for (const ranges of Object.values(properties))
            for (const [start, end] of ranges)
                for (const cp of [start - 1, start, end, end + 1])
                    if (cp >= 0 && cp <= 0x10ffff && (cp < 0xd800 || cp > 0xdfff)) {
                        const c = String.fromCodePoint(cp)
                        contexts.push(`${c}Σ`, `AΣ${c}`, `A${c}Σ`, `AΣ${c}A`)
                    }
        return { mappings, properties, folded, contexts: [...new Set(contexts)].map((text) => [text, normalize(text)]) }
    }, javascript)
    return { schemaVersion: 1, sourceCommit: "2d94defcf98175e4f5c9504e7f4fd1d33c35c975", sourceFunctionSha256: sha(declaration), executableSha256: sha(fs.readFileSync(executablePath)), runtime: { version: runtime.version, packaged: runtime.packaged, versions: runtime.versions }, ...result }
})
const hex = (value) => "0x" + value.toString(16)
const literal = (text) => '"' + [...text].map((character) => `\\u{${character.codePointAt(0).toString(16)}}`).join("") + '"'
let output = "// Generated from official FreeShow 1.6.7-beta.2: Electron 37.10.3, ICU 74.2, Unicode 15.1.\n// tools/generate-case-table.mjs records source/runtime checksums in tests/unicode-oracle.json.\n// Contextual Final_Sigma is handled in paths.rs.\n"
output += "pub static LOWERCASE: &[(u32, &str)] = &[\n" + data.mappings.map(([cp, lower]) => `    (${hex(cp)}, ${literal(lower)}),`).join("\n") + "\n];\n"
for (const [name, ranges] of Object.entries(data.properties)) output += `pub static ${name}: &[(u32, u32)] = &[\n` + ranges.map(([start, end]) => `    (${hex(start)}, ${hex(end)}),`).join("\n") + "\n];\n"
const formatted = spawnSync("rustfmt", ["--emit", "stdout", "--edition", "2021"], { input: output, encoding: "utf8" })
assert.equal(formatted.status, 0, formatted.stderr)
const table = fileURLToPath(new URL("../src/case_table.rs", import.meta.url))
const corpus = fileURLToPath(new URL("../tests/unicode-oracle.json", import.meta.url))
const serialized = JSON.stringify(data) + "\n"
if (process.argv.includes("--check")) {
    assert.equal(fs.readFileSync(table, "utf8"), formatted.stdout, "Generated Rust Unicode table drift")
    assert.equal(fs.readFileSync(corpus, "utf8"), serialized, "Packaged Electron Unicode corpus drift")
} else {
    fs.mkdirSync(new URL("../tests/", import.meta.url), { recursive: true })
    fs.writeFileSync(table, formatted.stdout)
    fs.writeFileSync(corpus, serialized)
}
process.stdout.write(JSON.stringify({ version: data.runtime.version, icu: data.runtime.versions.icu, unicode: data.runtime.versions.unicode, lowerMappings: data.mappings.length, foldedScalars: data.folded.length, contexts: data.contexts.length, executableSha256: data.executableSha256 }) + "\n")
